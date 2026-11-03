use std::sync::Arc;

use parking_lot::Mutex;
use rusplay_core::FrameBytes;

const RETAINED_BUFFERS: usize = 4;

#[derive(Debug, Clone, Default)]
pub(crate) struct FramePool {
    idle: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FramePool {
    pub(crate) fn take(&self) -> Vec<u8> {
        let mut bytes = self.idle.lock().pop().unwrap_or_default();
        bytes.clear();

        bytes
    }

    pub(crate) fn wrap(&self, bytes: Vec<u8>) -> FrameBytes {
        let idle = Arc::clone(&self.idle);

        FrameBytes::recycled(bytes, move |bytes| Self::keep(&idle, bytes))
    }

    pub(crate) fn give_back(&self, bytes: Vec<u8>) {
        Self::keep(&self.idle, bytes);
    }

    fn keep(idle: &Mutex<Vec<Vec<u8>>>, bytes: Vec<u8>) {
        let mut idle = idle.lock();
        if idle.len() < RETAINED_BUFFERS {
            idle.push(bytes);
        }
    }

    #[cfg(test)]
    fn idle_count(&self) -> usize {
        self.idle.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obu::AccessUnit;

    fn encode(pool: &FramePool, chunks: &[&[u8]]) -> FrameBytes {
        let mut unit = AccessUnit::new(true, pool.take());
        for chunk in chunks {
            unit.push(chunk);
        }

        pool.wrap(unit.finish())
    }

    #[test]
    fn a_dropped_frame_buffer_is_the_next_frames_buffer() {
        let pool = FramePool::default();

        let first = encode(&pool, &[&[0x32, 1, 2, 3]]);
        let address = first.as_ptr();
        drop(first);
        let second = encode(&pool, &[&[0x32, 9, 8]]);

        assert_eq!(second.as_ptr(), address);
        assert_eq!(second[..], [0x12, 0, 0x32, 9, 8]);
        assert_eq!(pool.idle_count(), 0);
    }

    #[test]
    fn steady_state_encoding_reuses_one_buffer_for_every_frame() {
        let pool = FramePool::default();
        let payload = [0x32u8; 4096];

        let warm = encode(&pool, &[&payload]);
        let address = warm.as_ptr();
        drop(warm);

        for _ in 0..16 {
            let frame = encode(&pool, &[&payload[..1000], &payload[..2000]]);
            assert_eq!(frame.as_ptr(), address);
            assert_eq!(frame.len(), 3002);
        }
    }

    #[test]
    fn a_frame_in_flight_is_never_handed_out_again() {
        let pool = FramePool::default();

        let held = encode(&pool, &[&[0x32, 1]]);
        let other = encode(&pool, &[&[0x32, 2]]);

        assert_ne!(held.as_ptr(), other.as_ptr());
        assert_eq!(held[..], [0x12, 0, 0x32, 1]);
        assert_eq!(other[..], [0x12, 0, 0x32, 2]);
    }

    #[test]
    fn the_pool_keeps_only_a_few_idle_buffers() {
        let pool = FramePool::default();

        let held: Vec<_> = (0..RETAINED_BUFFERS + 3)
            .map(|_| encode(&pool, &[&[0x32]]))
            .collect();
        drop(held);

        assert_eq!(pool.idle_count(), RETAINED_BUFFERS);
    }

    #[test]
    fn a_frame_outliving_the_pool_still_drops_cleanly() {
        let pool = FramePool::default();
        let frame = encode(&pool, &[&[0x32, 7]]);

        drop(pool);

        assert_eq!(frame[..], [0x12, 0, 0x32, 7]);
    }

    #[test]
    fn a_buffer_given_back_is_cleared_and_reused() {
        let pool = FramePool::default();
        let bytes = vec![1, 2, 3, 4];
        let address = bytes.as_ptr();

        pool.give_back(bytes);
        let reused = pool.take();

        assert_eq!(reused.len(), 0);
        assert_eq!(reused.as_ptr(), address);
        assert!(reused.capacity() >= 4);
    }
}
