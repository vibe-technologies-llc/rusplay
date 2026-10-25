use std::fmt;

use serde::{Deserialize, Serialize};

use crate::color::HdrLevels;

const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransferFunction {
    #[default]
    Bt709,
    Srgb,
    Gamma22,
    Pq,
    Hlg,
    Linear,
}

impl TransferFunction {
    pub const ALL: [Self; 6] = [
        Self::Bt709,
        Self::Srgb,
        Self::Gamma22,
        Self::Pq,
        Self::Hlg,
        Self::Linear,
    ];

    pub const fn is_hdr(self) -> bool {
        matches!(self, Self::Pq | Self::Hlg)
    }

    pub const fn h273_code(self) -> u8 {
        match self {
            Self::Bt709 => 1,
            Self::Gamma22 => 4,
            Self::Linear => 8,
            Self::Srgb => 13,
            Self::Pq => 16,
            Self::Hlg => 18,
        }
    }

    pub fn from_h273_code(code: u8) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|transfer| transfer.h273_code() == code)
    }
}

impl fmt::Display for TransferFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bt709 => "bt709",
            Self::Srgb => "srgb",
            Self::Gamma22 => "gamma22",
            Self::Pq => "pq",
            Self::Hlg => "hlg",
            Self::Linear => "linear",
        })
    }
}

pub fn pq_from_nits(nits: f64) -> f64 {
    let normalized = (nits / f64::from(HdrLevels::MAX_NITS)).clamp(0.0, 1.0);
    let power = normalized.powf(PQ_M1);

    ((PQ_C1 + PQ_C2 * power) / (1.0 + PQ_C3 * power)).powf(PQ_M2)
}

pub fn pq_to_nits(code: f64) -> f64 {
    let power = code.clamp(0.0, 1.0).powf(1.0 / PQ_M2);
    let normalized = ((power - PQ_C1).max(0.0) / (PQ_C2 - PQ_C3 * power)).powf(1.0 / PQ_M1);

    normalized * f64::from(HdrLevels::MAX_NITS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pq_matches_st2084_reference_codes() {
        assert!((pq_from_nits(100.0) - 0.508_078).abs() < 1e-6);
        assert!((pq_from_nits(1000.0) - 0.751_827).abs() < 1e-6);
        assert!((pq_from_nits(10_000.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn pq_round_trips() {
        for nits in [0.0, 0.01, 1.0, 100.0, 203.0, 1000.0, 4000.0, 10_000.0] {
            assert!((pq_to_nits(pq_from_nits(nits)) - nits).abs() < 1e-9 * nits.max(1.0));
        }
    }

    #[test]
    fn transfer_h273_codes_round_trip() {
        for transfer in TransferFunction::ALL {
            assert_eq!(
                TransferFunction::from_h273_code(transfer.h273_code()),
                Some(transfer)
            );
        }
    }
}
