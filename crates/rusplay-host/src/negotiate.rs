use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use rusplay_config::VideoConfig;
use rusplay_core::{CodecFormat, FormatSet};

use crate::HostError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegotiatedFormat {
    pub configured: CodecFormat,
    pub chosen: CodecFormat,
}

impl NegotiatedFormat {
    pub fn is_fallback(&self) -> bool {
        self.chosen != self.configured
    }
}

pub(crate) const fn configured_format(video: &VideoConfig) -> CodecFormat {
    CodecFormat::new(video.codec, video.pixel_format)
}

pub(crate) fn negotiate(
    configured: CodecFormat,
    encodable: FormatSet,
    decodable: FormatSet,
) -> Result<NegotiatedFormat, HostError> {
    encodable
        .intersection(decodable)
        .closest_to(configured)
        .map(|chosen| NegotiatedFormat { configured, chosen })
        .ok_or(HostError::NoCommonFormat {
            encodable,
            decodable,
        })
}

#[derive(Debug, Default)]
struct NegotiationState {
    encodable: Option<FormatSet>,
    agreed: Option<NegotiatedFormat>,
}

#[derive(Debug, Default)]
pub(crate) struct Negotiation {
    state: Mutex<NegotiationState>,
    agreed: Condvar,
}

impl Negotiation {
    pub(crate) fn offer(&self, encodable: FormatSet) {
        self.state.lock().encodable = Some(encodable);
    }

    pub(crate) fn encodable(&self) -> Option<FormatSet> {
        self.state.lock().encodable
    }

    pub(crate) fn agree(&self, format: NegotiatedFormat) {
        self.state.lock().agreed = Some(format);
        self.agreed.notify_all();
    }

    pub(crate) fn agreed(&self) -> Option<NegotiatedFormat> {
        self.state.lock().agreed
    }

    pub(crate) fn wait_agreed(&self, timeout: Duration) -> Option<NegotiatedFormat> {
        let mut state = self.state.lock();
        if state.agreed.is_none() {
            self.agreed.wait_for(&mut state, timeout);
        }

        state.agreed
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread};

    use rusplay_core::{BitDepth, ChromaSubsampling, Codec, PixelFormat};

    use super::*;

    const fn format(codec: Codec, chroma: ChromaSubsampling, depth: BitDepth) -> CodecFormat {
        CodecFormat::new(codec, PixelFormat { chroma, depth })
    }

    const HEVC_422_8: CodecFormat = format(Codec::Hevc, ChromaSubsampling::Yuv422, BitDepth::Eight);
    const HEVC_420_8: CodecFormat = format(Codec::Hevc, ChromaSubsampling::Yuv420, BitDepth::Eight);
    const AV1_420_8: CodecFormat = format(Codec::Av1, ChromaSubsampling::Yuv420, BitDepth::Eight);
    const AV1_420_10: CodecFormat = format(Codec::Av1, ChromaSubsampling::Yuv420, BitDepth::Ten);

    fn set(formats: &[CodecFormat]) -> FormatSet {
        formats.iter().copied().collect()
    }

    #[test]
    fn the_configured_format_is_kept_when_both_ends_support_it() {
        let negotiated = negotiate(HEVC_422_8, FormatSet::ALL, FormatSet::ALL).unwrap();

        assert_eq!(negotiated.chosen, HEVC_422_8);
        assert!(!negotiated.is_fallback());
    }

    #[test]
    fn the_choice_lies_in_both_sets() {
        let encodable = set(&[HEVC_422_8, HEVC_420_8, AV1_420_10]);
        let decodable = set(&[HEVC_420_8, AV1_420_8, AV1_420_10]);

        let negotiated = negotiate(HEVC_422_8, encodable, decodable).unwrap();

        assert_eq!(negotiated.chosen, HEVC_420_8);
        assert_eq!(negotiated.configured, HEVC_422_8);
        assert!(negotiated.is_fallback());
    }

    #[test]
    fn a_format_only_the_encoder_has_is_never_chosen() {
        let negotiated = negotiate(
            HEVC_422_8,
            set(&[HEVC_422_8, AV1_420_8]),
            set(&[AV1_420_8, AV1_420_10]),
        )
        .unwrap();

        assert_eq!(negotiated.chosen, AV1_420_8);
    }

    #[test]
    fn no_shared_format_is_a_specific_error_naming_both_sets() {
        let encodable = set(&[HEVC_422_8, HEVC_420_8]);
        let decodable = set(&[AV1_420_8]);

        assert!(matches!(
            negotiate(HEVC_422_8, encodable, decodable),
            Err(HostError::NoCommonFormat { encodable: e, decodable: d })
                if e == encodable && d == decodable
        ));
        assert!(matches!(
            negotiate(HEVC_422_8, FormatSet::ALL, FormatSet::EMPTY),
            Err(HostError::NoCommonFormat { .. })
        ));
    }

    #[test]
    fn the_configured_format_comes_from_the_video_section() {
        let video = VideoConfig {
            codec: Codec::Av1,
            pixel_format: AV1_420_10.format,
            ..VideoConfig::default()
        };

        assert_eq!(configured_format(&video), AV1_420_10);
    }

    #[test]
    fn a_waiting_encoder_wakes_on_agreement() {
        let negotiation = Arc::new(Negotiation::default());
        let agreed = NegotiatedFormat {
            configured: HEVC_422_8,
            chosen: HEVC_420_8,
        };

        assert_eq!(negotiation.wait_agreed(Duration::from_millis(1)), None);

        let waiter = {
            let negotiation = Arc::clone(&negotiation);
            thread::spawn(move || negotiation.wait_agreed(Duration::from_secs(5)))
        };
        negotiation.offer(FormatSet::ALL);
        negotiation.agree(agreed);

        assert_eq!(waiter.join().unwrap(), Some(agreed));
        assert_eq!(negotiation.encodable(), Some(FormatSet::ALL));
        assert_eq!(negotiation.agreed(), Some(agreed));
    }
}
