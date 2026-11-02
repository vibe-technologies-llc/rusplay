use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

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
    use super::*;

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
