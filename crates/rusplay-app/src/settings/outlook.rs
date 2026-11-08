use rusplay_config::{VideoDecoderConfig, VideoEncoderConfig};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, CodecFormat, DepthPreference, FormatPreference, FormatSet,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlookLine {
    pub text: String,
    pub attention: bool,
}

pub fn encoder_backend(config: &VideoEncoderConfig) -> String {
    match config {
        VideoEncoderConfig::Auto(_) => "automatic".to_owned(),
        VideoEncoderConfig::Vaapi(vaapi) => format!("VAAPI ({})", vaapi.device.display()),
        VideoEncoderConfig::Nvenc(nvenc) => format!("NVENC (GPU {})", nvenc.gpu_index),
        VideoEncoderConfig::X264(_) => "x264".to_owned(),
        VideoEncoderConfig::X265(_) => "x265".to_owned(),
        VideoEncoderConfig::SvtAv1(_) => "SVT-AV1".to_owned(),
    }
}

pub fn decoder_backend(config: &VideoDecoderConfig) -> String {
    match config {
        VideoDecoderConfig::Auto(_) => "automatic".to_owned(),
        VideoDecoderConfig::Vaapi(vaapi) if vaapi.copy_output => {
            format!("VAAPI ({}, CPU copy)", vaapi.device.display())
        }
        VideoDecoderConfig::Vaapi(vaapi) => format!("VAAPI ({})", vaapi.device.display()),
        VideoDecoderConfig::Nvdec(nvdec) => format!("NVDEC (GPU {})", nvdec.gpu_index),
        VideoDecoderConfig::Software(_) => "software".to_owned(),
    }
}

pub fn detected(label: String, automatic: bool) -> String {
    if automatic {
        format!("detected {label}")
    } else {
        label
    }
}

pub fn audio_gap(codec: &str, flac_compiled: bool) -> Option<&'static str> {
    if codec == "flac" && !flac_compiled {
        Some("FLAC is not included in this build, so audio will not start.")
    } else {
        None
    }
}

pub fn encoder_outlook(
    backend: &str,
    preference: FormatPreference,
    available: FormatSet,
) -> OutlookLine {
    let Some(produced) = available.best_for(preference) else {
        return OutlookLine {
            text: format!("This {backend} encoder has no format it can send."),
            attention: true,
        };
    };
    let configured = preference.configured(produced);

    if produced != configured {
        return OutlookLine {
            text: format!(
                "{} does not exist on this {backend} encoder, so it will send {}.",
                format_name(configured),
                format_name(produced)
            ),
            attention: true,
        };
    }

    let deep = match preference.depth {
        DepthPreference::Auto(_) => available.best_for(FormatPreference {
            depth: DepthPreference::Auto(BitDepth::Ten),
            ..preference
        }),
        DepthPreference::Pinned(_) => None,
    };
    let text = match deep.filter(|deep| *deep != produced) {
        Some(deep) => format!(
            "This {backend} encoder will send {}, or {} from a 10-bit or HDR source.",
            format_name(produced),
            format_name(deep)
        ),
        None => format!(
            "This {backend} encoder will send {}.",
            format_name(produced)
        ),
    };

    OutlookLine {
        text,
        attention: false,
    }
}

pub fn decoder_outlook(backend: &str, available: FormatSet) -> Vec<OutlookLine> {
    if available.is_empty() {
        return vec![line(
            format!("This {backend} decoder has no format it can play."),
            true,
        )];
    }

    let mut lines = Vec::new();
    lines.extend(depth_lines(backend, available));
    lines.extend(chroma_lines(backend, available));
    lines.extend(codec_lines(backend, available));

    if lines.is_empty() {
        lines.push(line(
            format!("This {backend} decoder can play every codec, chroma and bit depth."),
            false,
        ));
    }

    lines
}

fn depth_lines(backend: &str, available: FormatSet) -> Vec<OutlookLine> {
    ceiling(
        backend,
        available,
        DEEPEST,
        |format, depth| format.format.depth == depth,
        |depth| format!("{}-bit", depth.bits()),
        |best| format!("{best} is the deepest it can play"),
    )
}

fn chroma_lines(backend: &str, available: FormatSet) -> Vec<OutlookLine> {
    ceiling(
        backend,
        available,
        WIDEST,
        |format, chroma| format.format.chroma == chroma,
        |chroma| chroma.to_string(),
        |best| format!("{best} is the widest chroma it can play"),
    )
}

fn codec_lines(backend: &str, available: FormatSet) -> Vec<OutlookLine> {
    CODECS
        .into_iter()
        .filter(|codec| !available.iter().any(|format| format.codec == *codec))
        .map(|codec| {
            line(
                format!(
                    "{} does not exist on this {backend} decoder.",
                    codec_name(codec)
                ),
                true,
            )
        })
        .collect()
}

fn ceiling<T, Matches, Name, Best>(
    backend: &str,
    available: FormatSet,
    order: &[T],
    matches: Matches,
    name: Name,
    best_text: Best,
) -> Vec<OutlookLine>
where
    T: Copy,
    Matches: Fn(CodecFormat, T) -> bool,
    Name: Fn(T) -> String,
    Best: Fn(&str) -> String,
{
    let present: Vec<T> = order
        .iter()
        .copied()
        .filter(|item| available.iter().any(|format| matches(format, *item)))
        .collect();
    let Some(&best) = present.first() else {
        return Vec::new();
    };
    let best_at = order.iter().position(|item| {
        available.iter().any(|format| matches(format, *item)) && name(*item) == name(best)
    });
    let Some(best_at) = best_at else {
        return Vec::new();
    };

    let mut lines = Vec::new();
    let above: Vec<String> = order[..best_at].iter().copied().map(&name).collect();
    if !above.is_empty() {
        lines.push(line(
            format!(
                "{} {} on this {backend} decoder, so {}.",
                join(&above),
                verb(above.len()),
                best_text(&name(best))
            ),
            true,
        ));
    }

    let holes: Vec<String> = order[best_at + 1..]
        .iter()
        .copied()
        .filter(|item| !available.iter().any(|format| matches(format, *item)))
        .map(&name)
        .collect();
    for hole in holes {
        lines.push(line(
            format!("{hole} does not exist on this {backend} decoder."),
            true,
        ));
    }

    lines
}

fn verb(count: usize) -> &'static str {
    if count == 1 {
        "does not exist"
    } else {
        "do not exist"
    }
}

fn join(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        _ => {
            let mut text = parts[..parts.len() - 1].join(", ");
            text.push_str(" and ");
            text.push_str(&parts[parts.len() - 1]);
            text
        }
    }
}

fn line(text: String, attention: bool) -> OutlookLine {
    OutlookLine { text, attention }
}

fn format_name(format: CodecFormat) -> String {
    format!(
        "{} {} {}-bit",
        codec_name(format.codec),
        format.format.chroma,
        format.format.depth.bits()
    )
}

fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "H.264",
        Codec::Hevc => "HEVC",
        Codec::Av1 => "AV1",
    }
}

const CODECS: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];
const DEEPEST: &[BitDepth] = &[BitDepth::Twelve, BitDepth::Ten, BitDepth::Eight];
const WIDEST: &[ChromaSubsampling] = &[
    ChromaSubsampling::Yuv444,
    ChromaSubsampling::Yuv422,
    ChromaSubsampling::Yuv420,
];

#[cfg(test)]
mod tests {
    use rusplay_core::{ChromaSubsampling, PixelFormat};

    use super::*;

    fn format(codec: Codec, chroma: ChromaSubsampling, depth: BitDepth) -> CodecFormat {
        CodecFormat::new(codec, PixelFormat { chroma, depth })
    }

    fn without_twelve() -> FormatSet {
        CodecFormat::all()
            .filter(|format| format.format.depth != BitDepth::Twelve)
            .collect()
    }

    fn only(formats: &[CodecFormat]) -> FormatSet {
        formats.iter().copied().collect()
    }

    #[test]
    fn an_encoder_falls_back_when_the_selected_format_does_not_exist() {
        let configured = format(Codec::Hevc, ChromaSubsampling::Yuv444, BitDepth::Twelve);
        let outlook = encoder_outlook(
            "VAAPI",
            FormatPreference::pinned(configured),
            without_twelve(),
        );

        assert!(outlook.attention);
        assert_eq!(
            outlook.text,
            "HEVC 4:4:4 12-bit does not exist on this VAAPI encoder, so it will send HEVC 4:4:4 10-bit."
        );
    }

    #[test]
    fn an_encoder_that_can_send_the_selection_says_so() {
        let configured = format(Codec::Hevc, ChromaSubsampling::Yuv422, BitDepth::Ten);
        let outlook = encoder_outlook(
            "NVENC",
            FormatPreference::pinned(configured),
            FormatSet::ALL,
        );

        assert!(!outlook.attention);
        assert_eq!(
            outlook.text,
            "This NVENC encoder will send HEVC 4:2:2 10-bit."
        );
    }

    #[test]
    fn an_encoder_with_one_codec_names_that_fallback() {
        let configured = format(Codec::Av1, ChromaSubsampling::Yuv444, BitDepth::Twelve);
        let only_h264 = only(&[format(
            Codec::H264,
            ChromaSubsampling::Yuv420,
            BitDepth::Eight,
        )]);
        let outlook = encoder_outlook("x264", FormatPreference::pinned(configured), only_h264);

        assert!(outlook.attention);
        assert_eq!(
            outlook.text,
            "AV1 4:4:4 12-bit does not exist on this x264 encoder, so it will send H.264 4:2:0 8-bit."
        );
    }

    #[test]
    fn an_automatic_format_names_what_an_eight_and_a_ten_bit_source_get() {
        let available = only(&[
            format(Codec::Av1, ChromaSubsampling::Yuv420, BitDepth::Eight),
            format(Codec::Av1, ChromaSubsampling::Yuv420, BitDepth::Ten),
            format(Codec::Hevc, ChromaSubsampling::Yuv444, BitDepth::Eight),
        ]);
        let automatic = rusplay_config::format_preference(
            rusplay_config::CodecChoice::Auto,
            rusplay_config::PixelFormatChoice::default(),
            None,
        );

        let outlook = encoder_outlook("VAAPI", automatic, available);

        assert!(!outlook.attention);
        assert_eq!(
            outlook.text,
            "This VAAPI encoder will send HEVC 4:4:4 8-bit, or AV1 4:2:0 10-bit from a 10-bit or HDR source."
        );
    }

    #[test]
    fn an_encoder_with_nothing_to_send_says_so() {
        let configured = format(Codec::Hevc, ChromaSubsampling::Yuv422, BitDepth::Ten);
        let outlook = encoder_outlook(
            "VAAPI",
            FormatPreference::pinned(configured),
            FormatSet::EMPTY,
        );

        assert_eq!(
            outlook.text,
            "This VAAPI encoder has no format it can send."
        );
    }

    #[test]
    fn a_decoder_without_twelve_bit_names_ten_bit_as_its_deepest() {
        let lines = decoder_outlook("VAAPI", without_twelve());

        assert_eq!(
            lines[0].text,
            "12-bit does not exist on this VAAPI decoder, so 10-bit is the deepest it can play."
        );
        assert!(lines[0].attention);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn a_decoder_without_wide_chroma_names_the_widest_it_can_play() {
        let available = CodecFormat::all()
            .filter(|format| format.format.chroma == ChromaSubsampling::Yuv420)
            .collect();
        let lines = decoder_outlook("NVDEC", available);

        assert!(lines.iter().any(|line| {
            line.text
                == "4:4:4 and 4:2:2 do not exist on this NVDEC decoder, so 4:2:0 is the widest chroma it can play."
        }));
    }

    #[test]
    fn a_decoder_without_a_codec_names_that_codec() {
        let available = CodecFormat::all()
            .filter(|format| format.codec != Codec::Av1)
            .collect();
        let lines = decoder_outlook("software", available);

        assert!(
            lines
                .iter()
                .any(|line| { line.text == "AV1 does not exist on this software decoder." })
        );
    }

    #[test]
    fn a_decoder_that_can_play_everything_says_so() {
        let lines = decoder_outlook("software", FormatSet::ALL);

        assert_eq!(lines.len(), 1);
        assert!(!lines[0].attention);
        assert_eq!(
            lines[0].text,
            "This software decoder can play every codec, chroma and bit depth."
        );
    }

    #[test]
    fn a_shallower_hole_is_not_described_as_the_ceiling() {
        let available = CodecFormat::all()
            .filter(|format| format.format.depth != BitDepth::Eight)
            .collect();
        let lines = decoder_outlook("VAAPI", available);

        assert!(
            lines
                .iter()
                .any(|line| { line.text == "8-bit does not exist on this VAAPI decoder." })
        );
        assert!(!lines.iter().any(|line| line.text.contains("deepest")));
    }

    #[test]
    fn flac_without_the_build_is_a_gap_and_opus_is_not() {
        assert_eq!(
            audio_gap("flac", false),
            Some("FLAC is not included in this build, so audio will not start.")
        );
        assert_eq!(audio_gap("flac", true), None);
        assert_eq!(audio_gap("opus", false), None);
    }
}
