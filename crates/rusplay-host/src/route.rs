use rusplay_config::Config;
use rusplay_core::{ColorSpace, Resolution, TransferFunction};
use rusplay_scale::{
    ColorMapSettings, ColorPlan, ColorVolume, DynamicPeak, GamutMapping, InputLight,
    LineariseSettings, OutputLight, ScaleError, ScaleSettings, Sigmoid, SourceChannels,
    ToneMapping,
};

use crate::upload::UploadFormat;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceRead {
    Raw,
    Srgb,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Route {
    pub read: SourceRead,
    pub prepare: Option<LineariseSettings>,
    pub scale: Option<ScaleSettings>,
    pub relinearise: Option<LineariseSettings>,
    pub map: Option<ColorMapSettings>,
    pub input: InputLight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteKey {
    pub upload: UploadFormat,
    pub color: ColorSpace,
    pub capture: Resolution,
}

impl Route {
    pub(crate) fn plan(config: &Config, key: &RouteKey) -> Result<Self, ScaleError> {
        let RouteKey {
            upload,
            color: source,
            capture,
        } = *key;
        let stream = &config.video.color;
        let resolution = config.video.resolution.unwrap_or(capture);
        let scale = (resolution != capture).then(|| scale_settings(config, capture, resolution));
        let map = map_settings(config, &source);
        let needs_map = !ColorPlan::new(&map)?.is_identity();
        let linear_scale = scale.is_some_and(|settings| linear_light(config, &settings));
        let encoded = !needs_map && same_encoding(&source, stream) && !linear_scale;
        let map = needs_map.then_some(map);
        let linearise = LineariseSettings {
            channels: upload.channels,
            transfer: source.transfer,
            hdr: source.hdr,
            matrix: source.matrix_coefficients(),
            output: OutputLight::Linear,
        };
        let prepare_encoded =
            (!upload.channels.reads_as_rgba()).then(|| LineariseSettings::encoded(upload.channels));

        if encoded {
            return Ok(Self {
                read: SourceRead::Raw,
                prepare: prepare_encoded,
                scale,
                relinearise: None,
                map: None,
                input: InputLight::Encoded,
            });
        }

        if scale.is_some() && !linear_scale {
            return Ok(Self {
                read: SourceRead::Raw,
                prepare: prepare_encoded,
                scale,
                relinearise: Some(LineariseSettings {
                    channels: SourceChannels::RGBA,
                    ..linearise
                }),
                map,
                input: InputLight::Linear,
            });
        }

        let srgb_view = source.transfer == TransferFunction::Srgb
            && upload.srgb_view.is_some()
            && upload.channels.reads_as_rgba();

        Ok(Self {
            read: if srgb_view {
                SourceRead::Srgb
            } else {
                SourceRead::Raw
            },
            prepare: (!srgb_view).then_some(linearise),
            scale,
            relinearise: None,
            map,
            input: InputLight::Linear,
        })
    }
}

fn same_encoding(source: &ColorSpace, stream: &ColorSpace) -> bool {
    source.transfer == stream.transfer && (!source.transfer.is_hdr() || source.hdr == stream.hdr)
}

fn linear_light(config: &Config, settings: &ScaleSettings) -> bool {
    let upscaling = settings.target.width > settings.source.width
        || settings.target.height > settings.source.height;

    upscaling || config.scaling.downscale.linear_light
}

fn scale_settings(config: &Config, source: Resolution, target: Resolution) -> ScaleSettings {
    let scaling = &config.scaling;
    let upscaling = target.width > source.width || target.height > source.height;

    if upscaling {
        let upscale = &scaling.upscale;
        ScaleSettings {
            filter: upscale.filter,
            source,
            target,
            antiringing: upscale.antiringing,
            sigmoid: upscale.sigmoid.then_some(Sigmoid {
                center: upscale.sigmoid_center,
                slope: upscale.sigmoid_slope,
            }),
        }
    } else {
        ScaleSettings {
            filter: scaling.downscale.filter,
            source,
            target,
            antiringing: 0.0,
            sigmoid: None,
        }
    }
}

fn map_settings(config: &Config, source: &ColorSpace) -> ColorMapSettings {
    let tone = &config.color.tone_mapping;
    let gamut = &config.color.gamut_mapping;

    ColorMapSettings {
        source: ColorVolume::of_content(source),
        target: ColorVolume::of_content(&config.video.color),
        tone: ToneMapping {
            operator: tone.operator,
            dynamic_peak: tone.dynamic_peak.then(|| DynamicPeak {
                percentile: f64::from(tone.peak_percentile),
                smoothing_frames: tone.smoothing_frames,
                scene_threshold_db: f64::from(tone.scene_threshold_db),
            }),
        },
        gamut: GamutMapping {
            mode: gamut.mode,
            knee: f64::from(gamut.knee),
        },
    }
}

#[cfg(test)]
mod tests {
    use rusplay_capture::DrmFourcc;
    use rusplay_core::{ColorPrimaries, HdrLevels};

    use super::*;
    use crate::upload::upload_format_for;

    const CAPTURE: Resolution = Resolution::new(64, 32);

    fn key(fourcc: [u8; 4], color: ColorSpace) -> RouteKey {
        RouteKey {
            upload: upload_format_for(DrmFourcc::from_code(fourcc)).unwrap(),
            color,
            capture: CAPTURE,
        }
    }

    fn srgb() -> ColorSpace {
        ColorSpace {
            transfer: TransferFunction::Srgb,
            ..ColorSpace::default()
        }
    }

    fn pq_bt2020() -> ColorSpace {
        ColorSpace {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Pq,
            ..ColorSpace::default()
        }
    }

    fn stream(color: ColorSpace, resolution: Option<Resolution>) -> Config {
        let mut config = Config::default();
        config.video.color = color;
        config.video.resolution = resolution;
        config
    }

    #[test]
    fn an_srgb_capture_linearises_through_the_srgb_view() {
        let config = stream(ColorSpace::default(), None);

        let route = Route::plan(&config, &key(*b"XR24", srgb())).unwrap();

        assert_eq!(route.read, SourceRead::Srgb);
        assert_eq!(route.prepare, None);
        assert_eq!(route.map, None);
        assert_eq!(route.input, InputLight::Linear);
    }

    #[test]
    fn a_capture_already_in_the_stream_encoding_is_converted_as_it_is() {
        let config = stream(pq_bt2020(), None);

        let red_low = Route::plan(&config, &key(*b"XB30", pq_bt2020())).unwrap();
        let blue_low = Route::plan(&config, &key(*b"XR30", pq_bt2020())).unwrap();

        assert_eq!(red_low.read, SourceRead::Raw);
        assert_eq!(red_low.prepare, None);
        assert_eq!(red_low.input, InputLight::Encoded);
        assert_eq!(
            blue_low
                .prepare
                .map(|prepare| (prepare.channels, prepare.output)),
            Some((
                upload_format_for(DrmFourcc::from_code(*b"XR30"))
                    .unwrap()
                    .channels,
                OutputLight::Encoded
            ))
        );
        assert_eq!(blue_low.input, InputLight::Encoded);
    }

    #[test]
    fn an_hdr_capture_into_an_sdr_stream_is_linearised_and_tone_mapped() {
        let config = stream(ColorSpace::default(), None);

        let route = Route::plan(&config, &key(*b"XR30", pq_bt2020())).unwrap();
        let prepare = route.prepare.unwrap();
        let map = route.map.unwrap();

        assert_eq!(prepare.transfer, TransferFunction::Pq);
        assert_eq!(prepare.output, OutputLight::Linear);
        assert_eq!(map.source.primaries, ColorPrimaries::Bt2020);
        assert_eq!(map.target.primaries, ColorPrimaries::Bt709);
        assert!(ColorPlan::new(&map).unwrap().tone().is_some());
        assert_eq!(route.input, InputLight::Linear);
    }

    #[test]
    fn hdr_levels_that_differ_keep_the_linear_path() {
        let mut source = pq_bt2020();
        source.hdr = HdrLevels {
            reference_white_nits: 100,
            ..HdrLevels::default()
        };
        let config = stream(pq_bt2020(), None);

        let route = Route::plan(&config, &key(*b"XB30", source)).unwrap();

        assert_eq!(route.input, InputLight::Linear);
        assert!(route.map.is_some());
    }

    #[test]
    fn downscaling_in_linear_light_linearises_first() {
        let config = stream(srgb(), Some(Resolution::new(32, 16)));

        let route = Route::plan(&config, &key(*b"XB24", srgb())).unwrap();

        assert_eq!(route.read, SourceRead::Srgb);
        assert!(route.scale.is_some());
        assert_eq!(route.relinearise, None);
        assert_eq!(route.input, InputLight::Linear);
    }

    #[test]
    fn downscaling_in_gamma_light_scales_the_encoded_image() {
        let mut config = stream(srgb(), Some(Resolution::new(32, 16)));
        config.scaling.downscale.linear_light = false;

        let same = Route::plan(&config, &key(*b"XR24", srgb())).unwrap();
        let other = Route::plan(&config, &key(*b"XR30", pq_bt2020())).unwrap();

        assert_eq!(same.read, SourceRead::Raw);
        assert_eq!(same.prepare, None);
        assert!(same.scale.is_some());
        assert_eq!(same.relinearise, None);
        assert_eq!(same.input, InputLight::Encoded);
        assert_eq!(
            other.prepare.map(|prepare| prepare.output),
            Some(OutputLight::Encoded)
        );
        let relinearise = other.relinearise.unwrap();
        assert_eq!(relinearise.channels, SourceChannels::RGBA);
        assert_eq!(relinearise.transfer, TransferFunction::Pq);
        assert!(other.map.is_some());
        assert_eq!(other.input, InputLight::Linear);
    }

    #[test]
    fn upscaling_stays_in_linear_light() {
        let mut config = stream(srgb(), Some(Resolution::new(128, 64)));
        config.scaling.downscale.linear_light = false;

        let route = Route::plan(&config, &key(*b"XR24", srgb())).unwrap();

        assert_eq!(route.read, SourceRead::Srgb);
        assert!(route.scale.and_then(|scale| scale.sigmoid).is_some());
        assert_eq!(route.input, InputLight::Linear);
    }
}
