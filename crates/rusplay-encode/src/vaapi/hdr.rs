use rusplay_core::HdrLevels;

use crate::{
    hdr::{St2086, content_light_payload},
    vaapi::{
        bits::{BitWriter, leb128, nal_unit_prefix, packed},
        va::PackedHeader,
    },
};

const SEI_MASTERING_DISPLAY: u64 = 137;
const SEI_CONTENT_LIGHT: u64 = 144;
const H264_SEI_HEADER: [u8; 1] = [6];
const HEVC_PREFIX_SEI_HEADER: [u8; 2] = [39 << 1, 1];
const OBU_METADATA_HEADER: u8 = (5 << 3) | (1 << 1);
const AV1_METADATA_CONTENT_LIGHT: usize = 1;
const AV1_METADATA_MASTERING_DISPLAY: usize = 2;
const AV1_TRAILING_BITS: u8 = 0x80;

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
        sei_message(
            &mut rbsp,
            SEI_MASTERING_DISPLAY,
            &St2086::of(display).sei_payload(),
        );
    }
    if let Some(light) = &hdr.content_light {
        sei_message(&mut rbsp, SEI_CONTENT_LIGHT, &content_light_payload(*light));
    }
    rbsp.trailing_bits();

    Some(nal_unit_prefix(nal.header(), rbsp))
}

fn sei_message(rbsp: &mut BitWriter, kind: u64, payload: &[u8]) {
    rbsp.bits(kind, 8);
    rbsp.bits(payload.len() as u64, 8);
    for &byte in payload {
        rbsp.bits(u64::from(byte), 8);
    }
}

pub(super) fn metadata_obus(hdr: &HdrLevels) -> Option<PackedHeader> {
    let mut obus = Vec::new();

    if let Some(display) = &hdr.mastering {
        let body = St2086::of(display).av1_payload();
        push_metadata_obu(&mut obus, AV1_METADATA_MASTERING_DISPLAY, &body);
    }
    if let Some(light) = &hdr.content_light {
        let body = content_light_payload(*light);
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

#[cfg(test)]
mod tests {
    use rusplay_core::{ColorPrimaries, ContentLightLevel, Luminance, MasteringDisplay};

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
        assert_eq!(&bytes[3..5], &[0xb5, 0x3f]);
        assert_eq!(&bytes[19..23], &(1000_u32 << 8).to_be_bytes());
        assert_eq!(&bytes[23..27], &82_u32.to_be_bytes());
        assert_eq!(bytes[27], 0x80);
        assert_eq!(&bytes[28..], &[0x2a, 6, 1, 0x03, 0xe8, 0x01, 0x90, 0x80]);
    }
}
