use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PointerMode {
    #[default]
    Absolute,
    Relative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InputConfig {
    pub keyboard: bool,
    pub pointer: bool,
    pub gamepads: bool,
    pub pointer_mode: PointerMode,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            keyboard: true,
            pointer: true,
            gamepads: true,
            pointer_mode: PointerMode::default(),
        }
    }
}
