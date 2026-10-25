use crate::DrmFourcc;

pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

pub const DEFAULT_MODIFIERS: [u64; 2] = [DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatModifiers {
    pub format: DrmFourcc,
    pub modifiers: Vec<u64>,
}

impl FormatModifiers {
    pub fn new(format: DrmFourcc, modifiers: impl IntoIterator<Item = u64>) -> Self {
        Self {
            format,
            modifiers: modifiers.into_iter().collect(),
        }
    }

    pub fn with_defaults(format: DrmFourcc) -> Self {
        Self::new(format, DEFAULT_MODIFIERS)
    }
}

pub fn modifiers_for(table: &[FormatModifiers], format: DrmFourcc) -> &[u64] {
    table
        .iter()
        .find(|entry| entry.format == format)
        .map_or(&[], |entry| entry.modifiers.as_slice())
}

pub fn choose_modifier(acceptable: &[u64], offered: &[u64]) -> Option<u64> {
    acceptable
        .iter()
        .copied()
        .find(|modifier| offered.contains(modifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XR24: DrmFourcc = DrmFourcc::from_code(*b"XR24");
    const AMD_TILED: u64 = 0x0200_0000_0000_0001;

    #[test]
    fn our_preference_order_wins() {
        let acceptable = [DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR];
        let offered = [DRM_FORMAT_MOD_LINEAR, AMD_TILED, DRM_FORMAT_MOD_INVALID];

        assert_eq!(
            choose_modifier(&acceptable, &offered),
            Some(DRM_FORMAT_MOD_INVALID)
        );
    }

    #[test]
    fn disjoint_lists_choose_nothing() {
        assert_eq!(
            choose_modifier(&[DRM_FORMAT_MOD_LINEAR], &[AMD_TILED]),
            None
        );
    }

    #[test]
    fn table_lookup_finds_the_format() {
        let table = [FormatModifiers::new(XR24, [AMD_TILED])];

        assert_eq!(modifiers_for(&table, XR24), [AMD_TILED]);
        assert_eq!(modifiers_for(&table, DrmFourcc::from_code(*b"AR24")), []);
    }

    #[test]
    fn defaults_include_linear_and_implicit() {
        let entry = FormatModifiers::with_defaults(XR24);

        assert!(entry.modifiers.contains(&DRM_FORMAT_MOD_LINEAR));
        assert!(entry.modifiers.contains(&DRM_FORMAT_MOD_INVALID));
    }
}
