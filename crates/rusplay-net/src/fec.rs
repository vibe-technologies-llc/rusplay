#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FecScheme {
    #[default]
    None,
    ReedSolomon {
        data_shards: u8,
        parity_shards: u8,
    },
}

impl FecScheme {
    pub fn for_overhead(data_shards: u8, overhead_percent: u8) -> Self {
        let parity = u16::from(data_shards) * u16::from(overhead_percent);
        let parity_shards = u8::try_from(parity.div_ceil(100)).unwrap_or(u8::MAX);

        if parity_shards == 0 {
            Self::None
        } else {
            Self::ReedSolomon {
                data_shards,
                parity_shards,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overhead_rounds_parity_up() {
        assert_eq!(
            FecScheme::for_overhead(16, 10),
            FecScheme::ReedSolomon {
                data_shards: 16,
                parity_shards: 2,
            }
        );
    }

    #[test]
    fn zero_overhead_disables_fec() {
        assert_eq!(FecScheme::for_overhead(16, 0), FecScheme::None);
    }
}
