use std::{
    fs,
    path::{Path, PathBuf},
};

const DEVICE_DIRECTORY: &str = "/dev/dri";
const DRM_CLASS_DIRECTORY: &str = "/sys/class/drm";
const RENDER_NODE_PREFIX: &str = "renderD";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderNode {
    pub path: PathBuf,
    pub pci_bus_id: Option<String>,
}

impl RenderNode {
    pub fn is_on(&self, pci_bus_id: &str) -> bool {
        self.pci_bus_id
            .as_deref()
            .is_some_and(|own| own.eq_ignore_ascii_case(pci_bus_id))
    }
}

pub fn render_nodes() -> Vec<RenderNode> {
    nodes_in(Path::new(DEVICE_DIRECTORY), Path::new(DRM_CLASS_DIRECTORY))
}

fn nodes_in(device_directory: &Path, drm_class_directory: &Path) -> Vec<RenderNode> {
    let Ok(entries) = fs::read_dir(device_directory) else {
        return Vec::new();
    };

    let mut nodes: Vec<(u32, RenderNode)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let minor: u32 = name
                .to_str()?
                .strip_prefix(RENDER_NODE_PREFIX)?
                .parse()
                .ok()?;
            let pci_bus_id = pci_bus_id_of(&name, drm_class_directory);

            Some((
                minor,
                RenderNode {
                    path: entry.path(),
                    pci_bus_id,
                },
            ))
        })
        .collect();
    nodes.sort_by_key(|(minor, _)| *minor);

    nodes.into_iter().map(|(_, node)| node).collect()
}

fn pci_bus_id_of(name: &std::ffi::OsStr, drm_class_directory: &Path) -> Option<String> {
    let device = fs::canonicalize(drm_class_directory.join(name).join("device")).ok()?;

    device
        .file_name()
        .and_then(|address| address.to_str())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("rusplay-va-nodes-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();

        directory
    }

    #[test]
    fn render_nodes_are_listed_in_minor_order_with_their_pci_address() {
        let root = scratch("listed");
        let dev = root.join("dev");
        let class = root.join("class");
        let pci = root.join("pci");

        fs::create_dir_all(&dev).unwrap();
        for (node, address) in [
            ("renderD129", "0000:0B:00.0"),
            ("renderD128", "0000:03:00.0"),
        ] {
            fs::write(dev.join(node), b"").unwrap();
            fs::create_dir_all(class.join(node)).unwrap();
            fs::create_dir_all(pci.join(address)).unwrap();
            symlink(pci.join(address), class.join(node).join("device")).unwrap();
        }
        fs::write(dev.join("card0"), b"").unwrap();
        fs::write(dev.join("renderD130"), b"").unwrap();

        let nodes = nodes_in(&dev, &class);

        assert_eq!(
            nodes,
            [
                RenderNode {
                    path: dev.join("renderD128"),
                    pci_bus_id: Some("0000:03:00.0".to_owned()),
                },
                RenderNode {
                    path: dev.join("renderD129"),
                    pci_bus_id: Some("0000:0b:00.0".to_owned()),
                },
                RenderNode {
                    path: dev.join("renderD130"),
                    pci_bus_id: None,
                },
            ]
        );
        assert!(nodes[1].is_on("0000:0B:00.0"));
        assert!(!nodes[2].is_on("0000:0b:00.0"));
    }

    #[test]
    fn a_missing_device_directory_lists_nothing() {
        let root = scratch("missing");

        assert_eq!(nodes_in(&root.join("absent"), &root), []);
    }
}
