#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RefreshRegion {
    pub(super) location: u32,
    pub(super) size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RefreshCadence {
    units: u32,
    period: u32,
    frame: u32,
}

impl RefreshCadence {
    pub(super) fn new(units: u32, period: u32) -> Self {
        Self {
            units: units.max(1),
            period: period.max(1),
            frame: 0,
        }
    }

    pub(super) const fn restart(&mut self) {
        self.frame = 0;
    }

    fn refreshed_after(&self, frames: u32) -> u32 {
        let refreshed = u64::from(frames) * u64::from(self.units) / u64::from(self.period);

        u32::try_from(refreshed).unwrap_or(self.units)
    }

    pub(super) fn advance(&mut self) -> Option<RefreshRegion> {
        let location = self.refreshed_after(self.frame);
        let size = self.refreshed_after(self.frame + 1) - location;
        self.frame = (self.frame + 1) % self.period;

        (size > 0).then_some(RefreshRegion { location, size })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASES: [(u32, u32); 9] = [
        (17, 60),
        (34, 60),
        (4, 12),
        (1, 60),
        (60, 60),
        (17, 17),
        (68, 60),
        (135, 60),
        (240, 7),
    ];

    fn sweep(cadence: &mut RefreshCadence, period: u32) -> Vec<Option<RefreshRegion>> {
        (0..period).map(|_| cadence.advance()).collect()
    }

    fn coverage(regions: &[Option<RefreshRegion>], units: u32) -> Vec<u32> {
        let mut covered = vec![0; units as usize];
        for region in regions.iter().flatten() {
            for unit in region.location..region.location + region.size {
                covered[unit as usize] += 1;
            }
        }

        covered
    }

    #[test]
    fn a_sweep_refreshes_every_unit_exactly_once() {
        for (units, period) in CASES {
            let mut cadence = RefreshCadence::new(units, period);

            let regions = sweep(&mut cadence, period);
            let refreshed: u32 = regions.iter().flatten().map(|region| region.size).sum();

            assert_eq!(refreshed, units, "{units} units over {period} frames");
            assert!(
                coverage(&regions, units).iter().all(|&count| count == 1),
                "{units} units over {period} frames"
            );
        }
    }

    #[test]
    fn regions_follow_each_other_and_wrap_after_the_period() {
        for (units, period) in CASES {
            let mut cadence = RefreshCadence::new(units, period);

            let first = sweep(&mut cadence, period);
            let second = sweep(&mut cadence, period);
            let ends: Vec<u32> = first
                .iter()
                .flatten()
                .map(|region| region.location + region.size)
                .collect();
            let starts: Vec<u32> = first
                .iter()
                .flatten()
                .map(|region| region.location)
                .collect();

            assert_eq!(starts[0], 0, "{units} units over {period} frames");
            assert_eq!(starts[1..], ends[..ends.len() - 1]);
            assert_eq!(ends.last(), Some(&units));
            assert_eq!(first, second);
        }
    }

    #[test]
    fn fewer_units_than_frames_leave_frames_without_a_region() {
        let mut cadence = RefreshCadence::new(17, 60);

        let regions = sweep(&mut cadence, 60);
        let refreshing = regions.iter().flatten().count();
        let longest_gap = regions
            .split(Option::is_some)
            .map(<[Option<RefreshRegion>]>::len)
            .max();

        assert_eq!(refreshing, 17);
        assert!(regions.iter().flatten().all(|region| region.size == 1));
        assert_eq!(longest_gap, Some(3));
    }

    #[test]
    fn more_units_than_frames_refresh_on_every_frame() {
        let mut cadence = RefreshCadence::new(68, 60);

        let regions = sweep(&mut cadence, 60);
        let sizes: Vec<u32> = regions.iter().flatten().map(|region| region.size).collect();

        assert_eq!(sizes.len(), 60);
        assert!(sizes.iter().all(|size| (1..=2).contains(size)));
    }

    #[test]
    fn any_window_of_one_period_covers_every_unit() {
        for (units, period) in CASES {
            let mut cadence = RefreshCadence::new(units, period);

            let regions = sweep(&mut cadence, period * 3);

            for start in 0..period as usize {
                let window = &regions[start..start + period as usize];
                assert!(
                    coverage(window, units).iter().all(|&count| count == 1),
                    "{units} units over {period} frames from frame {start}"
                );
            }
        }
    }

    #[test]
    fn a_restart_begins_the_sweep_again() {
        let mut cadence = RefreshCadence::new(4, 12);

        let before: Vec<_> = (0..5).map(|_| cadence.advance()).collect();
        cadence.restart();
        let after = sweep(&mut cadence, 12);

        assert_eq!(
            before[2],
            Some(RefreshRegion {
                location: 0,
                size: 1
            })
        );
        assert_eq!(
            after[2],
            Some(RefreshRegion {
                location: 0,
                size: 1
            })
        );
        assert_eq!(
            after[11],
            Some(RefreshRegion {
                location: 3,
                size: 1
            })
        );
    }

    #[test]
    fn degenerate_sizes_still_refresh() {
        let mut cadence = RefreshCadence::new(0, 0);

        assert_eq!(
            cadence.advance(),
            Some(RefreshRegion {
                location: 0,
                size: 1
            })
        );
        assert_eq!(
            cadence.advance(),
            Some(RefreshRegion {
                location: 0,
                size: 1
            })
        );
    }
}
