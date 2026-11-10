use std::path::Path;

use rusplay_core::{BitDepth, ChromaSubsampling, Codec, ColorSpace, FrameRate};

use crate::{
    AutoDecoderConfig, AutoEncoderConfig, ChromaChoice, CodecChoice, Config, DebandConfig,
    DepthChoice, FrameRateChoice, InputConfig, LEGACY_DEFAULT_RENDER_NODE, StreamColorMode,
    VaapiConfig, VaapiDecoderConfig, VideoDecoderConfig, VideoEncoderConfig,
    scaling::LEGACY_DEBAND_THRESHOLD,
};

const LEGACY_FRAME_RATE: FrameRate = FrameRate::integer(60);
const LEGACY_CODEC: Codec = Codec::Hevc;
const LEGACY_CHROMA: ChromaSubsampling = ChromaSubsampling::Yuv422;
const LEGACY_DEPTH: BitDepth = BitDepth::Eight;
const LEGACY_RESEND_DELAY_MS: u16 = 8;
const NETWORK_SECTION: &str = "network";
const RETIRED_BITRATE_HINT_MAX: &str = "bitrate_hint_max_kbps";

pub fn unpin_legacy_defaults(config: &mut Config) {
    let video = &mut config.video;

    if video.frame_rate == FrameRateChoice::Fixed(LEGACY_FRAME_RATE) {
        video.frame_rate = FrameRateChoice::Auto;
    }
    if video.codec.fixed() == Some(LEGACY_CODEC) {
        video.codec = CodecChoice::Auto;
    }
    if video.pixel_format.chroma.fixed() == Some(LEGACY_CHROMA) {
        video.pixel_format.chroma = ChromaChoice::Auto;
    }
    if video.pixel_format.depth.fixed() == Some(LEGACY_DEPTH) {
        video.pixel_format.depth = DepthChoice::Auto;
    }

    video.color_mode = if video.color == ColorSpace::default() {
        StreamColorMode::Source
    } else {
        StreamColorMode::Configured
    };

    if let VideoEncoderConfig::Vaapi(VaapiConfig {
        device,
        rate_control,
        low_power: true,
        params,
    }) = &video.encoder
        && is_legacy_node(device)
    {
        video.encoder = VideoEncoderConfig::Auto(AutoEncoderConfig {
            rate_control: *rate_control,
            params: params.clone(),
        });
    }

    if let VideoDecoderConfig::Vaapi(VaapiDecoderConfig {
        device,
        copy_output: false,
    }) = &video.decoder
        && is_legacy_node(device)
    {
        video.decoder = VideoDecoderConfig::Auto(AutoDecoderConfig::default());
    }
}

pub fn retune_legacy_deband(config: &mut Config) {
    let deband = &mut config.scaling.deband;
    let retuned = DebandConfig::default();
    let is_legacy = |threshold: f32| threshold.to_bits() == LEGACY_DEBAND_THRESHOLD.to_bits();

    if is_legacy(deband.threshold) {
        deband.threshold = retuned.threshold;
    }
    if is_legacy(deband.chroma_threshold) {
        deband.chroma_threshold = retuned.chroma_threshold;
    }
}

pub fn drop_retired_keys(table: &mut toml::Table) {
    if let Some(toml::Value::Table(network)) = table.get_mut(NETWORK_SECTION) {
        network.remove(RETIRED_BITRATE_HINT_MAX);
    }
}

pub fn retune_legacy_resend_delay(config: &mut Config) {
    if config.input.resend_delay_ms == LEGACY_RESEND_DELAY_MS {
        config.input.resend_delay_ms = InputConfig::default().resend_delay_ms;
    }
}

fn is_legacy_node(device: &Path) -> bool {
    device == Path::new(LEGACY_DEFAULT_RENDER_NODE)
}

#[cfg(test)]
mod tests {
    use rusplay_core::{ColorPrimaries, TransferFunction};

    use super::*;
    use crate::{CONFIG_VERSION, RateControl};

    const VERSION_ONE: &str = r#"
[video]
codec = "hevc"
bitrate_kbps = 50000

[video.frame_rate]
numerator = 60
denominator = 1

[video.pixel_format]
chroma = "4:2:2"
depth = "eight"

[video.encoder]
backend = "vaapi"
device = "/dev/dri/renderD128"
low_power = true

[video.encoder.rate_control]
mode = "vbr"
max_bitrate_kbps = 90000

[video.decoder]
backend = "vaapi"
device = "/dev/dri/renderD128"
copy_output = false
"#;

    const VERSION_ONE_PINNED: &str = r#"
[video]
codec = "av1"

[video.frame_rate]
numerator = 120
denominator = 1

[video.pixel_format]
chroma = "4:2:0"
depth = "ten"

[video.color]
primaries = "bt2020"
transfer = "pq"

[video.encoder]
backend = "vaapi"
device = "/dev/dri/renderD129"

[video.decoder]
backend = "nvdec"
"#;

    fn parse(text: &str) -> Config {
        Config::parse(text, Path::new("test.toml")).unwrap()
    }

    #[test]
    fn a_version_one_file_turns_the_old_shipped_defaults_into_auto() {
        let config = parse(VERSION_ONE);

        assert_eq!(config.video.frame_rate, FrameRateChoice::Auto);
        assert_eq!(config.video.codec, CodecChoice::Auto);
        assert_eq!(config.video.pixel_format.chroma, ChromaChoice::Auto);
        assert_eq!(config.video.pixel_format.depth, DepthChoice::Auto);
        assert_eq!(config.video.color_mode, StreamColorMode::Source);
        assert_eq!(config.video.bitrate_kbps, 50_000);
        assert_eq!(
            config.video.encoder,
            VideoEncoderConfig::Auto(AutoEncoderConfig {
                rate_control: RateControl::Vbr {
                    max_bitrate_kbps: 90_000
                },
                params: crate::EncoderParams::new(),
            })
        );
        assert_eq!(
            config.video.decoder,
            VideoDecoderConfig::Auto(AutoDecoderConfig::default())
        );
    }

    #[test]
    fn a_version_one_file_keeps_every_value_that_was_changed() {
        let config = parse(VERSION_ONE_PINNED);

        assert_eq!(
            config.video.frame_rate,
            FrameRateChoice::Fixed(FrameRate::integer(120))
        );
        assert_eq!(config.video.codec, CodecChoice::Av1);
        assert_eq!(config.video.pixel_format.chroma, ChromaChoice::Yuv420);
        assert_eq!(config.video.pixel_format.depth, DepthChoice::Ten);
        assert_eq!(config.video.color_mode, StreamColorMode::Configured);
        assert_eq!(config.video.color.primaries, ColorPrimaries::Bt2020);
        assert_eq!(config.video.color.transfer, TransferFunction::Pq);
        assert!(matches!(
            &config.video.encoder,
            VideoEncoderConfig::Vaapi(vaapi) if vaapi.device == Path::new("/dev/dri/renderD129")
        ));
        assert!(matches!(config.video.decoder, VideoDecoderConfig::Nvdec(_)));
    }

    #[test]
    fn a_current_file_is_read_as_written_even_where_it_names_the_old_defaults() {
        let current = format!("version = {CONFIG_VERSION}\n{VERSION_ONE}");

        let config = parse(&current);

        assert_eq!(
            config.video.frame_rate,
            FrameRateChoice::Fixed(LEGACY_FRAME_RATE)
        );
        assert_eq!(config.video.codec, CodecChoice::Hevc);
        assert!(matches!(config.video.encoder, VideoEncoderConfig::Vaapi(_)));
        assert!(matches!(config.video.decoder, VideoDecoderConfig::Vaapi(_)));
    }

    #[test]
    fn a_version_two_file_keeps_its_video_choices_and_loses_the_old_deband_thresholds() {
        let stored = format!(
            "version = 2\n{VERSION_ONE}\n[scaling.deband]\niterations = 3\nthreshold = 3.0\nchroma_threshold = 3.0\n"
        );

        let config = parse(&stored);

        let deband = config.scaling.deband;
        assert_eq!(config.video.codec, CodecChoice::Hevc);
        assert!(matches!(config.video.encoder, VideoEncoderConfig::Vaapi(_)));
        assert_eq!(deband.iterations, 3);
        assert_eq!(
            (deband.threshold, deband.chroma_threshold, deband.limit),
            (2.0, 2.0, 0.5)
        );
    }

    #[test]
    fn an_older_file_keeps_a_deband_threshold_that_was_changed() {
        let config = parse("[scaling.deband]\nthreshold = 4.0\nchroma_threshold = 0.0\n");

        let deband = config.scaling.deband;
        assert_eq!((deband.threshold, deband.chroma_threshold), (4.0, 0.0));
    }

    #[test]
    fn a_current_file_keeps_a_deband_threshold_equal_to_the_old_default() {
        let stored = format!("version = {CONFIG_VERSION}\n[scaling.deband]\nthreshold = 3.0\n");

        assert_eq!(parse(&stored).scaling.deband.threshold, 3.0);
    }

    #[test]
    fn a_version_three_file_loses_the_old_resend_delay_and_keeps_a_changed_one() {
        let untouched = parse("version = 3\n[input]\nresend_delay_ms = 8\n");
        let changed = parse("version = 3\n[input]\nresend_delay_ms = 12\n");
        let unversioned = parse("[input]\nresend_delay_ms = 8\n");

        assert_eq!(untouched.input.resend_delay_ms, 2);
        assert_eq!(changed.input.resend_delay_ms, 12);
        assert_eq!(unversioned.input.resend_delay_ms, 2);
    }

    #[test]
    fn an_older_file_may_still_carry_the_retired_bitrate_hint_maximum() {
        let stored = "version = 3\n[network]\nport = 4000\nbitrate_hint_max_kbps = 200000\n";
        let current =
            format!("version = {CONFIG_VERSION}\n[network]\nbitrate_hint_max_kbps = 200000\n");

        assert_eq!(parse(stored).network.port, 4000);
        assert!(Config::parse(&current, Path::new("test.toml")).is_err());
    }

    #[test]
    fn a_current_file_keeps_a_resend_delay_equal_to_the_old_default() {
        let stored = format!("version = {CONFIG_VERSION}\n[input]\nresend_delay_ms = 8\n");

        assert_eq!(parse(&stored).input.resend_delay_ms, 8);
    }

    #[test]
    fn an_empty_file_is_the_default_config() {
        assert_eq!(parse(""), Config::default());
    }

    #[test]
    fn a_saved_config_names_its_version_and_loads_back_unchanged() {
        let mut config = Config::default();
        config.video.codec = CodecChoice::Hevc;
        config.video.frame_rate = FrameRateChoice::Fixed(LEGACY_FRAME_RATE);

        let text = config.to_toml_string().unwrap();

        assert!(text.starts_with(&format!("version = {CONFIG_VERSION}\n")));
        assert_eq!(parse(&text), config);
    }

    #[test]
    fn a_newer_or_malformed_version_is_refused() {
        let newer = format!("version = {}\n", CONFIG_VERSION + 1);

        assert!(matches!(
            Config::parse(&newer, Path::new("t.toml")),
            Err(crate::ConfigError::VersionUnsupported { version, .. }) if version == CONFIG_VERSION + 1
        ));
        assert!(matches!(
            Config::parse("version = \"two\"\n", Path::new("t.toml")),
            Err(crate::ConfigError::VersionNotInteger { .. })
        ));
    }
}
