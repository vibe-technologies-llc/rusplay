use rusplay_core::{Chromaticity, MasteringDisplay};
#[cfg(any(feature = "vaapi", feature = "nvenc"))]
use rusplay_core::{ContentLightLevel, Luminance};

#[cfg(any(feature = "vaapi", feature = "nvenc"))]
const AV1_CHROMATICITY_UNITS: u64 = 1 << 16;
#[cfg(any(feature = "vaapi", feature = "nvenc"))]
const AV1_MAX_LUMINANCE_UNITS: u64 = 1 << 8;
#[cfg(any(feature = "vaapi", feature = "nvenc"))]
const AV1_MIN_LUMINANCE_UNITS: u64 = 1 << 14;
#[cfg(any(feature = "vaapi", feature = "nvenc"))]
const MASTERING_PAYLOAD_LEN: usize = 24;
#[cfg(any(feature = "vaapi", feature = "nvenc"))]
const CONTENT_LIGHT_PAYLOAD_LEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct St2086 {
    pub green: [u16; 2],
    pub blue: [u16; 2],
    pub red: [u16; 2],
    pub white: [u16; 2],
    pub max_luminance: u32,
    pub min_luminance: u32,
}

impl St2086 {
    pub(crate) fn of(display: &MasteringDisplay) -> Self {
        let chromaticities = display.primaries.chromaticities();
        let units = |point: Chromaticity| {
            let (x, y) = point.to_units();
            [x, y]
        };

        Self {
            green: units(chromaticities.green()),
            blue: units(chromaticities.blue()),
            red: units(chromaticities.red()),
            white: units(chromaticities.white()),
            max_luminance: display.max_nits.units(),
            min_luminance: display.min_nits.units(),
        }
    }

    #[cfg(any(feature = "vaapi", feature = "nvenc"))]
    pub(crate) fn sei_payload(&self) -> [u8; MASTERING_PAYLOAD_LEN] {
        let mut payload = [0; MASTERING_PAYLOAD_LEN];
        let points = [self.green, self.blue, self.red, self.white];
        for (slot, [x, y]) in payload.as_chunks_mut::<4>().0.iter_mut().zip(points) {
            slot[..2].copy_from_slice(&x.to_be_bytes());
            slot[2..].copy_from_slice(&y.to_be_bytes());
        }
        payload[16..20].copy_from_slice(&self.max_luminance.to_be_bytes());
        payload[20..].copy_from_slice(&self.min_luminance.to_be_bytes());

        payload
    }

    #[cfg(any(feature = "vaapi", feature = "nvenc"))]
    pub(crate) fn av1_fixed_point(&self) -> Self {
        let point = |[x, y]: [u16; 2]| [av1_chromaticity(x), av1_chromaticity(y)];

        Self {
            green: point(self.green),
            blue: point(self.blue),
            red: point(self.red),
            white: point(self.white),
            max_luminance: av1_luminance(self.max_luminance, AV1_MAX_LUMINANCE_UNITS),
            min_luminance: av1_luminance(self.min_luminance, AV1_MIN_LUMINANCE_UNITS),
        }
    }

    #[cfg(feature = "vaapi")]
    pub(crate) fn av1_payload(&self) -> [u8; MASTERING_PAYLOAD_LEN] {
        let fixed = self.av1_fixed_point();
        let mut payload = [0; MASTERING_PAYLOAD_LEN];
        let points = [fixed.red, fixed.green, fixed.blue, fixed.white];
        for (slot, [x, y]) in payload.as_chunks_mut::<4>().0.iter_mut().zip(points) {
            slot[..2].copy_from_slice(&x.to_be_bytes());
            slot[2..].copy_from_slice(&y.to_be_bytes());
        }
        payload[16..20].copy_from_slice(&fixed.max_luminance.to_be_bytes());
        payload[20..].copy_from_slice(&fixed.min_luminance.to_be_bytes());

        payload
    }

    #[cfg(any(feature = "x265", test))]
    pub(crate) fn x265_string(&self) -> String {
        let [gx, gy] = self.green;
        let [bx, by] = self.blue;
        let [rx, ry] = self.red;
        let [wx, wy] = self.white;

        format!(
            "G({gx},{gy})B({bx},{by})R({rx},{ry})WP({wx},{wy})L({},{})",
            self.max_luminance, self.min_luminance
        )
    }

    #[cfg(any(feature = "svt-av1", test))]
    pub(crate) fn svt_av1_string(&self) -> String {
        let one = f64::from(Chromaticity::UNITS_PER_ONE);
        let nit = f64::from(rusplay_core::Luminance::UNITS_PER_NIT);
        let point =
            |[x, y]: [u16; 2]| format!("({:.5},{:.5})", f64::from(x) / one, f64::from(y) / one);

        format!(
            "G{}B{}R{}WP{}L({:.4},{:.4})",
            point(self.green),
            point(self.blue),
            point(self.red),
            point(self.white),
            f64::from(self.max_luminance) / nit,
            f64::from(self.min_luminance) / nit,
        )
    }
}

#[cfg(any(feature = "vaapi", feature = "nvenc"))]
pub(crate) fn content_light_payload(light: ContentLightLevel) -> [u8; CONTENT_LIGHT_PAYLOAD_LEN] {
    let [cll_high, cll_low] = light.max_cll_nits.to_be_bytes();
    let [fall_high, fall_low] = light.max_fall_nits.to_be_bytes();

    [cll_high, cll_low, fall_high, fall_low]
}

#[cfg(any(feature = "vaapi", feature = "nvenc"))]
fn rescale(value: u64, to: u64, from: u64) -> u64 {
    (value * to + from / 2) / from
}

#[cfg(any(feature = "vaapi", feature = "nvenc"))]
fn av1_chromaticity(units: u16) -> u16 {
    let scaled = rescale(
        u64::from(units),
        AV1_CHROMATICITY_UNITS,
        u64::from(Chromaticity::UNITS_PER_ONE),
    );

    u16::try_from(scaled).unwrap_or(u16::MAX)
}

#[cfg(any(feature = "vaapi", feature = "nvenc"))]
fn av1_luminance(units: u32, fixed_point: u64) -> u32 {
    let scaled = rescale(
        u64::from(units),
        fixed_point,
        u64::from(Luminance::UNITS_PER_NIT),
    );

    u32::try_from(scaled).unwrap_or(u32::MAX)
}

#[cfg(test)]
pub(crate) mod fixtures {
    use rusplay_core::{
        ColorPrimaries, ColorRange, ColorSpace, ContentLightLevel, HdrLevels, Luminance,
        MasteringDisplay, MatrixCoefficients, SignalledColor, TransferFunction,
    };

    pub(crate) fn hdr10_color() -> ColorSpace {
        ColorSpace {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Pq,
            matrix: Some(MatrixCoefficients::Bt2020Ncl),
            range: ColorRange::Limited,
            hdr: HdrLevels {
                mastering: Some(MasteringDisplay {
                    primaries: ColorPrimaries::DisplayP3,
                    max_nits: Luminance::from_whole_nits(1000).unwrap(),
                    min_nits: Luminance::from_units(50).unwrap(),
                }),
                content_light: Some(ContentLightLevel {
                    max_cll_nits: 1000,
                    max_fall_nits: 400,
                }),
                ..HdrLevels::default()
            },
        }
    }

    pub(crate) fn signalled(color: &ColorSpace) -> SignalledColor {
        SignalledColor {
            primaries: Some(color.primaries),
            transfer: Some(color.transfer),
            matrix: Some(color.matrix_coefficients()),
            range: Some(color.range),
            mastering: color.hdr.mastering,
            content_light: color.hdr.content_light,
        }
    }
}

#[cfg(test)]
mod tests {
    use rusplay_core::{ColorPrimaries, Luminance};

    use super::*;

    fn bt2020_display() -> MasteringDisplay {
        MasteringDisplay {
            primaries: ColorPrimaries::Bt2020,
            max_nits: Luminance::from_whole_nits(1000).unwrap(),
            min_nits: Luminance::from_units(50).unwrap(),
        }
    }

    #[test]
    fn st2086_orders_primaries_green_blue_red() {
        let units = St2086::of(&bt2020_display());

        assert_eq!(units.green, [8_500, 39_850]);
        assert_eq!(units.blue, [6_550, 2_300]);
        assert_eq!(units.red, [35_400, 14_600]);
        assert_eq!(units.white, [15_635, 16_450]);
        assert_eq!(units.max_luminance, 10_000_000);
        assert_eq!(units.min_luminance, 50);
    }

    #[cfg(any(feature = "vaapi", feature = "nvenc"))]
    #[test]
    fn sei_payload_orders_green_blue_red_white_then_luminance() {
        let payload = St2086::of(&bt2020_display()).sei_payload();

        assert_eq!(&payload[..4], &[0x21, 0x34, 0x9b, 0xaa]);
        assert_eq!(&payload[16..20], &[0x00, 0x98, 0x96, 0x80]);
        assert_eq!(&payload[20..], &[0, 0, 0, 50]);
    }

    #[cfg(any(feature = "vaapi", feature = "nvenc"))]
    #[test]
    fn av1_fixed_point_rescales_chromaticities_and_luminance() {
        let fixed = St2086::of(&bt2020_display()).av1_fixed_point();

        assert_eq!(
            fixed.red,
            [av1_chromaticity(35_400), av1_chromaticity(14_600)]
        );
        assert_eq!(fixed.green[0], av1_chromaticity(8_500));
        assert_eq!(fixed.max_luminance, 1000 << 8);
        assert_eq!(fixed.min_luminance, 82);
    }

    #[cfg(feature = "vaapi")]
    #[test]
    fn av1_payload_uses_av1_fixed_point_in_red_green_blue_order() {
        let payload = St2086::of(&bt2020_display()).av1_payload();

        assert_eq!(&payload[..2], &av1_chromaticity(35_400).to_be_bytes());
        assert_eq!(&payload[16..20], &(1000_u32 << 8).to_be_bytes());
        assert_eq!(&payload[20..], &82_u32.to_be_bytes());
    }

    #[cfg(any(feature = "vaapi", feature = "nvenc"))]
    #[test]
    fn content_light_payload_is_two_big_endian_words() {
        let light = rusplay_core::ContentLightLevel {
            max_cll_nits: 1000,
            max_fall_nits: 400,
        };

        assert_eq!(content_light_payload(light), [0x03, 0xe8, 0x01, 0x90]);
    }

    #[cfg(any(feature = "vaapi", feature = "nvenc"))]
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

    #[test]
    fn x265_takes_integer_st2086_units() {
        assert_eq!(
            St2086::of(&bt2020_display()).x265_string(),
            "G(8500,39850)B(6550,2300)R(35400,14600)WP(15635,16450)L(10000000,50)"
        );
    }

    #[test]
    fn svt_av1_takes_chromaticities_and_nits() {
        assert_eq!(
            St2086::of(&bt2020_display()).svt_av1_string(),
            "G(0.17000,0.79700)B(0.13100,0.04600)R(0.70800,0.29200)WP(0.31270,0.32900)L(1000.0000,0.0050)"
        );
    }
}
