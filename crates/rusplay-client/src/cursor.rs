use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::{Condvar, Mutex};
use rusplay_protocol::{CursorImage, CursorMessage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorState {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    pub image: Option<Arc<CursorImage>>,
}

#[derive(Debug, Default)]
struct LatestState {
    cursor: Option<CursorState>,
    updates: u64,
}

#[derive(Debug, Clone, Default)]
pub struct LatestCursor {
    shared: Arc<(Mutex<LatestState>, Condvar)>,
}

impl LatestCursor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn latest(&self) -> Option<CursorState> {
        self.shared.0.lock().cursor.clone()
    }

    pub fn updates(&self) -> u64 {
        self.shared.0.lock().updates
    }

    pub fn wait_after(&self, seen: u64, timeout: Duration) -> Option<CursorState> {
        let deadline = Instant::now() + timeout;
        let (state, changed) = &*self.shared;
        let mut state = state.lock();

        while state.updates == seen {
            if changed.wait_until(&mut state, deadline).timed_out() {
                break;
            }
        }
        if state.updates == seen {
            return None;
        }

        state.cursor.clone()
    }

    pub(crate) fn update(&self, message: CursorMessage) {
        let (state, changed) = &*self.shared;
        let mut state = state.lock();

        let image = match message.image {
            Some(image) => Some(Arc::new(image)),
            None => state
                .cursor
                .as_ref()
                .and_then(|previous| previous.image.clone()),
        };
        state.cursor = Some(CursorState {
            x: message.x,
            y: message.y,
            visible: message.visible,
            image,
        });
        state.updates += 1;
        drop(state);

        changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    fn image(marker: u8) -> CursorImage {
        CursorImage {
            width: 1,
            height: 1,
            hotspot_x: 0,
            hotspot_y: 0,
            rgba: vec![marker; 4],
        }
    }

    fn message(x: i32, image: Option<CursorImage>) -> CursorMessage {
        CursorMessage {
            x,
            y: 2 * x,
            visible: true,
            image,
        }
    }

    #[test]
    fn nothing_is_known_before_the_first_update() {
        let cursor = LatestCursor::new();

        assert_eq!(cursor.latest(), None);
        assert_eq!(cursor.updates(), 0);
    }

    #[test]
    fn the_newest_position_replaces_the_older_one() {
        let cursor = LatestCursor::new();

        cursor.update(message(1, None));
        cursor.update(message(5, None));

        let latest = cursor.latest().unwrap();
        assert_eq!((latest.x, latest.y), (5, 10));
        assert_eq!(cursor.updates(), 2);
    }

    #[test]
    fn a_position_only_update_keeps_the_last_image() {
        let cursor = LatestCursor::new();

        cursor.update(message(1, Some(image(7))));
        let first = cursor.latest().unwrap().image.unwrap();
        cursor.update(message(2, None));
        let second = cursor.latest().unwrap().image.unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(second.rgba, vec![7; 4]);
    }

    #[test]
    fn a_new_image_replaces_the_old_one() {
        let cursor = LatestCursor::new();

        cursor.update(message(1, Some(image(7))));
        let first = cursor.latest().unwrap().image.unwrap();
        cursor.update(message(1, Some(image(9))));
        let second = cursor.latest().unwrap().image.unwrap();

        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(second.rgba, vec![9; 4]);
    }

    #[test]
    fn waiting_returns_once_something_newer_arrives() {
        let cursor = LatestCursor::new();
        let writer = cursor.clone();

        let waiter = thread::spawn(move || cursor.wait_after(0, Duration::from_secs(5)));
        thread::sleep(Duration::from_millis(20));
        writer.update(message(3, None));

        assert_eq!(waiter.join().unwrap().unwrap().x, 3);
    }

    #[test]
    fn waiting_times_out_when_nothing_changed() {
        let cursor = LatestCursor::new();
        cursor.update(message(3, None));

        assert_eq!(cursor.wait_after(1, Duration::from_millis(10)), None);
    }
}
