use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use rusplay_config::{
    CodecChoice, PixelFormatChoice, StreamColorMode, VideoConfig, format_preference,
};
use rusplay_core::{BitDepth, CodecFormat, FormatPreference, FormatSet};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StreamChoice {
    codec: CodecChoice,
    pixel_format: PixelFormatChoice,
    color_mode: StreamColorMode,
}

impl StreamChoice {
    pub(crate) const fn of(video: &VideoConfig) -> Self {
        Self {
            codec: video.codec,
            pixel_format: video.pixel_format,
            color_mode: video.color_mode,
        }
    }

    pub(crate) const fn follows_source(self) -> bool {
        self.pixel_format.depth.fixed().is_none()
            || matches!(self.color_mode, StreamColorMode::Source)
    }

    pub(crate) const fn preference(self, source_depth: Option<BitDepth>) -> FormatPreference {
        format_preference(self.codec, self.pixel_format, source_depth)
    }
}

pub(crate) fn negotiate(
    preference: FormatPreference,
    encodable: FormatSet,
    decodable: FormatSet,
) -> Result<NegotiatedFormat, HostError> {
    encodable
        .intersection(decodable)
        .best_for(preference)
        .map(|chosen| NegotiatedFormat {
            configured: preference.configured(chosen),
            chosen,
        })
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

    fn negotiate(
        configured: CodecFormat,
        encodable: FormatSet,
        decodable: FormatSet,
    ) -> Result<NegotiatedFormat, HostError> {
        super::negotiate(FormatPreference::pinned(configured), encodable, decodable)
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
    fn hevc_is_kept_when_only_av1_matches_the_configured_depth() {
        let configured = format(Codec::Hevc, ChromaSubsampling::Yuv420, BitDepth::Ten);
        let encodable = set(&[configured, HEVC_420_8, AV1_420_10]);
        let decodable = set(&[HEVC_420_8, AV1_420_10]);

        let negotiated = negotiate(configured, encodable, decodable).unwrap();

        assert_eq!(negotiated.chosen, HEVC_420_8);
        assert_eq!(negotiated.configured, configured);
        assert!(negotiated.is_fallback());
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
    fn a_pinned_video_section_prefers_exactly_its_format() {
        let video = VideoConfig {
            codec: CodecChoice::pinned(Codec::Av1),
            pixel_format: PixelFormatChoice::pinned(AV1_420_10.format),
            color_mode: StreamColorMode::Configured,
            ..VideoConfig::default()
        };
        let choice = StreamChoice::of(&video);

        assert_eq!(
            choice.preference(Some(BitDepth::Eight)),
            FormatPreference::pinned(AV1_420_10)
        );
        assert!(!choice.follows_source());
    }

    #[test]
    fn auto_follows_the_source_depth_and_is_never_a_fallback() {
        let choice = StreamChoice::of(&VideoConfig::default());
        let encodable = set(&[HEVC_420_8, AV1_420_8, AV1_420_10]);
        let decodable = set(&[HEVC_420_8, AV1_420_8, AV1_420_10]);

        let ten =
            super::negotiate(choice.preference(Some(BitDepth::Ten)), encodable, decodable).unwrap();
        let eight = super::negotiate(choice.preference(None), encodable, decodable).unwrap();

        assert!(choice.follows_source());
        assert_eq!(ten.chosen, AV1_420_10);
        assert_eq!(eight.chosen, AV1_420_8);
        assert!(!ten.is_fallback() && !eight.is_fallback());
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
