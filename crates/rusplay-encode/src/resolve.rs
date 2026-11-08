use rusplay_config::{
    AutoEncoderConfig, NvencConfig, SvtAv1Config, VaapiConfig, VideoEncoderConfig, X264Config,
    X265Config,
};

use crate::capabilities;

struct Candidate {
    config: VideoEncoderConfig,
    pci_bus_id: Option<String>,
}

pub fn resolve(config: &VideoEncoderConfig, preferred_gpu: Option<&str>) -> VideoEncoderConfig {
    let VideoEncoderConfig::Auto(auto) = config else {
        return config.clone();
    };

    ordered(hardware(auto), preferred_gpu)
        .into_iter()
        .chain(software(auto))
        .find(can_encode)
        .unwrap_or_else(|| config.clone())
}

fn can_encode(config: &VideoEncoderConfig) -> bool {
    capabilities(config).is_ok_and(|found| !found.format_set().is_empty())
}

fn ordered(mut candidates: Vec<Candidate>, preferred_gpu: Option<&str>) -> Vec<VideoEncoderConfig> {
    if let Some(preferred) = preferred_gpu {
        candidates.sort_by_key(|candidate| {
            !candidate
                .pci_bus_id
                .as_deref()
                .is_some_and(|own| own.eq_ignore_ascii_case(preferred))
        });
    }

    candidates
        .into_iter()
        .map(|candidate| candidate.config)
        .collect()
}

fn hardware(auto: &AutoEncoderConfig) -> Vec<Candidate> {
    let mut candidates = vaapi(auto);
    candidates.extend(nvenc(auto));
    candidates
}

#[cfg(feature = "vaapi")]
fn vaapi(auto: &AutoEncoderConfig) -> Vec<Candidate> {
    rusplay_va::render_nodes()
        .into_iter()
        .map(|node| Candidate {
            config: VideoEncoderConfig::Vaapi(VaapiConfig {
                device: node.path,
                rate_control: auto.rate_control,
                params: auto.params.clone(),
                ..VaapiConfig::default()
            }),
            pci_bus_id: node.pci_bus_id,
        })
        .collect()
}

#[cfg(not(feature = "vaapi"))]
fn vaapi(_: &AutoEncoderConfig) -> Vec<Candidate> {
    Vec::new()
}

#[cfg(feature = "nvenc")]
fn nvenc(auto: &AutoEncoderConfig) -> Vec<Candidate> {
    let Ok(driver) = rusplay_cuda::Driver::load() else {
        return Vec::new();
    };
    let count = driver.device_count().unwrap_or(0);

    (0..count)
        .map(|gpu_index| Candidate {
            config: VideoEncoderConfig::Nvenc(NvencConfig {
                gpu_index,
                rate_control: auto.rate_control,
                params: auto.params.clone(),
                ..NvencConfig::default()
            }),
            pci_bus_id: driver.pci_bus_id(gpu_index).ok(),
        })
        .collect()
}

#[cfg(not(feature = "nvenc"))]
fn nvenc(_: &AutoEncoderConfig) -> Vec<Candidate> {
    Vec::new()
}

fn software(auto: &AutoEncoderConfig) -> [VideoEncoderConfig; 3] {
    [
        VideoEncoderConfig::SvtAv1(SvtAv1Config {
            params: auto.params.clone(),
            ..SvtAv1Config::default()
        }),
        VideoEncoderConfig::X265(X265Config {
            params: auto.params.clone(),
            ..X265Config::default()
        }),
        VideoEncoderConfig::X264(X264Config {
            params: auto.params.clone(),
            ..X264Config::default()
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: &str, pci: Option<&str>) -> Candidate {
        Candidate {
            config: VideoEncoderConfig::Vaapi(VaapiConfig {
                device: name.into(),
                ..VaapiConfig::default()
            }),
            pci_bus_id: pci.map(str::to_owned),
        }
    }

    fn devices(configs: &[VideoEncoderConfig]) -> Vec<String> {
        configs
            .iter()
            .map(|config| match config {
                VideoEncoderConfig::Vaapi(vaapi) => vaapi.device.display().to_string(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn the_gpu_the_host_renders_on_is_tried_first_and_the_rest_keep_their_order() {
        let candidates = || {
            vec![
                candidate("a", Some("0000:03:00.0")),
                candidate("b", None),
                candidate("c", Some("0000:0B:00.0")),
            ]
        };

        assert_eq!(
            devices(&ordered(candidates(), Some("0000:0b:00.0"))),
            ["c", "a", "b"]
        );
        assert_eq!(devices(&ordered(candidates(), None)), ["a", "b", "c"]);
    }

    #[test]
    #[cfg(any(feature = "x264", feature = "x265", feature = "svt-av1"))]
    fn auto_always_finds_an_encoder_when_a_software_one_is_compiled() {
        let resolved = resolve(&VideoEncoderConfig::default(), None);

        eprintln!("auto encoder resolved to {resolved:?}");
        assert!(!matches!(resolved, VideoEncoderConfig::Auto(_)));
        assert!(can_encode(&resolved));
    }

    #[test]
    fn a_pinned_backend_is_never_replaced() {
        let pinned = VideoEncoderConfig::X264(X264Config::default());

        assert_eq!(resolve(&pinned, Some("0000:03:00.0")), pinned);
    }

    #[test]
    fn the_software_fallbacks_carry_the_auto_parameters() {
        let mut auto = AutoEncoderConfig::default();
        auto.params.insert("keyint".to_owned(), "120".to_owned());

        assert!(software(&auto).iter().all(|config| match config {
            VideoEncoderConfig::SvtAv1(svt) => svt.params == auto.params,
            VideoEncoderConfig::X265(x265) => x265.params == auto.params,
            VideoEncoderConfig::X264(x264) => x264.params == auto.params,
            _ => false,
        }));
    }
}
