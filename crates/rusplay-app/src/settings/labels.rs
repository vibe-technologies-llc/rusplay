use std::borrow::Cow;

use rusplay_config::ConfigRole;

use crate::settings::{
    primaries,
    schema::{Field, Kind, Section},
};

const SEGMENTED_LIMIT: usize = 4;
const SEGMENTED_LABEL_CHARS: usize = 18;

pub fn choice_label(value: &str) -> Cow<'static, str> {
    if let Some(label) = named(value) {
        return Cow::Borrowed(label);
    }

    if let Some(amount) = milliseconds(value) {
        return Cow::Owned(format!("{amount} ms"));
    }

    if is_bare_number(value) {
        return Cow::Owned(value.to_owned());
    }

    Cow::Owned(sentence_case(value))
}

pub const fn section_summary(section: Section, role: ConfigRole) -> &'static str {
    let host = matches!(role, ConfigRole::Host);

    match section {
        Section::Video if host => "Stream format, the encoder and the bitrate.",
        Section::Video => "Decoder, presentation and the bitrate ceiling.",
        Section::Audio => "Codec, buffers and the microphone.",
        Section::Network if host => "Socket, transport, discovery and hosting.",
        Section::Network => "Socket, transport, discovery and pairing.",
        Section::Input if host => "What the host injects, and the clipboard.",
        Section::Input => "What the client captures, and the clipboard.",
        Section::Capture => "Source, cursor and the virtual output.",
        Section::Scaling if host => "Downscale filter and chroma siting.",
        Section::Scaling => "Upscale filter and chroma siting.",
        Section::Color if host => "Tone and gamut mapping into the stream.",
        Section::Color => "Tone mapping, gamut mapping and the display.",
    }
}

pub fn segmented_choice(optional: bool, options: &[&str]) -> bool {
    !optional
        && (1..=SEGMENTED_LIMIT).contains(&options.len())
        && options
            .iter()
            .all(|option| choice_label(option).chars().count() <= SEGMENTED_LABEL_CHARS)
}

pub fn field_matches(field: &Field, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }

    let query = query.to_lowercase();

    contains(field.label, &query)
        || contains(field.path, &query)
        || contains(field.group, &query)
        || contains(field.section.title(), &query)
        || (!field.unit.is_empty() && contains(field.unit, &query))
        || choice_options(field).iter().any(|option| {
            contains(option, &query) || contains(choice_label(option).as_ref(), &query)
        })
}

fn choice_options(field: &Field) -> &'static [&'static str] {
    match field.kind {
        Kind::Choice(options) | Kind::MultiChoice(options) => options,
        Kind::Primaries => primaries::CHOICES,
        Kind::Flag
        | Kind::Integer { .. }
        | Kind::Decimal
        | Kind::Text
        | Kind::Address
        | Kind::Resolution
        | Kind::FrameRate
        | Kind::Pairs => &[],
    }
}

fn contains(haystack: &str, query: &str) -> bool {
    haystack.to_lowercase().contains(query)
}

fn milliseconds(value: &str) -> Option<&str> {
    let amount = value.strip_suffix("ms")?;

    if amount.is_empty() || !amount.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
        return None;
    }

    Some(amount)
}

fn is_bare_number(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_digit() || ch == '.' || ch == ':' || ch == '/')
}

fn sentence_case(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut capitalise = true;

    for ch in value.chars() {
        if ch == '-' || ch == '_' {
            out.push(' ');
            capitalise = true;
        } else if capitalise {
            out.extend(ch.to_uppercase());
            capitalise = false;
        } else {
            out.push(ch);
        }
    }

    out
}

fn named(value: &str) -> Option<&'static str> {
    NAMES
        .iter()
        .find(|(key, _)| *key == value)
        .map(|(_, label)| *label)
}

const NAMES: &[(&str, &str)] = &[
    ("h264", "H.264"),
    ("hevc", "HEVC"),
    ("av1", "AV1"),
    ("eight", "8-bit"),
    ("ten", "10-bit"),
    ("twelve", "12-bit"),
    ("bt709", "BT.709"),
    ("bt2020", "BT.2020"),
    ("bt601", "BT.601"),
    ("bt601-525", "BT.601 525"),
    ("bt601-625", "BT.601 625"),
    ("bt2020-ncl", "BT.2020 NCL"),
    ("display-p3", "Display P3"),
    ("dci-p3", "DCI-P3"),
    ("srgb", "sRGB"),
    ("gamma22", "Gamma 2.2"),
    ("pq", "PQ"),
    ("hlg", "HLG"),
    ("bt2390", "BT.2390"),
    ("bt2446a", "BT.2446 A"),
    ("vaapi", "VAAPI"),
    ("nvenc", "NVENC"),
    ("nvdec", "NVDEC"),
    ("x264", "x264"),
    ("x265", "x265"),
    ("svt-av1", "SVT-AV1"),
    ("cbr", "CBR"),
    ("vbr", "VBR"),
    ("opus", "Opus"),
    ("flac", "FLAC"),
    ("fifo", "FIFO"),
    ("lut", "LUT"),
    ("psnr", "PSNR"),
    ("ssim", "SSIM"),
    ("rgba8", "8-bit RGBA"),
    ("rgb10a2", "10-bit RGB"),
    ("rgba16-float", "16-bit float"),
    ("extended-linear", "Extended linear"),
    ("blue-noise", "Blue noise"),
    ("constant-quality", "Constant quality"),
    ("ultra-low-latency", "Ultra-low latency"),
    ("low-latency", "Low latency"),
    ("high-quality", "High quality"),
    ("zerolatency", "Zero latency"),
    ("stillimage", "Still image"),
    ("fastdecode", "Fast decode"),
    ("ultrafast", "Ultra fast"),
    ("superfast", "Super fast"),
    ("veryfast", "Very fast"),
    ("veryslow", "Very slow"),
    ("xbox360", "Xbox 360"),
    ("dualsense", "DualSense"),
    ("dualshock4", "DualShock 4"),
    ("ctrl-alt", "Ctrl+Alt"),
    ("ctrl-shift", "Ctrl+Shift"),
    ("alt-shift", "Alt+Shift"),
    ("ctrl-alt-shift", "Ctrl+Alt+Shift"),
    ("client-to-host", "Client to host"),
    ("host-to-client", "Host to client"),
    ("center", "Centre"),
    ("lanczos2", "Lanczos 2"),
    ("lanczos3", "Lanczos 3"),
    ("lanczos4", "Lanczos 4"),
    ("catmull-rom", "Catmull-Rom"),
    ("ewa-lanczos", "EWA Lanczos"),
    ("ewa-lanczos-sharp", "EWA Lanczos sharp"),
    ("ewa-ginseng", "EWA Ginseng"),
    ("ewa-hann", "EWA Hann"),
    ("ewa-robidoux", "EWA Robidoux"),
    ("ewa-robidoux-sharp", "EWA Robidoux sharp"),
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{choice_label, field_matches, section_summary, segmented_choice};
    use crate::settings::{
        primaries,
        schema::{FIELDS, Kind, Section},
    };

    #[test]
    fn cryptic_values_read_as_names() {
        assert_eq!(choice_label("h264").as_ref(), "H.264");
        assert_eq!(choice_label("eight").as_ref(), "8-bit");
        assert_eq!(choice_label("svt-av1").as_ref(), "SVT-AV1");
        assert_eq!(
            choice_label("constant-quality").as_ref(),
            "Constant quality"
        );
        assert_eq!(choice_label("ctrl-alt-shift").as_ref(), "Ctrl+Alt+Shift");
        assert_eq!(
            choice_label("ewa-lanczos-sharp").as_ref(),
            "EWA Lanczos sharp"
        );
        assert_eq!(choice_label("2.5ms").as_ref(), "2.5 ms");
        assert_eq!(choice_label("4:2:2").as_ref(), "4:2:2");
        assert_eq!(choice_label("48000").as_ref(), "48000");
        assert_eq!(choice_label("f12").as_ref(), "F12");
        assert_eq!(choice_label("p3").as_ref(), "P3");
        assert_eq!(choice_label("center").as_ref(), "Centre");
    }

    #[test]
    fn every_choice_list_has_a_distinct_label() {
        for field in FIELDS {
            let options = match field.kind {
                Kind::Choice(options) | Kind::MultiChoice(options) => options,
                Kind::Primaries => primaries::CHOICES,
                Kind::Flag
                | Kind::Integer { .. }
                | Kind::Decimal
                | Kind::Text
                | Kind::Address
                | Kind::Resolution
                | Kind::FrameRate
                | Kind::Pairs => continue,
            };
            let mut labels = BTreeSet::new();

            for option in options {
                let label = choice_label(option);

                assert_ne!(label.as_ref(), "", "{}", field.path);
                assert!(
                    labels.insert(label.to_string()),
                    "duplicate label {label} in {}",
                    field.path
                );
            }
        }
    }

    #[test]
    fn short_closed_sets_are_segmented_and_long_ones_are_not() {
        let codec = field("video.codec");
        let filters = field("scaling.upscale.filter");
        let tune = FIELDS
            .iter()
            .find(|field| field.path == "video.encoder.tune" && field.optional)
            .unwrap();
        let Kind::Choice(codecs) = codec.kind else {
            panic!("codec");
        };
        let Kind::Choice(filter_names) = filters.kind else {
            panic!("filters");
        };
        let Kind::Choice(tunes) = tune.kind else {
            panic!("tune");
        };

        assert!(segmented_choice(codec.optional, codecs));
        assert!(!segmented_choice(filters.optional, filter_names));
        assert!(!segmented_choice(tune.optional, tunes));
    }

    #[test]
    fn search_matches_a_label_a_path_and_a_choice_name() {
        let codec = field("video.codec");
        let audio = field("audio.codec");

        assert!(field_matches(codec, "h.264"));
        assert!(field_matches(codec, "video.codec"));
        assert!(field_matches(codec, "H264"));
        assert!(!field_matches(codec, "jitter"));
        assert!(field_matches(audio, "flac"));
        assert!(!field_matches(audio, "h.264"));
        assert!(field_matches(field("video.color.transfer"), "colour"));
    }

    #[test]
    fn every_section_has_a_summary() {
        for section in Section::ALL {
            assert_ne!(
                section_summary(section, rusplay_config::ConfigRole::Host),
                ""
            );
            assert_ne!(
                section_summary(section, rusplay_config::ConfigRole::Client),
                ""
            );
        }
    }

    fn field(path: &str) -> &'static crate::settings::schema::Field {
        FIELDS.iter().find(|field| field.path == path).unwrap()
    }
}
