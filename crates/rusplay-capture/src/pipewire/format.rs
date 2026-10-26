use std::{io::Cursor, mem};

use pipewire::spa::{
    param::{
        ParamType,
        format::{FormatProperties, MediaSubtype, MediaType},
        video::VideoFormat,
    },
    pod::{
        ChoiceValue, Object, Property, PropertyFlags, Value, deserialize::PodDeserializer,
        serialize::PodSerializer,
    },
    sys,
    utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes},
};
use rusplay_core::{ColorPrimaries, ColorSpace, FrameRate, Resolution};

use crate::{
    CaptureError, DrmFourcc,
    modifier::{FormatModifiers, choose_modifier, modifiers_for},
    pipewire::color::{
        DESCRIBABLE_PRIMARIES, DESCRIBABLE_TRANSFERS, SpaColorimetry, spa_primaries_for,
        spa_transfer_for,
    },
};

const FORMAT_TABLE: [(VideoFormat, DrmFourcc); 16] = [
    (VideoFormat::BGRx, DrmFourcc::from_code(*b"XR24")),
    (VideoFormat::BGRA, DrmFourcc::from_code(*b"AR24")),
    (VideoFormat::RGBx, DrmFourcc::from_code(*b"XB24")),
    (VideoFormat::RGBA, DrmFourcc::from_code(*b"AB24")),
    (VideoFormat::xRGB, DrmFourcc::from_code(*b"BX24")),
    (VideoFormat::ARGB, DrmFourcc::from_code(*b"BA24")),
    (VideoFormat::xBGR, DrmFourcc::from_code(*b"RX24")),
    (VideoFormat::ABGR, DrmFourcc::from_code(*b"RA24")),
    (VideoFormat::xRGB_210LE, DrmFourcc::from_code(*b"XR30")),
    (VideoFormat::ARGB_210LE, DrmFourcc::from_code(*b"AR30")),
    (VideoFormat::xBGR_210LE, DrmFourcc::from_code(*b"XB30")),
    (VideoFormat::ABGR_210LE, DrmFourcc::from_code(*b"AB30")),
    (VideoFormat::RGBx_102LE, DrmFourcc::from_code(*b"RX30")),
    (VideoFormat::RGBA_102LE, DrmFourcc::from_code(*b"RA30")),
    (VideoFormat::BGRx_102LE, DrmFourcc::from_code(*b"BX30")),
    (VideoFormat::BGRA_102LE, DrmFourcc::from_code(*b"BA30")),
];

const EIGHT_BIT_FORMATS: [DrmFourcc; 4] = [
    DrmFourcc::from_code(*b"XR24"),
    DrmFourcc::from_code(*b"AR24"),
    DrmFourcc::from_code(*b"XB24"),
    DrmFourcc::from_code(*b"AB24"),
];

const TEN_BIT_FORMATS: [DrmFourcc; 4] = [
    DrmFourcc::from_code(*b"XR30"),
    DrmFourcc::from_code(*b"AR30"),
    DrmFourcc::from_code(*b"XB30"),
    DrmFourcc::from_code(*b"AB30"),
];

const FALLBACK_SIZE: Resolution = Resolution::new(1920, 1080);
const MIN_SIZE: Rectangle = Rectangle {
    width: 1,
    height: 1,
};
const MAX_SIZE: Rectangle = Rectangle {
    width: 16384,
    height: 16384,
};
const MAX_FRAME_RATE: Fraction = Fraction {
    num: 1000,
    denom: 1,
};
const MIN_FRAME_RATE: Fraction = Fraction { num: 0, denom: 1 };

const PROP_FLAG_DROP: u32 = 1 << 5;

const DEFAULT_BUFFER_COUNT: i32 = 4;
const MIN_BUFFER_COUNT: i32 = 2;
const MAX_BUFFER_COUNT: i32 = 16;

pub fn default_formats(ten_bit_first: bool) -> Vec<DrmFourcc> {
    let (first, second) = if ten_bit_first {
        (TEN_BIT_FORMATS, EIGHT_BIT_FORMATS)
    } else {
        (EIGHT_BIT_FORMATS, TEN_BIT_FORMATS)
    };

    first.into_iter().chain(second).collect()
}

pub(crate) fn spa_format_for(fourcc: DrmFourcc) -> Option<VideoFormat> {
    FORMAT_TABLE
        .iter()
        .find(|(_, candidate)| *candidate == fourcc)
        .map(|(spa, _)| *spa)
}

pub(crate) fn fourcc_for(spa: VideoFormat) -> Option<DrmFourcc> {
    FORMAT_TABLE
        .iter()
        .find(|(candidate, _)| *candidate == spa)
        .map(|(_, fourcc)| *fourcc)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FormatRequest {
    pub size_hint: Option<Resolution>,
    pub frame_rate: FrameRate,
    pub color: Option<ColorOffer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColorOffer {
    pub transfer: u32,
    pub primaries: u32,
}

impl ColorOffer {
    pub fn preferring(color: &ColorSpace) -> Self {
        let fallback = if color.transfer.is_hdr() {
            ColorPrimaries::Bt2020
        } else {
            ColorPrimaries::Bt709
        };

        Self {
            transfer: spa_transfer_for(color.transfer),
            primaries: spa_primaries_for(color.primaries)
                .or(spa_primaries_for(fallback))
                .unwrap_or(sys::SPA_VIDEO_COLOR_PRIMARIES_UNKNOWN),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NegotiatedFormat {
    pub fourcc: DrmFourcc,
    pub size: Resolution,
    pub modifier: Option<u64>,
    pub color: ColorSpace,
}

impl NegotiatedFormat {
    pub fn memory(self) -> BufferMemory {
        match self.modifier {
            Some(_) => BufferMemory::DmaBuf,
            None => BufferMemory::Shared,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModifierOffer {
    Absent,
    Fixed(u64),
    Choice(Vec<u64>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedFormat {
    pub fourcc: DrmFourcc,
    pub size: Resolution,
    pub modifiers: ModifierOffer,
    pub colorimetry: SpaColorimetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormatDecision {
    Accept(NegotiatedFormat),
    Fixate { fourcc: DrmFourcc, modifier: u64 },
    FallBackToShm,
}

pub(crate) fn decide(
    parsed: &ParsedFormat,
    table: &[FormatModifiers],
    color: ColorSpace,
) -> FormatDecision {
    let accept = |modifier| {
        FormatDecision::Accept(NegotiatedFormat {
            fourcc: parsed.fourcc,
            size: parsed.size,
            modifier,
            color,
        })
    };

    match &parsed.modifiers {
        ModifierOffer::Absent => accept(None),
        ModifierOffer::Fixed(modifier) => accept(Some(*modifier)),
        ModifierOffer::Choice(offered) => {
            match choose_modifier(modifiers_for(table, parsed.fourcc), offered) {
                Some(modifier) => FormatDecision::Fixate {
                    fourcc: parsed.fourcc,
                    modifier,
                },
                None => FormatDecision::FallBackToShm,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BufferMemory {
    Shared,
    DmaBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModifierRequest<'a> {
    Negotiate(&'a [u64]),
    Fixed(u64),
}

pub(crate) fn dmabuf_enum_format(
    fourcc: DrmFourcc,
    modifiers: ModifierRequest<'_>,
    request: FormatRequest,
) -> Result<Option<Vec<u8>>, CaptureError> {
    let Some(spa) = spa_format_for(fourcc) else {
        return Ok(None);
    };
    let modifier = match modifiers {
        ModifierRequest::Negotiate(modifiers) => {
            let values: Vec<i64> = modifiers
                .iter()
                .map(|modifier| modifier.cast_signed())
                .collect();
            let Some(&default) = values.first() else {
                return Ok(None);
            };
            Property {
                key: FormatProperties::VideoModifier.as_raw(),
                flags: PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE,
                value: Value::Choice(ChoiceValue::Long(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default,
                        alternatives: values,
                    },
                ))),
            }
        }
        ModifierRequest::Fixed(modifier) => Property {
            key: FormatProperties::VideoModifier.as_raw(),
            flags: PropertyFlags::MANDATORY,
            value: Value::Long(modifier.cast_signed()),
        },
    };

    let mut properties = media_properties();
    properties.push(Property::new(
        FormatProperties::VideoFormat.as_raw(),
        Value::Id(Id(spa.as_raw())),
    ));
    properties.push(modifier);
    properties.extend(geometry_properties(request));
    properties.extend(color_properties(request.color));

    serialize(Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties,
    })
    .map(Some)
}

pub(crate) fn enum_formats(
    formats: &[DrmFourcc],
    dmabuf: Option<&[FormatModifiers]>,
    request: FormatRequest,
) -> Result<Vec<Vec<u8>>, CaptureError> {
    let mut pods = Vec::new();

    if let Some(table) = dmabuf {
        for fourcc in formats {
            let modifiers = ModifierRequest::Negotiate(modifiers_for(table, *fourcc));
            pods.extend(dmabuf_enum_format(*fourcc, modifiers, request)?);
        }
    }
    pods.extend(shm_enum_format(formats, request)?);

    Ok(pods)
}

pub(crate) fn shm_enum_format(
    formats: &[DrmFourcc],
    request: FormatRequest,
) -> Result<Option<Vec<u8>>, CaptureError> {
    let spa_formats: Vec<Id> = formats
        .iter()
        .filter_map(|fourcc| spa_format_for(*fourcc))
        .map(|spa| Id(spa.as_raw()))
        .collect();
    let Some(&default) = spa_formats.first() else {
        return Ok(None);
    };

    let format = Value::Choice(ChoiceValue::Id(Choice(
        ChoiceFlags::empty(),
        ChoiceEnum::Enum {
            default,
            alternatives: spa_formats,
        },
    )));

    let mut properties = media_properties();
    properties.push(Property::new(
        FormatProperties::VideoFormat.as_raw(),
        format,
    ));
    properties.extend(geometry_properties(request));
    properties.extend(color_properties(request.color));

    serialize(Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties,
    })
    .map(Some)
}

fn media_properties() -> Vec<Property> {
    vec![
        Property::new(
            FormatProperties::MediaType.as_raw(),
            Value::Id(Id(MediaType::Video.as_raw())),
        ),
        Property::new(
            FormatProperties::MediaSubtype.as_raw(),
            Value::Id(Id(MediaSubtype::Raw.as_raw())),
        ),
    ]
}

fn color_properties(offer: Option<ColorOffer>) -> Vec<Property> {
    let Some(offer) = offer else {
        return Vec::new();
    };

    vec![
        preferred_id(
            FormatProperties::VideoTransferFunction,
            offer.transfer,
            &DESCRIBABLE_TRANSFERS,
        ),
        preferred_id(
            FormatProperties::VideoColorPrimaries,
            offer.primaries,
            &DESCRIBABLE_PRIMARIES,
        ),
    ]
}

fn preferred_id(key: FormatProperties, preferred: u32, all: &[u32]) -> Property {
    let alternatives = [preferred]
        .into_iter()
        .chain(all.iter().copied().filter(|id| *id != preferred))
        .map(Id)
        .collect();

    Property {
        key: key.as_raw(),
        flags: PropertyFlags::from_bits_retain(PROP_FLAG_DROP),
        value: Value::Choice(ChoiceValue::Id(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Enum {
                default: Id(preferred),
                alternatives,
            },
        ))),
    }
}

fn geometry_properties(request: FormatRequest) -> [Property; 2] {
    let size = request.size_hint.unwrap_or(FALLBACK_SIZE);
    let default_size = Rectangle {
        width: size.width,
        height: size.height,
    };
    let default_rate = Fraction {
        num: request.frame_rate.numerator,
        denom: request.frame_rate.denominator,
    };

    [
        Property::new(
            FormatProperties::VideoSize.as_raw(),
            Value::Choice(ChoiceValue::Rectangle(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Range {
                    default: default_size,
                    min: MIN_SIZE,
                    max: MAX_SIZE,
                },
            ))),
        ),
        Property::new(
            FormatProperties::VideoFramerate.as_raw(),
            Value::Choice(ChoiceValue::Fraction(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Range {
                    default: default_rate,
                    min: MIN_FRAME_RATE,
                    max: MAX_FRAME_RATE,
                },
            ))),
        ),
    ]
}

pub(crate) fn buffers_param(memory: BufferMemory) -> Result<Vec<u8>, CaptureError> {
    let data_types = match memory {
        BufferMemory::Shared => (1 << sys::SPA_DATA_MemPtr) | (1 << sys::SPA_DATA_MemFd),
        BufferMemory::DmaBuf => 1 << sys::SPA_DATA_DmaBuf,
    };

    serialize(Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            Property::new(
                sys::SPA_PARAM_BUFFERS_buffers,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: DEFAULT_BUFFER_COUNT,
                        min: MIN_BUFFER_COUNT,
                        max: MAX_BUFFER_COUNT,
                    },
                ))),
            ),
            Property::new(
                sys::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Flags {
                        default: data_types,
                        flags: Vec::new(),
                    },
                ))),
            ),
        ],
    })
}

pub(crate) fn meta_param(meta_type: u32, size: MetaSize) -> Result<Vec<u8>, CaptureError> {
    let size = match size {
        MetaSize::Fixed(size) => Value::Int(size),
        MetaSize::Range { default, min, max } => Value::Choice(ChoiceValue::Int(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range { default, min, max },
        ))),
    };

    serialize(Object {
        type_: SpaTypes::ObjectParamMeta.as_raw(),
        id: ParamType::Meta.as_raw(),
        properties: vec![
            Property::new(sys::SPA_PARAM_META_type, Value::Id(Id(meta_type))),
            Property::new(sys::SPA_PARAM_META_size, size),
        ],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetaSize {
    Fixed(i32),
    Range { default: i32, min: i32, max: i32 },
}

pub(crate) fn header_meta_param() -> Result<Vec<u8>, CaptureError> {
    meta_param(
        sys::SPA_META_Header,
        MetaSize::Fixed(size_as_i32(mem::size_of::<sys::spa_meta_header>())),
    )
}

pub(crate) fn size_as_i32(size: usize) -> i32 {
    i32::try_from(size).unwrap_or(i32::MAX)
}

fn serialize(object: Object) -> Result<Vec<u8>, CaptureError> {
    let param = object.id;
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|_| CaptureError::PodSerialize { param })
}

pub(crate) fn parse_object(bytes: &[u8]) -> Result<Object, CaptureError> {
    match PodDeserializer::deserialize_any_from(bytes) {
        Ok((_, Value::Object(object))) => Ok(object),
        _ => Err(CaptureError::FormatPodMalformed),
    }
}

pub(crate) fn parse_format(bytes: &[u8]) -> Result<ParsedFormat, CaptureError> {
    let object = parse_object(bytes)?;

    let spa_format = fixed_id(property(&object, FormatProperties::VideoFormat.as_raw())?)
        .ok_or(CaptureError::FormatPodMalformed)?;
    let size = fixed_rectangle(property(&object, FormatProperties::VideoSize.as_raw())?)
        .ok_or(CaptureError::FormatPodMalformed)?;
    let fourcc = fourcc_for(VideoFormat(spa_format))
        .ok_or(CaptureError::UnsupportedVideoFormat { spa_format })?;

    let modifiers = match property(&object, FormatProperties::VideoModifier.as_raw()) {
        Ok(modifier) => modifier_offer(modifier).ok_or(CaptureError::FormatPodMalformed)?,
        Err(_) => ModifierOffer::Absent,
    };

    Ok(ParsedFormat {
        fourcc,
        size: Resolution::new(size.width, size.height),
        modifiers,
        colorimetry: parse_colorimetry(&object)?,
    })
}

fn parse_colorimetry(object: &Object) -> Result<SpaColorimetry, CaptureError> {
    let id = |key: FormatProperties| match property(object, key.as_raw()) {
        Ok(property) => chosen_id(property).ok_or(CaptureError::FormatPodMalformed),
        Err(_) => Ok(0),
    };

    Ok(SpaColorimetry {
        range: id(FormatProperties::VideoColorRange)?,
        matrix: id(FormatProperties::VideoColorMatrix)?,
        transfer: id(FormatProperties::VideoTransferFunction)?,
        primaries: id(FormatProperties::VideoColorPrimaries)?,
    })
}

fn modifier_offer(property: &Property) -> Option<ModifierOffer> {
    match &property.value {
        Value::Long(value)
        | Value::Choice(ChoiceValue::Long(Choice(_, ChoiceEnum::None(value)))) => {
            Some(ModifierOffer::Fixed(value.cast_unsigned()))
        }
        Value::Choice(ChoiceValue::Long(Choice(
            _,
            ChoiceEnum::Enum {
                default,
                alternatives,
            },
        ))) => {
            let mut offered = vec![default.cast_unsigned()];
            for alternative in alternatives {
                let alternative = alternative.cast_unsigned();
                if !offered.contains(&alternative) {
                    offered.push(alternative);
                }
            }
            Some(ModifierOffer::Choice(offered))
        }
        _ => None,
    }
}

pub(crate) fn property(object: &Object, key: u32) -> Result<&Property, CaptureError> {
    object
        .properties
        .iter()
        .find(|property| property.key == key)
        .ok_or(CaptureError::FormatPropertyMissing { key })
}

fn fixed_id(property: &Property) -> Option<u32> {
    match &property.value {
        Value::Id(Id(id)) | Value::Choice(ChoiceValue::Id(Choice(_, ChoiceEnum::None(Id(id))))) => {
            Some(*id)
        }
        _ => None,
    }
}

fn chosen_id(property: &Property) -> Option<u32> {
    match &property.value {
        Value::Choice(ChoiceValue::Id(Choice(_, ChoiceEnum::Enum { default, .. }))) => {
            Some(default.0)
        }
        _ => fixed_id(property),
    }
}

fn fixed_rectangle(property: &Property) -> Option<Rectangle> {
    match &property.value {
        Value::Rectangle(rectangle)
        | Value::Choice(ChoiceValue::Rectangle(Choice(_, ChoiceEnum::None(rectangle)))) => {
            Some(*rectangle)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rusplay_core::{ColorPrimaries, ColorRange, HdrLevels, TransferFunction};

    use super::*;
    use crate::{
        modifier::{DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR},
        pipewire::color::describe_color,
    };

    const XR24: DrmFourcc = DrmFourcc::from_code(*b"XR24");
    const AMD_TILED: u64 = 0x0200_0000_0000_0001;

    fn request() -> FormatRequest {
        FormatRequest {
            size_hint: Some(Resolution::new(2560, 1440)),
            frame_rate: FrameRate::integer(144),
            color: None,
        }
    }

    fn fixed_format(spa: VideoFormat, width: u32, height: u32) -> Vec<u8> {
        let mut properties = media_properties();
        properties.push(Property::new(
            FormatProperties::VideoFormat.as_raw(),
            Value::Id(Id(spa.as_raw())),
        ));
        properties.push(Property::new(
            FormatProperties::VideoSize.as_raw(),
            Value::Rectangle(Rectangle { width, height }),
        ));
        serialize(Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::Format.as_raw(),
            properties,
        })
        .unwrap()
    }

    fn with_colorimetry(bytes: &[u8], colorimetry: SpaColorimetry) -> Vec<u8> {
        let mut object = parse_object(bytes).unwrap();
        for (key, id) in [
            (FormatProperties::VideoColorRange, colorimetry.range),
            (FormatProperties::VideoColorMatrix, colorimetry.matrix),
            (
                FormatProperties::VideoTransferFunction,
                colorimetry.transfer,
            ),
            (FormatProperties::VideoColorPrimaries, colorimetry.primaries),
        ] {
            object
                .properties
                .push(Property::new(key.as_raw(), Value::Id(Id(id))));
        }
        serialize(object).unwrap()
    }

    fn with_modifier(bytes: &[u8], modifier: Property) -> Vec<u8> {
        let mut object = parse_object(bytes).unwrap();
        object.properties.push(modifier);
        serialize(object).unwrap()
    }

    fn table() -> Vec<FormatModifiers> {
        vec![FormatModifiers::new(
            XR24,
            [AMD_TILED, DRM_FORMAT_MOD_LINEAR],
        )]
    }

    #[test]
    fn every_mapped_format_round_trips() {
        for (spa, fourcc) in FORMAT_TABLE {
            assert_eq!(spa_format_for(fourcc), Some(spa));
            assert_eq!(fourcc_for(spa), Some(fourcc));
        }
    }

    #[test]
    fn default_formats_follow_the_requested_depth() {
        let eight = default_formats(false);
        let ten = default_formats(true);

        assert_eq!(eight[0], DrmFourcc::from_code(*b"XR24"));
        assert_eq!(ten[0], DrmFourcc::from_code(*b"XR30"));
        assert_eq!(eight.len(), ten.len());
    }

    #[test]
    fn shm_enum_format_lists_formats_in_preference_order() {
        let formats = default_formats(false);

        let bytes = shm_enum_format(&formats, request()).unwrap().unwrap();
        let object = parse_object(&bytes).unwrap();
        let format = property(&object, FormatProperties::VideoFormat.as_raw()).unwrap();
        let size = property(&object, FormatProperties::VideoSize.as_raw()).unwrap();

        assert_eq!(object.id, ParamType::EnumFormat.as_raw());
        let Value::Choice(ChoiceValue::Id(Choice(
            _,
            ChoiceEnum::Enum {
                default,
                alternatives,
            },
        ))) = &format.value
        else {
            panic!("expected an enum choice, got {:?}", format.value);
        };
        assert_eq!(*default, Id(VideoFormat::BGRx.as_raw()));
        assert_eq!(alternatives.len(), formats.len());
        assert!(matches!(
            &size.value,
            Value::Choice(ChoiceValue::Rectangle(Choice(
                _,
                ChoiceEnum::Range {
                    default: Rectangle {
                        width: 2560,
                        height: 1440
                    },
                    ..
                }
            )))
        ));
    }

    #[test]
    fn shm_enum_format_skips_unmapped_formats() {
        let formats = [DrmFourcc::from_code(*b"NV12")];

        assert_eq!(shm_enum_format(&formats, request()).unwrap(), None);
    }

    #[test]
    fn fixed_format_parses() {
        let bytes = fixed_format(VideoFormat::BGRx, 1920, 1080);

        let format = parse_format(&bytes).unwrap();

        assert_eq!(format.fourcc, DrmFourcc::from_code(*b"XR24"));
        assert_eq!(format.size, Resolution::new(1920, 1080));
        assert_eq!(format.modifiers, ModifierOffer::Absent);
    }

    #[test]
    fn a_format_without_colour_properties_has_unknown_colorimetry() {
        let bytes = fixed_format(VideoFormat::BGRx, 64, 64);

        let format = parse_format(&bytes).unwrap();

        assert_eq!(format.colorimetry, SpaColorimetry::default());
    }

    #[test]
    fn a_ten_bit_hdr_format_reports_its_colorimetry() {
        let bytes = with_colorimetry(
            &fixed_format(VideoFormat::xBGR_210LE, 3840, 2160),
            SpaColorimetry {
                range: sys::SPA_VIDEO_COLOR_RANGE_0_255,
                matrix: sys::SPA_VIDEO_COLOR_MATRIX_RGB,
                transfer: sys::SPA_VIDEO_TRANSFER_SMPTE2084,
                primaries: sys::SPA_VIDEO_COLOR_PRIMARIES_BT2020,
            },
        );

        let format = parse_format(&bytes).unwrap();
        let color = describe_color(&format.colorimetry, HdrLevels::default()).unwrap();

        assert_eq!(format.fourcc, DrmFourcc::from_code(*b"XB30"));
        assert_eq!(color.primaries, ColorPrimaries::Bt2020);
        assert_eq!(color.transfer, TransferFunction::Pq);
        assert_eq!(color.range, ColorRange::Full);
    }

    #[test]
    fn a_malformed_colour_property_is_rejected() {
        let bytes = with_modifier(
            &fixed_format(VideoFormat::BGRx, 64, 64),
            Property::new(
                FormatProperties::VideoTransferFunction.as_raw(),
                Value::Int(1),
            ),
        );

        assert!(matches!(
            parse_format(&bytes),
            Err(CaptureError::FormatPodMalformed)
        ));
    }

    #[test]
    fn unmapped_format_is_rejected() {
        let bytes = fixed_format(VideoFormat::NV12, 1920, 1080);

        assert!(matches!(
            parse_format(&bytes),
            Err(CaptureError::UnsupportedVideoFormat { .. })
        ));
    }

    #[test]
    fn buffers_param_requests_shared_memory() {
        let bytes = buffers_param(BufferMemory::Shared).unwrap();
        let object = parse_object(&bytes).unwrap();
        let data_type = property(&object, sys::SPA_PARAM_BUFFERS_dataType).unwrap();

        let Value::Choice(ChoiceValue::Int(Choice(_, ChoiceEnum::Flags { default, .. }))) =
            &data_type.value
        else {
            panic!("expected a flags choice, got {:?}", data_type.value);
        };
        assert_eq!(
            *default,
            (1 << sys::SPA_DATA_MemPtr) | (1 << sys::SPA_DATA_MemFd)
        );
    }

    #[test]
    fn header_meta_param_has_the_header_size() {
        let bytes = header_meta_param().unwrap();
        let object = parse_object(&bytes).unwrap();

        let size = property(&object, sys::SPA_PARAM_META_size).unwrap();

        assert_eq!(
            size.value,
            Value::Int(size_as_i32(mem::size_of::<sys::spa_meta_header>()))
        );
    }

    #[test]
    fn dmabuf_enum_format_offers_modifiers_without_fixation() {
        let modifiers = [AMD_TILED, DRM_FORMAT_MOD_INVALID];

        let bytes = dmabuf_enum_format(XR24, ModifierRequest::Negotiate(&modifiers), request())
            .unwrap()
            .unwrap();
        let object = parse_object(&bytes).unwrap();
        let modifier = property(&object, FormatProperties::VideoModifier.as_raw()).unwrap();

        assert!(modifier.flags.contains(PropertyFlags::MANDATORY));
        assert!(modifier.flags.contains(PropertyFlags::DONT_FIXATE));
        assert_eq!(
            modifier_offer(modifier),
            Some(ModifierOffer::Choice(modifiers.to_vec()))
        );
    }

    #[test]
    fn fixated_dmabuf_enum_format_carries_one_modifier() {
        let bytes = dmabuf_enum_format(XR24, ModifierRequest::Fixed(AMD_TILED), request())
            .unwrap()
            .unwrap();
        let object = parse_object(&bytes).unwrap();
        let modifier = property(&object, FormatProperties::VideoModifier.as_raw()).unwrap();

        assert!(!modifier.flags.contains(PropertyFlags::DONT_FIXATE));
        assert_eq!(
            modifier_offer(modifier),
            Some(ModifierOffer::Fixed(AMD_TILED))
        );
    }

    #[test]
    fn formats_without_modifiers_get_no_dmabuf_pod() {
        let negotiate = ModifierRequest::Negotiate(&[]);

        assert_eq!(
            dmabuf_enum_format(XR24, negotiate, request()).unwrap(),
            None
        );
    }

    #[test]
    fn enum_formats_put_dmabuf_before_shared_memory() {
        let formats = [XR24, DrmFourcc::from_code(*b"AR24")];

        let with_dmabuf = enum_formats(&formats, Some(&table()), request()).unwrap();
        let without = enum_formats(&formats, None, request()).unwrap();

        assert_eq!(with_dmabuf.len(), 2);
        assert_eq!(without.len(), 1);
        let first = parse_object(&with_dmabuf[0]).unwrap();
        assert!(property(&first, FormatProperties::VideoModifier.as_raw()).is_ok());
        let last = parse_object(&with_dmabuf[1]).unwrap();
        assert!(property(&last, FormatProperties::VideoModifier.as_raw()).is_err());
    }

    #[test]
    fn modifier_choice_is_fixated_with_our_preference() {
        let bytes = with_modifier(
            &fixed_format(VideoFormat::BGRx, 1920, 1080),
            Property {
                key: FormatProperties::VideoModifier.as_raw(),
                flags: PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE,
                value: Value::Choice(ChoiceValue::Long(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: DRM_FORMAT_MOD_LINEAR.cast_signed(),
                        alternatives: vec![
                            DRM_FORMAT_MOD_LINEAR.cast_signed(),
                            AMD_TILED.cast_signed(),
                        ],
                    },
                ))),
            },
        );

        let parsed = parse_format(&bytes).unwrap();

        assert_eq!(
            parsed.modifiers,
            ModifierOffer::Choice(vec![DRM_FORMAT_MOD_LINEAR, AMD_TILED])
        );
        assert_eq!(
            decide(&parsed, &table(), ColorSpace::default()),
            FormatDecision::Fixate {
                fourcc: XR24,
                modifier: AMD_TILED
            }
        );
    }

    #[test]
    fn unacceptable_modifier_choice_falls_back_to_shared_memory() {
        let parsed = ParsedFormat {
            fourcc: XR24,
            size: Resolution::new(1920, 1080),
            modifiers: ModifierOffer::Choice(vec![DRM_FORMAT_MOD_INVALID]),
            colorimetry: SpaColorimetry::default(),
        };

        assert_eq!(
            decide(&parsed, &table(), ColorSpace::default()),
            FormatDecision::FallBackToShm
        );
    }

    #[test]
    fn fixed_modifier_is_accepted_as_dmabuf() {
        let bytes = with_modifier(
            &fixed_format(VideoFormat::BGRx, 1920, 1080),
            Property::new(
                FormatProperties::VideoModifier.as_raw(),
                Value::Long(AMD_TILED.cast_signed()),
            ),
        );

        let parsed = parse_format(&bytes).unwrap();
        let decision = decide(&parsed, &table(), ColorSpace::default());

        let FormatDecision::Accept(format) = decision else {
            panic!("expected the format to be accepted, got {decision:?}");
        };
        assert_eq!(format.modifier, Some(AMD_TILED));
        assert_eq!(format.memory(), BufferMemory::DmaBuf);
    }

    #[test]
    fn missing_modifier_is_accepted_as_shared_memory() {
        let parsed = parse_format(&fixed_format(VideoFormat::BGRx, 64, 64)).unwrap();

        let decision = decide(&parsed, &table(), ColorSpace::default());

        let FormatDecision::Accept(format) = decision else {
            panic!("expected the format to be accepted, got {decision:?}");
        };
        assert_eq!(format.memory(), BufferMemory::Shared);
    }

    #[test]
    fn buffers_param_requests_dmabuf() {
        let bytes = buffers_param(BufferMemory::DmaBuf).unwrap();
        let object = parse_object(&bytes).unwrap();
        let data_type = property(&object, sys::SPA_PARAM_BUFFERS_dataType).unwrap();

        assert!(matches!(
            &data_type.value,
            Value::Choice(ChoiceValue::Int(Choice(_, ChoiceEnum::Flags { default, .. })))
                if *default == 1 << sys::SPA_DATA_DmaBuf
        ));
    }

    fn offered_ids(object: &Object, key: FormatProperties) -> (PropertyFlags, u32, Vec<u32>) {
        let property = property(object, key.as_raw()).unwrap();
        let Value::Choice(ChoiceValue::Id(Choice(
            _,
            ChoiceEnum::Enum {
                default,
                alternatives,
            },
        ))) = &property.value
        else {
            panic!("expected an id enum, got {:?}", property.value);
        };

        (
            property.flags,
            default.0,
            alternatives.iter().map(|id| id.0).collect(),
        )
    }

    fn hdr_stream() -> ColorSpace {
        ColorSpace {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Pq,
            ..ColorSpace::default()
        }
    }

    #[test]
    fn every_enum_format_offers_the_stream_colour_first_and_drops_it_when_unmatched() {
        let offer = FormatRequest {
            color: Some(ColorOffer::preferring(&hdr_stream())),
            ..request()
        };

        let pods = enum_formats(&default_formats(true), Some(&table()), offer).unwrap();

        assert_eq!(pods.len(), 2);
        for bytes in &pods {
            let object = parse_object(bytes).unwrap();
            let (transfer_flags, transfer, transfers) =
                offered_ids(&object, FormatProperties::VideoTransferFunction);
            let (primaries_flags, primaries, all_primaries) =
                offered_ids(&object, FormatProperties::VideoColorPrimaries);

            assert_eq!(transfer, sys::SPA_VIDEO_TRANSFER_SMPTE2084);
            assert_eq!(primaries, sys::SPA_VIDEO_COLOR_PRIMARIES_BT2020);
            assert_eq!(transfers[0], transfer);
            assert_eq!(transfers.len(), DESCRIBABLE_TRANSFERS.len());
            assert_eq!(all_primaries.len(), DESCRIBABLE_PRIMARIES.len());
            for flags in [transfer_flags, primaries_flags] {
                assert_eq!(flags.bits(), PROP_FLAG_DROP);
            }
        }
    }

    #[test]
    fn an_sdr_stream_prefers_srgb_and_bt709() {
        let offer = ColorOffer::preferring(&ColorSpace::default());

        assert_eq!(offer.transfer, sys::SPA_VIDEO_TRANSFER_BT709);
        assert_eq!(offer.primaries, sys::SPA_VIDEO_COLOR_PRIMARIES_BT709);
        assert_eq!(
            ColorOffer::preferring(&ColorSpace {
                transfer: TransferFunction::Srgb,
                ..ColorSpace::default()
            })
            .transfer,
            sys::SPA_VIDEO_TRANSFER_SRGB
        );
    }

    #[test]
    fn without_an_offer_the_colour_is_left_to_the_producer() {
        let bytes = shm_enum_format(&default_formats(false), request())
            .unwrap()
            .unwrap();
        let object = parse_object(&bytes).unwrap();

        assert!(property(&object, FormatProperties::VideoTransferFunction.as_raw()).is_err());
        assert!(property(&object, FormatProperties::VideoColorPrimaries.as_raw()).is_err());
    }

    #[test]
    fn an_unfixated_colour_choice_reads_as_its_default() {
        let offer = FormatRequest {
            color: Some(ColorOffer::preferring(&hdr_stream())),
            ..request()
        };
        let mut object = parse_object(&fixed_format(VideoFormat::xBGR_210LE, 64, 64)).unwrap();
        object.properties.extend(color_properties(offer.color));

        let format = parse_format(&serialize(object).unwrap()).unwrap();

        assert_eq!(
            format.colorimetry.transfer,
            sys::SPA_VIDEO_TRANSFER_SMPTE2084
        );
        assert_eq!(
            format.colorimetry.primaries,
            sys::SPA_VIDEO_COLOR_PRIMARIES_BT2020
        );
    }
}
