use std::collections::BTreeMap;

use crate::NetError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FecScheme {
    #[default]
    None,
    ReedSolomon {
        data_shards: u8,
        parity_shards: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FecBlock {
    pub first_fragment: u16,
    pub data_shards: u8,
    pub parity_shards: u8,
}

impl FecScheme {
    pub fn for_overhead(data_shards: u8, overhead_percent: u8) -> Self {
        let parity = u16::from(data_shards) * u16::from(overhead_percent);
        let parity_shards = u8::try_from(parity.div_ceil(100)).unwrap_or(u8::MAX);

        if parity_shards == 0 || data_shards == 0 {
            Self::None
        } else {
            Self::ReedSolomon {
                data_shards,
                parity_shards,
            }
        }
    }

    pub fn blocks(self, fragment_count: u16) -> Vec<FecBlock> {
        let Self::ReedSolomon {
            data_shards,
            parity_shards,
        } = self
        else {
            return Vec::new();
        };

        if fragment_count == 0 {
            return Vec::new();
        }

        let fragments = usize::from(fragment_count);
        let block_count = fragments.div_ceil(usize::from(data_shards));
        let base = fragments / block_count;
        let larger_blocks = fragments % block_count;

        let mut first_fragment = 0;
        (0..block_count)
            .map(|index| {
                let size = base + usize::from(index < larger_blocks);
                let parity = (size * usize::from(parity_shards))
                    .div_ceil(usize::from(data_shards))
                    .clamp(1, usize::from(u8::MAX));
                let block = FecBlock {
                    first_fragment,
                    data_shards: u8::try_from(size).unwrap_or(u8::MAX),
                    parity_shards: u8::try_from(parity).unwrap_or(u8::MAX),
                };
                first_fragment += u16::try_from(size).unwrap_or(u16::MAX);
                block
            })
            .collect()
    }
}

impl FecBlock {
    pub fn encode(&self, data: &[&[u8]]) -> Result<Vec<Vec<u8>>, NetError> {
        if data.len() != usize::from(self.data_shards) {
            return Err(NetError::FecShardCount {
                expected: usize::from(self.data_shards),
                found: data.len(),
            });
        }

        let parity = reed_solomon_simd::encode(
            usize::from(self.data_shards),
            usize::from(self.parity_shards),
            data,
        )?;

        Ok(parity)
    }

    pub fn recover<'a>(
        &self,
        data: impl IntoIterator<Item = (usize, &'a [u8])>,
        parity: impl IntoIterator<Item = (usize, &'a [u8])>,
    ) -> Result<BTreeMap<usize, Vec<u8>>, NetError> {
        let restored = reed_solomon_simd::decode(
            usize::from(self.data_shards),
            usize::from(self.parity_shards),
            data,
            parity,
        )?;

        Ok(restored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHARD_LEN: usize = 64;

    fn shards(count: usize, seed: u64) -> Vec<Vec<u8>> {
        let mut state = seed | 1;
        (0..count)
            .map(|_| {
                (0..SHARD_LEN)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        state.to_le_bytes()[0]
                    })
                    .collect()
            })
            .collect()
    }

    fn block(data: u8, parity: u8) -> FecBlock {
        FecBlock {
            first_fragment: 0,
            data_shards: data,
            parity_shards: parity,
        }
    }

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

    #[test]
    fn zero_data_shards_disable_fec() {
        assert_eq!(FecScheme::for_overhead(0, 50), FecScheme::None);
    }

    #[test]
    fn none_scheme_has_no_blocks() {
        assert_eq!(FecScheme::None.blocks(10).len(), 0);
    }

    #[test]
    fn blocks_cover_every_fragment_without_overlap() {
        let scheme = FecScheme::for_overhead(16, 25);

        for fragments in [1u16, 2, 15, 16, 17, 33, 100, 1000, u16::MAX] {
            let blocks = scheme.blocks(fragments);
            let mut next = 0u32;

            for block in &blocks {
                assert_eq!(u32::from(block.first_fragment), next);
                assert!((1..=16).contains(&block.data_shards));
                assert!(block.parity_shards >= 1);
                next += u32::from(block.data_shards);
            }

            assert_eq!(next, u32::from(fragments));
        }
    }

    #[test]
    fn blocks_are_balanced() {
        let blocks = FecScheme::for_overhead(16, 10).blocks(17);

        let sizes: Vec<u8> = blocks.iter().map(|block| block.data_shards).collect();

        assert_eq!(sizes, [9, 8]);
    }

    #[test]
    fn single_fragment_gets_one_parity_shard() {
        let blocks = FecScheme::for_overhead(16, 10).blocks(1);

        assert_eq!(blocks, [block(1, 1)]);
    }

    #[test]
    fn recovers_every_loss_pattern_up_to_parity() {
        let block = block(4, 2);
        let data = shards(4, 7);
        let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();
        let parity = block.encode(&refs).unwrap();

        for lost_mask in 0u32..(1 << 6) {
            if lost_mask.count_ones() > 2 {
                continue;
            }

            let present_data = data
                .iter()
                .enumerate()
                .filter(|(index, _)| lost_mask & (1 << index) == 0)
                .map(|(index, shard)| (index, shard.as_slice()));
            let present_parity = parity
                .iter()
                .enumerate()
                .filter(|(index, _)| lost_mask & (1 << (index + 4)) == 0)
                .map(|(index, shard)| (index, shard.as_slice()));

            let restored = block.recover(present_data, present_parity).unwrap();
            let missing_data = (0..4).filter(|index| lost_mask & (1 << index) != 0).count();

            for (index, shard) in &restored {
                assert_eq!(shard, &data[*index]);
            }
            assert_eq!(restored.len(), missing_data);
        }
    }

    #[test]
    fn too_many_losses_fail() {
        let block = block(4, 2);
        let data = shards(4, 11);
        let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();
        let parity = block.encode(&refs).unwrap();

        let result = block.recover(
            [(0, data[0].as_slice()), (1, data[1].as_slice())],
            [(0, parity[0].as_slice())],
        );

        assert!(matches!(result, Err(NetError::FecCoding(_))));
    }

    #[test]
    fn wrong_shard_count_is_rejected() {
        let block = block(4, 2);
        let data = shards(3, 5);
        let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();

        assert!(matches!(
            block.encode(&refs),
            Err(NetError::FecShardCount {
                expected: 4,
                found: 3
            })
        ));
    }

    #[test]
    fn odd_shard_length_is_rejected() {
        let block = block(2, 1);
        let data = [vec![1u8; 5], vec![2u8; 5]];
        let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();

        assert!(matches!(block.encode(&refs), Err(NetError::FecCoding(_))));
    }
}
