use std::mem::discriminant;

use rusplay_config::{
    Config, HostProfile, NvdecDecoderConfig, PointerMode, PresentMode, SoftwareDecoderConfig,
    VaapiDecoderConfig, VideoDecoderConfig,
};

const PRESENT_MODES: [(PresentMode, &str); 3] = [
    (PresentMode::Mailbox, "Mailbox"),
    (PresentMode::Immediate, "Immediate"),
    (PresentMode::Fifo, "Fifo"),
];
const FRAME_LATENCIES: [(u8, &str); 3] = [(1, "1"), (2, "2"), (3, "3")];
const SWITCH: [(bool, &str); 2] = [(true, "On"), (false, "Off")];
const POINTER_MODES: [(PointerMode, &str); 3] = [
    (PointerMode::Absolute, "Absolute"),
    (PointerMode::Relative, "Relative"),
    (PointerMode::Auto, "Auto"),
];
const UNLISTED_VALUE: &str = "custom";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Global,
    Override,
}

impl Source {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Override => "override",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileChoice {
    pub label: &'static str,
    pub selected: bool,
    pub result: HostProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRow {
    pub label: &'static str,
    pub source: Source,
    pub effective: &'static str,
    pub choices: Vec<ProfileChoice>,
}

pub fn rows(profile: &HostProfile, global: &Config) -> Vec<ProfileRow> {
    vec![
        decoder_row(profile, global),
        row(
            "Present mode",
            profile,
            global.video.present_mode,
            &PRESENT_MODES,
            |profile| &mut profile.present_mode,
            |profile| profile.present_mode,
        ),
        row(
            "Frames in flight",
            profile,
            global.video.present_frame_latency,
            &FRAME_LATENCIES,
            |profile| &mut profile.present_frame_latency,
            |profile| profile.present_frame_latency,
        ),
        row(
            "Keyboard",
            profile,
            global.input.keyboard,
            &SWITCH,
            |profile| &mut profile.keyboard,
            |profile| profile.keyboard,
        ),
        row(
            "Pointer",
            profile,
            global.input.pointer,
            &SWITCH,
            |profile| &mut profile.pointer,
            |profile| profile.pointer,
        ),
        row(
            "Gamepads",
            profile,
            global.input.gamepads,
            &SWITCH,
            |profile| &mut profile.gamepads,
            |profile| profile.gamepads,
        ),
        row(
            "Pointer mode",
            profile,
            global.input.pointer_mode,
            &POINTER_MODES,
            |profile| &mut profile.pointer_mode,
            |profile| profile.pointer_mode,
        ),
    ]
}

fn row<T: Copy + PartialEq>(
    label: &'static str,
    profile: &HostProfile,
    global: T,
    values: &[(T, &'static str)],
    field: fn(&mut HostProfile) -> &mut Option<T>,
    current: fn(&HostProfile) -> Option<T>,
) -> ProfileRow {
    let overridden = current(profile);
    let effective = overridden.unwrap_or(global);

    let mut choices = vec![ProfileChoice {
        label: "Global",
        selected: overridden.is_none(),
        result: with_field(profile, field, None),
    }];
    choices.extend(values.iter().map(|&(value, name)| ProfileChoice {
        label: name,
        selected: overridden == Some(value),
        result: with_field(profile, field, Some(value)),
    }));

    ProfileRow {
        label,
        source: source_of(overridden.is_some()),
        effective: label_of(values, &effective),
        choices,
    }
}

fn decoder_row(profile: &HostProfile, global: &Config) -> ProfileRow {
    let global_decoder = &global.video.decoder;
    let effective = profile.decoder.as_ref().unwrap_or(global_decoder);

    let mut choices = vec![ProfileChoice {
        label: "Global",
        selected: profile.decoder.is_none(),
        result: HostProfile {
            decoder: None,
            ..profile.clone()
        },
    }];
    choices.extend(
        decoder_backends(global_decoder)
            .into_iter()
            .map(|(backend, name)| ProfileChoice {
                label: name,
                selected: profile
                    .decoder
                    .as_ref()
                    .is_some_and(|set| same_backend(set, &backend)),
                result: HostProfile {
                    decoder: Some(backend),
                    ..profile.clone()
                },
            }),
    );

    ProfileRow {
        label: "Decoder",
        source: source_of(profile.decoder.is_some()),
        effective: backend_name(effective),
        choices,
    }
}

fn decoder_backends(global: &VideoDecoderConfig) -> [(VideoDecoderConfig, &'static str); 3] {
    [
        VideoDecoderConfig::Vaapi(VaapiDecoderConfig::default()),
        VideoDecoderConfig::Nvdec(NvdecDecoderConfig::default()),
        VideoDecoderConfig::Software(SoftwareDecoderConfig::default()),
    ]
    .map(|backend| {
        let name = backend_name(&backend);
        if same_backend(&backend, global) {
            (global.clone(), name)
        } else {
            (backend, name)
        }
    })
}

fn same_backend(left: &VideoDecoderConfig, right: &VideoDecoderConfig) -> bool {
    discriminant(left) == discriminant(right)
}

const fn backend_name(decoder: &VideoDecoderConfig) -> &'static str {
    match decoder {
        VideoDecoderConfig::Vaapi(_) => "VAAPI",
        VideoDecoderConfig::Nvdec(_) => "NVDEC",
        VideoDecoderConfig::Software(_) => "Software",
    }
}

fn with_field<T>(
    profile: &HostProfile,
    field: fn(&mut HostProfile) -> &mut Option<T>,
    value: Option<T>,
) -> HostProfile {
    let mut result = profile.clone();
    *field(&mut result) = value;

    result
}

fn label_of<T: PartialEq>(values: &[(T, &'static str)], value: &T) -> &'static str {
    values
        .iter()
        .find(|(candidate, _)| candidate == value)
        .map_or(UNLISTED_VALUE, |(_, name)| name)
}

const fn source_of(overridden: bool) -> Source {
    if overridden {
        Source::Override
    } else {
        Source::Global
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn find<'a>(rows: &'a [ProfileRow], label: &str) -> &'a ProfileRow {
        rows.iter().find(|row| row.label == label).unwrap()
    }

    fn pick<'a>(row: &'a ProfileRow, label: &str) -> &'a ProfileChoice {
        row.choices
            .iter()
            .find(|choice| choice.label == label)
            .unwrap()
    }

    #[test]
    fn an_empty_profile_shows_every_row_as_global() {
        let global = Config::default();

        let rows = rows(&HostProfile::default(), &global);

        assert_eq!(rows.len(), 7);
        assert!(rows.iter().all(|row| row.source == Source::Global));
        assert!(rows.iter().all(|row| row.choices[0].selected));
        assert_eq!(find(&rows, "Present mode").effective, "Mailbox");
        assert_eq!(find(&rows, "Keyboard").effective, "On");
        assert_eq!(find(&rows, "Decoder").effective, "VAAPI");
    }

    #[test]
    fn an_override_is_in_effect_over_the_global_value() {
        let mut global = Config::default();
        global.video.present_mode = PresentMode::Immediate;
        let profile = HostProfile {
            present_mode: Some(PresentMode::Fifo),
            gamepads: Some(false),
            ..HostProfile::default()
        };

        let rows = rows(&profile, &global);

        let present = find(&rows, "Present mode");
        assert_eq!(present.source, Source::Override);
        assert_eq!(present.effective, "Fifo");
        assert!(pick(present, "Fifo").selected);
        assert!(!pick(present, "Global").selected);
        assert_eq!(find(&rows, "Gamepads").effective, "Off");
        assert_eq!(find(&rows, "Keyboard").source, Source::Global);
    }

    #[test]
    fn global_follows_the_global_config_when_it_changes() {
        let mut global = Config::default();
        global.input.keyboard = false;

        let rows = rows(&HostProfile::default(), &global);

        assert_eq!(find(&rows, "Keyboard").effective, "Off");
    }

    #[test]
    fn choosing_a_value_sets_only_its_field_and_global_clears_it() {
        let global = Config::default();
        let profile = HostProfile {
            pointer: Some(false),
            ..HostProfile::default()
        };
        let rows = rows(&profile, &global);

        let chosen = pick(find(&rows, "Present mode"), "Immediate")
            .result
            .clone();
        let cleared = pick(find(&rows, "Pointer"), "Global").result.clone();

        assert_eq!(chosen.present_mode, Some(PresentMode::Immediate));
        assert_eq!(chosen.pointer, Some(false));
        assert!(cleared.is_empty());
    }

    #[test]
    fn every_choice_merges_into_a_valid_config() {
        let global = Config::default();

        for row in rows(&HostProfile::default(), &global) {
            for choice in row.choices {
                assert!(choice.result.apply(&global).is_ok(), "{}", choice.label);
            }
        }
    }

    #[test]
    fn choosing_the_global_decoder_backend_keeps_its_settings() {
        let mut global = Config::default();
        global.video.decoder = VideoDecoderConfig::Vaapi(VaapiDecoderConfig {
            device: PathBuf::from("/dev/dri/renderD129"),
        });

        let rows = rows(&HostProfile::default(), &global);
        let decoder = find(&rows, "Decoder");

        assert_eq!(
            pick(decoder, "VAAPI").result.decoder,
            Some(global.video.decoder.clone())
        );
        assert_eq!(
            pick(decoder, "Software").result.decoder,
            Some(VideoDecoderConfig::Software(
                SoftwareDecoderConfig::default()
            ))
        );
    }

    #[test]
    fn a_decoder_override_is_marked_and_in_effect() {
        let global = Config::default();
        let profile = HostProfile {
            decoder: Some(VideoDecoderConfig::Software(
                SoftwareDecoderConfig::default(),
            )),
            ..HostProfile::default()
        };

        let rows = rows(&profile, &global);
        let decoder = find(&rows, "Decoder");

        assert_eq!(decoder.source, Source::Override);
        assert_eq!(decoder.effective, "Software");
        assert!(pick(decoder, "Software").selected);
        assert!(!pick(decoder, "VAAPI").selected);
    }
}
