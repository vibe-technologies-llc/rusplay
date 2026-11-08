use std::{
    fs,
    path::{Path, PathBuf},
};

use rusplay_config::VideoDecoderConfig;

use crate::ClientError;

const DRM_CLASS_DIRECTORY: &str = "/sys/class/drm";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Preferred,
    Candidate(usize),
    Mismatch,
}

fn pick(node_pci: &str, preferred: &str, candidates: &[String]) -> Pick {
    if preferred == node_pci {
        return Pick::Preferred;
    }

    if let Some(index) = candidates
        .iter()
        .position(|candidate| candidate == node_pci)
    {
        return Pick::Candidate(index);
    }

    if preferred.is_empty() {
        Pick::Preferred
    } else {
        Pick::Mismatch
    }
}

fn pci_address_of(render_node: &Path, drm_class_directory: &Path) -> Option<String> {
    let node = fs::canonicalize(render_node).ok()?;
    let name = node.file_name()?;
    let device = fs::canonicalize(drm_class_directory.join(name).join("device")).ok()?;

    device
        .file_name()
        .and_then(|address| address.to_str())
        .map(str::to_ascii_lowercase)
}

fn vaapi_render_node(decoder: &VideoDecoderConfig) -> Option<PathBuf> {
    match decoder {
        VideoDecoderConfig::Vaapi(vaapi) if !vaapi.copy_output => Some(vaapi.device.clone()),
        VideoDecoderConfig::Auto(_)
        | VideoDecoderConfig::Vaapi(_)
        | VideoDecoderConfig::Nvdec(_)
        | VideoDecoderConfig::Software(_) => None,
    }
}

pub(crate) struct Chosen {
    pub adapter: wgpu::Adapter,
    pub decoder: VideoDecoderConfig,
}

pub(crate) fn choose(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'_>,
    decoder: &VideoDecoderConfig,
) -> Result<Chosen, ClientError> {
    let preferred = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(surface),
        ..wgpu::RequestAdapterOptions::default()
    }))
    .map_err(ClientError::WindowAdapter)?;
    let presenting = preferred.get_info().device_pci_bus_id;
    let decoder = rusplay_decode::resolve(
        decoder,
        Some(presenting.as_str()).filter(|pci| !pci.is_empty()),
    );

    let adapter = adapter_for(instance, surface, preferred, &decoder)?;

    Ok(Chosen { adapter, decoder })
}

fn adapter_for(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'_>,
    preferred: wgpu::Adapter,
    decoder: &VideoDecoderConfig,
) -> Result<wgpu::Adapter, ClientError> {
    let Some(device) = vaapi_render_node(decoder) else {
        return Ok(preferred);
    };
    let Some(node_pci) = pci_address_of(&device, Path::new(DRM_CLASS_DIRECTORY)) else {
        return Ok(preferred);
    };

    let presentable: Vec<wgpu::Adapter> =
        pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()))
            .into_iter()
            .filter(|adapter| adapter.is_surface_supported(surface))
            .collect();
    let addresses: Vec<String> = presentable
        .iter()
        .map(|adapter| adapter.get_info().device_pci_bus_id)
        .collect();
    let preferred_pci = preferred.get_info().device_pci_bus_id;

    match pick(&node_pci, &preferred_pci, &addresses) {
        Pick::Preferred => Ok(preferred),
        Pick::Candidate(index) => Ok(presentable.into_iter().nth(index).unwrap_or(preferred)),
        Pick::Mismatch => Err(ClientError::DecoderAdapterMismatch {
            device,
            decoder_pci: node_pci,
            adapter_pci: preferred_pci,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn addresses(list: &[&str]) -> Vec<String> {
        list.iter().map(|address| (*address).to_owned()).collect()
    }

    #[test]
    fn the_preferred_adapter_on_the_decoders_gpu_is_kept() {
        let candidates = addresses(&["0000:03:00.0", "0000:65:00.0"]);

        assert_eq!(
            pick("0000:03:00.0", "0000:03:00.0", &candidates),
            Pick::Preferred
        );
    }

    #[test]
    fn another_presentable_adapter_on_the_decoders_gpu_is_chosen() {
        let candidates = addresses(&["0000:65:00.0", "0000:03:00.0"]);

        assert_eq!(
            pick("0000:03:00.0", "0000:65:00.0", &candidates),
            Pick::Candidate(1)
        );
    }

    #[test]
    fn a_decoder_gpu_that_cannot_present_is_a_mismatch() {
        let candidates = addresses(&["0000:65:00.0"]);

        assert_eq!(
            pick("0000:03:00.0", "0000:65:00.0", &candidates),
            Pick::Mismatch
        );
    }

    #[test]
    fn an_adapter_without_a_bus_id_is_trusted() {
        let candidates = addresses(&[""]);

        assert_eq!(pick("0000:03:00.0", "", &candidates), Pick::Preferred);
    }

    #[test]
    fn the_pci_address_comes_from_the_class_directory() {
        let root = std::env::temp_dir().join(format!("rusplay-adapter-{}", std::process::id()));
        let pci = root.join("devices/pci0000:00/0000:00:01.1/0000:03:00.A");
        let class = root.join("class/drm/renderD128");
        let node = root.join("dev/renderD128");
        fs::create_dir_all(&pci).unwrap();
        fs::create_dir_all(&class).unwrap();
        fs::create_dir_all(root.join("dev")).unwrap();
        symlink(&pci, class.join("device")).unwrap();
        fs::write(&node, b"").unwrap();

        let address = pci_address_of(&node, &root.join("class/drm"));
        let missing = pci_address_of(&root.join("dev/renderD999"), &root.join("class/drm"));
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(address.as_deref(), Some("0000:03:00.a"));
        assert_eq!(missing, None);
    }
}
