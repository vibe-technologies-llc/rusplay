use rusplay_core::{Chromaticity, MasteringDisplay};

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
