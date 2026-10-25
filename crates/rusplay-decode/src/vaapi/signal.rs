use rusplay_core::{ContentLightLevel, MasteringDisplay, SignalledColor};

use crate::metadata::{Primaries, av1_mastering_display, content_light, mastering_display};

const EMULATION_PREVENTION_BYTE: u8 = 3;
const RBSP_STOP_BYTE: u8 = 0x80;
const SEI_MASTERING_DISPLAY: u32 = 137;
const SEI_CONTENT_LIGHT: u32 = 144;
const MASTERING_SEI_LEN: usize = 24;
const CONTENT_LIGHT_SEI_LEN: usize = 4;
const AV1_METADATA_CONTENT_LIGHT: u64 = 1;
const AV1_METADATA_MASTERING_DISPLAY: u64 = 2;
const AV1_MASTERING_LEN: usize = 24;
const AV1_CONTENT_LIGHT_LEN: usize = 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Hdr {
    mastering: Option<MasteringDisplay>,
    content_light: Option<ContentLightLevel>,
}

impl Hdr {
    fn merge(&mut self, newer: Self) {
        self.mastering = newer.mastering.or(self.mastering);
        self.content_light = newer.content_light.or(self.content_light);
    }
}

#[derive(Debug, Default)]
pub(super) struct HdrTracker {
    current: Hdr,
    pending: Hdr,
}

impl HdrTracker {
    pub(super) fn read_sei(&mut self, payload: &[u8]) {
        let rbsp = unescape(payload);
        let mut rest = rbsp.as_slice();

        while rest.len() > 1 || rest.first().is_some_and(|byte| *byte != RBSP_STOP_BYTE) {
            let Some((kind, after_kind)) = sei_number(rest) else {
                return;
            };
            let Some((size, after_size)) = sei_number(after_kind) else {
                return;
            };
            let Some(message) = usize::try_from(size)
                .ok()
                .and_then(|size| after_size.get(..size))
            else {
                return;
            };

            match kind {
                SEI_MASTERING_DISPLAY => {
                    if let Some(display) = sei_mastering_display(message) {
                        self.pending.mastering = Some(display);
                    }
                }
                SEI_CONTENT_LIGHT => {
                    if let Some(light) = content_light_of(message, CONTENT_LIGHT_SEI_LEN) {
                        self.pending.content_light = Some(light);
                    }
                }
                _ => {}
            }
            rest = &after_size[message.len()..];
        }
    }

    pub(super) fn read_av1_metadata(&mut self, payload: &[u8]) {
        let Some((kind, body)) = leb128(payload) else {
            return;
        };

        match kind {
            AV1_METADATA_CONTENT_LIGHT => {
                if let Some(light) = content_light_of(body, AV1_CONTENT_LIGHT_LEN) {
                    self.pending.content_light = Some(light);
                }
            }
            AV1_METADATA_MASTERING_DISPLAY => {
                if let Some(display) = av1_mastering(body) {
                    self.pending.mastering = Some(display);
                }
            }
            _ => {}
        }
    }

    pub(super) fn resolve(&mut self, base: SignalledColor, random_access: bool) -> SignalledColor {
        let pending = std::mem::take(&mut self.pending);
        if random_access {
            self.current = pending;
        } else {
            self.current.merge(pending);
        }

        SignalledColor {
            mastering: self.current.mastering,
            content_light: self.current.content_light,
            ..base
        }
    }
}

pub(super) fn vui_color(
    signal_type_present: bool,
    full_range: bool,
    description_present: bool,
    codes: [u32; 3],
) -> SignalledColor {
    let mut color = SignalledColor::default();
    if !signal_type_present {
        return color;
    }

    color = color.with_full_range(full_range);
    if description_present {
        let [primaries, transfer, matrix] = codes.map(|code| u8::try_from(code).unwrap_or(0));
        color = color.with_h273_codes(primaries, transfer, matrix);
    }

    color
}

fn unescape(payload: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(payload.len());
    let mut zeros = 0;

    for &byte in payload {
        if zeros >= 2 && byte == EMULATION_PREVENTION_BYTE {
            zeros = 0;
            continue;
        }
        rbsp.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }

    rbsp
}

fn sei_number(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let mut value = 0_u32;
    for (index, &byte) in bytes.iter().enumerate() {
        value = value.checked_add(u32::from(byte))?;
        if byte != 0xff {
            return Some((value, &bytes[index + 1..]));
        }
    }

    None
}

fn leb128(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let mut value = 0_u64;
    for (index, &byte) in bytes.iter().enumerate().take(8) {
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Some((value, &bytes[index + 1..]));
        }
    }

    None
}

fn be16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn point(bytes: &[u8], at: usize) -> [u16; 2] {
    [be16(bytes, at), be16(bytes, at + 2)]
}

fn sei_mastering_display(message: &[u8]) -> Option<MasteringDisplay> {
    let message = message.get(..MASTERING_SEI_LEN)?;

    mastering_display(
        Primaries {
            green: point(message, 0),
            blue: point(message, 4),
            red: point(message, 8),
            white: point(message, 12),
        },
        be32(message, 16),
        be32(message, 20),
    )
}

fn av1_mastering(body: &[u8]) -> Option<MasteringDisplay> {
    let body = body.get(..AV1_MASTERING_LEN)?;

    av1_mastering_display(
        [point(body, 0), point(body, 4), point(body, 8)],
        point(body, 12),
        be32(body, 16),
        be32(body, 20),
    )
}

fn content_light_of(body: &[u8], len: usize) -> Option<ContentLightLevel> {
    let body = body.get(..len)?;

    Some(content_light(be16(body, 0), be16(body, 2)))
}

#[cfg(test)]
mod tests {
    use rusplay_core::{
        ColorPrimaries, ColorRange, Luminance, MatrixCoefficients, TransferFunction,
    };

    use super::*;

    fn bt2020_mastering_sei() -> Vec<u8> {
        let mut message = Vec::new();
        for value in [
            8_500_u16, 39_850, 6_550, 2_300, 35_400, 14_600, 15_635, 16_450,
        ] {
            message.extend_from_slice(&value.to_be_bytes());
        }
        message.extend_from_slice(&10_000_000_u32.to_be_bytes());
        message.extend_from_slice(&50_u32.to_be_bytes());
        message
    }

    fn sei(messages: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut payload = Vec::new();
        for (kind, body) in messages {
            payload.push(*kind);
            payload.push(u8::try_from(body.len()).unwrap());
            payload.extend_from_slice(body);
        }
        payload.push(RBSP_STOP_BYTE);
        payload
    }

    #[test]
    fn sei_carries_mastering_display_and_content_light() {
        let mut tracker = HdrTracker::default();

        tracker.read_sei(&sei(&[
            (137, bt2020_mastering_sei()),
            (144, vec![3, 232, 0, 200]),
        ]));
        let color = tracker.resolve(SignalledColor::default(), true);

        let mastering = color.mastering.unwrap();
        assert_eq!(mastering.primaries, ColorPrimaries::Bt2020);
        assert_eq!(
            mastering.max_nits,
            Luminance::from_whole_nits(1000).unwrap()
        );
        assert_eq!(mastering.min_nits.units(), 50);
        assert_eq!(color.content_light, Some(content_light(1000, 200)));
    }

    #[test]
    fn emulation_prevention_bytes_are_removed() {
        let escaped = [0, 0, 3, 1, 0, 0, 3, 0];

        assert_eq!(unescape(&escaped), vec![0, 0, 1, 0, 0, 0]);
    }

    #[test]
    fn metadata_persists_until_the_next_random_access_point() {
        let mut tracker = HdrTracker::default();

        tracker.read_sei(&sei(&[(144, vec![3, 232, 0, 200])]));
        let keyframe = tracker.resolve(SignalledColor::default(), true);
        let inter = tracker.resolve(SignalledColor::default(), false);
        let next_keyframe = tracker.resolve(SignalledColor::default(), true);

        assert!(keyframe.content_light.is_some());
        assert_eq!(inter.content_light, keyframe.content_light);
        assert_eq!(next_keyframe.content_light, None);
    }

    #[test]
    fn av1_metadata_obus_carry_hdr_metadata() {
        let mut tracker = HdrTracker::default();
        let mut mastering = vec![2];
        for value in [
            45_875_u16, 21_627, 8_847, 45_220, 4_260, 1_507, 20_493, 21_561,
        ] {
            mastering.extend_from_slice(&value.to_be_bytes());
        }
        mastering.extend_from_slice(&(1000_u32 << 8).to_be_bytes());
        mastering.extend_from_slice(&(1_u32 << 14).to_be_bytes());

        tracker.read_av1_metadata(&mastering);
        tracker.read_av1_metadata(&[1, 0x05, 0xdc, 0x01, 0x90]);
        let color = tracker.resolve(SignalledColor::default(), true);

        let display = color.mastering.unwrap();
        assert_eq!(display.max_nits, Luminance::from_whole_nits(1000).unwrap());
        assert_eq!(display.min_nits, Luminance::from_whole_nits(1).unwrap());
        assert_eq!(color.content_light, Some(content_light(1500, 400)));
    }

    #[test]
    fn vui_without_a_colour_description_signals_only_the_range() {
        let range_only = vui_color(true, false, false, [9, 16, 9]);
        let described = vui_color(true, true, true, [9, 16, 9]);
        let absent = vui_color(false, true, true, [9, 16, 9]);

        assert_eq!(
            range_only,
            SignalledColor {
                range: Some(ColorRange::Limited),
                ..SignalledColor::default()
            }
        );
        assert_eq!(described.primaries, Some(ColorPrimaries::Bt2020));
        assert_eq!(described.transfer, Some(TransferFunction::Pq));
        assert_eq!(described.matrix, Some(MatrixCoefficients::Bt2020Ncl));
        assert_eq!(described.range, Some(ColorRange::Full));
        assert_eq!(absent, SignalledColor::default());
    }
}
