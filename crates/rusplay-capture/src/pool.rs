use std::sync::Arc;

use parking_lot::Mutex;
use rusplay_core::ShmBytes;

const RETAINED_BUFFERS: usize = 3;

#[derive(Debug, Clone, Default)]
pub(crate) struct ShmPool {
    idle: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl ShmPool {
    pub(crate) fn copy_of(&self, pixels: &[u8]) -> ShmBytes {
        let mut bytes = self.take(pixels.len());
        bytes.extend_from_slice(pixels);

        let idle = Arc::clone(&self.idle);
        ShmBytes::recycled(bytes, move |bytes| {
            let mut idle = idle.lock();
            if idle.len() < RETAINED_BUFFERS {
                idle.push(bytes);
            }
        })
    }

    fn take(&self, len: usize) -> Vec<u8> {
        let mut idle = self.idle.lock();

        while let Some(mut bytes) = idle.pop() {
            if bytes.capacity() >= len {
                bytes.clear();
                return bytes;
            }
        }

        Vec::with_capacity(len)
    }

    #[cfg(test)]
    fn idle_count(&self) -> usize {
        self.idle.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_copy_is_reused_for_the_next_frame_without_reallocating() {
        let pool = ShmPool::default();

        let first = pool.copy_of(&[1; 64]);
        let address = first.as_ptr();
        drop(first);
        let second = pool.copy_of(&[2; 64]);

        assert_eq!(second.as_ptr(), address);
        assert_eq!(second[..], [2; 64]);
        assert_eq!(pool.idle_count(), 0);
    }

    #[test]
    fn a_buffer_too_small_for_the_new_frame_is_not_reused() {
        let pool = ShmPool::default();
        drop(pool.copy_of(&[1; 8]));

        let large = pool.copy_of(&[3; 256]);

        assert_eq!(large.len(), 256);
        assert!(large.iter().all(|byte| *byte == 3));
        assert_eq!(pool.idle_count(), 0);
    }

    #[test]
    fn the_pool_keeps_only_a_few_idle_buffers() {
        let pool = ShmPool::default();

        let held: Vec<_> = (0..RETAINED_BUFFERS + 2)
            .map(|_| pool.copy_of(&[0; 16]))
            .collect();
        drop(held);

        assert_eq!(pool.idle_count(), RETAINED_BUFFERS);
    }

    #[test]
    fn a_frame_outliving_the_pool_still_drops_cleanly() {
        let pool = ShmPool::default();
        let copy = pool.copy_of(&[5; 4]);

        drop(pool);

        assert_eq!(copy[..], [5; 4]);
    }
}
