use std::{collections::BTreeMap, fmt};

use reed_solomon_simd::{EncoderResult, ReedSolomonEncoder};

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

#[derive(Default)]
pub struct FecEncoder {
    engine: Option<ReedSolomonEncoder>,
}

pub struct BlockEncoding<'a> {
    engine: &'a mut ReedSolomonEncoder,
    expected: usize,
    added: usize,
}

impl FecEncoder {
    pub fn begin(
        &mut self,
        block: &FecBlock,
        shard_len: usize,
    ) -> Result<BlockEncoding<'_>, NetError> {
        let data_shards = usize::from(block.data_shards);
        let parity_shards = usize::from(block.parity_shards);

        let engine = match &mut self.engine {
            Some(engine) => {
                engine.reset(data_shards, parity_shards, shard_len)?;
                engine
            }
            empty => empty.insert(ReedSolomonEncoder::new(
                data_shards,
                parity_shards,
                shard_len,
            )?),
        };

        Ok(BlockEncoding {
            engine,
            expected: data_shards,
            added: 0,
        })
    }

    pub fn encode<'a>(
        &'a mut self,
        block: &FecBlock,
        data: &[&[u8]],
    ) -> Result<EncoderResult<'a>, NetError> {
        let shard_len = data.first().map_or(0, |shard| shard.len());
        let mut encoding = self.begin(block, shard_len)?;
        for shard in data {
            encoding.add(shard)?;
        }

        encoding.finish()
    }
}

impl Clone for FecEncoder {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl fmt::Debug for FecEncoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FecEncoder")
            .field("warm", &self.engine.is_some())
            .finish()
    }
}

impl<'a> BlockEncoding<'a> {
    pub fn add(&mut self, shard: &[u8]) -> Result<(), NetError> {
        self.engine.add_original_shard(shard)?;
        self.added += 1;

        Ok(())
    }

    pub fn finish(self) -> Result<EncoderResult<'a>, NetError> {
        if self.added != self.expected {
            return Err(NetError::FecShardCount {
                expected: self.expected,
                found: self.added,
            });
        }

        Ok(self.engine.encode()?)
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

    fn parity_of(block: FecBlock, refs: &[&[u8]]) -> Result<Vec<Vec<u8>>, NetError> {
        let mut encoder = FecEncoder::default();
        let result = encoder.encode(&block, refs)?;

        Ok(result.recovery_iter().map(<[u8]>::to_vec).collect())
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
        let parity = parity_of(block, &refs).unwrap();

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
        let parity = parity_of(block, &refs).unwrap();

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
            parity_of(block, &refs),
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

        assert!(matches!(
            parity_of(block, &refs),
            Err(NetError::FecCoding(_))
        ));
    }

    #[test]
    fn parity_matches_the_one_shot_encoder() {
        for (data_shards, parity_shards, seed) in [(1, 1, 3), (4, 2, 7), (16, 4, 9), (32, 8, 13)] {
            let block = block(data_shards, parity_shards);
            let data = shards(usize::from(data_shards), seed);
            let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();

            let parity = parity_of(block, &refs).unwrap();

            assert_eq!(
                parity,
                reed_solomon_simd::encode(
                    usize::from(data_shards),
                    usize::from(parity_shards),
                    &refs
                )
                .unwrap()
            );
        }
    }

    #[test]
    fn one_encoder_serves_blocks_of_changing_shape() {
        let mut encoder = FecEncoder::default();
        let shapes = [(4u8, 2u8, 64usize), (9, 3, 64), (2, 1, 32), (4, 2, 64)];

        for (index, (data_shards, parity_shards, shard_len)) in shapes.into_iter().enumerate() {
            let block = block(data_shards, parity_shards);
            let data: Vec<Vec<u8>> = shards(usize::from(data_shards), index as u64 + 1)
                .into_iter()
                .map(|shard| shard[..shard_len].to_vec())
                .collect();
            let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();

            let reused: Vec<Vec<u8>> = encoder
                .encode(&block, &refs)
                .unwrap()
                .recovery_iter()
                .map(<[u8]>::to_vec)
                .collect();

            assert_eq!(reused, parity_of(block, &refs).unwrap());
        }
    }

    #[test]
    fn a_failed_block_leaves_the_encoder_usable() {
        let mut encoder = FecEncoder::default();
        let block = block(4, 2);
        let short = shards(3, 5);
        let short_refs: Vec<&[u8]> = short.iter().map(Vec::as_slice).collect();
        let full = shards(4, 5);
        let full_refs: Vec<&[u8]> = full.iter().map(Vec::as_slice).collect();

        let rejected = encoder.encode(&block, &short_refs).is_err();
        let parity: Vec<Vec<u8>> = encoder
            .encode(&block, &full_refs)
            .unwrap()
            .recovery_iter()
            .map(<[u8]>::to_vec)
            .collect();

        assert!(rejected);
        assert_eq!(parity, parity_of(block, &full_refs).unwrap());
    }
}
