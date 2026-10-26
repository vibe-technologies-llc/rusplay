use rusplay_core::{Chromaticities, HdrLevels, Matrix3, pq_from_nits};

const ICTCP_SCALE: f64 = 4096.0;
const MAX_NITS: f64 = HdrLevels::MAX_NITS as f64;

const OKLAB_XYZ_TO_LMS: Matrix3 = Matrix3([
    [0.818_933_010_1, 0.361_866_742_4, -0.128_859_713_7],
    [0.032_984_543_6, 0.929_311_871_5, 0.036_145_638_7],
    [0.048_200_301_8, 0.264_366_269_1, 0.633_851_707_0],
]);
const OKLAB_LMS_TO_LAB: Matrix3 = Matrix3([
    [0.210_454_255_3, 0.793_617_785_0, -0.004_072_046_8],
    [1.977_998_495_1, -2.428_592_205_0, 0.450_593_709_9],
    [0.025_904_037_1, 0.782_771_766_2, -0.808_675_766_0],
]);
const ICTCP_RGB_TO_LMS: [[f64; 3]; 3] = [
    [1688.0, 2146.0, 262.0],
    [683.0, 2951.0, 462.0],
    [99.0, 309.0, 3688.0],
];
const ICTCP_LMS_TO_ITP: [[f64; 3]; 3] = [
    [2048.0, 2048.0, 0.0],
    [6610.0, -13613.0, 7003.0],
    [17933.0, -17390.0, -543.0],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkingSpace {
    Oklab,
    Ictcp,
}

impl WorkingSpace {
    pub(crate) const fn code(self) -> u32 {
        match self {
            Self::Oklab => 0,
            Self::Ictcp => 1,
        }
    }

    pub fn xyz_to_lms(self) -> Matrix3 {
        match self {
            Self::Oklab => {
                let white = OKLAB_XYZ_TO_LMS.apply(Chromaticities::BT709.white().xyz());
                Matrix3::diagonal(white.map(f64::recip)) * OKLAB_XYZ_TO_LMS
            }
            Self::Ictcp => scaled(ICTCP_RGB_TO_LMS) * Chromaticities::BT2020.xyz_to_rgb(),
        }
    }

    pub(crate) fn shape(self, lms: [f64; 3], pq_scale: f64) -> [f64; 3] {
        match self {
            Self::Ictcp => {
                lms.map(|value| value.signum() * pq_from_nits((value * pq_scale).abs() * MAX_NITS))
            }
            Self::Oklab => lms.map(f64::cbrt),
        }
    }

    pub fn lms_to_lab(self) -> Matrix3 {
        match self {
            Self::Oklab => OKLAB_LMS_TO_LAB,
            Self::Ictcp => scaled(ICTCP_LMS_TO_ITP),
        }
    }
}

fn scaled(rows: [[f64; 3]; 3]) -> Matrix3 {
    Matrix3(rows.map(|row| row.map(|value| value / ICTCP_SCALE)))
}
