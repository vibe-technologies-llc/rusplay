use crate::{InputError, InputEvent, InputSink};

#[derive(Debug)]
pub struct SplitSink<D, G> {
    pub desktop: D,
    pub gamepads: G,
}

impl<D, G> SplitSink<D, G> {
    pub fn new(desktop: D, gamepads: G) -> Self {
        Self { desktop, gamepads }
    }
}

impl<D: InputSink, G: InputSink> InputSink for SplitSink<D, G> {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        match event {
            InputEvent::Gamepad { .. } => self.gamepads.inject(event),
            _ => self.desktop.inject(event),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GamepadEvent;

    #[derive(Default)]
    struct Recorder(Vec<InputEvent>);

    impl InputSink for Recorder {
        fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
            self.0.push(event);
            Ok(())
        }
    }

    #[test]
    fn gamepad_events_go_to_the_gamepad_sink_and_the_rest_to_the_desktop() {
        let mut sink = SplitSink::new(Recorder::default(), Recorder::default());
        let key = InputEvent::Key {
            keycode: 30,
            pressed: true,
        };
        let pad = InputEvent::Gamepad {
            slot: 0,
            event: GamepadEvent::Connected,
        };

        sink.inject(key).unwrap();
        sink.inject(pad).unwrap();
        sink.inject(InputEvent::Scroll { dx: 0.0, dy: 1.0 })
            .unwrap();

        assert_eq!(sink.gamepads.0, vec![pad]);
        assert_eq!(sink.desktop.0.len(), 2);
        assert_eq!(sink.desktop.0[0], key);
    }
}
