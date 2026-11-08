use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, DepthPreference, FormatPreference, FrameRate, PixelFormat,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const UNNEGOTIATED_CODEC: Codec = Codec::Hevc;
pub const UNNEGOTIATED_PIXEL_FORMAT: PixelFormat = PixelFormat {
    chroma: ChromaSubsampling::Yuv420,
    depth: BitDepth::Eight,
};
pub const UNDETECTED_FRAME_RATE: FrameRate = FrameRate::integer(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodecChoice {
    #[default]
    Auto,
    H264,
    Hevc,
    Av1,
}

impl CodecChoice {
    pub const fn fixed(self) -> Option<Codec> {
        match self {
            Self::Auto => None,
            Self::H264 => Some(Codec::H264),
            Self::Hevc => Some(Codec::Hevc),
            Self::Av1 => Some(Codec::Av1),
        }
    }

    pub const fn pinned(codec: Codec) -> Self {
        match codec {
            Codec::H264 => Self::H264,
            Codec::Hevc => Self::Hevc,
            Codec::Av1 => Self::Av1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChromaChoice {
    #[default]
    Auto,
    #[serde(rename = "4:2:0")]
    Yuv420,
    #[serde(rename = "4:2:2")]
    Yuv422,
    #[serde(rename = "4:4:4")]
    Yuv444,
}

impl ChromaChoice {
    pub const fn fixed(self) -> Option<ChromaSubsampling> {
        match self {
            Self::Auto => None,
            Self::Yuv420 => Some(ChromaSubsampling::Yuv420),
            Self::Yuv422 => Some(ChromaSubsampling::Yuv422),
            Self::Yuv444 => Some(ChromaSubsampling::Yuv444),
        }
    }

    pub const fn pinned(chroma: ChromaSubsampling) -> Self {
        match chroma {
            ChromaSubsampling::Yuv420 => Self::Yuv420,
            ChromaSubsampling::Yuv422 => Self::Yuv422,
            ChromaSubsampling::Yuv444 => Self::Yuv444,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DepthChoice {
    #[default]
    Auto,
    Eight,
    Ten,
    Twelve,
}

impl DepthChoice {
    pub const fn fixed(self) -> Option<BitDepth> {
        match self {
            Self::Auto => None,
            Self::Eight => Some(BitDepth::Eight),
            Self::Ten => Some(BitDepth::Ten),
            Self::Twelve => Some(BitDepth::Twelve),
        }
    }

    pub const fn pinned(depth: BitDepth) -> Self {
        match depth {
            BitDepth::Eight => Self::Eight,
            BitDepth::Ten => Self::Ten,
            BitDepth::Twelve => Self::Twelve,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PixelFormatChoice {
    pub chroma: ChromaChoice,
    pub depth: DepthChoice,
}

impl PixelFormatChoice {
    pub const fn pinned(format: PixelFormat) -> Self {
        Self {
            chroma: ChromaChoice::pinned(format.chroma),
            depth: DepthChoice::pinned(format.depth),
        }
    }

    pub const fn fixed(self) -> Option<PixelFormat> {
        match (self.chroma.fixed(), self.depth.fixed()) {
            (Some(chroma), Some(depth)) => Some(PixelFormat { chroma, depth }),
            _ => None,
        }
    }

    pub const fn or_unnegotiated(self) -> PixelFormat {
        PixelFormat {
            chroma: match self.chroma.fixed() {
                Some(chroma) => chroma,
                None => UNNEGOTIATED_PIXEL_FORMAT.chroma,
            },
            depth: match self.depth.fixed() {
                Some(depth) => depth,
                None => UNNEGOTIATED_PIXEL_FORMAT.depth,
            },
        }
    }
}

pub const fn format_preference(
    codec: CodecChoice,
    pixel_format: PixelFormatChoice,
    source_depth: Option<BitDepth>,
) -> FormatPreference {
    FormatPreference {
        codec: codec.fixed(),
        chroma: pixel_format.chroma.fixed(),
        depth: match (pixel_format.depth.fixed(), source_depth) {
            (Some(depth), _) => DepthPreference::Pinned(depth),
            (None, Some(depth)) => DepthPreference::Auto(depth),
            (None, None) => DepthPreference::Auto(BitDepth::Eight),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FrameRateChoice {
    #[default]
    Auto,
    Fixed(FrameRate),
}

impl FrameRateChoice {
    pub const fn fixed(self) -> Option<FrameRate> {
        match self {
            Self::Auto => None,
            Self::Fixed(rate) => Some(rate),
        }
    }

    pub const fn or_undetected(self) -> FrameRate {
        match self {
            Self::Auto => UNDETECTED_FRAME_RATE,
            Self::Fixed(rate) => rate,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum AutoWord {
    Auto,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FrameRateRepr {
    Word(AutoWord),
    Rate(FrameRate),
}

impl Serialize for FrameRateChoice {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::Fixed(rate) => rate.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for FrameRateChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match FrameRateRepr::deserialize(deserializer)? {
            FrameRateRepr::Word(AutoWord::Auto) => Self::Auto,
            FrameRateRepr::Rate(rate) => Self::Fixed(rate),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StreamColorMode {
    #[default]
    Source,
    Configured,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Holder {
        rate: FrameRateChoice,
        codec: CodecChoice,
        format: PixelFormatChoice,
    }

    #[test]
    fn auto_and_pinned_values_round_trip_through_toml() {
        let auto = Holder {
            rate: FrameRateChoice::Auto,
            codec: CodecChoice::Auto,
            format: PixelFormatChoice::default(),
        };
        let pinned = Holder {
            rate: FrameRateChoice::Fixed(FrameRate::new(60_000, 1001)),
            codec: CodecChoice::Av1,
            format: PixelFormatChoice::pinned(PixelFormat {
                chroma: ChromaSubsampling::Yuv444,
                depth: BitDepth::Ten,
            }),
        };

        let auto_text = toml::to_string(&auto).unwrap();
        let pinned_text = toml::to_string(&pinned).unwrap();

        assert!(auto_text.contains("rate = \"auto\""));
        assert!(auto_text.contains("codec = \"auto\""));
        assert_eq!(toml::from_str::<Holder>(&auto_text).unwrap(), auto);
        assert_eq!(toml::from_str::<Holder>(&pinned_text).unwrap(), pinned);
    }

    #[test]
    fn a_pinned_value_maps_back_to_the_core_type() {
        for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
            assert_eq!(CodecChoice::pinned(codec).fixed(), Some(codec));
        }
        for chroma in [
            ChromaSubsampling::Yuv420,
            ChromaSubsampling::Yuv422,
            ChromaSubsampling::Yuv444,
        ] {
            assert_eq!(ChromaChoice::pinned(chroma).fixed(), Some(chroma));
        }
        for depth in [BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve] {
            assert_eq!(DepthChoice::pinned(depth).fixed(), Some(depth));
        }

        assert_eq!(CodecChoice::Auto.fixed(), None);
        assert_eq!(PixelFormatChoice::default().fixed(), None);
        assert_eq!(
            PixelFormatChoice::default().or_unnegotiated(),
            UNNEGOTIATED_PIXEL_FORMAT
        );
        assert_eq!(FrameRateChoice::Auto.or_undetected(), UNDETECTED_FRAME_RATE);
    }

    #[test]
    fn a_frame_rate_that_is_neither_auto_nor_a_rate_is_refused() {
        assert!(toml::from_str::<Holder>("rate = \"fast\"\ncodec = \"auto\"\n[format]\n").is_err());
    }
}
