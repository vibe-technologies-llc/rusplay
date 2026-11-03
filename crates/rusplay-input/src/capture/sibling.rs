use std::path::PathBuf;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeCaps {
    pub south_button: bool,
    pub accelerometer: bool,
    pub abs_axes: Vec<u16>,
}

impl NodeCaps {
    fn has_axes(&self, axes: &[u16]) -> bool {
        axes.iter().all(|axis| self.abs_axes.contains(axis))
    }

    pub fn role(&self) -> Option<NodeRole> {
        if self.south_button && self.has_axes(&[ABS_X, ABS_Y]) {
            Some(NodeRole::Pad)
        } else if self.south_button {
            None
        } else if self.accelerometer && self.has_axes(&[ABS_X, ABS_Y, ABS_Z]) {
            Some(NodeRole::Motion)
        } else if !self.accelerometer && self.has_axes(&[ABS_MT_POSITION_X, ABS_MT_POSITION_Y]) {
            Some(NodeRole::Touchpad)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    Pad,
    Motion,
    Touchpad,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeIdentity {
    pub vendor: u16,
    pub product: u16,
    pub uniq: Option<String>,
    pub phys: Option<String>,
    pub parent: Option<PathBuf>,
}

impl NodeIdentity {
    pub fn belongs_with(&self, other: &Self) -> bool {
        if self.vendor != other.vendor || self.product != other.product {
            return false;
        }

        if let Some(same_parent) = both(self.parent.as_ref(), other.parent.as_ref()) {
            return same_parent;
        }

        let verdicts = [
            both(self.uniq.as_ref(), other.uniq.as_ref()),
            both(self.phys.as_ref(), other.phys.as_ref()),
        ];

        verdicts.iter().flatten().all(|agree| *agree) && verdicts.iter().any(Option::is_some)
    }
}

fn both<T: PartialEq>(left: Option<&T>, right: Option<&T>) -> Option<bool> {
    Some(left? == right?)
}

pub fn non_empty(text: Option<&str>) -> Option<String> {
    text.filter(|text| !text.is_empty()).map(str::to_owned)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeDescription {
    pub path: PathBuf,
    pub role: NodeRole,
    pub identity: NodeIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedPad {
    pub pad: PathBuf,
    pub motion: Option<PathBuf>,
    pub touchpad: Option<PathBuf>,
}

pub fn pair_siblings(nodes: &[NodeDescription]) -> Vec<PairedPad> {
    let mut ordered: Vec<&NodeDescription> = nodes.iter().collect();
    ordered.sort_by(|left, right| left.path.cmp(&right.path));

    let mut claimed: Vec<&PathBuf> = Vec::new();
    let mut pairs = Vec::new();

    for pad in ordered.iter().filter(|node| node.role == NodeRole::Pad) {
        let mut claim = |role: NodeRole| {
            let sibling = ordered.iter().find(|node| {
                node.role == role
                    && !claimed.contains(&&node.path)
                    && pad.identity.belongs_with(&node.identity)
            })?;

            claimed.push(&sibling.path);
            Some(sibling.path.clone())
        };

        let motion = claim(NodeRole::Motion);
        let touchpad = claim(NodeRole::Touchpad);
        pairs.push(PairedPad {
            pad: pad.path.clone(),
            motion,
            touchpad,
        });
    }

    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    const SONY: u16 = 0x054c;
    const DUALSENSE: u16 = 0x0ce6;
    const MAC: &str = "a0:fa:9c:11:22:33";
    const PHYS: &str = "usb-0000:00:14.0-2/input0";
    const PARENT: &str =
        "/sys/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.3/0003:054C:0CE6.0004";

    fn identity(uniq: Option<&str>, phys: Option<&str>, parent: Option<&str>) -> NodeIdentity {
        NodeIdentity {
            vendor: SONY,
            product: DUALSENSE,
            uniq: non_empty(uniq),
            phys: non_empty(phys),
            parent: parent.map(PathBuf::from),
        }
    }

    fn node(path: &str, role: NodeRole, identity: NodeIdentity) -> NodeDescription {
        NodeDescription {
            path: PathBuf::from(path),
            role,
            identity,
        }
    }

    fn dualsense(base: u32, uniq: &str, parent: &str) -> Vec<NodeDescription> {
        let who = || identity(Some(uniq), Some(PHYS), Some(parent));

        vec![
            node(&format!("/dev/input/event{base}"), NodeRole::Pad, who()),
            node(
                &format!("/dev/input/event{}", base + 1),
                NodeRole::Touchpad,
                who(),
            ),
            node(
                &format!("/dev/input/event{}", base + 2),
                NodeRole::Motion,
                who(),
            ),
        ]
    }

    fn caps(south: bool, accelerometer: bool, axes: &[u16]) -> NodeCaps {
        NodeCaps {
            south_button: south,
            accelerometer,
            abs_axes: axes.to_vec(),
        }
    }

    #[test]
    fn nodes_are_classified_by_their_capabilities() {
        let pad = caps(true, false, &[0, 1, 2, 3, 4, 5, 16, 17]);
        let motion = caps(false, true, &[0, 1, 2, 3, 4, 5]);
        let touchpad = caps(false, false, &[0x35, 0x36, 0x39, 0x2f]);
        let keyboard = caps(false, false, &[]);
        let mouse_with_wheel_axes = caps(false, false, &[0, 1]);
        let sensor_without_z = caps(false, true, &[0, 1]);
        let button_only_pad = caps(true, false, &[]);

        assert_eq!(pad.role(), Some(NodeRole::Pad));
        assert_eq!(motion.role(), Some(NodeRole::Motion));
        assert_eq!(touchpad.role(), Some(NodeRole::Touchpad));
        assert_eq!(keyboard.role(), None);
        assert_eq!(mouse_with_wheel_axes.role(), None);
        assert_eq!(sensor_without_z.role(), None);
        assert_eq!(button_only_pad.role(), None);
    }

    #[test]
    fn a_dualsense_pairs_with_its_motion_and_touchpad_nodes() {
        let nodes = dualsense(10, MAC, PARENT);

        let pairs = pair_siblings(&nodes);

        assert_eq!(
            pairs,
            vec![PairedPad {
                pad: "/dev/input/event10".into(),
                motion: Some("/dev/input/event12".into()),
                touchpad: Some("/dev/input/event11".into()),
            }]
        );
    }

    #[test]
    fn two_identical_pads_keep_their_own_siblings() {
        let mut nodes = dualsense(10, MAC, PARENT);
        nodes.extend(dualsense(
            20,
            "a0:fa:9c:44:55:66",
            "/sys/devices/other/0003:054C:0CE6.0005",
        ));
        nodes.reverse();

        let pairs = pair_siblings(&nodes);

        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].pad, PathBuf::from("/dev/input/event10"));
        assert_eq!(pairs[0].motion, Some("/dev/input/event12".into()));
        assert_eq!(pairs[1].pad, PathBuf::from("/dev/input/event20"));
        assert_eq!(pairs[1].touchpad, Some("/dev/input/event21".into()));
    }

    #[test]
    fn the_same_uniq_on_a_different_parent_is_another_device() {
        let pad = identity(Some(MAC), None, Some(PARENT));
        let stranger = identity(Some(MAC), None, Some("/sys/devices/elsewhere"));

        assert!(!pad.belongs_with(&stranger));
    }

    #[test]
    fn the_same_parent_decides_even_when_the_strings_differ() {
        let pad = identity(Some(MAC), Some(PHYS), Some(PARENT));
        let sibling = identity(None, Some("other"), Some(PARENT));

        assert!(pad.belongs_with(&sibling));
    }

    #[test]
    fn a_shared_uniq_pairs_without_a_known_parent() {
        let pad = identity(Some(MAC), Some(PHYS), None);
        let sibling = identity(Some(MAC), Some(PHYS), None);
        let other_pad = identity(Some("other"), Some(PHYS), None);

        assert!(pad.belongs_with(&sibling));
        assert!(!pad.belongs_with(&other_pad));
    }

    #[test]
    fn phys_alone_pairs_when_nothing_else_is_known() {
        let pad = identity(None, Some(PHYS), None);

        assert!(pad.belongs_with(&identity(None, Some(PHYS), None)));
        assert!(!pad.belongs_with(&identity(None, Some("usb-0000:00:14.0-3/input0"), None)));
    }

    #[test]
    fn nothing_known_in_common_never_pairs() {
        let pad = identity(Some(MAC), None, None);
        let blank = identity(None, Some(PHYS), None);

        assert!(!pad.belongs_with(&blank));
        assert!(!NodeIdentity::default().belongs_with(&NodeIdentity::default()));
    }

    #[test]
    fn another_model_never_pairs_even_with_equal_identity_strings() {
        let pad = identity(Some(MAC), Some(PHYS), Some(PARENT));
        let other_model = NodeIdentity {
            product: 0x0df2,
            ..pad.clone()
        };

        assert!(!pad.belongs_with(&other_model));
    }

    #[test]
    fn a_laptop_touchpad_is_not_taken_for_a_pad_touchpad() {
        let mut nodes = dualsense(10, MAC, PARENT);
        nodes.push(node(
            "/dev/input/event3",
            NodeRole::Touchpad,
            NodeIdentity {
                vendor: 0x06cb,
                product: 0xce2c,
                uniq: None,
                phys: Some("i2c-ELAN0412:00".into()),
                parent: Some("/sys/devices/platform/i2c".into()),
            },
        ));

        let pairs = pair_siblings(&nodes);

        assert_eq!(pairs[0].touchpad, Some("/dev/input/event11".into()));
    }

    #[test]
    fn a_pad_without_siblings_pairs_with_nothing() {
        let nodes = vec![node(
            "/dev/input/event5",
            NodeRole::Pad,
            identity(Some(MAC), Some(PHYS), Some(PARENT)),
        )];

        assert_eq!(
            pair_siblings(&nodes),
            vec![PairedPad {
                pad: "/dev/input/event5".into(),
                motion: None,
                touchpad: None,
            }]
        );
    }

    #[test]
    fn one_sibling_is_never_claimed_by_two_pads() {
        let mut nodes = dualsense(10, MAC, PARENT);
        nodes.push(node(
            "/dev/input/event30",
            NodeRole::Pad,
            identity(Some(MAC), Some(PHYS), Some(PARENT)),
        ));

        let pairs = pair_siblings(&nodes);

        assert_eq!(pairs[0].motion, Some("/dev/input/event12".into()));
        assert_eq!(pairs[1].motion, None);
        assert_eq!(pairs[1].touchpad, None);
    }

    #[test]
    fn blank_strings_are_unknown() {
        assert_eq!(non_empty(Some("")), None);
        assert_eq!(non_empty(None), None);
        assert_eq!(non_empty(Some("x")), Some("x".to_owned()));
    }
}
