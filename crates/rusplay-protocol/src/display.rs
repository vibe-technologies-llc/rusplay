use rusplay_core::{ColorPrimaries, FrameRate, HdrLevels, Luminance, Resolution, TransferFunction};

use crate::{
    ProtocolError, StreamDescription,
    stream::{
        CHROMATICITIES_LEN, Reader, primaries_extra_len, read_levels, read_luminance,
        read_primaries, write_primaries,
    },
};

const FIXED_LEN: usize = 4 + 4 + 4 + 1 + 1 + 4 + 4 + 4;
const MILLIHERTZ_PER_HERTZ: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DisplayVolume {
    pub primaries: ColorPrimaries,
    pub transfer: TransferFunction,
    pub reference_white_nits: u32,
    pub peak_nits: u32,
    pub black: Luminance,
}

impl DisplayVolume {
    pub const fn srgb() -> Self {
        Self {
            primaries: ColorPrimaries::Bt709,
            transfer: TransferFunction::Srgb,
            reference_white_nits: HdrLevels::BT2408_REFERENCE_WHITE_NITS,
            peak_nits: HdrLevels::BT2408_REFERENCE_WHITE_NITS,
            black: Luminance::ZERO,
        }
    }

    fn validate(&self) -> Result<(), ProtocolError> {
        read_levels(self.reference_white_nits, self.peak_nits)?;

        let white_units =
            u64::from(self.reference_white_nits) * u64::from(Luminance::UNITS_PER_NIT);
        if u64::from(self.black.units()) >= white_units {
            return Err(ProtocolError::DisplayBlackNotBelowWhite {
                black_units: self.black.units(),
                reference_white_nits: self.reference_white_nits,
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientDisplay {
    resolution: Resolution,
    refresh_millihertz: u32,
    volume: DisplayVolume,
}

impl ClientDisplay {
    pub const MIN_REFRESH_MILLIHERTZ: u32 = MILLIHERTZ_PER_HERTZ;
    pub const MAX_REFRESH_MILLIHERTZ: u32 = 1000 * MILLIHERTZ_PER_HERTZ;
    pub const MAX_ENCODED_LEN: usize = FIXED_LEN + CHROMATICITIES_LEN;

    pub fn new(
        resolution: Resolution,
        refresh_millihertz: u32,
        volume: DisplayVolume,
    ) -> Result<Self, ProtocolError> {
        let axis = 1..=StreamDescription::MAX_DIMENSION;
        if !axis.contains(&resolution.width) || !axis.contains(&resolution.height) {
            return Err(ProtocolError::DisplayResolutionOutOfRange {
                width: resolution.width,
                height: resolution.height,
                max: StreamDescription::MAX_DIMENSION,
            });
        }

        let refresh = Self::MIN_REFRESH_MILLIHERTZ..=Self::MAX_REFRESH_MILLIHERTZ;
        if !refresh.contains(&refresh_millihertz) {
            return Err(ProtocolError::DisplayRefreshOutOfRange {
                millihertz: refresh_millihertz,
                min: Self::MIN_REFRESH_MILLIHERTZ,
                max: Self::MAX_REFRESH_MILLIHERTZ,
            });
        }

        volume.validate()?;

        Ok(Self {
            resolution,
            refresh_millihertz,
            volume,
        })
    }

    pub const fn resolution(&self) -> Resolution {
        self.resolution
    }

    pub const fn refresh_millihertz(&self) -> u32 {
        self.refresh_millihertz
    }

    pub const fn volume(&self) -> &DisplayVolume {
        &self.volume
    }

    pub fn frame_rate(&self) -> FrameRate {
        let divisor = greatest_common_divisor(self.refresh_millihertz, MILLIHERTZ_PER_HERTZ);

        FrameRate::new(
            self.refresh_millihertz / divisor,
            MILLIHERTZ_PER_HERTZ / divisor,
        )
    }

    pub const fn encoded_len(&self) -> usize {
        FIXED_LEN + primaries_extra_len(self.volume.primaries)
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.resolution.width.to_be_bytes());
        out.extend_from_slice(&self.resolution.height.to_be_bytes());
        out.extend_from_slice(&self.refresh_millihertz.to_be_bytes());
        write_primaries(self.volume.primaries, out);
        out.push(self.volume.transfer.h273_code());
        out.extend_from_slice(&self.volume.reference_white_nits.to_be_bytes());
        out.extend_from_slice(&self.volume.peak_nits.to_be_bytes());
        out.extend_from_slice(&self.volume.black.units().to_be_bytes());
    }

    pub(crate) fn read(reader: &mut Reader<'_>) -> Result<Self, ProtocolError> {
        let resolution = Resolution::new(reader.u32()?, reader.u32()?);
        let refresh_millihertz = reader.u32()?;
        let primaries = read_primaries(reader)?;
        let transfer = reader.u8()?;
        let transfer = TransferFunction::from_h273_code(transfer)
            .ok_or(ProtocolError::UnknownTransferFunction(transfer))?;
        let reference_white_nits = reader.u32()?;
        let peak_nits = reader.u32()?;
        let black = read_luminance(reader.u32()?)?;

        Self::new(
            resolution,
            refresh_millihertz,
            DisplayVolume {
                primaries,
                transfer,
                reference_white_nits,
                peak_nits,
                black,
            },
        )
    }
}

const fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        (left, right) = (right, left % right);
    }

    left
}

#[cfg(test)]
mod tests {
    use rusplay_core::{Chromaticities, Chromaticity};

    use super::*;
    use crate::{ControlMessage, ControlPacket};

    fn custom_volume() -> DisplayVolume {
        DisplayVolume {
            primaries: ColorPrimaries::Custom(
                Chromaticities::new(
                    Chromaticity::from_units(34_000, 16_000).unwrap(),
                    Chromaticity::from_units(13_250, 34_500).unwrap(),
                    Chromaticity::from_units(7_500, 3_000).unwrap(),
                    Chromaticity::from_units(15_700, 16_450).unwrap(),
                )
                .unwrap(),
            ),
            transfer: TransferFunction::Pq,
            reference_white_nits: 203,
            peak_nits: 1000,
            black: Luminance::from_units(50).unwrap(),
        }
    }

    fn display(volume: DisplayVolume) -> ClientDisplay {
        ClientDisplay::new(Resolution::new(2560, 1440), 143_981, volume).unwrap()
    }

    fn encode(display: &ClientDisplay) -> Vec<u8> {
        let mut out = Vec::new();
        display.write(&mut out);
        out
    }

    fn decode(bytes: &[u8]) -> Result<ClientDisplay, ProtocolError> {
        ClientDisplay::read(&mut Reader::new(bytes, 1))
    }

    #[test]
    fn displays_round_trip_with_named_and_custom_primaries() {
        for volume in [DisplayVolume::srgb(), custom_volume()] {
            let display = display(volume);

            let bytes = encode(&display);

            assert_eq!(bytes.len(), display.encoded_len());
            assert_eq!(decode(&bytes), Ok(display));
        }
    }

    #[test]
    fn the_longest_display_fits_the_advertised_maximum() {
        let display = display(custom_volume());

        assert_eq!(display.encoded_len(), ClientDisplay::MAX_ENCODED_LEN);
    }

    #[test]
    fn the_layout_is_big_endian_size_then_refresh_then_volume() {
        let display =
            ClientDisplay::new(Resolution::new(1920, 1080), 60_000, DisplayVolume::srgb()).unwrap();

        let bytes = encode(&display);

        assert_eq!(bytes[0..4], 1920_u32.to_be_bytes());
        assert_eq!(bytes[4..8], 1080_u32.to_be_bytes());
        assert_eq!(bytes[8..12], 60_000_u32.to_be_bytes());
        assert_eq!(bytes[12], ColorPrimaries::Bt709.h273_code().unwrap());
    }

    #[test]
    fn the_frame_rate_is_the_reduced_refresh() {
        let rate = |millihertz| {
            ClientDisplay::new(Resolution::new(1, 1), millihertz, DisplayVolume::srgb())
                .unwrap()
                .frame_rate()
        };

        assert_eq!(rate(60_000), FrameRate::new(60, 1));
        assert_eq!(rate(59_940), FrameRate::new(2997, 50));
        assert_eq!(rate(143_981), FrameRate::new(143_981, 1000));
        assert_eq!(rate(1_000), FrameRate::new(1, 1));
    }

    #[test]
    fn each_resolution_axis_is_range_checked() {
        let max = StreamDescription::MAX_DIMENSION;

        for (width, height) in [
            (0, 1080),
            (1920, 0),
            (max + 1, 1080),
            (1920, max + 1),
            (u32::MAX, u32::MAX),
        ] {
            assert_eq!(
                ClientDisplay::new(
                    Resolution::new(width, height),
                    60_000,
                    DisplayVolume::srgb()
                ),
                Err(ProtocolError::DisplayResolutionOutOfRange { width, height, max })
            );
        }

        assert!(
            ClientDisplay::new(Resolution::new(max, max), 60_000, DisplayVolume::srgb()).is_ok()
        );
    }

    #[test]
    fn the_refresh_is_range_checked() {
        for millihertz in [
            0,
            ClientDisplay::MIN_REFRESH_MILLIHERTZ - 1,
            ClientDisplay::MAX_REFRESH_MILLIHERTZ + 1,
            u32::MAX,
        ] {
            assert_eq!(
                ClientDisplay::new(
                    Resolution::new(1920, 1080),
                    millihertz,
                    DisplayVolume::srgb()
                ),
                Err(ProtocolError::DisplayRefreshOutOfRange {
                    millihertz,
                    min: ClientDisplay::MIN_REFRESH_MILLIHERTZ,
                    max: ClientDisplay::MAX_REFRESH_MILLIHERTZ,
                })
            );
        }
    }

    #[test]
    fn the_volume_is_range_checked() {
        let with = |change: fn(&mut DisplayVolume)| {
            let mut volume = DisplayVolume::srgb();
            change(&mut volume);
            ClientDisplay::new(Resolution::new(1920, 1080), 60_000, volume)
        };

        assert_eq!(
            with(|volume| volume.reference_white_nits = 0),
            Err(ProtocolError::ReferenceWhiteOutOfRange(0))
        );
        assert_eq!(
            with(|volume| volume.peak_nits = 10_001),
            Err(ProtocolError::PeakOutOfRange(10_001))
        );
        assert_eq!(
            with(|volume| volume.black = Luminance::from_whole_nits(203).unwrap()),
            Err(ProtocolError::DisplayBlackNotBelowWhite {
                black_units: 2_030_000,
                reference_white_nits: 203,
            })
        );
    }

    #[test]
    fn decoding_range_checks_every_field_it_reads() {
        let good = encode(&display(DisplayVolume::srgb()));

        let mut zero_width = good.clone();
        zero_width[0..4].copy_from_slice(&0_u32.to_be_bytes());
        let mut huge_refresh = good.clone();
        huge_refresh[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        let mut unknown_primaries = good.clone();
        unknown_primaries[12] = 0xFE;
        let mut unknown_transfer = good.clone();
        unknown_transfer[13] = 0xFE;
        let mut white_zero = good.clone();
        white_zero[14..18].copy_from_slice(&0_u32.to_be_bytes());
        let mut black_huge = good.clone();
        black_huge[22..26].copy_from_slice(&u32::MAX.to_be_bytes());

        assert!(matches!(
            decode(&zero_width),
            Err(ProtocolError::DisplayResolutionOutOfRange { width: 0, .. })
        ));
        assert!(matches!(
            decode(&huge_refresh),
            Err(ProtocolError::DisplayRefreshOutOfRange { .. })
        ));
        assert_eq!(
            decode(&unknown_primaries),
            Err(ProtocolError::UnknownColorPrimaries(0xFE))
        );
        assert_eq!(
            decode(&unknown_transfer),
            Err(ProtocolError::UnknownTransferFunction(0xFE))
        );
        assert_eq!(
            decode(&white_zero),
            Err(ProtocolError::ReferenceWhiteOutOfRange(0))
        );
        assert_eq!(
            decode(&black_huge),
            Err(ProtocolError::LuminanceOutOfRange(u32::MAX))
        );
    }

    #[test]
    fn the_control_message_round_trips_in_a_datagram() {
        let message = ControlMessage::Display(display(custom_volume()));
        let packet = ControlPacket::Message { id: 5, message };

        let mut buf = [0; ControlMessage::MAX_ENCODED_LEN];
        let len = message.encode(&mut buf).unwrap();

        assert_eq!(len, message.encoded_len());
        assert!(len <= ControlMessage::MAX_ENCODED_LEN);
        assert_eq!(buf[0], 15);
        assert_eq!(ControlMessage::decode(&buf[..len]), Ok(message));
        assert_eq!(
            ControlPacket::read(&packet.to_datagram().unwrap()),
            Ok(packet)
        );
    }

    #[test]
    fn the_control_message_refuses_trailing_bytes_and_every_truncation() {
        let message = ControlMessage::Display(display(DisplayVolume::srgb()));
        let mut buf = [0; ControlMessage::MAX_ENCODED_LEN + 1];
        let len = message.encode(&mut buf).unwrap();

        assert_eq!(
            ControlMessage::decode(&buf[..=len]),
            Err(ProtocolError::TrailingBytes { extra: 1 })
        );
        for cut in 0..len {
            assert!(matches!(
                ControlMessage::decode(&buf[..cut]),
                Err(ProtocolError::BufferTooShort { .. })
            ));
        }
    }

    #[test]
    fn the_control_message_range_checks_a_hostile_size() {
        let message = ControlMessage::Display(display(DisplayVolume::srgb()));
        let mut buf = [0; ControlMessage::MAX_ENCODED_LEN];
        let len = message.encode(&mut buf).unwrap();
        buf[1..5].copy_from_slice(&u32::MAX.to_be_bytes());
        buf[5..9].copy_from_slice(&u32::MAX.to_be_bytes());

        assert!(matches!(
            ControlMessage::decode(&buf[..len]),
            Err(ProtocolError::DisplayResolutionOutOfRange { .. })
        ));
    }

    #[test]
    fn every_truncation_is_refused() {
        let good = encode(&display(custom_volume()));

        for len in 0..good.len() {
            assert!(matches!(
                decode(&good[..len]),
                Err(ProtocolError::BufferTooShort { .. })
            ));
        }
    }
}
