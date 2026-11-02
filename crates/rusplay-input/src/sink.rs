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

    fn inject_batch(&mut self, events: &[InputEvent], results: &mut Vec<Result<(), InputError>>) {
        let is_pad = |event: &InputEvent| matches!(event, InputEvent::Gamepad { .. });
        let pads: Vec<InputEvent> = events.iter().copied().filter(is_pad).collect();
        let desktop: Vec<InputEvent> = events.iter().copied().filter(|e| !is_pad(e)).collect();
        let mut pad_results = Vec::new();
        let mut desktop_results = Vec::new();

        self.gamepads.inject_batch(&pads, &mut pad_results);
        self.desktop.inject_batch(&desktop, &mut desktop_results);

        let mut pad_results = pad_results.into_iter();
        let mut desktop_results = desktop_results.into_iter();
        results.clear();
        results.extend(events.iter().map(|event| {
            let next = if is_pad(event) {
                pad_results.next()
            } else {
                desktop_results.next()
            };

            next.unwrap_or(Err(InputError::SinkClosed))
        }));
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

    struct RejectsKeys;

    impl InputSink for RejectsKeys {
        fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
            match event {
                InputEvent::Key { .. } => Err(InputError::SinkClosed),
                _ => Ok(()),
            }
        }
    }

    #[test]
    fn a_batch_is_split_between_the_sinks_and_keeps_one_result_per_event() {
        let mut sink = SplitSink::new(RejectsKeys, Recorder::default());
        let key = InputEvent::Key {
            keycode: 30,
            pressed: true,
        };
        let pad = InputEvent::Gamepad {
            slot: 0,
            event: GamepadEvent::Connected,
        };
        let scroll = InputEvent::Scroll { dx: 0.0, dy: 1.0 };
        let mut results = Vec::new();

        sink.inject_batch(&[key, pad, scroll], &mut results);

        assert_eq!(
            results.iter().map(Result::is_ok).collect::<Vec<_>>(),
            vec![false, true, true]
        );
        assert_eq!(sink.gamepads.0, vec![pad]);
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
