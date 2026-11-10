use rusplay_config::EncoderParams;

use crate::{
    Backend, EncodeError,
    vaapi::radeonsi::{Preset, QualityModes, QualityOverrides, is_radeonsi},
};

const BACKEND: Backend = Backend::Vaapi;
const INTRA_REFRESH_FRAMES: &str = "intra-refresh-frames";
const QUALITY_LEVEL: &str = "quality-level";
const PRESET: &str = "preset";
const PRE_ENCODE: &str = "pre-encode";
const VBAQ: &str = "vbaq";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct VaapiParams {
    pub(super) intra_refresh_frames: Option<u32>,
    pub(super) quality_level: Option<u32>,
    pub(super) radeonsi: QualityOverrides,
}

fn invalid(key: &str, value: &str) -> EncodeError {
    EncodeError::InvalidParameterValue {
        backend: BACKEND,
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

fn flag(key: &str, value: &str) -> Result<bool, EncodeError> {
    match value {
        "1" | "true" | "on" => Ok(true),
        "0" | "false" | "off" => Ok(false),
        _ => Err(invalid(key, value)),
    }
}

fn driver_lacks(key: &str) -> EncodeError {
    EncodeError::DriverParameterUnsupported {
        backend: BACKEND,
        key: key.to_owned(),
    }
}

impl VaapiParams {
    pub(super) fn parse(params: &EncoderParams) -> Result<Self, EncodeError> {
        let mut parsed = Self::default();

        for (key, value) in params {
            let number = || value.parse::<u32>().map_err(|_| invalid(key, value));
            match key.as_str() {
                INTRA_REFRESH_FRAMES => parsed.intra_refresh_frames = Some(number()?),
                QUALITY_LEVEL => parsed.quality_level = Some(number()?),
                PRESET => {
                    let preset = Preset::parse(value).ok_or_else(|| invalid(key, value))?;
                    parsed.radeonsi.preset = Some(preset);
                }
                PRE_ENCODE => parsed.radeonsi.pre_encode = Some(flag(key, value)?),
                VBAQ => parsed.radeonsi.vbaq = Some(flag(key, value)?),
                _ => {
                    return Err(EncodeError::UnknownParameter {
                        backend: BACKEND,
                        key: key.clone(),
                    });
                }
            }
        }

        match (parsed.quality_level, parsed.radeonsi_key()) {
            (Some(_), Some(key)) => Err(EncodeError::ConflictingParameters {
                backend: BACKEND,
                first: QUALITY_LEVEL.to_owned(),
                second: key.to_owned(),
            }),
            _ => Ok(parsed),
        }
    }

    fn radeonsi_key(&self) -> Option<&'static str> {
        let QualityOverrides {
            preset,
            pre_encode,
            vbaq,
        } = self.radeonsi;

        [
            (PRESET, preset.is_some()),
            (PRE_ENCODE, pre_encode.is_some()),
            (VBAQ, vbaq.is_some()),
        ]
        .into_iter()
        .find_map(|(key, set)| set.then_some(key))
    }

    pub(super) fn quality_level(
        &self,
        vendor: &str,
        levels: Option<u32>,
    ) -> Result<Option<u32>, EncodeError> {
        let levels = levels.unwrap_or(0);

        if let Some(level) = self.quality_level {
            return if level == 0 || level > levels {
                Err(invalid(QUALITY_LEVEL, &level.to_string()))
            } else {
                Ok(Some(level))
            };
        }
        let Some(key) = self.radeonsi_key() else {
            return Ok(None);
        };

        let level = self
            .radeonsi
            .over(QualityModes::DRIVER_DEFAULT)
            .quality_level();
        if is_radeonsi(vendor) && level.is_none_or(|level| level <= levels) {
            Ok(level)
        } else {
            Err(driver_lacks(key))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RADEONSI: &str = "Mesa Gallium driver 26.2.6-arch3.1 for AMD Radeon RX 9070 XT \
                            (radeonsi, gfx1201, ACO, DRM 3.64, 7.3.3-1-cachyos)";
    const INTEL: &str = "Intel iHD driver for Intel(R) Gen Graphics - 25.3.4";
    const RADEONSI_LEVELS: Option<u32> = Some(32);

    fn params(pairs: &[(&str, &str)]) -> EncoderParams {
        pairs
            .iter()
            .map(|&(key, value)| (key.to_owned(), value.to_owned()))
            .collect()
    }

    fn parsed(pairs: &[(&str, &str)]) -> VaapiParams {
        VaapiParams::parse(&params(pairs)).unwrap()
    }

    #[test]
    fn known_keys_parse() {
        let parsed = parsed(&[("intra-refresh-frames", "30"), ("quality-level", "2")]);

        assert_eq!(parsed.intra_refresh_frames, Some(30));
        assert_eq!(parsed.quality_level, Some(2));
    }

    #[test]
    fn radeonsi_keys_parse() {
        let parsed = parsed(&[
            ("preset", "high-quality"),
            ("pre-encode", "on"),
            ("vbaq", "0"),
        ]);

        assert_eq!(
            parsed.radeonsi,
            QualityOverrides {
                preset: Some(Preset::HighQuality),
                pre_encode: Some(true),
                vbaq: Some(false),
            }
        );
    }

    #[test]
    fn unknown_key_is_named() {
        let error = VaapiParams::parse(&params(&[("no-such-knob", "1")])).unwrap_err();

        assert!(matches!(
            error,
            EncodeError::UnknownParameter { ref key, .. } if key == "no-such-knob"
        ));
    }

    #[test]
    fn bad_value_is_named() {
        for (bad_key, bad_value) in [
            ("quality-level", "fast"),
            ("preset", "fastest"),
            ("vbaq", "maybe"),
            ("pre-encode", "2"),
        ] {
            let error = VaapiParams::parse(&params(&[(bad_key, bad_value)])).unwrap_err();

            assert!(matches!(
                error,
                EncodeError::InvalidParameterValue { ref key, ref value, .. }
                    if key == bad_key && value == bad_value
            ));
        }
    }

    #[test]
    fn the_raw_level_and_a_named_mode_conflict() {
        let error =
            VaapiParams::parse(&params(&[("quality-level", "3"), ("vbaq", "on")])).unwrap_err();

        assert!(matches!(
            error,
            EncodeError::ConflictingParameters { ref first, ref second, .. }
                if first == "quality-level" && second == "vbaq"
        ));
    }

    #[test]
    fn quality_level_is_checked_against_the_driver_range() {
        let parsed = parsed(&[("quality-level", "40")]);

        assert!(parsed.quality_level(RADEONSI, Some(32)).is_err());
        assert!(parsed.quality_level(INTEL, None).is_err());
        assert!(matches!(
            VaapiParams::default().quality_level(INTEL, None),
            Ok(None)
        ));
    }

    #[test]
    fn the_raw_level_passes_through_on_every_driver() {
        for vendor in [RADEONSI, INTEL, ""] {
            for level in [1, 2, 7] {
                let parsed = parsed(&[("quality-level", &level.to_string())]);

                assert!(matches!(
                    parsed.quality_level(vendor, Some(32)),
                    Ok(Some(sent)) if sent == level
                ));
            }
        }
    }

    #[test]
    fn named_modes_become_the_radeonsi_bits() {
        let parsed = parsed(&[("preset", "quality"), ("pre-encode", "on"), ("vbaq", "on")]);

        assert!(matches!(
            parsed.quality_level(RADEONSI, RADEONSI_LEVELS),
            Ok(Some(0x1d))
        ));
    }

    #[test]
    fn named_modes_need_radeonsi() {
        let parsed = parsed(&[("vbaq", "on")]);

        for (vendor, levels) in [(INTEL, Some(7)), ("", None), (RADEONSI, None)] {
            assert!(matches!(
                parsed.quality_level(vendor, levels),
                Err(EncodeError::DriverParameterUnsupported { ref key, .. }) if key == "vbaq"
            ));
        }
    }

    #[test]
    fn without_a_key_every_driver_keeps_its_own_default() {
        for vendor in [RADEONSI, INTEL, ""] {
            assert!(matches!(
                VaapiParams::default().quality_level(vendor, Some(32)),
                Ok(None)
            ));
        }
    }

    #[test]
    fn naming_the_driver_default_sends_no_level() {
        let parsed = parsed(&[("preset", "speed"), ("pre-encode", "off"), ("vbaq", "off")]);

        assert!(matches!(
            parsed.quality_level(RADEONSI, RADEONSI_LEVELS),
            Ok(None)
        ));
    }
}
