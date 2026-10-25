use std::{sync::Arc, time::Duration};

use parking_lot::{Condvar, Mutex};

#[derive(Debug, PartialEq, Eq)]
pub enum Received<T> {
    Value(T),
    Empty,
    Closed,
}

#[derive(Debug)]
struct State<T> {
    pending: Option<T>,
    closed: bool,
    superseded: u64,
}

#[derive(Debug)]
struct Shared<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
}

#[derive(Debug)]
pub struct Publisher<T> {
    shared: Arc<Shared<T>>,
}

#[derive(Debug)]
pub struct Subscriber<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for Subscriber<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

pub fn latest<T>() -> (Publisher<T>, Subscriber<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            pending: None,
            closed: false,
            superseded: 0,
        }),
        ready: Condvar::new(),
    });

    (
        Publisher {
            shared: Arc::clone(&shared),
        },
        Subscriber { shared },
    )
}

impl<T> Publisher<T> {
    pub fn replace(&self, value: T) -> Option<T> {
        let stale = {
            let mut guard = self.shared.state.lock();
            let stale = guard.pending.replace(value);
            if stale.is_some() {
                guard.superseded += 1;
            }
            stale
        };

        self.shared.ready.notify_all();
        stale
    }

    pub fn merge(&self, value: T, combine: impl FnOnce(T, T) -> T) {
        {
            let mut guard = self.shared.state.lock();
            let merged = match guard.pending.take() {
                Some(stale) => {
                    guard.superseded += 1;
                    combine(value, stale)
                }
                None => value,
            };
            guard.pending = Some(merged);
        }

        self.shared.ready.notify_all();
    }

    pub fn close(&self) {
        self.shared.state.lock().closed = true;
        self.shared.ready.notify_all();
    }
}

impl<T> Drop for Publisher<T> {
    fn drop(&mut self) {
        self.close();
    }
}

impl<T> Subscriber<T> {
    pub fn recv(&self) -> Option<T> {
        let mut state = self.shared.state.lock();
        loop {
            if let Some(value) = state.pending.take() {
                return Some(value);
            }
            if state.closed {
                return None;
            }
            self.shared.ready.wait(&mut state);
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Received<T> {
        let mut state = self.shared.state.lock();
        let _ = self.shared.ready.wait_while_for(
            &mut state,
            |state| state.pending.is_none() && !state.closed,
            timeout,
        );

        Self::take(&mut state)
    }

    pub fn try_recv(&self) -> Received<T> {
        Self::take(&mut self.shared.state.lock())
    }

    pub fn superseded(&self) -> u64 {
        self.shared.state.lock().superseded
    }

    fn take(state: &mut State<T>) -> Received<T> {
        match state.pending.take() {
            Some(value) => Received::Value(value),
            None if state.closed => Received::Closed,
            None => Received::Empty,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn newer_value_replaces_unconsumed_one() {
        let (publisher, subscriber) = latest();

        let first = publisher.replace(1);
        let second = publisher.replace(2);

        assert_eq!(first, None);
        assert_eq!(second, Some(1));
        assert_eq!(subscriber.try_recv(), Received::Value(2));
        assert_eq!(subscriber.try_recv(), Received::Empty);
        assert_eq!(subscriber.superseded(), 1);
    }

    #[test]
    fn merge_combines_fresh_with_stale() {
        let (publisher, subscriber) = latest();

        publisher.merge(vec![1], |mut fresh, stale| {
            fresh.extend(stale);
            fresh
        });
        publisher.merge(vec![2], |mut fresh, stale| {
            fresh.extend(stale);
            fresh
        });

        assert_eq!(subscriber.try_recv(), Received::Value(vec![2, 1]));
        assert_eq!(subscriber.superseded(), 1);
    }

    #[test]
    fn pending_value_survives_close() {
        let (publisher, subscriber) = latest();

        publisher.replace(7);
        drop(publisher);

        assert_eq!(subscriber.recv(), Some(7));
        assert_eq!(subscriber.recv(), None);
        assert_eq!(subscriber.try_recv(), Received::Closed);
    }

    #[test]
    fn recv_blocks_until_a_value_arrives() {
        let (publisher, subscriber) = latest();

        let producer = thread::spawn(move || {
            publisher.replace(42);
        });

        assert_eq!(subscriber.recv(), Some(42));
        producer.join().unwrap();
    }

    #[test]
    fn recv_timeout_reports_empty() {
        let (_publisher, subscriber) = latest::<u8>();

        assert_eq!(
            subscriber.recv_timeout(Duration::from_millis(1)),
            Received::Empty
        );
    }
}
