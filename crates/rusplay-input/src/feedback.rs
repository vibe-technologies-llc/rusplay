use crate::RumbleCommand;

pub const TRIGGER_EFFECT_LEN: usize = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadFeedback {
    Rumble(RumbleCommand),
    Lightbar {
        slot: u8,
        red: u8,
        green: u8,
        blue: u8,
    },
    PlayerLeds {
        slot: u8,
        mask: u8,
    },
    TriggerEffect {
        slot: u8,
        right: bool,
        effect: [u8; TRIGGER_EFFECT_LEN],
    },
}

impl PadFeedback {
    pub const fn slot(&self) -> u8 {
        match self {
            Self::Rumble(command) => command.slot,
            Self::Lightbar { slot, .. }
            | Self::PlayerLeds { slot, .. }
            | Self::TriggerEffect { slot, .. } => *slot,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackOutcome {
    Applied,
    Dropped,
}
