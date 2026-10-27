use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::{Condvar, Mutex};
use rusplay_core::{Resolution, Timestamp};
use rusplay_scale::ColorVolume;

use crate::FrameColor;

#[derive(Debug, Clone)]
pub struct PresentedFrame {
    pub frame: u32,
    pub keyframe: bool,
    pub source: Resolution,
    pub color: FrameColor,
    pub captured: Timestamp,
    pub received_at: Instant,
    pub decoded_at: Instant,
    pub submitted_at: Instant,
    pub texture: Arc<wgpu::Texture>,
}

impl PresentedFrame {
    pub fn resolution(&self) -> Resolution {
        Resolution::new(self.texture.width(), self.texture.height())
    }

    pub fn view(&self) -> wgpu::TextureView {
        self.texture
            .create_view(&wgpu::TextureViewDescriptor::default())
    }
}

pub trait FrameSink: Send + 'static {
    fn present(&mut self, frame: PresentedFrame);

    fn color_target(&self) -> Option<ColorVolume> {
        None
    }
}

impl<F: FnMut(PresentedFrame) + Send + 'static> FrameSink for F {
    fn present(&mut self, frame: PresentedFrame) {
        self(frame);
    }
}

#[derive(Debug, Default)]
struct LatestState {
    frame: Option<PresentedFrame>,
    presented: u64,
}

#[derive(Debug, Clone, Default)]
pub struct LatestFrame {
    shared: Arc<(Mutex<LatestState>, Condvar)>,
}

impl LatestFrame {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn take(&self) -> Option<PresentedFrame> {
        self.shared.0.lock().frame.take()
    }

    pub fn wait(&self, timeout: Duration) -> Option<PresentedFrame> {
        let deadline = Instant::now() + timeout;
        let (state, filled) = &*self.shared;
        let mut state = state.lock();

        while state.frame.is_none() {
            if filled.wait_until(&mut state, deadline).timed_out() {
                break;
            }
        }
        state.frame.take()
    }

    pub fn presented(&self) -> u64 {
        self.shared.0.lock().presented
    }
}

impl FrameSink for LatestFrame {
    fn present(&mut self, frame: PresentedFrame) {
        let (state, filled) = &*self.shared;
        let mut state = state.lock();
        let replaced = state.frame.replace(frame);
        state.presented += 1;
        drop(state);
        filled.notify_all();

        drop(replaced);
    }
}
