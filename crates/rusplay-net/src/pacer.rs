use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pacer {
    bytes_per_second: u64,
}

impl Pacer {
    pub const fn from_kbps(kbps: u32) -> Self {
        Self {
            bytes_per_second: kbps as u64 * 1000 / 8,
        }
    }

    pub fn interval_for(self, bytes: usize) -> Duration {
        if self.bytes_per_second == 0 {
            return Duration::ZERO;
        }

        let nanos = bytes as u128 * 1_000_000_000 / u128::from(self.bytes_per_second);
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_megabit_paces_a_kilobyte_at_eight_ms() {
        let pacer = Pacer::from_kbps(1000);

        assert_eq!(pacer.interval_for(1000), Duration::from_millis(8));
    }
}
