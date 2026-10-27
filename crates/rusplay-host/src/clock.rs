use std::time::Instant;

#[derive(Debug, Clone, Copy)]
pub(crate) struct StreamClock {
    origin: Instant,
}

impl StreamClock {
    pub(crate) fn start() -> Self {
        Self {
            origin: Instant::now(),
        }
    }

    pub(crate) fn micros(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use super::*;

    #[test]
    fn the_clock_counts_microseconds_from_its_start() {
        let clock = StreamClock::start();

        thread::sleep(Duration::from_millis(2));

        assert!(clock.micros() >= 2_000);
    }
}
