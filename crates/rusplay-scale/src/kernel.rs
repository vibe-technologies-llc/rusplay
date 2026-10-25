use std::f64::consts::PI;

use rusplay_core::ScaleFilter;

use crate::bessel;

const JINC_FIRST_ZERO: f64 = 1.219_669_891_266_504_5;
const JINC_THIRD_ZERO: f64 = 3.238_315_484_166_236;
const EWA_LANCZOS_SHARP_BLUR: f64 = 0.981_250_564_426_935_6;
const ROBIDOUX: (f64, f64) = (0.378_215_755_093_998_7, 0.310_892_122_453_000_7);
const ROBIDOUX_SHARP: (f64, f64) = (0.262_014_512_399_014_2, 0.368_992_743_800_492_9);
const MITCHELL: (f64, f64) = (1.0 / 3.0, 1.0 / 3.0);
const CATMULL_ROM: (f64, f64) = (0.0, 0.5);
const HERMITE: (f64, f64) = (0.0, 0.0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Geometry {
    Separable,
    Polar,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Kernel {
    filter: ScaleFilter,
}

impl Kernel {
    pub const fn new(filter: ScaleFilter) -> Self {
        Self { filter }
    }

    pub const fn filter(self) -> ScaleFilter {
        self.filter
    }

    pub const fn geometry(self) -> Geometry {
        if self.filter.is_polar() {
            Geometry::Polar
        } else {
            Geometry::Separable
        }
    }

    pub fn radius(self) -> f64 {
        match self.filter {
            ScaleFilter::Bilinear | ScaleFilter::Hermite => 1.0,
            ScaleFilter::CatmullRom
            | ScaleFilter::Mitchell
            | ScaleFilter::Lanczos2
            | ScaleFilter::EwaRobidoux
            | ScaleFilter::EwaRobidouxSharp => 2.0,
            ScaleFilter::Lanczos3 => 3.0,
            ScaleFilter::Lanczos4 => 4.0,
            ScaleFilter::EwaLanczos | ScaleFilter::EwaGinseng | ScaleFilter::EwaHann => {
                JINC_THIRD_ZERO
            }
            ScaleFilter::EwaLanczosSharp => JINC_THIRD_ZERO * EWA_LANCZOS_SHARP_BLUR,
        }
    }

    pub fn weight(self, distance: f64) -> f64 {
        let x = distance.abs();
        let radius = self.radius();
        if x >= radius {
            return 0.0;
        }

        match self.filter {
            ScaleFilter::Bilinear => 1.0 - x,
            ScaleFilter::Hermite => bc_spline(HERMITE, x),
            ScaleFilter::CatmullRom => bc_spline(CATMULL_ROM, x),
            ScaleFilter::Mitchell => bc_spline(MITCHELL, x),
            ScaleFilter::Lanczos2 | ScaleFilter::Lanczos3 | ScaleFilter::Lanczos4 => {
                sinc(x) * sinc(x / radius)
            }
            ScaleFilter::EwaLanczos => jinc(x) * jinc(x * JINC_FIRST_ZERO / radius),
            ScaleFilter::EwaLanczosSharp => {
                let unblurred = x / EWA_LANCZOS_SHARP_BLUR;
                jinc(unblurred) * jinc(unblurred * JINC_FIRST_ZERO / JINC_THIRD_ZERO)
            }
            ScaleFilter::EwaGinseng => jinc(x) * sinc(x / radius),
            ScaleFilter::EwaHann => jinc(x) * (0.5 + 0.5 * (PI * x / radius).cos()),
            ScaleFilter::EwaRobidoux => bc_spline(ROBIDOUX, x),
            ScaleFilter::EwaRobidouxSharp => bc_spline(ROBIDOUX_SHARP, x),
        }
    }
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }

    let px = PI * x;
    px.sin() / px
}

fn jinc(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }

    let px = PI * x;
    2.0 * bessel::j1(px) / px
}

fn bc_spline((b, c): (f64, f64), x: f64) -> f64 {
    let x2 = x * x;
    let x3 = x2 * x;

    let value = if x < 1.0 {
        (12.0 - 9.0 * b - 6.0 * c) * x3 + (-18.0 + 12.0 * b + 6.0 * c) * x2 + (6.0 - 2.0 * b)
    } else {
        (-b - 6.0 * c) * x3
            + (6.0 * b + 30.0 * c) * x2
            + (-12.0 * b - 48.0 * c) * x
            + (8.0 * b + 24.0 * c)
    };

    value / 6.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f64 = 1e-9;

    #[test]
    fn interpolating_kernels_are_one_at_center_and_zero_at_integers() {
        for filter in [
            ScaleFilter::Bilinear,
            ScaleFilter::Hermite,
            ScaleFilter::CatmullRom,
            ScaleFilter::Lanczos3,
        ] {
            let kernel = Kernel::new(filter);

            assert!((kernel.weight(0.0) - 1.0).abs() < EPSILON, "{filter}");
            assert!(kernel.weight(1.0).abs() < EPSILON, "{filter}");
            assert!(kernel.weight(2.0).abs() < EPSILON, "{filter}");
        }
    }

    #[test]
    fn jinc_vanishes_at_its_first_zero() {
        assert!(jinc(JINC_FIRST_ZERO).abs() < 1e-7);
    }

    #[test]
    fn ewa_filters_are_polar() {
        assert_eq!(
            Kernel::new(ScaleFilter::EwaLanczos).geometry(),
            Geometry::Polar
        );
        assert_eq!(
            Kernel::new(ScaleFilter::Hermite).geometry(),
            Geometry::Separable
        );
    }

    #[test]
    fn weights_vanish_outside_the_radius() {
        let kernel = Kernel::new(ScaleFilter::EwaLanczosSharp);

        assert_eq!(kernel.weight(kernel.radius()), 0.0);
        assert!(kernel.weight(0.5) > 0.0);
    }
}
