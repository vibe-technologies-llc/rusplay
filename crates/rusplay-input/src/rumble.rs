use crate::InputError;

pub const MAX_RUMBLE_EFFECTS: u8 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RumbleCommand {
    pub slot: u8,
    pub strong: u16,
    pub weak: u16,
    pub delay_ms: u16,
    pub duration_ms: u16,
}

impl RumbleCommand {
    pub const UNTIL_REPLACED: u16 = 0;

    pub const fn stop(slot: u8) -> Self {
        Self {
            slot,
            strong: 0,
            weak: 0,
            delay_ms: 0,
            duration_ms: Self::UNTIL_REPLACED,
        }
    }

    pub const fn is_stop(&self) -> bool {
        self.strong == 0 && self.weak == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RumbleEffect {
    pub strong: u16,
    pub weak: u16,
    pub length_ms: u16,
    pub delay_ms: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RumbleTracker {
    slot: u8,
    effects: [Option<RumbleEffect>; MAX_RUMBLE_EFFECTS as usize],
    active: Option<u8>,
}

impl RumbleTracker {
    pub const fn new(slot: u8) -> Self {
        Self {
            slot,
            effects: [None; MAX_RUMBLE_EFFECTS as usize],
            active: None,
        }
    }

    pub fn upload(
        &mut self,
        id: i16,
        effect: RumbleEffect,
    ) -> Result<Option<RumbleCommand>, InputError> {
        let id = self.checked(i64::from(id))?;
        self.effects[usize::from(id)] = Some(effect);

        Ok((self.active == Some(id)).then(|| self.command(effect, 1)))
    }

    pub fn erase(&mut self, id: u32) -> Result<Option<RumbleCommand>, InputError> {
        let id = self.checked(i64::from(id))?;
        self.effects[usize::from(id)] = None;

        Ok(self.deactivate(id))
    }

    pub fn play(&mut self, id: u32, count: i32) -> Result<Option<RumbleCommand>, InputError> {
        let id = self.checked(i64::from(id))?;
        let effect = self.effects[usize::from(id)].ok_or(InputError::RumbleEffectUnknown {
            slot: self.slot,
            id: i64::from(id),
        })?;

        if count <= 0 {
            return Ok(self.deactivate(id));
        }

        self.active = Some(id);

        Ok(Some(self.command(effect, count)))
    }

    fn deactivate(&mut self, id: u8) -> Option<RumbleCommand> {
        if self.active == Some(id) {
            self.active = None;
            Some(RumbleCommand::stop(self.slot))
        } else {
            None
        }
    }

    fn command(&self, effect: RumbleEffect, count: i32) -> RumbleCommand {
        let repeats = u32::try_from(count).unwrap_or(1);
        let duration = u32::from(effect.length_ms)
            .saturating_mul(repeats)
            .min(u32::from(u16::MAX));

        RumbleCommand {
            slot: self.slot,
            strong: effect.strong,
            weak: effect.weak,
            delay_ms: effect.delay_ms,
            duration_ms: u16::try_from(duration).unwrap_or(u16::MAX),
        }
    }

    fn checked(&self, id: i64) -> Result<u8, InputError> {
        u8::try_from(id)
            .ok()
            .filter(|&index| index < MAX_RUMBLE_EFFECTS)
            .ok_or(InputError::RumbleEffectUnknown {
                slot: self.slot,
                id,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effect(strong: u16, weak: u16, length_ms: u16) -> RumbleEffect {
        RumbleEffect {
            strong,
            weak,
            length_ms,
            delay_ms: 5,
        }
    }

    #[test]
    fn play_starts_the_effect_with_its_timing() {
        let mut tracker = RumbleTracker::new(3);
        tracker.upload(0, effect(0xC000, 0x4000, 250)).unwrap();

        let command = tracker.play(0, 1).unwrap().unwrap();

        assert_eq!(
            command,
            RumbleCommand {
                slot: 3,
                strong: 0xC000,
                weak: 0x4000,
                delay_ms: 5,
                duration_ms: 250,
            }
        );
    }

    #[test]
    fn repeat_count_multiplies_the_duration_and_saturates() {
        let mut tracker = RumbleTracker::new(0);
        tracker.upload(0, effect(1, 1, 300)).unwrap();
        tracker.upload(1, effect(1, 1, 0)).unwrap();

        assert_eq!(tracker.play(0, 3).unwrap().unwrap().duration_ms, 900);
        assert_eq!(
            tracker.play(0, i32::MAX).unwrap().unwrap().duration_ms,
            u16::MAX
        );
        assert_eq!(
            tracker.play(1, 4).unwrap().unwrap().duration_ms,
            RumbleCommand::UNTIL_REPLACED
        );
    }

    #[test]
    fn stopping_the_active_effect_emits_a_stop() {
        let mut tracker = RumbleTracker::new(1);
        tracker.upload(2, effect(9, 9, 100)).unwrap();
        tracker.play(2, 1).unwrap();

        let stop = tracker.play(2, 0).unwrap().unwrap();

        assert!(stop.is_stop());
        assert_eq!(stop.slot, 1);
        assert_eq!(tracker.play(2, 0).unwrap(), None);
    }

    #[test]
    fn stopping_an_inactive_effect_is_silent() {
        let mut tracker = RumbleTracker::new(1);
        tracker.upload(0, effect(9, 9, 100)).unwrap();
        tracker.upload(1, effect(1, 1, 100)).unwrap();
        tracker.play(0, 1).unwrap();
        tracker.play(1, 1).unwrap();

        assert_eq!(tracker.play(0, 0).unwrap(), None);
        assert!(tracker.play(1, 0).unwrap().unwrap().is_stop());
    }

    #[test]
    fn erasing_the_active_effect_stops_it() {
        let mut tracker = RumbleTracker::new(1);
        tracker.upload(0, effect(9, 9, 100)).unwrap();
        tracker.play(0, 1).unwrap();

        let stop = tracker.erase(0).unwrap().unwrap();

        assert!(stop.is_stop());
        assert!(matches!(
            tracker.play(0, 1),
            Err(InputError::RumbleEffectUnknown { slot: 1, id: 0 })
        ));
    }

    #[test]
    fn uploading_over_the_active_effect_restarts_it_with_the_new_strength() {
        let mut tracker = RumbleTracker::new(1);
        tracker.upload(4, effect(100, 100, 400)).unwrap();
        tracker.play(4, 1).unwrap();

        let restart = tracker.upload(4, effect(200, 50, 400)).unwrap().unwrap();

        assert_eq!((restart.strong, restart.weak), (200, 50));
        assert_eq!(tracker.upload(5, effect(1, 1, 1)).unwrap(), None);
    }

    #[test]
    fn ids_outside_the_table_are_rejected() {
        let mut tracker = RumbleTracker::new(4);

        assert!(matches!(
            tracker.play(0, 1),
            Err(InputError::RumbleEffectUnknown { slot: 4, id: 0 })
        ));
        assert!(matches!(
            tracker.erase(99),
            Err(InputError::RumbleEffectUnknown { slot: 4, id: 99 })
        ));
        assert!(matches!(
            tracker.upload(16, effect(1, 1, 1)),
            Err(InputError::RumbleEffectUnknown { slot: 4, id: 16 })
        ));
        assert!(matches!(
            tracker.upload(-1, effect(1, 1, 1)),
            Err(InputError::RumbleEffectUnknown { slot: 4, id: -1 })
        ));
    }
}
