use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

type Release = Box<dyn FnOnce() + Send>;

pub struct BufferLease {
    release: Option<Release>,
}

impl BufferLease {
    pub fn new(release: impl FnOnce() + Send + 'static) -> Self {
        Self {
            release: Some(Box::new(release)),
        }
    }

    pub const fn detached() -> Self {
        Self { release: None }
    }

    pub const fn is_detached(&self) -> bool {
        self.release.is_none()
    }
}

impl Drop for BufferLease {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl fmt::Debug for BufferLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BufferLease")
            .field("detached", &self.is_detached())
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ShmFallback {
    requested: Arc<AtomicBool>,
}

impl ShmFallback {
    pub fn request(&self) {
        self.requested.store(true, Ordering::Release);
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[test]
    fn a_lease_releases_its_buffer_once_when_dropped() {
        let released = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&released);

        let lease = BufferLease::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        let held = released.load(Ordering::Relaxed);
        drop(lease);

        assert_eq!(held, 0);
        assert_eq!(released.load(Ordering::Relaxed), 1);
        assert!(BufferLease::detached().is_detached());
    }

    #[test]
    fn a_fallback_request_is_seen_by_every_clone() {
        let fallback = ShmFallback::default();
        let seen = fallback.clone();

        let before = seen.is_requested();
        fallback.request();

        assert!(!before);
        assert!(seen.is_requested());
    }
}
