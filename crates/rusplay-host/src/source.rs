use std::{fmt, sync::Arc, time::Duration};

use parking_lot::{Mutex, MutexGuard};
use rusplay_capture::{CaptureError, Frame};

pub trait FrameSource: Send {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError>;
}

pub struct CaptureFeed {
    source: Mutex<Box<dyn FrameSource>>,
    retained: Mutex<Option<Frame>>,
}

impl CaptureFeed {
    pub fn new(source: impl FrameSource + 'static) -> Arc<Self> {
        Arc::new(Self {
            source: Mutex::new(Box::new(source)),
            retained: Mutex::new(None),
        })
    }

    pub(crate) fn source(&self) -> MutexGuard<'_, Box<dyn FrameSource>> {
        self.source.lock()
    }

    pub(crate) fn retain(&self, frame: Frame) {
        *self.retained.lock() = Some(frame);
    }

    pub(crate) fn take_retained(&self) -> Option<Frame> {
        self.retained.lock().take()
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
        CaptureError, FormatModifiers, Frame, PipeWireCapture, PortalSession, RestoreTokenStore,
        StreamOptions,
    };
    use rusplay_config::Config;

    use crate::{CaptureFeed, FrameSource, HostError, upload::UPLOAD_FORMATS};

    impl FrameSource for PipeWireCapture {
        fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
            self.next_frame_timeout(timeout)
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

        let formats: Vec<_> = UPLOAD_FORMATS.iter().map(|(fourcc, _)| *fourcc).collect();
        let mut options = StreamOptions::new(capture, &config.video).with_modifiers(
            formats
                .iter()
                .copied()
                .map(FormatModifiers::with_defaults)
                .collect(),
        );
        options.formats = formats;
        options.prefer_dmabuf = false;

        Ok(CaptureFeed::new(PipeWireCapture::from_portal(
            session, options,
        )?))
    }
}

#[cfg(feature = "pipewire")]
pub use self::portal::open_portal_capture;

#[cfg(not(feature = "pipewire"))]
pub fn open_portal_capture(
    _config: &rusplay_config::Config,
) -> Result<Arc<CaptureFeed>, crate::HostError> {
    Err(crate::HostError::CaptureNotCompiled)
}
