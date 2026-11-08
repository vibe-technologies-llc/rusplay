use rusplay_config::{
    NvdecDecoderConfig, SoftwareDecoderConfig, VaapiDecoderConfig, VideoDecoderConfig,
};

use crate::capabilities;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    path: std::path::PathBuf,
    pci_bus_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Device {
    gpu_index: u32,
    pci_bus_id: Option<String>,
}

pub fn resolve(config: &VideoDecoderConfig, presenting_gpu: Option<&str>) -> VideoDecoderConfig {
    if !matches!(config, VideoDecoderConfig::Auto(_)) {
        return config.clone();
    }

    candidates(&render_nodes(), &cuda_devices(), presenting_gpu)
        .into_iter()
        .find(can_decode)
        .unwrap_or_else(|| config.clone())
}

fn can_decode(config: &VideoDecoderConfig) -> bool {
    capabilities(config).is_ok_and(|found| !found.formats().is_empty())
}

fn is_on(pci_bus_id: Option<&str>, gpu: &str) -> bool {
    pci_bus_id.is_some_and(|own| own.eq_ignore_ascii_case(gpu))
}

fn candidates(
    nodes: &[Node],
    devices: &[Device],
    presenting_gpu: Option<&str>,
) -> Vec<VideoDecoderConfig> {
    let vaapi = |node: &Node, copy_output| {
        VideoDecoderConfig::Vaapi(VaapiDecoderConfig {
            device: node.path.clone(),
            copy_output,
        })
    };
    let nvdec = |device: &Device| {
        VideoDecoderConfig::Nvdec(NvdecDecoderConfig {
            gpu_index: device.gpu_index,
        })
    };
    let software = VideoDecoderConfig::Software(SoftwareDecoderConfig::default());

    let Some(gpu) = presenting_gpu else {
        return nodes
            .iter()
            .map(|node| vaapi(node, false))
            .chain(devices.iter().map(nvdec))
            .chain([software])
            .collect();
    };

    let (local_nodes, other_nodes): (Vec<&Node>, Vec<&Node>) = nodes
        .iter()
        .partition(|node| is_on(node.pci_bus_id.as_deref(), gpu));
    let (local_devices, other_devices): (Vec<&Device>, Vec<&Device>) = devices
        .iter()
        .partition(|device| is_on(device.pci_bus_id.as_deref(), gpu));

    local_nodes
        .into_iter()
        .map(|node| vaapi(node, false))
        .chain(local_devices.into_iter().map(nvdec))
        .chain(other_devices.into_iter().map(nvdec))
        .chain(other_nodes.into_iter().map(|node| vaapi(node, true)))
        .chain([software])
        .collect()
}

#[cfg(feature = "vaapi")]
fn render_nodes() -> Vec<Node> {
    rusplay_va::render_nodes()
        .into_iter()
        .map(|node| Node {
            path: node.path,
            pci_bus_id: node.pci_bus_id,
        })
        .collect()
}

#[cfg(not(feature = "vaapi"))]
fn render_nodes() -> Vec<Node> {
    Vec::new()
}

#[cfg(feature = "nvdec")]
fn cuda_devices() -> Vec<Device> {
    let Ok(driver) = rusplay_cuda::Driver::load() else {
        return Vec::new();
    };

    (0..driver.device_count().unwrap_or(0))
        .map(|gpu_index| Device {
            gpu_index,
            pci_bus_id: driver.pci_bus_id(gpu_index).ok(),
        })
        .collect()
}

#[cfg(not(feature = "nvdec"))]
fn cuda_devices() -> Vec<Device> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: &str = "0000:03:00.0";
    const OTHER: &str = "0000:0b:00.0";

    fn node(name: &str, pci: Option<&str>) -> Node {
        Node {
            path: name.into(),
            pci_bus_id: pci.map(str::to_owned),
        }
    }

    fn device(gpu_index: u32, pci: &str) -> Device {
        Device {
            gpu_index,
            pci_bus_id: Some(pci.to_owned()),
        }
    }

    fn describe(configs: &[VideoDecoderConfig]) -> Vec<String> {
        configs
            .iter()
            .map(|config| match config {
                VideoDecoderConfig::Vaapi(vaapi) if vaapi.copy_output => {
                    format!("vaapi {} copy", vaapi.device.display())
                }
                VideoDecoderConfig::Vaapi(vaapi) => format!("vaapi {}", vaapi.device.display()),
                VideoDecoderConfig::Nvdec(nvdec) => format!("nvdec {}", nvdec.gpu_index),
                VideoDecoderConfig::Software(_) => "software".to_owned(),
                VideoDecoderConfig::Auto(_) => "auto".to_owned(),
            })
            .collect()
    }

    #[test]
    fn decoders_on_the_presenting_gpu_come_first_and_other_vaapi_nodes_copy_through_the_cpu() {
        let nodes = [
            node("other", Some(OTHER)),
            node("unknown", None),
            node("local", Some("0000:03:00.0")),
        ];
        let devices = [device(0, OTHER), device(1, LOCAL)];

        assert_eq!(
            describe(&candidates(&nodes, &devices, Some(LOCAL))),
            [
                "vaapi local",
                "nvdec 1",
                "nvdec 0",
                "vaapi other copy",
                "vaapi unknown copy",
                "software",
            ]
        );
    }

    #[test]
    fn without_a_window_every_node_is_tried_in_order_then_nvdec_then_software() {
        let nodes = [node("first", Some(LOCAL)), node("second", None)];
        let devices = [device(0, OTHER)];

        assert_eq!(
            describe(&candidates(&nodes, &devices, None)),
            ["vaapi first", "vaapi second", "nvdec 0", "software"]
        );
    }

    #[test]
    #[cfg(feature = "software")]
    fn auto_always_finds_a_decoder_when_the_software_one_is_compiled() {
        let resolved = resolve(&VideoDecoderConfig::default(), None);

        eprintln!("auto decoder resolved to {resolved:?}");
        assert!(!matches!(resolved, VideoDecoderConfig::Auto(_)));
        assert!(can_decode(&resolved));
    }

    #[test]
    fn a_pinned_decoder_is_never_replaced() {
        let pinned = VideoDecoderConfig::Nvdec(NvdecDecoderConfig { gpu_index: 2 });

        assert_eq!(resolve(&pinned, Some(LOCAL)), pinned);
    }
}
