use parking_lot::{Condvar, Mutex};

#[derive(Debug)]
pub(crate) struct Closed;

#[derive(Debug)]
struct SlotState<T> {
    value: Option<T>,
    closed: bool,
    nudged: bool,
}

#[derive(Debug)]
pub(crate) enum Taken<T> {
    Value(T),
    Nudged,
    Closed,
}

#[derive(Debug)]
pub(crate) struct Slot<T> {
    state: Mutex<SlotState<T>>,
    filled: Condvar,
}

impl<T> Slot<T> {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(SlotState {
                value: None,
                closed: false,
                nudged: false,
            }),
            filled: Condvar::new(),
        }
    }

    pub(crate) fn put(&self, value: T) -> Result<Option<T>, Closed> {
        let mut state = self.state.lock();
        if state.closed {
            return Err(Closed);
        }

        let replaced = state.value.replace(value);
        drop(state);
        self.filled.notify_one();

        Ok(replaced)
    }

    #[cfg(test)]
    pub(crate) fn take(&self) -> Option<T> {
        loop {
            match self.take_or_nudge() {
                Taken::Value(value) => return Some(value),
                Taken::Nudged => {}
                Taken::Closed => return None,
            }
        }
    }

    pub(crate) fn take_or_nudge(&self) -> Taken<T> {
        let mut state = self.state.lock();

        loop {
            if let Some(value) = state.value.take() {
                return Taken::Value(value);
            }
            if state.closed {
                return Taken::Closed;
            }
            if state.nudged {
                state.nudged = false;
                return Taken::Nudged;
            }
            self.filled.wait(&mut state);
        }
    }

    pub(crate) fn nudge(&self) {
        let mut state = self.state.lock();
        state.nudged = true;
        drop(state);
        self.filled.notify_one();
    }

    pub(crate) fn close(&self) -> Option<T> {
        let mut state = self.state.lock();
        state.closed = true;
        let pending = state.value.take();
        drop(state);
        self.filled.notify_all();

        pending
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread};

    use super::*;

    #[test]
    fn newest_value_replaces_the_unread_one() {
        let slot = Slot::new();

        let first = slot.put(1).unwrap();
        let second = slot.put(2).unwrap();

        assert_eq!(first, None);
        assert_eq!(second, Some(1));
        assert_eq!(slot.take(), Some(2));
    }

    #[test]
    fn a_nudge_wakes_a_waiting_reader_once_and_a_value_comes_first() {
        let slot = Arc::new(Slot::<u32>::new());
        let reader = {
            let slot = Arc::clone(&slot);
            thread::spawn(move || matches!(slot.take_or_nudge(), Taken::Nudged))
        };

        slot.nudge();
        assert!(reader.join().unwrap());

        slot.nudge();
        slot.put(7).unwrap();
        assert!(matches!(slot.take_or_nudge(), Taken::Value(7)));
        assert!(matches!(slot.take_or_nudge(), Taken::Nudged));

        slot.close();
        assert!(matches!(slot.take_or_nudge(), Taken::Closed));
    }

    #[test]
    fn closing_wakes_a_waiting_reader_and_refuses_writers() {
        let slot = Arc::new(Slot::<u32>::new());
        let reader = {
            let slot = Arc::clone(&slot);
            thread::spawn(move || slot.take())
        };

        slot.close();

        assert_eq!(reader.join().unwrap(), None);
        assert!(slot.put(3).is_err());
    }
}
