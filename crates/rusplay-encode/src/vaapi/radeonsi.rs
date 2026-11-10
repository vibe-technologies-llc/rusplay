const DRIVER: &str = "radeonsi";
const VALID_SETTING: u32 = 1;
const PRESET_SHIFT: u32 = 1;
const PRE_ENCODE_SHIFT: u32 = 3;
const VBAQ_SHIFT: u32 = 4;

pub(super) fn is_radeonsi(vendor: &str) -> bool {
    vendor
        .split_once('(')
        .and_then(|(_, renderer)| renderer.split(',').next())
        .is_some_and(|driver| driver.trim() == DRIVER)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Preset {
    Speed,
    Balanced,
    Quality,
    HighQuality,
}

impl Preset {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "speed" => Some(Self::Speed),
            "balanced" => Some(Self::Balanced),
            "quality" => Some(Self::Quality),
            "high-quality" => Some(Self::HighQuality),
            _ => None,
        }
    }

    const fn bits(self) -> u32 {
        match self {
            Self::Speed => 0,
            Self::Balanced => 1,
            Self::Quality => 2,
            Self::HighQuality => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct QualityModes {
    pub(super) preset: Preset,
    pub(super) pre_encode: bool,
    pub(super) vbaq: bool,
}

impl QualityModes {
    pub(super) const DRIVER_DEFAULT: Self = Self {
        preset: Preset::Speed,
        pre_encode: false,
        vbaq: false,
    };

    pub(super) fn quality_level(self) -> Option<u32> {
        (self != Self::DRIVER_DEFAULT).then(|| {
            VALID_SETTING
                | self.preset.bits() << PRESET_SHIFT
                | u32::from(self.pre_encode) << PRE_ENCODE_SHIFT
                | u32::from(self.vbaq) << VBAQ_SHIFT
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct QualityOverrides {
    pub(super) preset: Option<Preset>,
    pub(super) pre_encode: Option<bool>,
    pub(super) vbaq: Option<bool>,
}

impl QualityOverrides {
    pub(super) fn over(self, base: QualityModes) -> QualityModes {
        QualityModes {
            preset: self.preset.unwrap_or(base.preset),
            pre_encode: self.pre_encode.unwrap_or(base.pre_encode),
            vbaq: self.vbaq.unwrap_or(base.vbaq),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RADEONSI: &str = "Mesa Gallium driver 26.2.6-arch3.1 for AMD Radeon RX 9070 XT \
                            (radeonsi, gfx1201, ACO, DRM 3.64, 7.3.3-1-cachyos)";

    fn modes(preset: Preset, pre_encode: bool, vbaq: bool) -> QualityModes {
        QualityModes {
            preset,
            pre_encode,
            vbaq,
        }
    }

    #[test]
    fn only_the_radeonsi_vendor_string_is_radeonsi() {
        assert!(is_radeonsi(RADEONSI));
        assert!(is_radeonsi(
            "Mesa Gallium driver 25.0.1 for AMD Radeon RX 6800 (radeonsi, navi21, LLVM 19.1.7)"
        ));
        assert!(!is_radeonsi(
            "Intel iHD driver for Intel(R) Gen Graphics - 25.3.4"
        ));
        assert!(!is_radeonsi(
            "Mesa Gallium driver 26.2.6 for NV170 (nouveau)"
        ));
        assert!(!is_radeonsi(""));
    }

    #[test]
    fn modes_pack_into_the_bits_mesa_reads() {
        assert_eq!(
            modes(Preset::Balanced, false, false).quality_level(),
            Some(0x3)
        );
        assert_eq!(
            modes(Preset::Balanced, false, true).quality_level(),
            Some(0x13)
        );
        assert_eq!(modes(Preset::Speed, true, false).quality_level(), Some(0x9));
        assert_eq!(modes(Preset::Speed, true, true).quality_level(), Some(0x19));
        assert_eq!(
            modes(Preset::Quality, false, false).quality_level(),
            Some(0x5)
        );
        assert_eq!(
            modes(Preset::HighQuality, true, true).quality_level(),
            Some(0x1f)
        );
    }

    #[test]
    fn the_driver_default_sends_no_level() {
        assert_eq!(QualityModes::DRIVER_DEFAULT.quality_level(), None);
    }

    #[test]
    fn no_modes_pack_into_the_suggested_level() {
        for preset in [
            Preset::Speed,
            Preset::Balanced,
            Preset::Quality,
            Preset::HighQuality,
        ] {
            for pre_encode in [false, true] {
                for vbaq in [false, true] {
                    assert_ne!(modes(preset, pre_encode, vbaq).quality_level(), Some(1));
                }
            }
        }
    }

    #[test]
    fn overrides_replace_only_the_fields_they_name() {
        let base = modes(Preset::Balanced, false, true);
        let overrides = QualityOverrides {
            vbaq: Some(false),
            ..QualityOverrides::default()
        };

        assert_eq!(QualityOverrides::default().over(base), base);
        assert_eq!(overrides.over(base), modes(Preset::Balanced, false, false));
    }

    #[test]
    fn preset_names_parse() {
        assert_eq!(Preset::parse("speed"), Some(Preset::Speed));
        assert_eq!(Preset::parse("balanced"), Some(Preset::Balanced));
        assert_eq!(Preset::parse("quality"), Some(Preset::Quality));
        assert_eq!(Preset::parse("high-quality"), Some(Preset::HighQuality));
        assert_eq!(Preset::parse("fast"), None);
    }
}
