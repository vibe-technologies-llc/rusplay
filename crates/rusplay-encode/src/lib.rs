mod backend;
mod capabilities;
pub mod device;
mod error;
#[cfg(any(
    feature = "x264",
    feature = "x265",
    feature = "svt-av1",
    feature = "vaapi",
    feature = "nvenc"
))]
#[cfg(all(test, any(feature = "vaapi", feature = "nvenc")))]
mod hardware;
#[cfg(any(
    feature = "x264",
    feature = "x265",
    feature = "svt-av1",
    feature = "vaapi",
    feature = "nvenc"
))]
mod hdr;
pub mod input;
#[cfg(feature = "nvenc")]
mod nvenc;
#[cfg(any(feature = "vaapi", feature = "nvenc"))]
mod obu;
#[cfg(any(feature = "vaapi", feature = "nvenc"))]
mod pool;
mod rate;
#[cfg(any(feature = "x264", feature = "x265", feature = "svt-av1"))]
mod software;
#[cfg(feature = "svt-av1")]
mod svt_av1;
#[cfg(all(test, any(feature = "x264", feature = "x265", feature = "svt-av1")))]
mod testing;
#[cfg(feature = "vaapi")]
mod vaapi;
#[cfg(feature = "x264")]
mod x264;
#[cfg(feature = "x265")]
mod x265;
#[cfg(any(feature = "x264", feature = "x265"))]
mod x26x;

use rusplay_config::VideoConfig;
use rusplay_core::{ChromaSiting, EncodedFrame, Frame, Timestamp};

pub use crate::{
    backend::Backend,
    capabilities::{Capabilities, capabilities},
    error::{ApiVersion, EncodeError, NvencCall, NvencStatus},
};

pub trait VideoEncoder: Send {
    fn encode(&mut self, frame: &Frame) -> Result<Option<EncodedFrame>, EncodeError>;

    fn reads_planes(&self) -> bool {
        false
    }

    fn encode_planes(
        &mut self,
        planes: &input::YuvFrame<'_>,
        timestamp: Timestamp,
    ) -> Result<Option<EncodedFrame>, EncodeError> {
        let _ = (planes, timestamp);
        Err(EncodeError::PlanesUnsupported)
    }

    fn request_keyframe(&mut self);

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), EncodeError>;
}

pub fn open(config: &VideoConfig) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    open_sited(config, ChromaSiting::default())
}

pub fn open_sited(
    config: &VideoConfig,
    #[cfg_attr(
        not(any(
            feature = "vaapi",
            feature = "nvenc",
            feature = "x264",
            feature = "x265",
            feature = "svt-av1"
        )),
        expect(unused_variables)
    )]
    siting: ChromaSiting,
) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    let backend = Backend::from(&config.encoder);

    backend.check_codec(config.codec)?;

    match &config.encoder {
        #[cfg(feature = "x264")]
        rusplay_config::VideoEncoderConfig::X264(x264) => {
            Ok(Box::new(x264::X264Encoder::new(config, x264, siting)?))
        }
        #[cfg(feature = "x265")]
        rusplay_config::VideoEncoderConfig::X265(x265) => {
            Ok(Box::new(x265::X265Encoder::new(config, x265, siting)?))
        }
        #[cfg(feature = "svt-av1")]
        rusplay_config::VideoEncoderConfig::SvtAv1(svt) => {
            Ok(Box::new(svt_av1::SvtAv1Encoder::new(config, svt, siting)?))
        }
        #[cfg(feature = "vaapi")]
        rusplay_config::VideoEncoderConfig::Vaapi(vaapi) => {
            Ok(Box::new(vaapi::VaapiEncoder::new(config, vaapi, siting)?))
        }
        #[cfg(feature = "nvenc")]
        rusplay_config::VideoEncoderConfig::Nvenc(nvenc) => {
            Ok(Box::new(nvenc::NvencEncoder::new(config, nvenc, siting)?))
        }
        #[cfg(not(all(
            feature = "vaapi",
            feature = "nvenc",
            feature = "x264",
            feature = "x265",
            feature = "svt-av1"
        )))]
        _ => Err(EncodeError::BackendNotCompiled { backend }),
    }
}
