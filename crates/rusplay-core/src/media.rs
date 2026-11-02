use std::ops::Deref;

use crate::{Codec, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub codec: Codec,
    pub timestamp: Timestamp,
    pub keyframe: bool,
    pub data: FrameBytes,
}

#[derive(Debug, Clone, Default)]
pub struct FrameBytes {
    bytes: Vec<u8>,
    start: usize,
}

impl FrameBytes {
    pub fn skipping(bytes: Vec<u8>, skip: usize) -> Self {
        let start = skip.min(bytes.len());

        Self { bytes, start }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[self.start..]
    }

    pub fn into_vec(mut self) -> Vec<u8> {
        self.bytes.drain(..self.start);

        self.bytes
    }
}

impl Deref for FrameBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsRef<[u8]> for FrameBytes {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl From<Vec<u8>> for FrameBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self { bytes, start: 0 }
    }
}

impl PartialEq for FrameBytes {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for FrameBytes {}

impl PartialEq<Vec<u8>> for FrameBytes {
    fn eq(&self, other: &Vec<u8>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl PartialEq<[u8]> for FrameBytes {
    fn eq(&self, other: &[u8]) -> bool {
        self.as_slice() == other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipping_exposes_the_tail_without_moving_the_bytes() {
        let bytes = vec![1, 2, 3, 4, 5];
        let base = bytes.as_ptr();

        let data = FrameBytes::skipping(bytes, 2);

        assert_eq!(&*data, &[3, 4, 5]);
        assert_eq!(data.len(), 3);
        assert_eq!(data.as_ptr(), base.wrapping_add(2));
    }

    #[test]
    fn skipping_past_the_end_leaves_nothing() {
        let data = FrameBytes::skipping(vec![1, 2], 9);

        assert!(data.is_empty());
        assert_eq!(data, Vec::<u8>::new());
    }

    #[test]
    fn equality_ignores_the_skipped_prefix() {
        let skipped = FrameBytes::skipping(vec![9, 9, 7, 8], 2);
        let plain = FrameBytes::from(vec![7, 8]);

        assert_eq!(skipped, plain);
        assert_eq!(skipped, vec![7, 8]);
        assert_eq!(skipped.into_vec(), vec![7, 8]);
    }
}
