use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use parking_lot::{Mutex, MutexGuard};
use rusplay_capture::{CaptureError, CursorUpdate, Frame, Subscriber};
use rusplay_core::Resolution;

pub trait FrameSource: Send {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError>;

    fn cursor_feed(&self) -> Option<Subscriber<CursorUpdate>> {
        None
    }
}

pub struct CaptureFeed {
    source: Mutex<Box<dyn FrameSource>>,
    retained: Mutex<Option<Frame>>,
    cursor: Option<Subscriber<CursorUpdate>>,
    cursor_state: Mutex<Option<CursorUpdate>>,
    capture_size: AtomicU64,
}

impl CaptureFeed {
    pub fn new(source: impl FrameSource + 'static) -> Arc<Self> {
        Arc::new(Self {
            cursor: source.cursor_feed(),
            source: Mutex::new(Box::new(source)),
            retained: Mutex::new(None),
            cursor_state: Mutex::new(None),
            capture_size: AtomicU64::new(0),
        })
    }

    pub(crate) fn source(&self) -> MutexGuard<'_, Box<dyn FrameSource>> {
        self.source.lock()
    }

    pub(crate) fn retain(&self, frame: Frame) {
        *self.retained.lock() = Some(frame);
    }

    pub(crate) fn set_capture_size(&self, size: Resolution) {
        let packed = u64::from(size.width) << u32::BITS | u64::from(size.height);
        self.capture_size.store(packed, Ordering::Relaxed);
    }

    pub(crate) fn capture_size(&self) -> Option<Resolution> {
        let packed = self.capture_size.load(Ordering::Relaxed).to_be_bytes();
        let (width, height) = packed.split_at(4);
        let size = Resolution::new(
            u32::from_be_bytes(width.try_into().ok()?),
            u32::from_be_bytes(height.try_into().ok()?),
        );

        (size.pixel_count() > 0).then_some(size)
    }

    pub(crate) fn take_retained(&self) -> Option<Frame> {
        self.retained.lock().take()
    }

    pub(crate) fn cursor_subscriber(&self) -> Option<Subscriber<CursorUpdate>> {
        self.cursor.clone()
    }

    pub(crate) fn take_cursor_state(&self) -> Option<CursorUpdate> {
        self.cursor_state.lock().take()
    }

    pub(crate) fn keep_cursor_state(&self, state: Option<CursorUpdate>) {
        *self.cursor_state.lock() = state;
    }
}

impl fmt::Debug for CaptureFeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureFeed")
            .field("retained", &self.retained.lock().is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "pipewire")]
mod portal {
    use std::{sync::Arc, time::Duration};

    use rusplay_capture::{
        CaptureError, CaptureSource, CursorUpdate, FormatModifiers, Frame, PipeWireCapture,
        PortalSession, RestoreTokenStore, StreamOptions, Subscriber,
    };
    use rusplay_config::Config;

    use crate::{CaptureFeed, FrameSource, HostError, upload::upload_format_for};

    impl FrameSource for PipeWireCapture {
        fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
            self.next_frame_timeout(timeout)
        }

        fn cursor_feed(&self) -> Option<Subscriber<CursorUpdate>> {
            CaptureSource::cursor_feed(self)
        }
    }

    pub fn open_portal_capture(config: &Config) -> Result<Arc<CaptureFeed>, HostError> {
        let capture = &config.capture;
        let session = if capture.persist_session {
            let store = RestoreTokenStore::from_config(capture)?;
            pollster::block_on(PortalSession::open_persistent(capture, &store))?
        } else {
            pollster::block_on(PortalSession::open(capture))?
        };

        feed_from_session(session, config)
    }

    pub(crate) fn feed_from_session(
        session: PortalSession,
        config: &Config,
    ) -> Result<Arc<CaptureFeed>, HostError> {
        let mut options = StreamOptions::new(&config.capture, &config.video);
        options
            .formats
            .retain(|fourcc| upload_format_for(*fourcc).is_ok());
        options.modifiers = options
            .formats
            .iter()
            .copied()
            .map(FormatModifiers::with_defaults)
            .collect();
        options.prefer_dmabuf = false;

        Ok(CaptureFeed::new(PipeWireCapture::from_portal(
            session, options,
        )?))
    }
}

#[cfg(feature = "pipewire")]
pub(crate) use self::portal::feed_from_session;
#[cfg(feature = "pipewire")]
pub use self::portal::open_portal_capture;

#[cfg(not(feature = "pipewire"))]
pub fn open_portal_capture(
    _config: &rusplay_config::Config,
) -> Result<Arc<CaptureFeed>, crate::HostError> {
    Err(crate::HostError::CaptureNotCompiled)
}
