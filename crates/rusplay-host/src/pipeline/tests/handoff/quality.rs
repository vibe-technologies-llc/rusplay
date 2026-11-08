use rusplay_core::{QualityScore, SamplePicture, SamplePlane, TemporalOrder, measure};

use super::*;
use crate::quality::{PictureSample, Sample, SampleKind};

const RESOLUTION: Resolution = Resolution::new(402, 226);
const WARMUP_FRAMES: u32 = 3;
const MIN_PSNR_DB: f32 = 30.0;

struct Sampled {
    history: SamplePlane,
    source: SamplePicture,
    reconstruction: SamplePicture,
}

fn sample(config: Config) -> Option<(Sampled, bool)> {
    let mut pipeline = pipeline(config)?;

    for index in 0..WARMUP_FRAMES {
        pipeline
            .encode(&shm_frame(RESOLUTION, index), index == 0)
            .unwrap();
        assert!(pipeline.take_sample().is_none());
    }

    pipeline.sample_next(SampleKind::Luma);
    pipeline
        .encode(&shm_frame(RESOLUTION, WARMUP_FRAMES), false)
        .unwrap();
    let Some(Sample::Luma(history)) = pipeline.take_sample() else {
        panic!("a luma sample was asked for");
    };

    pipeline.sample_next(SampleKind::Picture);
    pipeline
        .encode(&shm_frame(RESOLUTION, WARMUP_FRAMES + 1), false)
        .unwrap();
    let Some(Sample::Picture(picture)) = pipeline.take_sample() else {
        panic!("a picture sample was asked for");
    };
    let PictureSample {
        source,
        reconstruction: Some(reconstruction),
    } = *picture
    else {
        panic!("the picture sample carries a reconstruction");
    };

    pipeline
        .encode(&shm_frame(RESOLUTION, WARMUP_FRAMES + 2), false)
        .unwrap();
    assert!(pipeline.take_sample().is_none());

    let handed_off = pipeline.hands_off();
    Some((
        Sampled {
            history,
            source,
            reconstruction,
        },
        handed_off,
    ))
}

fn score(sampled: &Sampled) -> QualityScore {
    measure(
        &sampled.source,
        &sampled.reconstruction,
        &[&sampled.history],
        TemporalOrder::First,
    )
    .unwrap()
}

fn assert_plausible(sampled: &Sampled, format: PixelFormat, what: &str) {
    let score = score(sampled);

    assert_eq!(sampled.source.resolution(), RESOLUTION, "{what}");
    assert_eq!(sampled.source.format(), format, "{what}");
    assert_eq!(sampled.reconstruction.format(), format, "{what}");
    assert_eq!(sampled.history.width(), RESOLUTION.width as usize, "{what}");
    assert!(
        score.psnr.luma.value() > MIN_PSNR_DB,
        "{what}: luma psnr {}",
        score.psnr.luma
    );
    assert!(
        score.xpsnr.luma.value() > MIN_PSNR_DB,
        "{what}: luma xpsnr {}",
        score.xpsnr.luma
    );
}

#[test]
fn the_handoff_samples_the_same_source_as_the_readback() {
    for depth in [BitDepth::Eight, BitDepth::Ten] {
        let config = vaapi_config(Codec::Av1, depth, true);
        if !supported(&config) {
            continue;
        }
        let format = config.video.pixel_format;
        let Some((handed_off, used_handoff)) = sample(config) else {
            return;
        };
        let (read_back, used_readback) = sample(vaapi_config(Codec::Av1, depth, false)).unwrap();

        assert!(used_handoff, "{depth:?}: the dma-buf path ran");
        assert!(!used_readback);
        assert_eq!(handed_off.source, read_back.source, "{depth:?}");
        assert_eq!(handed_off.history, read_back.history, "{depth:?}");
        assert_plausible(&handed_off, (format).or_unnegotiated(), "vaapi hand-off");
        assert_plausible(&read_back, (format).or_unnegotiated(), "vaapi readback");
    }
}

#[cfg(feature = "x264")]
#[test]
fn a_software_encoder_samples_from_the_mapped_readback() {
    let mut config = vaapi_config(Codec::H264, BitDepth::Eight, false);
    config.video.encoder = VideoEncoderConfig::X264(rusplay_config::X264Config::default());
    let format = config.video.pixel_format;

    let Some((sampled, handed_off)) = sample(config) else {
        return;
    };

    assert!(!handed_off);
    assert_plausible(&sampled, (format).or_unnegotiated(), "x264");
}

#[cfg(feature = "svt-av1")]
#[test]
fn an_encoder_without_reconstruction_answers_unsupported() {
    let mut config = vaapi_config(Codec::Av1, BitDepth::Eight, false);
    config.video.encoder = VideoEncoderConfig::SvtAv1(rusplay_config::SvtAv1Config::default());
    let Some(mut pipeline) = pipeline(config) else {
        return;
    };
    let small = Resolution::new(128, 64);

    pipeline.sample_next(SampleKind::Picture);
    pipeline.encode(&shm_frame(small, 0), true).unwrap();

    assert!(matches!(pipeline.take_sample(), Some(Sample::Unsupported)));
}
