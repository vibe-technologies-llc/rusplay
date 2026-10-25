use rusplay_core::{Chromaticity, HdrLevels, Luminance};

use crate::{
    hdr::St2086,
    vaapi::{
        bits::{BitWriter, leb128, nal_unit_prefix, packed},
        va::PackedHeader,
    },
};

const SEI_MASTERING_DISPLAY: u64 = 137;
const SEI_CONTENT_LIGHT: u64 = 144;
const MASTERING_PAYLOAD_LEN: u64 = 24;
const CONTENT_LIGHT_PAYLOAD_LEN: u64 = 4;
const H264_SEI_HEADER: [u8; 1] = [6];
const HEVC_PREFIX_SEI_HEADER: [u8; 2] = [39 << 1, 1];
const OBU_METADATA_HEADER: u8 = (5 << 3) | (1 << 1);
const AV1_METADATA_CONTENT_LIGHT: usize = 1;
const AV1_METADATA_MASTERING_DISPLAY: usize = 2;
const AV1_TRAILING_BITS: u8 = 0x80;
const AV1_CHROMATICITY_UNITS: u64 = 1 << 16;
const AV1_MAX_LUMINANCE_UNITS: u64 = 1 << 8;
const AV1_MIN_LUMINANCE_UNITS: u64 = 1 << 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SeiNal {
    H264,
    Hevc,
}

impl SeiNal {
    const fn header(self) -> &'static [u8] {
        match self {
            Self::H264 => &H264_SEI_HEADER,
            Self::Hevc => &HEVC_PREFIX_SEI_HEADER,
        }
    }
}

pub(super) fn sei(nal: SeiNal, hdr: &HdrLevels) -> Option<PackedHeader> {
    if hdr.mastering.is_none() && hdr.content_light.is_none() {
        return None;
    }

    let mut rbsp = BitWriter::new();
    if let Some(display) = &hdr.mastering {
        let units = St2086::of(display);
        rbsp.bits(SEI_MASTERING_DISPLAY, 8);
        rbsp.bits(MASTERING_PAYLOAD_LEN, 8);
        for [x, y] in [units.green, units.blue, units.red, units.white] {
            rbsp.bits(u64::from(x), 16);
            rbsp.bits(u64::from(y), 16);
        }
        rbsp.bits(u64::from(units.max_luminance), 32);
        rbsp.bits(u64::from(units.min_luminance), 32);
    }
    if let Some(light) = &hdr.content_light {
        rbsp.bits(SEI_CONTENT_LIGHT, 8);
        rbsp.bits(CONTENT_LIGHT_PAYLOAD_LEN, 8);
        rbsp.bits(u64::from(light.max_cll_nits), 16);
        rbsp.bits(u64::from(light.max_fall_nits), 16);
    }
    rbsp.trailing_bits();

    Some(nal_unit_prefix(nal.header(), rbsp))
}

pub(super) fn metadata_obus(hdr: &HdrLevels) -> Option<PackedHeader> {
    let mut obus = Vec::new();

    if let Some(display) = &hdr.mastering {
        let units = St2086::of(display);
        let mut body = Vec::new();
        for [x, y] in [units.red, units.green, units.blue, units.white] {
            body.extend_from_slice(&av1_chromaticity(x).to_be_bytes());
            body.extend_from_slice(&av1_chromaticity(y).to_be_bytes());
        }
        body.extend_from_slice(
            &av1_luminance(units.max_luminance, AV1_MAX_LUMINANCE_UNITS).to_be_bytes(),
        );
        body.extend_from_slice(
            &av1_luminance(units.min_luminance, AV1_MIN_LUMINANCE_UNITS).to_be_bytes(),
        );
        push_metadata_obu(&mut obus, AV1_METADATA_MASTERING_DISPLAY, &body);
    }
    if let Some(light) = &hdr.content_light {
        let mut body = Vec::new();
        body.extend_from_slice(&light.max_cll_nits.to_be_bytes());
        body.extend_from_slice(&light.max_fall_nits.to_be_bytes());
        push_metadata_obu(&mut obus, AV1_METADATA_CONTENT_LIGHT, &body);
    }

    (!obus.is_empty()).then(|| packed(obus))
}

fn push_metadata_obu(out: &mut Vec<u8>, kind: usize, body: &[u8]) {
    let kind = leb128(kind, 1);
    let size = kind.len() + body.len() + 1;

    out.push(OBU_METADATA_HEADER);
    out.extend(leb128(size, 1));
    out.extend(kind);
    out.extend_from_slice(body);
    out.push(AV1_TRAILING_BITS);
}

fn rescale(value: u64, to: u64, from: u64) -> u64 {
    (value * to + from / 2) / from
}

fn av1_chromaticity(units: u16) -> u16 {
    let scaled = rescale(
        u64::from(units),
        AV1_CHROMATICITY_UNITS,
        u64::from(Chromaticity::UNITS_PER_ONE),
    );

    u16::try_from(scaled).unwrap_or(u16::MAX)
}

fn av1_luminance(units: u32, fixed_point: u64) -> u32 {
    let scaled = rescale(
        u64::from(units),
        fixed_point,
        u64::from(Luminance::UNITS_PER_NIT),
    );

    u32::try_from(scaled).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use rusplay_core::{ColorPrimaries, ContentLightLevel, MasteringDisplay};

    use super::*;

    fn hdr10() -> HdrLevels {
        HdrLevels {
            mastering: Some(MasteringDisplay {
                primaries: ColorPrimaries::Bt2020,
                max_nits: Luminance::from_whole_nits(1000).unwrap(),
                min_nits: Luminance::from_units(50).unwrap(),
            }),
            content_light: Some(ContentLightLevel {
                max_cll_nits: 1000,
                max_fall_nits: 400,
            }),
            ..HdrLevels::default()
        }
    }

    #[test]
    fn sdr_streams_carry_no_hdr_headers() {
        assert!(sei(SeiNal::Hevc, &HdrLevels::default()).is_none());
        assert!(metadata_obus(&HdrLevels::default()).is_none());
    }

    #[test]
    fn hevc_sei_holds_both_messages_in_one_prefix_nal() {
        let header = sei(SeiNal::Hevc, &hdr10()).unwrap();
        let bytes = &header.bytes;

        assert_eq!(&bytes[..8], &[0, 0, 0, 1, 0x4e, 0x01, 137, 24]);
        assert_eq!(&bytes[8..12], &[0x21, 0x34, 0x9b, 0xaa]);
        assert_eq!(&bytes[24..28], &[0x00, 0x98, 0x96, 0x80]);
        assert_eq!(&bytes[28..33], &[0, 0, 3, 0, 50]);
        assert_eq!(&bytes[33..39], &[144, 4, 0x03, 0xe8, 0x01, 0x90]);
        assert_eq!(&bytes[39..], &[0x80]);
        assert_eq!(header.bits, bytes.len() * 8);
    }

    #[test]
    fn av1_metadata_obus_use_av1_fixed_point() {
        let bytes = metadata_obus(&hdr10()).unwrap().bytes;

        assert_eq!(&bytes[..3], &[0x2a, 26, 2]);
        assert_eq!(&bytes[3..5], &av1_chromaticity(35_400).to_be_bytes());
        assert_eq!(&bytes[19..23], &(1000_u32 << 8).to_be_bytes());
        assert_eq!(&bytes[23..27], &82_u32.to_be_bytes());
        assert_eq!(bytes[27], 0x80);
        assert_eq!(&bytes[28..], &[0x2a, 6, 1, 0x03, 0xe8, 0x01, 0x90, 0x80]);
    }

    #[test]
    fn av1_chromaticity_round_trips_through_st2086_units() {
        for units in [0_u16, 1, 6_550, 15_635, 35_400, 49_999] {
            let back = rescale(
                u64::from(av1_chromaticity(units)),
                u64::from(Chromaticity::UNITS_PER_ONE),
                AV1_CHROMATICITY_UNITS,
            );

            assert_eq!(back, u64::from(units));
        }
    }
}
