use crate::{
    GamepadEvent, InputError, InputEvent, InputSink, RumbleCommand,
    gamepad::xbox360::{Report, Xbox360Mapper},
};

pub trait GamepadDevice {
    fn emit(&mut self, report: Report) -> Result<(), InputError>;

    fn poll_rumble(&mut self, _out: &mut Vec<RumbleCommand>) -> Result<(), InputError> {
        Ok(())
    }
}

pub trait GamepadFactory {
    type Device: GamepadDevice;

    fn create(&mut self, slot: u8) -> Result<Self::Device, InputError>;
}

struct Slot<D> {
    device: D,
    mapper: Xbox360Mapper,
}

pub struct GamepadPool<F: GamepadFactory> {
    factory: F,
    slots: Vec<Option<Slot<F::Device>>>,
}

impl<F: GamepadFactory> GamepadPool<F> {
    pub fn new(factory: F, max_slots: u8) -> Self {
        Self {
            factory,
            slots: (0..max_slots).map(|_| None).collect(),
        }
    }

    pub fn max_slots(&self) -> u8 {
        u8::try_from(self.slots.len()).unwrap_or(u8::MAX)
    }

    pub fn is_connected(&self, slot: u8) -> bool {
        self.slots
            .get(usize::from(slot))
            .is_some_and(Option::is_some)
    }

    pub fn poll_rumble(&mut self, out: &mut Vec<RumbleCommand>) -> Result<(), InputError> {
        self.slots
            .iter_mut()
            .flatten()
            .try_for_each(|connected| connected.device.poll_rumble(out))
    }

    pub fn handle(&mut self, slot: u8, event: GamepadEvent) -> Result<(), InputError> {
        let entry = self
            .slots
            .get_mut(usize::from(slot))
            .ok_or(InputError::GamepadSlotOutOfRange(slot))?;

        match event {
            GamepadEvent::Connected => {
                *entry = None;
                *entry = Some(Slot {
                    device: self.factory.create(slot)?,
                    mapper: Xbox360Mapper::default(),
                });
                Ok(())
            }
            GamepadEvent::Disconnected => {
                *entry = None;
                Ok(())
            }
            other => {
                let connected = entry
                    .as_mut()
                    .ok_or(InputError::GamepadNotConnected(slot))?;
                match connected.mapper.map(other)? {
                    Some(report) => connected.device.emit(report),
                    None => Ok(()),
                }
            }
        }
    }
}

impl<F: GamepadFactory> InputSink for GamepadPool<F> {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        match event {
            InputEvent::Gamepad { slot, event } => self.handle(slot, event),
            _ => Err(InputError::NonGamepadEvent),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::*;
    use crate::{GamepadAxis, GamepadButton};

    type Log = Rc<RefCell<Vec<(u8, Report)>>>;

    struct FakeDevice {
        slot: u8,
        log: Log,
    }

    impl GamepadDevice for FakeDevice {
        fn emit(&mut self, report: Report) -> Result<(), InputError> {
            self.log.borrow_mut().push((self.slot, report));
            Ok(())
        }

        fn poll_rumble(&mut self, out: &mut Vec<RumbleCommand>) -> Result<(), InputError> {
            out.push(RumbleCommand {
                strong: 1,
                ..RumbleCommand::stop(self.slot)
            });
            Ok(())
        }
    }

    struct FakeFactory {
        log: Log,
        created: Vec<u8>,
    }

    impl GamepadFactory for FakeFactory {
        type Device = FakeDevice;

        fn create(&mut self, slot: u8) -> Result<FakeDevice, InputError> {
            self.created.push(slot);
            Ok(FakeDevice {
                slot,
                log: Rc::clone(&self.log),
            })
        }
    }

    fn pool(max_slots: u8) -> (GamepadPool<FakeFactory>, Log) {
        let log = Log::default();
        let factory = FakeFactory {
            log: Rc::clone(&log),
            created: Vec::new(),
        };

        (GamepadPool::new(factory, max_slots), log)
    }

    fn button(pressed: bool) -> GamepadEvent {
        GamepadEvent::Button {
            button: GamepadButton::SOUTH,
            pressed,
        }
    }

    #[test]
    fn events_before_connect_are_rejected() {
        let (mut pool, _) = pool(4);

        let result = pool.handle(1, button(true));

        assert!(matches!(result, Err(InputError::GamepadNotConnected(1))));
    }

    #[test]
    fn slots_beyond_the_configured_maximum_are_rejected() {
        let (mut pool, _) = pool(2);

        let connect = pool.handle(2, GamepadEvent::Connected);
        let press = pool.handle(200, button(true));

        assert!(matches!(connect, Err(InputError::GamepadSlotOutOfRange(2))));
        assert!(matches!(press, Err(InputError::GamepadSlotOutOfRange(200))));
        assert_eq!(pool.max_slots(), 2);
    }

    #[test]
    fn connected_slot_forwards_reports_to_its_own_device() {
        let (mut pool, log) = pool(4);

        pool.handle(0, GamepadEvent::Connected).unwrap();
        pool.handle(3, GamepadEvent::Connected).unwrap();
        pool.handle(3, button(true)).unwrap();
        pool.handle(
            0,
            GamepadEvent::Axis {
                axis: GamepadAxis::LEFT_X,
                value: 1.0,
            },
        )
        .unwrap();

        let log = log.borrow();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].0, 3);
        assert_eq!(log[1].0, 0);
        assert!(pool.is_connected(0) && pool.is_connected(3));
        assert!(!pool.is_connected(1));
    }

    #[test]
    fn disconnect_drops_the_device_and_later_events_fail() {
        let (mut pool, _) = pool(4);
        pool.handle(2, GamepadEvent::Connected).unwrap();

        pool.handle(2, GamepadEvent::Disconnected).unwrap();
        let result = pool.handle(2, button(true));

        assert!(!pool.is_connected(2));
        assert!(matches!(result, Err(InputError::GamepadNotConnected(2))));
    }

    #[test]
    fn disconnecting_an_empty_slot_is_fine() {
        let (mut pool, _) = pool(4);

        assert!(pool.handle(1, GamepadEvent::Disconnected).is_ok());
    }

    #[test]
    fn reconnecting_creates_a_fresh_device() {
        let (mut pool, _) = pool(4);

        pool.handle(0, GamepadEvent::Connected).unwrap();
        pool.handle(0, GamepadEvent::Connected).unwrap();

        assert_eq!(pool.factory.created, vec![0, 0]);
    }

    #[test]
    fn rumble_is_collected_from_connected_slots_only() {
        let (mut pool, _) = pool(4);
        pool.handle(1, GamepadEvent::Connected).unwrap();
        pool.handle(3, GamepadEvent::Connected).unwrap();
        pool.handle(1, GamepadEvent::Disconnected).unwrap();
        let mut out = Vec::new();

        pool.poll_rumble(&mut out).unwrap();

        assert_eq!(
            out.iter().map(|command| command.slot).collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[test]
    fn sink_routes_gamepad_events_and_refuses_the_rest() {
        let (mut pool, log) = pool(4);

        pool.inject(InputEvent::Gamepad {
            slot: 0,
            event: GamepadEvent::Connected,
        })
        .unwrap();
        pool.inject(InputEvent::Gamepad {
            slot: 0,
            event: button(true),
        })
        .unwrap();
        let other = pool.inject(InputEvent::Key {
            keycode: 30,
            pressed: true,
        });

        assert_eq!(log.borrow().len(), 1);
        assert!(matches!(other, Err(InputError::NonGamepadEvent)));
    }
}
