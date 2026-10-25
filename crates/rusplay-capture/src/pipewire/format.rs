use std::{io::Cursor, mem};

use pipewire::spa::{
    param::{
        ParamType,
        format::{FormatProperties, MediaSubtype, MediaType},
        video::VideoFormat,
    },
    pod::{
        ChoiceValue, Object, Property, Value, deserialize::PodDeserializer,
        serialize::PodSerializer,
    },
    sys,
    utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes},
};
use rusplay_core::{FrameRate, Resolution};

use crate::{CaptureError, DrmFourcc};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NegotiatedFormat {
    pub fourcc: DrmFourcc,
    pub size: Resolution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BufferMemory {
    Shared,
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

pub(crate) fn parse_format(bytes: &[u8]) -> Result<NegotiatedFormat, CaptureError> {
    let object = parse_object(bytes)?;

    let spa_format = fixed_id(property(&object, FormatProperties::VideoFormat.as_raw())?)
        .ok_or(CaptureError::FormatPodMalformed)?;
    let size = fixed_rectangle(property(&object, FormatProperties::VideoSize.as_raw())?)
        .ok_or(CaptureError::FormatPodMalformed)?;
    let fourcc = fourcc_for(VideoFormat(spa_format))
        .ok_or(CaptureError::UnsupportedVideoFormat { spa_format })?;

    Ok(NegotiatedFormat {
        fourcc,
        size: Resolution::new(size.width, size.height),
    })
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
    use super::*;

    fn request() -> FormatRequest {
        FormatRequest {
            size_hint: Some(Resolution::new(2560, 1440)),
            frame_rate: FrameRate::integer(144),
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
}
