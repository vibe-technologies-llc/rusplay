use rusplay_config::EncoderParams;

use crate::{Backend, EncodeError};

const BACKEND: Backend = Backend::Vaapi;
const INTRA_REFRESH_FRAMES: &str = "intra-refresh-frames";
const QUALITY_LEVEL: &str = "quality-level";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct VaapiParams {
    pub(super) intra_refresh_frames: Option<u32>,
    pub(super) quality_level: Option<u32>,
}

fn invalid(key: &str, value: &str) -> EncodeError {
    EncodeError::InvalidParameterValue {
        backend: BACKEND,
        key: key.to_owned(),
        value: value.to_owned(),
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

    pub(super) fn check_quality_level(&self, levels: Option<u32>) -> Result<(), EncodeError> {
        match self.quality_level {
            Some(level) if level == 0 || level > levels.unwrap_or(0) => {
                Err(invalid(QUALITY_LEVEL, &level.to_string()))
            }
            _ => Ok(()),
        }
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
        let parsed = VaapiParams::parse(&params(&[
            ("intra-refresh-frames", "30"),
            ("quality-level", "2"),
        ]))
        .unwrap();

        assert_eq!(parsed.intra_refresh_frames, Some(30));
        assert_eq!(parsed.quality_level, Some(2));
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
        let error = VaapiParams::parse(&params(&[("quality-level", "fast")])).unwrap_err();

        assert!(matches!(
            error,
            EncodeError::InvalidParameterValue { ref key, ref value, .. }
                if key == "quality-level" && value == "fast"
        ));
    }

    #[test]
    fn quality_level_is_checked_against_the_driver_range() {
        let parsed = VaapiParams::parse(&params(&[("quality-level", "40")])).unwrap();

        assert!(parsed.check_quality_level(Some(32)).is_err());
        assert!(parsed.check_quality_level(None).is_err());
        assert!(VaapiParams::default().check_quality_level(None).is_ok());
    }
}
