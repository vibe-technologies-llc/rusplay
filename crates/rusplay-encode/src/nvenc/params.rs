use rusplay_config::EncoderParams;

use crate::{Backend, EncodeError, nvenc::ffi};

const BACKEND: Backend = Backend::Nvenc;
const INTRA_REFRESH_FRAMES: &str = "intra-refresh-frames";
const SPATIAL_AQ: &str = "spatial-aq";
const AQ_STRENGTH: &str = "aq-strength";
const TEMPORAL_AQ: &str = "temporal-aq";
const MULTIPASS: &str = "multipass";
const SPLIT_ENCODE: &str = "split-encode";
const AQ_STRENGTH_RANGE: std::ops::RangeInclusive<u32> = 1..=15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Multipass {
    Disabled,
    QuarterResolution,
    FullResolution,
}

impl Multipass {
    pub(super) const fn nvenc(self) -> u32 {
        match self {
            Self::Disabled => ffi::MULTI_PASS_DISABLED,
            Self::QuarterResolution => ffi::TWO_PASS_QUARTER_RESOLUTION,
            Self::FullResolution => ffi::TWO_PASS_FULL_RESOLUTION,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SplitEncode {
    Auto,
    AutoForced,
    Two,
    Three,
    Disabled,
}

impl SplitEncode {
    pub(super) const fn nvenc(self) -> u32 {
        match self {
            Self::Auto => ffi::SPLIT_AUTO_MODE,
            Self::AutoForced => ffi::SPLIT_AUTO_FORCED_MODE,
            Self::Two => ffi::SPLIT_TWO_FORCED_MODE,
            Self::Three => ffi::SPLIT_THREE_FORCED_MODE,
            Self::Disabled => ffi::SPLIT_DISABLE_MODE,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct NvencParams {
    pub(super) intra_refresh_frames: Option<u32>,
    pub(super) spatial_aq: Option<bool>,
    pub(super) aq_strength: Option<u32>,
    pub(super) temporal_aq: Option<bool>,
    pub(super) multipass: Option<Multipass>,
    pub(super) split_encode: Option<SplitEncode>,
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

impl NvencParams {
    pub(super) fn parse(params: &EncoderParams) -> Result<Self, EncodeError> {
        let mut parsed = Self::default();

        for (key, value) in params {
            let number = || value.parse::<u32>().map_err(|_| invalid(key, value));
            match key.as_str() {
                INTRA_REFRESH_FRAMES => parsed.intra_refresh_frames = Some(number()?),
                SPATIAL_AQ => parsed.spatial_aq = Some(flag(key, value)?),
                AQ_STRENGTH => {
                    let strength = number()?;
                    if !AQ_STRENGTH_RANGE.contains(&strength) {
                        return Err(invalid(key, value));
                    }
                    parsed.aq_strength = Some(strength);
                }
                TEMPORAL_AQ => parsed.temporal_aq = Some(flag(key, value)?),
                MULTIPASS => {
                    parsed.multipass = Some(match value.as_str() {
                        "disabled" => Multipass::Disabled,
                        "quarter-resolution" => Multipass::QuarterResolution,
                        "full-resolution" => Multipass::FullResolution,
                        _ => return Err(invalid(key, value)),
                    });
                }
                SPLIT_ENCODE => {
                    parsed.split_encode = Some(match value.as_str() {
                        "auto" => SplitEncode::Auto,
                        "auto-forced" => SplitEncode::AutoForced,
                        "two" => SplitEncode::Two,
                        "three" => SplitEncode::Three,
                        "disabled" => SplitEncode::Disabled,
                        _ => return Err(invalid(key, value)),
                    });
                }
                _ => {
                    return Err(EncodeError::UnknownParameter {
                        backend: BACKEND,
                        key: key.clone(),
                    });
                }
            }
        }

        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> EncoderParams {
        pairs
            .iter()
            .map(|&(key, value)| (key.to_owned(), value.to_owned()))
            .collect()
    }

    #[test]
    fn known_keys_parse() {
        let parsed = NvencParams::parse(&params(&[
            ("intra-refresh-frames", "30"),
            ("spatial-aq", "true"),
            ("aq-strength", "8"),
            ("temporal-aq", "0"),
            ("multipass", "quarter-resolution"),
            ("split-encode", "two"),
        ]))
        .unwrap();

        assert_eq!(
            parsed,
            NvencParams {
                intra_refresh_frames: Some(30),
                spatial_aq: Some(true),
                aq_strength: Some(8),
                temporal_aq: Some(false),
                multipass: Some(Multipass::QuarterResolution),
                split_encode: Some(SplitEncode::Two),
            }
        );
    }

    #[test]
    fn unknown_key_is_named() {
        let error = NvencParams::parse(&params(&[("lookahead", "8")])).unwrap_err();

        assert!(matches!(
            error,
            EncodeError::UnknownParameter { ref key, .. } if key == "lookahead"
        ));
    }

    #[test]
    fn bad_values_are_named() {
        for (key, value) in [
            ("aq-strength", "16"),
            ("aq-strength", "0"),
            ("spatial-aq", "maybe"),
            ("multipass", "half"),
            ("split-encode", "four"),
            ("intra-refresh-frames", "-1"),
        ] {
            let error = NvencParams::parse(&params(&[(key, value)])).unwrap_err();

            assert!(
                matches!(
                    error,
                    EncodeError::InvalidParameterValue { key: ref k, value: ref v, .. }
                        if k == key && v == value
                ),
                "{key}={value}"
            );
        }
    }

    #[test]
    fn modes_map_to_the_sdk_values() {
        assert_eq!(Multipass::FullResolution.nvenc(), 2);
        assert_eq!(SplitEncode::Disabled.nvenc(), 15);
        assert_eq!(SplitEncode::Three.nvenc(), 3);
    }
}
