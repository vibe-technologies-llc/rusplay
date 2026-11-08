use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use parking_lot::{Mutex, MutexGuard};
use rusplay_capture::{
    CaptureError, CursorUpdate, Frame, OutputOffer, ShmFallback, SourceRefresh, Subscriber,
};
use rusplay_core::{BitDepth, ColorSpace, FrameRate, Resolution};

use crate::upload::source_depth;

pub trait FrameSource: Send {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError>;

    fn cursor_feed(&self) -> Option<Subscriber<CursorUpdate>> {
        None
    }

    fn shm_fallback(&self) -> Option<ShmFallback> {
        None
    }

    fn output_offer(&self) -> Option<OutputOffer> {
        None
    }

    fn source_refresh(&self) -> Option<SourceRefresh> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceTraits {
    pub depth: BitDepth,
    pub color: ColorSpace,
}

impl SourceTraits {
    pub(crate) fn of(frame: &Frame) -> Self {
        let depth = match source_depth(frame.format) {
            BitDepth::Eight if frame.color.transfer.is_hdr() => BitDepth::Ten,
            depth => depth,
        };

        Self {
            depth,
            color: frame.color,
        }
    }
}

pub struct CaptureFeed {
    source: Mutex<Box<dyn FrameSource>>,
    retained: Mutex<Option<Frame>>,
    cursor: Option<Subscriber<CursorUpdate>>,
    cursor_state: Mutex<Option<CursorUpdate>>,
    fallback: Option<ShmFallback>,
    offer: Option<OutputOffer>,
    refresh: Option<SourceRefresh>,
    traits: Mutex<Option<SourceTraits>>,
    capture_size: AtomicU64,
}

impl CaptureFeed {
    pub fn new(source: impl FrameSource + 'static) -> Arc<Self> {
        Arc::new(Self {
            cursor: source.cursor_feed(),
            fallback: source.shm_fallback(),
            offer: source.output_offer(),
            refresh: source.source_refresh(),
            traits: Mutex::new(None),
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

    pub(crate) fn observe(&self, frame: &Frame) {
        self.set_capture_size(frame.resolution);
        *self.traits.lock() = Some(SourceTraits::of(frame));
    }

    pub(crate) fn source_traits(&self) -> Option<SourceTraits> {
        *self.traits.lock()
    }

    pub(crate) fn source_refresh(&self) -> Option<FrameRate> {
        self.refresh.as_ref().and_then(SourceRefresh::get)
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

    pub(crate) fn shm_fallback(&self) -> Option<ShmFallback> {
        self.fallback.clone()
    }

    pub(crate) fn output_offer(&self) -> Option<OutputOffer> {
        self.offer.clone()
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
        CaptureError, CaptureSource, CursorUpdate, DrmFourcc, FormatModifiers, Frame, OutputOffer,
        PipeWireCapture, PortalSession, RestoreTokenStore, ShmFallback, SourceRefresh,
        StreamOptions, Subscriber,
    };
    use rusplay_config::Config;
    use rusplay_scale::{DmaBufFormat, DmaBufImporter};

    use crate::{CaptureFeed, FrameSource, Gpu, HostError, upload::upload_format_for};

    impl FrameSource for PipeWireCapture {
        fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
            self.next_frame_timeout(timeout)
        }

        fn cursor_feed(&self) -> Option<Subscriber<CursorUpdate>> {
            CaptureSource::cursor_feed(self)
        }

        fn shm_fallback(&self) -> Option<ShmFallback> {
            Some(PipeWireCapture::shm_fallback(self))
        }

        fn output_offer(&self) -> Option<OutputOffer> {
            Some(PipeWireCapture::output_offer(self))
        }

        fn source_refresh(&self) -> Option<SourceRefresh> {
            Some(PipeWireCapture::source_refresh(self))
        }
    }

    pub fn open_portal_capture(config: &Config, gpu: &Gpu) -> Result<Arc<CaptureFeed>, HostError> {
        feed_from_session(open_capture_session(config)?, config, gpu)
    }

    pub(crate) fn open_capture_session(config: &Config) -> Result<PortalSession, HostError> {
        let capture = &config.capture;
        let session = if capture.persist_session {
            let store = RestoreTokenStore::from_config(capture)?;
            pollster::block_on(PortalSession::open_persistent(capture, &store))?
        } else {
            pollster::block_on(PortalSession::open(capture))?
        };

        Ok(session)
    }

    pub(crate) fn feed_from_session(
        session: PortalSession,
        config: &Config,
        gpu: &Gpu,
    ) -> Result<Arc<CaptureFeed>, HostError> {
        let importer = DmaBufImporter::new(&gpu.device).ok();
        let options = capture_options(config, |fourcc| {
            importable_modifiers(importer.as_ref(), fourcc)
        });

        Ok(CaptureFeed::new(PipeWireCapture::from_portal(
            session, options,
        )?))
    }

    fn importable_modifiers(importer: Option<&DmaBufImporter>, fourcc: DrmFourcc) -> Vec<u64> {
        importer
            .zip(DmaBufFormat::from_fourcc(fourcc.0))
            .map_or_default(|(importer, format)| importer.modifiers(format))
    }

    pub(crate) fn capture_options(
        config: &Config,
        importable: impl Fn(DrmFourcc) -> Vec<u64>,
    ) -> StreamOptions {
        let mut options = StreamOptions::new(&config.capture, &config.video);
        options
            .formats
            .retain(|fourcc| upload_format_for(*fourcc).is_ok());
        options.modifiers = options
            .formats
            .iter()
            .map(|fourcc| FormatModifiers::new(*fourcc, importable(*fourcc)))
            .collect();
        if options
            .modifiers
            .iter()
            .all(|entry| entry.modifiers.is_empty())
        {
            options.prefer_dmabuf = false;
        }

        options
    }

    #[cfg(test)]
    mod tests {
        use rusplay_capture::{DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR, modifiers_for};
        use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat};

        use super::*;

        const AMD_TILED: u64 = 0x0200_0000_0000_0001;
        const XR24: DrmFourcc = DrmFourcc::from_code(*b"XR24");

        fn ten_bit() -> Config {
            let mut config = Config::default();
            config.video.pixel_format = rusplay_config::PixelFormatChoice::pinned(PixelFormat {
                chroma: ChromaSubsampling::Yuv444,
                depth: BitDepth::Ten,
            });
            config
        }

        #[test]
        fn capture_offers_the_modifiers_the_gpu_imports_for_uploadable_formats() {
            let config = ten_bit();

            let options = capture_options(&config, |fourcc| {
                if fourcc == XR24 {
                    vec![AMD_TILED, DRM_FORMAT_MOD_LINEAR]
                } else {
                    vec![DRM_FORMAT_MOD_LINEAR]
                }
            });

            assert!(options.prefer_dmabuf);
            assert_eq!(options.formats[0], DrmFourcc::from_code(*b"XR30"));
            assert!(
                options
                    .formats
                    .iter()
                    .all(|fourcc| upload_format_for(*fourcc).is_ok())
            );
            assert_eq!(
                modifiers_for(&options.modifiers, XR24),
                [AMD_TILED, DRM_FORMAT_MOD_LINEAR]
            );
            assert_eq!(options.modifiers.len(), options.formats.len());
        }

        #[test]
        fn capture_without_importable_modifiers_asks_for_shared_memory() {
            let options = capture_options(&Config::default(), |_| Vec::new());

            assert!(!options.prefer_dmabuf);
        }

        #[test]
        fn capture_honours_prefer_dmabuf_off() {
            let mut config = Config::default();
            config.capture.prefer_dmabuf = false;

            let options = capture_options(&config, |_| vec![DRM_FORMAT_MOD_LINEAR]);

            assert!(!options.prefer_dmabuf);
        }

        #[test]
        fn the_gpu_never_offers_the_implicit_modifier() {
            let Some(gpu) = crate::hardware::require(Gpu::open().ok(), "a wgpu device") else {
                return;
            };
            let Some(importer) =
                crate::hardware::require(DmaBufImporter::new(&gpu.device).ok(), "dma-buf import")
            else {
                return;
            };

            let options = capture_options(&Config::default(), |fourcc| {
                importable_modifiers(Some(&importer), fourcc)
            });

            assert!(options.prefer_dmabuf);
            assert!(modifiers_for(&options.modifiers, XR24).contains(&DRM_FORMAT_MOD_LINEAR));
            for entry in &options.modifiers {
                assert!(!entry.modifiers.contains(&DRM_FORMAT_MOD_INVALID));
            }
        }
    }
}

#[cfg(feature = "pipewire")]
pub use self::portal::open_portal_capture;
#[cfg(all(feature = "pipewire", feature = "input"))]
pub(crate) use self::portal::{feed_from_session, open_capture_session};

#[cfg(not(feature = "pipewire"))]
pub fn open_portal_capture(
    _config: &rusplay_config::Config,
    _gpu: &crate::Gpu,
) -> Result<Arc<CaptureFeed>, crate::HostError> {
    Err(crate::HostError::CaptureNotCompiled)
}
