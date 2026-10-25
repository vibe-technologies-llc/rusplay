use rusplay_core::{FrameRate, Resolution};

#[derive(Debug, Clone, Copy)]
pub(super) struct Level {
    pub(super) id: u8,
    pub(super) max_picture_samples: u64,
    pub(super) max_sample_rate: u64,
    pub(super) max_kbps: u64,
    pub(super) max_kbps_high_tier: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Selected {
    pub(super) id: u8,
    pub(super) high_tier: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Demand {
    pub(super) picture_samples: u64,
    pub(super) sample_rate: u64,
    pub(super) kbps: u64,
}

impl Demand {
    pub(super) fn new(resolution: Resolution, frame_rate: FrameRate, kbps: u32) -> Self {
        let picture_samples = resolution.pixel_count();
        let numerator = u64::from(frame_rate.numerator);
        let denominator = u64::from(frame_rate.denominator).max(1);

        Self {
            picture_samples,
            sample_rate: (picture_samples * numerator).div_ceil(denominator),
            kbps: u64::from(kbps),
        }
    }
}

pub(super) fn select(levels: &[Level], demand: Demand, bitrate_scale_percent: u64) -> Selected {
    let scaled = |kbps: u64| kbps * bitrate_scale_percent / 100;

    for level in levels {
        if demand.picture_samples > level.max_picture_samples
            || demand.sample_rate > level.max_sample_rate
        {
            continue;
        }
        if demand.kbps <= scaled(level.max_kbps) {
            return Selected {
                id: level.id,
                high_tier: false,
            };
        }
        if level
            .max_kbps_high_tier
            .is_some_and(|high| demand.kbps <= scaled(high))
        {
            return Selected {
                id: level.id,
                high_tier: true,
            };
        }
    }

    levels.last().map_or(
        Selected {
            id: 0,
            high_tier: false,
        },
        |last| Selected {
            id: last.id,
            high_tier: last.max_kbps_high_tier.is_some(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEVELS: [Level; 2] = [
        Level {
            id: 1,
            max_picture_samples: 1000,
            max_sample_rate: 60_000,
            max_kbps: 100,
            max_kbps_high_tier: Some(300),
        },
        Level {
            id: 2,
            max_picture_samples: 4000,
            max_sample_rate: 240_000,
            max_kbps: 400,
            max_kbps_high_tier: None,
        },
    ];

    fn demand(picture_samples: u64, kbps: u64) -> Demand {
        Demand {
            picture_samples,
            sample_rate: picture_samples * 60,
            kbps,
        }
    }

    #[test]
    fn smallest_fitting_level_wins() {
        assert_eq!(select(&LEVELS, demand(900, 50), 100).id, 1);
        assert_eq!(select(&LEVELS, demand(2000, 50), 100).id, 2);
    }

    #[test]
    fn high_tier_absorbs_bitrate_before_a_bigger_level() {
        let selected = select(&LEVELS, demand(900, 200), 100);

        assert_eq!(
            selected,
            Selected {
                id: 1,
                high_tier: true
            }
        );
    }

    #[test]
    fn oversized_streams_get_the_top_level() {
        assert_eq!(select(&LEVELS, demand(9000, 50), 100).id, 2);
    }
}
