use crate::{Codec, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub codec: Codec,
    pub timestamp: Timestamp,
    pub keyframe: bool,
    pub data: Vec<u8>,
}
