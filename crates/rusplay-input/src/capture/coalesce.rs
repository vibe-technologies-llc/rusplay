use crate::{GamepadEvent, InputEvent, TouchEvent};

#[derive(Debug, Default, Clone)]
pub struct EventCoalescer {
    pending: Vec<InputEvent>,
}

impl EventCoalescer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn push(&mut self, event: InputEvent) {
        match (self.pending.last_mut(), event) {
            (
                Some(InputEvent::PointerMotion { dx, dy }),
                InputEvent::PointerMotion {
                    dx: more_x,
                    dy: more_y,
                },
            )
            | (
                Some(InputEvent::Scroll { dx, dy }),
                InputEvent::Scroll {
                    dx: more_x,
                    dy: more_y,
                },
            ) => {
                *dx += more_x;
                *dy += more_y;
            }
            (
                Some(last @ InputEvent::PointerAbsolute { .. }),
                InputEvent::PointerAbsolute { .. },
            ) => {
                *last = event;
            }
            (
                Some(last),
                InputEvent::Touch {
                    slot,
                    event: TouchEvent::Motion { .. },
                },
            ) if matches!(
                *last,
                InputEvent::Touch { slot: pending, event: TouchEvent::Motion { .. } }
                    if pending == slot
            ) =>
            {
                *last = event;
            }
            (
                _,
                InputEvent::Gamepad {
                    slot,
                    event: GamepadEvent::Axis { axis, .. },
                },
            ) => match self.superseded_axis(slot, axis) {
                Some(index) => self.pending[index] = event,
                None => self.pending.push(event),
            },
            _ => self.pending.push(event),
        }
    }

    pub fn drain_into(&mut self, out: &mut Vec<InputEvent>) {
        out.append(&mut self.pending);
    }

    fn superseded_axis(&self, slot: u8, axis: crate::GamepadAxis) -> Option<usize> {
        for (index, pending) in self.pending.iter().enumerate().rev() {
            match pending {
                InputEvent::Gamepad {
                    slot: pending_slot,
                    event:
                        GamepadEvent::Axis {
                            axis: pending_axis, ..
                        },
                } if *pending_slot == slot && *pending_axis == axis => return Some(index),
                InputEvent::Gamepad {
                    slot: pending_slot,
                    event: GamepadEvent::Connected | GamepadEvent::Disconnected,
                } if *pending_slot == slot => return None,
                _ => {}
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GamepadAxis, GamepadButton};

    fn drained(coalescer: &mut EventCoalescer) -> Vec<InputEvent> {
        let mut out = Vec::new();
        coalescer.drain_into(&mut out);
        out
    }

    fn axis(slot: u8, axis: GamepadAxis, value: f32) -> InputEvent {
        InputEvent::Gamepad {
            slot,
            event: GamepadEvent::Axis { axis, value },
        }
    }

    fn key(pressed: bool) -> InputEvent {
        InputEvent::Key {
            keycode: 30,
            pressed,
        }
    }

    #[test]
    fn consecutive_motion_deltas_are_summed() {
        let mut coalescer = EventCoalescer::new();

        coalescer.push(InputEvent::PointerMotion { dx: 1.0, dy: 2.0 });
        coalescer.push(InputEvent::PointerMotion { dx: 0.5, dy: -1.0 });
        coalescer.push(InputEvent::Scroll { dx: 0.0, dy: 1.0 });
        coalescer.push(InputEvent::Scroll { dx: 0.0, dy: 1.0 });

        assert_eq!(
            drained(&mut coalescer),
            vec![
                InputEvent::PointerMotion { dx: 1.5, dy: 1.0 },
                InputEvent::Scroll { dx: 0.0, dy: 2.0 },
            ]
        );
        assert!(coalescer.is_empty());
    }

    #[test]
    fn motion_is_not_merged_across_a_button() {
        let mut coalescer = EventCoalescer::new();
        let click = InputEvent::PointerButton {
            button: 0x110,
            pressed: true,
        };

        coalescer.push(InputEvent::PointerMotion { dx: 1.0, dy: 0.0 });
        coalescer.push(click);
        coalescer.push(InputEvent::PointerMotion { dx: 1.0, dy: 0.0 });

        assert_eq!(coalescer.len(), 3);
        assert_eq!(drained(&mut coalescer)[1], click);
    }

    #[test]
    fn absolute_positions_keep_only_the_latest() {
        let mut coalescer = EventCoalescer::new();

        coalescer.push(InputEvent::PointerAbsolute { x: 0.1, y: 0.1 });
        coalescer.push(InputEvent::PointerAbsolute { x: 0.9, y: 0.8 });

        assert_eq!(
            drained(&mut coalescer),
            vec![InputEvent::PointerAbsolute { x: 0.9, y: 0.8 }]
        );
    }

    #[test]
    fn keys_are_never_merged_or_reordered() {
        let mut coalescer = EventCoalescer::new();

        coalescer.push(key(true));
        coalescer.push(key(false));
        coalescer.push(key(true));

        assert_eq!(
            drained(&mut coalescer),
            vec![key(true), key(false), key(true)]
        );
    }

    #[test]
    fn an_axis_keeps_only_its_latest_value_per_slot() {
        let mut coalescer = EventCoalescer::new();
        let press = InputEvent::Gamepad {
            slot: 0,
            event: GamepadEvent::Button {
                button: GamepadButton::SOUTH,
                pressed: true,
            },
        };

        coalescer.push(axis(0, GamepadAxis::LEFT_X, 0.1));
        coalescer.push(press);
        coalescer.push(axis(0, GamepadAxis::LEFT_Y, 0.3));
        coalescer.push(axis(0, GamepadAxis::LEFT_X, 0.7));
        coalescer.push(axis(1, GamepadAxis::LEFT_X, 0.2));

        assert_eq!(
            drained(&mut coalescer),
            vec![
                axis(0, GamepadAxis::LEFT_X, 0.7),
                press,
                axis(0, GamepadAxis::LEFT_Y, 0.3),
                axis(1, GamepadAxis::LEFT_X, 0.2),
            ]
        );
    }

    #[test]
    fn touch_motion_keeps_the_latest_per_slot_and_never_crosses_other_events() {
        let motion = |slot, x| InputEvent::Touch {
            slot,
            event: TouchEvent::Motion { x, y: 0.0 },
        };
        let up = InputEvent::Touch {
            slot: 0,
            event: TouchEvent::Up,
        };
        let mut coalescer = EventCoalescer::new();

        coalescer.push(motion(0, 0.1));
        coalescer.push(motion(0, 0.2));
        coalescer.push(motion(1, 0.3));
        coalescer.push(motion(0, 0.4));
        coalescer.push(up);
        coalescer.push(motion(0, 0.5));

        assert_eq!(
            drained(&mut coalescer),
            vec![
                motion(0, 0.2),
                motion(1, 0.3),
                motion(0, 0.4),
                up,
                motion(0, 0.5),
            ]
        );
    }

    #[test]
    fn an_axis_is_not_merged_across_a_reconnect() {
        let mut coalescer = EventCoalescer::new();
        let reconnect = InputEvent::Gamepad {
            slot: 0,
            event: GamepadEvent::Connected,
        };

        coalescer.push(axis(0, GamepadAxis::LEFT_X, 0.1));
        coalescer.push(reconnect);
        coalescer.push(axis(0, GamepadAxis::LEFT_X, 0.2));

        assert_eq!(coalescer.len(), 3);
    }
}
