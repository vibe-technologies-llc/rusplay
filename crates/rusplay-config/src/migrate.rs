use std::path::Path;

use rusplay_core::{BitDepth, ChromaSubsampling, Codec, ColorSpace, FrameRate};

use crate::{
    AutoDecoderConfig, AutoEncoderConfig, ChromaChoice, CodecChoice, Config, DepthChoice,
    FrameRateChoice, LEGACY_DEFAULT_RENDER_NODE, StreamColorMode, VaapiConfig, VaapiDecoderConfig,
    VideoDecoderConfig, VideoEncoderConfig,
};

const LEGACY_FRAME_RATE: FrameRate = FrameRate::integer(60);
const LEGACY_CODEC: Codec = Codec::Hevc;
const LEGACY_CHROMA: ChromaSubsampling = ChromaSubsampling::Yuv422;
const LEGACY_DEPTH: BitDepth = BitDepth::Eight;

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
