mod error;
mod frame;
mod portal;

pub use crate::{
    error::CaptureError,
    frame::{DrmFourcc, Frame, FrameBuffer, Plane},
    portal::{PortalSession, PortalStream},
};

pub trait CaptureSource {
    fn next_frame(&mut self) -> Result<Frame, CaptureError>;
}
