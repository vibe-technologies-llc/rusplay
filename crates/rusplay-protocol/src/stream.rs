use rusplay_core::{
    BitDepth, ChromaSubsampling, Chromaticities, Chromaticity, Codec, ColorPrimaries, ColorRange,
    ColorSpace, ContentLightLevel, HdrLevels, Luminance, MasteringDisplay, MatrixCoefficients,
    PixelFormat, Resolution, TransferFunction,
};

use crate::ProtocolError;

const CUSTOM_PRIMARIES: u8 = 0;
const UNSPECIFIED_MATRIX: u8 = 2;
const HAS_MASTERING: u8 = 1;
const HAS_CONTENT_LIGHT: u8 = 1 << 1;
const KNOWN_HDR_FLAGS: u8 = HAS_MASTERING | HAS_CONTENT_LIGHT;
const CHROMATICITIES_LEN: usize = 16;
const FIXED_LEN: usize = 4 + 1 + 8 + 2 + 1 + 1 + 1 + 1 + 8 + 1;
const MASTERING_FIXED_LEN: usize = 1 + 8;
const CONTENT_LIGHT_LEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamDescription {
    pub generation: u32,
    pub codec: Codec,
    pub resolution: Resolution,
    pub format: PixelFormat,
    pub color: ColorSpace,
}

impl StreamDescription {
    pub const MAX_ENCODED_LEN: usize = FIXED_LEN
        + CHROMATICITIES_LEN
        + MASTERING_FIXED_LEN
        + CHROMATICITIES_LEN
        + CONTENT_LIGHT_LEN;

    pub const fn is_newer_than(&self, other: &Self) -> bool {
        self.generation.wrapping_sub(other.generation).cast_signed() > 0
    }

    pub const fn encoded_len(&self) -> usize {
        let hdr = &self.color.hdr;
        let mastering = match hdr.mastering {
            Some(mastering) => MASTERING_FIXED_LEN + primaries_extra_len(mastering.primaries),
            None => 0,
        };
        let content_light = if hdr.content_light.is_some() {
            CONTENT_LIGHT_LEN
        } else {
            0
        };

        FIXED_LEN + primaries_extra_len(self.color.primaries) + mastering + content_light
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        let color = &self.color;
        let hdr = &color.hdr;

        out.extend_from_slice(&self.generation.to_be_bytes());
        out.push(codec_tag(self.codec));
        out.extend_from_slice(&self.resolution.width.to_be_bytes());
        out.extend_from_slice(&self.resolution.height.to_be_bytes());
        out.push(chroma_tag(self.format.chroma));
        out.push(self.format.depth.bits());
        write_primaries(color.primaries, out);
        out.push(color.transfer.h273_code());
        out.push(
            color
                .matrix
                .map_or(UNSPECIFIED_MATRIX, MatrixCoefficients::h273_code),
        );
        out.push(match color.range {
            ColorRange::Limited => 0,
            ColorRange::Full => 1,
        });
        out.extend_from_slice(&hdr.reference_white_nits.to_be_bytes());
        out.extend_from_slice(&hdr.peak_nits.to_be_bytes());

        let mut flags = 0;
        if hdr.mastering.is_some() {
            flags |= HAS_MASTERING;
        }
        if hdr.content_light.is_some() {
            flags |= HAS_CONTENT_LIGHT;
        }
        out.push(flags);

        if let Some(mastering) = hdr.mastering {
            write_primaries(mastering.primaries, out);
            out.extend_from_slice(&mastering.max_nits.units().to_be_bytes());
            out.extend_from_slice(&mastering.min_nits.units().to_be_bytes());
        }
        if let Some(light) = hdr.content_light {
            out.extend_from_slice(&light.max_cll_nits.to_be_bytes());
            out.extend_from_slice(&light.max_fall_nits.to_be_bytes());
        }
    }

    pub(crate) fn read(reader: &mut Reader<'_>) -> Result<Self, ProtocolError> {
        let generation = reader.u32()?;
        let codec = read_codec(reader.u8()?)?;
        let resolution = Resolution::new(reader.u32()?, reader.u32()?);
        let format = PixelFormat {
            chroma: read_chroma(reader.u8()?)?,
            depth: read_depth(reader.u8()?)?,
        };
        let primaries = read_primaries(reader)?;
        let transfer = reader.u8()?;
        let transfer = TransferFunction::from_h273_code(transfer)
            .ok_or(ProtocolError::UnknownTransferFunction(transfer))?;
        let matrix = read_matrix(reader.u8()?)?;
        let range = match reader.u8()? {
            0 => ColorRange::Limited,
            1 => ColorRange::Full,
            unknown => return Err(ProtocolError::UnknownColorRange(unknown)),
        };
        let reference_white_nits = reader.u32()?;
        let peak_nits = reader.u32()?;

        let flags = reader.u8()?;
        if flags & !KNOWN_HDR_FLAGS != 0 {
            return Err(ProtocolError::UnknownHdrFlags(flags));
        }
        let mastering = if flags & HAS_MASTERING == 0 {
            None
        } else {
            Some(MasteringDisplay {
                primaries: read_primaries(reader)?,
                max_nits: read_luminance(reader.u32()?)?,
                min_nits: read_luminance(reader.u32()?)?,
            })
        };
        let content_light = if flags & HAS_CONTENT_LIGHT == 0 {
            None
        } else {
            Some(ContentLightLevel {
                max_cll_nits: reader.u16()?,
                max_fall_nits: reader.u16()?,
            })
        };

        Ok(Self {
            generation,
            codec,
            resolution,
            format,
            color: ColorSpace {
                primaries,
                transfer,
                matrix,
                range,
                hdr: HdrLevels {
                    reference_white_nits,
                    peak_nits,
                    mastering,
                    content_light,
                },
            },
        })
    }
}

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
    offset: usize,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(buf: &'a [u8], offset: usize) -> Self {
        Self { buf, at: 0, offset }
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.buf.len() - self.at
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], ProtocolError> {
        let bytes = self
            .buf
            .get(self.at..self.at + N)
            .and_then(|bytes| <[u8; N]>::try_from(bytes).ok())
            .ok_or(ProtocolError::BufferTooShort {
                needed: self.offset + self.at + N,
                available: self.offset + self.buf.len(),
            })?;
        self.at += N;

        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, ProtocolError> {
        Ok(u16::from_be_bytes(self.take()?))
    }

    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_be_bytes(self.take()?))
    }
}

const fn primaries_extra_len(primaries: ColorPrimaries) -> usize {
    match primaries {
        ColorPrimaries::Custom(_) => CHROMATICITIES_LEN,
        _ => 0,
    }
}

const fn codec_tag(codec: Codec) -> u8 {
    match codec {
        Codec::H264 => 0,
        Codec::Hevc => 1,
        Codec::Av1 => 2,
    }
}

fn read_codec(tag: u8) -> Result<Codec, ProtocolError> {
    match tag {
        0 => Ok(Codec::H264),
        1 => Ok(Codec::Hevc),
        2 => Ok(Codec::Av1),
        unknown => Err(ProtocolError::UnknownCodec(unknown)),
    }
}

const fn chroma_tag(chroma: ChromaSubsampling) -> u8 {
    match chroma {
        ChromaSubsampling::Yuv420 => 0,
        ChromaSubsampling::Yuv422 => 1,
        ChromaSubsampling::Yuv444 => 2,
    }
}

fn read_chroma(tag: u8) -> Result<ChromaSubsampling, ProtocolError> {
    match tag {
        0 => Ok(ChromaSubsampling::Yuv420),
        1 => Ok(ChromaSubsampling::Yuv422),
        2 => Ok(ChromaSubsampling::Yuv444),
        unknown => Err(ProtocolError::UnknownChroma(unknown)),
    }
}

fn read_depth(bits: u8) -> Result<BitDepth, ProtocolError> {
    [BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve]
        .into_iter()
        .find(|depth| depth.bits() == bits)
        .ok_or(ProtocolError::UnknownBitDepth(bits))
}

fn read_matrix(code: u8) -> Result<Option<MatrixCoefficients>, ProtocolError> {
    if code == UNSPECIFIED_MATRIX {
        return Ok(None);
    }

    MatrixCoefficients::ALL
        .into_iter()
        .find(|matrix| matrix.h273_code() == code)
        .map(Some)
        .ok_or(ProtocolError::UnknownMatrixCoefficients(code))
}

fn read_luminance(units: u32) -> Result<Luminance, ProtocolError> {
    Luminance::from_units(units).ok_or(ProtocolError::LuminanceOutOfRange(units))
}

fn write_primaries(primaries: ColorPrimaries, out: &mut Vec<u8>) {
    match primaries {
        ColorPrimaries::Custom(chromaticities) => {
            out.push(CUSTOM_PRIMARIES);
            for point in [
                chromaticities.red(),
                chromaticities.green(),
                chromaticities.blue(),
                chromaticities.white(),
            ] {
                let (x, y) = point.to_units();
                out.extend_from_slice(&x.to_be_bytes());
                out.extend_from_slice(&y.to_be_bytes());
            }
        }
        named => out.push(named.h273_code().unwrap_or(CUSTOM_PRIMARIES)),
    }
}

fn read_primaries(reader: &mut Reader<'_>) -> Result<ColorPrimaries, ProtocolError> {
    let code = reader.u8()?;
    if code != CUSTOM_PRIMARIES {
        return ColorPrimaries::from_h273_code(code)
            .ok_or(ProtocolError::UnknownColorPrimaries(code));
    }

    let mut point = || {
        let (x, y) = (reader.u16()?, reader.u16()?);
        Chromaticity::from_units(x, y).ok_or(ProtocolError::ChromaticityOutOfRange { x, y })
    };
    let (red, green, blue, white) = (point()?, point()?, point()?, point()?);

    Chromaticities::new(red, green, blue, white)
        .map(ColorPrimaries::Custom)
        .map_err(|_| ProtocolError::DegenerateGamut)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn custom_primaries() -> ColorPrimaries {
        ColorPrimaries::Custom(
            Chromaticities::new(
                Chromaticity::from_units(34_000, 16_000).unwrap(),
                Chromaticity::from_units(13_250, 34_500).unwrap(),
                Chromaticity::from_units(7_500, 3_000).unwrap(),
                Chromaticity::from_units(15_700, 16_450).unwrap(),
            )
            .unwrap(),
        )
    }

    pub(crate) fn hdr_description() -> StreamDescription {
        StreamDescription {
            generation: u32::MAX,
            codec: Codec::Av1,
            resolution: Resolution::new(3840, 2160),
            format: PixelFormat {
                chroma: ChromaSubsampling::Yuv444,
                depth: BitDepth::Ten,
            },
            color: ColorSpace {
                primaries: custom_primaries(),
                transfer: TransferFunction::Pq,
                matrix: Some(MatrixCoefficients::Bt2020Ncl),
                range: ColorRange::Limited,
                hdr: HdrLevels {
                    reference_white_nits: 203,
                    peak_nits: 1000,
                    mastering: Some(MasteringDisplay {
                        primaries: custom_primaries(),
                        max_nits: Luminance::from_whole_nits(4000).unwrap(),
                        min_nits: Luminance::from_units(50).unwrap(),
                    }),
                    content_light: Some(ContentLightLevel {
                        max_cll_nits: 1500,
                        max_fall_nits: 400,
                    }),
                },
            },
        }
    }

    fn sdr_description() -> StreamDescription {
        StreamDescription {
            generation: 3,
            codec: Codec::Hevc,
            resolution: Resolution::new(1920, 1080),
            format: PixelFormat::default(),
            color: ColorSpace::default(),
        }
    }

    fn round_trip(description: &StreamDescription) -> StreamDescription {
        let mut out = Vec::new();
        description.write(&mut out);
        assert_eq!(out.len(), description.encoded_len());

        let mut reader = Reader::new(&out, 0);
        let decoded = StreamDescription::read(&mut reader).unwrap();
        assert_eq!(reader.remaining(), 0);

        decoded
    }

    #[test]
    fn descriptions_round_trip() {
        for description in [hdr_description(), sdr_description()] {
            assert_eq!(round_trip(&description), description);
        }
    }

    #[test]
    fn every_named_primaries_round_trips() {
        for primaries in ColorPrimaries::NAMED {
            let mut description = sdr_description();
            description.color.primaries = primaries;

            assert_eq!(round_trip(&description), description);
        }
    }

    #[test]
    fn max_encoded_len_is_the_fully_described_hdr_stream() {
        assert_eq!(
            hdr_description().encoded_len(),
            StreamDescription::MAX_ENCODED_LEN
        );
    }

    #[test]
    fn newer_generation_survives_wrap_around() {
        let old = hdr_description();
        let new = StreamDescription {
            generation: old.generation.wrapping_add(1),
            ..old
        };

        assert!(new.is_newer_than(&old));
        assert!(!old.is_newer_than(&new));
        assert!(!old.is_newer_than(&old));
    }

    #[test]
    fn unknown_codes_are_rejected() {
        let mut out = Vec::new();
        sdr_description().write(&mut out);
        let corrupt = |at: usize, value: u8| {
            let mut bytes = out.clone();
            bytes[at] = value;
            StreamDescription::read(&mut Reader::new(&bytes, 0))
        };

        assert_eq!(corrupt(4, 9), Err(ProtocolError::UnknownCodec(9)));
        assert_eq!(corrupt(13, 7), Err(ProtocolError::UnknownChroma(7)));
        assert_eq!(corrupt(14, 9), Err(ProtocolError::UnknownBitDepth(9)));
        assert_eq!(corrupt(15, 3), Err(ProtocolError::UnknownColorPrimaries(3)));
        assert_eq!(
            corrupt(16, 3),
            Err(ProtocolError::UnknownTransferFunction(3))
        );
        assert_eq!(
            corrupt(17, 0),
            Err(ProtocolError::UnknownMatrixCoefficients(0))
        );
        assert_eq!(corrupt(18, 2), Err(ProtocolError::UnknownColorRange(2)));
        assert_eq!(corrupt(27, 4), Err(ProtocolError::UnknownHdrFlags(4)));
    }

    #[test]
    fn a_gamut_without_its_white_point_is_rejected() {
        let mut out = Vec::new();
        hdr_description().write(&mut out);
        out[16..20].copy_from_slice(&[0x3a, 0x98, 0x3a, 0x98]);
        out[20..24].copy_from_slice(&[0x1d, 0x4c, 0x1d, 0x4c]);

        assert_eq!(
            StreamDescription::read(&mut Reader::new(&out, 0)),
            Err(ProtocolError::DegenerateGamut)
        );
    }
}
