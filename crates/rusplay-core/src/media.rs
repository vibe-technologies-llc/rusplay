use std::{fmt, mem, ops::Deref};

use crate::{Codec, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub codec: Codec,
    pub timestamp: Timestamp,
    pub keyframe: bool,
    pub data: FrameBytes,
}

type Recycle = Box<dyn FnOnce(Vec<u8>) + Send>;

#[derive(Default)]
pub struct FrameBytes {
    bytes: Vec<u8>,
    start: usize,
    recycle: Option<Recycle>,
}

impl FrameBytes {
    pub fn skipping(bytes: Vec<u8>, skip: usize) -> Self {
        let start = skip.min(bytes.len());

        Self {
            bytes,
            start,
            recycle: None,
        }
    }

    pub fn recycled(bytes: Vec<u8>, recycle: impl FnOnce(Vec<u8>) + Send + 'static) -> Self {
        Self {
            bytes,
            start: 0,
            recycle: Some(Box::new(recycle)),
        }
    }

    #[must_use]
    pub fn skip(mut self, skip: usize) -> Self {
        self.start = self.start.saturating_add(skip).min(self.bytes.len());
        self
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[self.start..]
    }

    pub fn into_vec(mut self) -> Vec<u8> {
        self.recycle = None;
        let mut bytes = mem::take(&mut self.bytes);
        bytes.drain(..self.start);

        bytes
    }
}

impl Clone for FrameBytes {
    fn clone(&self) -> Self {
        Self::from(self.as_slice().to_vec())
    }
}

impl Drop for FrameBytes {
    fn drop(&mut self) {
        if let Some(recycle) = self.recycle.take() {
            recycle(mem::take(&mut self.bytes));
        }
    }
}

impl fmt::Debug for FrameBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrameBytes")
            .field("len", &self.len())
            .field("recycled", &self.recycle.is_some())
            .finish_non_exhaustive()
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
        Self {
            bytes,
            start: 0,
            recycle: None,
        }
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
    use std::sync::mpsc;

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

    #[test]
    fn dropped_recycled_bytes_return_their_buffer_once() {
        let (returned, idle) = mpsc::channel();

        let bytes = FrameBytes::recycled(vec![1, 2, 3], move |bytes| returned.send(bytes).unwrap());
        let address = bytes.as_ptr();
        let before = idle.try_recv();
        drop(bytes);
        let back = idle.try_recv().unwrap();

        assert!(before.is_err());
        assert_eq!(back, vec![1, 2, 3]);
        assert_eq!(back.as_ptr(), address);
        assert!(idle.try_recv().is_err());
    }

    #[test]
    fn into_vec_and_clone_detach_from_the_pool() {
        let (returned, idle) = mpsc::channel();
        let make = |bytes: Vec<u8>| {
            let returned = returned.clone();
            FrameBytes::recycled(bytes, move |bytes| returned.send(bytes).unwrap())
        };

        let original = make(vec![4, 5]);

        let cloned = original.clone();
        drop(cloned);
        let taken = make(vec![6]).into_vec();
        let detached = idle.try_recv();
        drop(original);

        assert!(detached.is_err());
        assert_eq!(taken, vec![6]);
        assert_eq!(idle.try_recv().unwrap(), vec![4, 5]);
    }
}
