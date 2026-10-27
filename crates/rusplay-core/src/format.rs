use std::{cmp::Ordering, fmt};

use crate::{BitDepth, ChromaSubsampling, Codec, PixelFormat};

const CODECS: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];
const CHROMAS: [ChromaSubsampling; 3] = [
    ChromaSubsampling::Yuv420,
    ChromaSubsampling::Yuv422,
    ChromaSubsampling::Yuv444,
];
const DEPTHS: [BitDepth; 3] = [BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve];
const CODEC_FALLBACK_ORDER: [Codec; 3] = [Codec::Av1, Codec::Hevc, Codec::H264];
const FORMATS_PER_CODEC: usize = CHROMAS.len() * DEPTHS.len();
const FORMAT_COUNT: usize = CODECS.len() * FORMATS_PER_CODEC;
const KNOWN_BITS: u32 = (1 << FORMAT_COUNT) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CodecFormat {
    pub codec: Codec,
    pub format: PixelFormat,
}

impl CodecFormat {
    pub const fn new(codec: Codec, format: PixelFormat) -> Self {
        Self { codec, format }
    }

    pub fn all() -> impl Iterator<Item = Self> {
        CODECS.into_iter().flat_map(|codec| {
            CHROMAS.into_iter().flat_map(move |chroma| {
                DEPTHS
                    .into_iter()
                    .map(move |depth| Self::new(codec, PixelFormat { chroma, depth }))
            })
        })
    }

    const fn bit(self) -> u32 {
        let index = codec_index(self.codec) * FORMATS_PER_CODEC
            + chroma_index(self.format.chroma) * DEPTHS.len()
            + depth_index(self.format.depth);

        1 << index
    }

    fn fallback_rank(self, preferred: Self) -> FallbackRank {
        FallbackRank {
            chroma: AxisRank::of(
                chroma_index(self.format.chroma),
                chroma_index(preferred.format.chroma),
            ),
            depth: AxisRank::of(
                depth_index(self.format.depth),
                depth_index(preferred.format.depth),
            ),
            codec: codec_rank(self.codec, preferred.codec),
        }
    }
}

impl fmt::Display for CodecFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {}-bit",
            self.codec,
            self.format.chroma,
            self.format.depth.bits()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AxisRank {
    Same,
    Above(usize),
    Below(usize),
}

impl AxisRank {
    fn of(candidate: usize, preferred: usize) -> Self {
        match candidate.cmp(&preferred) {
            Ordering::Equal => Self::Same,
            Ordering::Greater => Self::Above(candidate - preferred),
            Ordering::Less => Self::Below(preferred - candidate),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FallbackRank {
    chroma: AxisRank,
    depth: AxisRank,
    codec: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FormatSet(u32);

impl FormatSet {
    pub const EMPTY: Self = Self(0);
    pub const ALL: Self = Self(KNOWN_BITS);

    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !KNOWN_BITS == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, format: CodecFormat) -> bool {
        self.0 & format.bit() != 0
    }

    pub const fn insert(&mut self, format: CodecFormat) {
        self.0 |= format.bit();
    }

    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub fn iter(self) -> impl Iterator<Item = CodecFormat> {
        CodecFormat::all().filter(move |format| self.contains(*format))
    }

    pub fn closest_to(self, preferred: CodecFormat) -> Option<CodecFormat> {
        self.iter()
            .min_by_key(|candidate| candidate.fallback_rank(preferred))
    }
}

impl FromIterator<CodecFormat> for FormatSet {
    fn from_iter<I: IntoIterator<Item = CodecFormat>>(formats: I) -> Self {
        let mut set = Self::EMPTY;
        for format in formats {
            set.insert(format);
        }

        set
    }
}

impl fmt::Display for FormatSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("no format");
        }

        for (index, format) in self.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{format}")?;
        }

        Ok(())
    }
}

const fn codec_index(codec: Codec) -> usize {
    match codec {
        Codec::H264 => 0,
        Codec::Hevc => 1,
        Codec::Av1 => 2,
    }
}

const fn chroma_index(chroma: ChromaSubsampling) -> usize {
    match chroma {
        ChromaSubsampling::Yuv420 => 0,
        ChromaSubsampling::Yuv422 => 1,
        ChromaSubsampling::Yuv444 => 2,
    }
}

const fn depth_index(depth: BitDepth) -> usize {
    match depth {
        BitDepth::Eight => 0,
        BitDepth::Ten => 1,
        BitDepth::Twelve => 2,
    }
}

fn codec_rank(codec: Codec, preferred: Codec) -> usize {
    if codec == preferred {
        return 0;
    }

    1 + CODEC_FALLBACK_ORDER
        .iter()
        .filter(|&&candidate| candidate != preferred)
        .position(|&candidate| candidate == codec)
        .unwrap_or(CODECS.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn format(codec: Codec, chroma: ChromaSubsampling, depth: BitDepth) -> CodecFormat {
        CodecFormat::new(codec, PixelFormat { chroma, depth })
    }

    const HEVC_422_8: CodecFormat = format(Codec::Hevc, ChromaSubsampling::Yuv422, BitDepth::Eight);
    const HEVC_420_8: CodecFormat = format(Codec::Hevc, ChromaSubsampling::Yuv420, BitDepth::Eight);
    const HEVC_420_10: CodecFormat = format(Codec::Hevc, ChromaSubsampling::Yuv420, BitDepth::Ten);
    const HEVC_444_10: CodecFormat = format(Codec::Hevc, ChromaSubsampling::Yuv444, BitDepth::Ten);
    const AV1_420_8: CodecFormat = format(Codec::Av1, ChromaSubsampling::Yuv420, BitDepth::Eight);
    const AV1_420_10: CodecFormat = format(Codec::Av1, ChromaSubsampling::Yuv420, BitDepth::Ten);
    const AV1_444_10: CodecFormat = format(Codec::Av1, ChromaSubsampling::Yuv444, BitDepth::Ten);
    const H264_420_8: CodecFormat = format(Codec::H264, ChromaSubsampling::Yuv420, BitDepth::Eight);

    fn set(formats: &[CodecFormat]) -> FormatSet {
        formats.iter().copied().collect()
    }

    #[test]
    fn every_format_has_its_own_bit() {
        let bits = CodecFormat::all().map(CodecFormat::bit).collect::<Vec<_>>();

        assert_eq!(bits.len(), FORMAT_COUNT);
        assert_eq!(bits.iter().fold(0, |all, bit| all | bit), KNOWN_BITS);
        assert_eq!(bits.iter().map(|bit| bit.count_ones()).sum::<u32>(), 27);
    }

    #[test]
    fn a_set_holds_exactly_what_was_inserted() {
        let formats = set(&[HEVC_422_8, AV1_420_10]);

        assert!(formats.contains(HEVC_422_8));
        assert!(formats.contains(AV1_420_10));
        assert!(!formats.contains(HEVC_420_8));
        assert_eq!(formats.iter().collect::<Vec<_>>(), [HEVC_422_8, AV1_420_10]);
        assert!(FormatSet::EMPTY.is_empty());
        assert_eq!(FormatSet::ALL.iter().count(), FORMAT_COUNT);
    }

    #[test]
    fn bits_round_trip_and_unknown_bits_are_refused() {
        let formats = set(&[H264_420_8, AV1_444_10]);

        assert_eq!(FormatSet::from_bits(formats.bits()), Some(formats));
        assert_eq!(FormatSet::from_bits(KNOWN_BITS), Some(FormatSet::ALL));
        assert_eq!(FormatSet::from_bits(1 << FORMAT_COUNT), None);
        assert_eq!(FormatSet::from_bits(u32::MAX), None);
    }

    #[test]
    fn intersection_keeps_only_shared_formats() {
        let encodable = set(&[HEVC_422_8, HEVC_420_8, AV1_420_8]);
        let decodable = set(&[HEVC_420_8, AV1_420_8, H264_420_8]);

        assert_eq!(
            encodable.intersection(decodable),
            set(&[HEVC_420_8, AV1_420_8])
        );
        assert!(encodable.intersection(set(&[H264_420_8])).is_empty());
    }

    #[test]
    fn the_preferred_format_wins_when_present() {
        assert_eq!(FormatSet::ALL.closest_to(HEVC_422_8), Some(HEVC_422_8));
    }

    #[test]
    fn nothing_is_chosen_from_an_empty_set() {
        assert_eq!(FormatSet::EMPTY.closest_to(HEVC_422_8), None);
    }

    #[test]
    fn chroma_is_kept_over_depth_and_codec() {
        let candidates = set(&[HEVC_420_10, AV1_444_10, H264_420_8]);

        assert_eq!(candidates.closest_to(HEVC_444_10), Some(AV1_444_10));
    }

    #[test]
    fn depth_is_kept_over_codec() {
        let candidates = set(&[HEVC_420_8, AV1_420_10]);

        assert_eq!(candidates.closest_to(HEVC_420_10), Some(AV1_420_10));
    }

    #[test]
    fn a_radeonsi_host_falls_back_from_422_to_420_in_the_same_codec() {
        let radeonsi = set(&[HEVC_420_8, HEVC_420_10, AV1_420_8, AV1_420_10, H264_420_8]);

        assert_eq!(radeonsi.closest_to(HEVC_422_8), Some(HEVC_420_8));
    }

    #[test]
    fn chroma_rises_before_it_falls() {
        let hevc = |chroma| format(Codec::Hevc, chroma, BitDepth::Eight);
        let candidates = set(&[
            hevc(ChromaSubsampling::Yuv420),
            hevc(ChromaSubsampling::Yuv444),
        ]);

        assert_eq!(
            candidates.closest_to(HEVC_422_8),
            Some(hevc(ChromaSubsampling::Yuv444))
        );
    }

    #[test]
    fn depth_rises_before_it_falls_and_nearest_first() {
        let av1 = |depth| format(Codec::Av1, ChromaSubsampling::Yuv420, depth);
        let ten = av1(BitDepth::Ten);

        assert_eq!(
            set(&[av1(BitDepth::Eight), av1(BitDepth::Twelve)]).closest_to(ten),
            Some(av1(BitDepth::Twelve))
        );
        assert_eq!(
            set(&[
                av1(BitDepth::Eight),
                av1(BitDepth::Ten),
                av1(BitDepth::Twelve)
            ])
            .closest_to(av1(BitDepth::Eight)),
            Some(av1(BitDepth::Eight))
        );
        assert_eq!(
            set(&[av1(BitDepth::Ten), av1(BitDepth::Twelve)]).closest_to(av1(BitDepth::Eight)),
            Some(ten)
        );
    }

    #[test]
    fn chroma_falls_to_the_nearest_lower_step() {
        let h264 = |chroma| format(Codec::H264, chroma, BitDepth::Eight);
        let candidates = set(&[
            h264(ChromaSubsampling::Yuv420),
            h264(ChromaSubsampling::Yuv422),
        ]);

        assert_eq!(
            candidates.closest_to(h264(ChromaSubsampling::Yuv444)),
            Some(h264(ChromaSubsampling::Yuv422))
        );
    }

    #[test]
    fn other_codecs_follow_av1_then_hevc_then_h264() {
        let all_codecs = set(&[
            format(Codec::H264, ChromaSubsampling::Yuv420, BitDepth::Eight),
            format(Codec::Hevc, ChromaSubsampling::Yuv420, BitDepth::Eight),
            format(Codec::Av1, ChromaSubsampling::Yuv420, BitDepth::Eight),
        ]);
        let without_av1 = set(&[H264_420_8, HEVC_420_8]);
        let hevc_preferred = format(Codec::Hevc, ChromaSubsampling::Yuv422, BitDepth::Ten);
        let h264_preferred = format(Codec::H264, ChromaSubsampling::Yuv444, BitDepth::Ten);

        assert_eq!(
            set(&[H264_420_8, AV1_420_8]).closest_to(HEVC_420_8),
            Some(AV1_420_8)
        );
        assert_eq!(without_av1.closest_to(AV1_420_8), Some(HEVC_420_8));
        assert_eq!(all_codecs.closest_to(hevc_preferred), Some(HEVC_420_8));
        assert_eq!(all_codecs.closest_to(h264_preferred), Some(H264_420_8));
    }

    #[test]
    fn the_choice_is_always_a_member_and_the_order_is_total() {
        for preferred in CodecFormat::all() {
            let ranks = CodecFormat::all()
                .map(|candidate| candidate.fallback_rank(preferred))
                .collect::<Vec<_>>();
            let mut sorted = ranks.clone();
            sorted.sort();
            sorted.dedup();

            assert_eq!(sorted.len(), ranks.len());
            assert_eq!(FormatSet::ALL.closest_to(preferred), Some(preferred));
        }

        for bits in (0..=KNOWN_BITS).step_by(4099) {
            let candidates = FormatSet::from_bits(bits).unwrap();
            for preferred in CodecFormat::all() {
                let chosen = candidates.closest_to(preferred);

                assert_eq!(chosen.is_none(), candidates.is_empty());
                assert!(chosen.is_none_or(|chosen| candidates.contains(chosen)));
            }
        }
    }

    #[test]
    fn formats_print_codec_chroma_and_depth() {
        assert_eq!(HEVC_422_8.to_string(), "hevc 4:2:2 8-bit");
        assert_eq!(
            set(&[H264_420_8, AV1_420_10]).to_string(),
            "h264 4:2:0 8-bit, av1 4:2:0 10-bit"
        );
        assert_eq!(FormatSet::EMPTY.to_string(), "no format");
    }
}
