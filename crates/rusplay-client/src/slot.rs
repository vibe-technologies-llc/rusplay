use parking_lot::{Condvar, Mutex};

#[derive(Debug)]
pub(crate) struct Closed;

#[derive(Debug)]
struct SlotState<T> {
    value: Option<T>,
    closed: bool,
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

    pub(crate) fn take(&self) -> Option<T> {
        let mut state = self.state.lock();

        loop {
            if let Some(value) = state.value.take() {
                return Some(value);
            }
            if state.closed {
                return None;
            }
            self.filled.wait(&mut state);
        }
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
