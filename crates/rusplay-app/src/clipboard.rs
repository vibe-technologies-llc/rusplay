use std::sync::Arc;

use parking_lot::Mutex;
use rusplay_client::{
    BackendKind, ClipboardBackend, ClipboardError, ClipboardOpener, ClipboardPolicy,
    ClipboardState, DataControl, DataControlOptions, FocusClipboard, FocusClipboardHandle,
};

use crate::notice::error_chain;

pub type FocusSlot = Arc<Mutex<Option<FocusClipboardHandle>>>;

pub fn session_opener(slot: FocusSlot) -> ClipboardOpener {
    with_focus_fallback(Arc::new(data_control), slot)
}

pub fn with_focus_fallback(primary: ClipboardOpener, slot: FocusSlot) -> ClipboardOpener {
    Arc::new(move |policy| {
        primary(policy).or_else(|_| {
            let (handle, backend) = FocusClipboard::pair(policy);

            *slot.lock() = Some(handle);
            Ok(Box::new(backend))
        })
    })
}

pub fn notice_text(state: &ClipboardState) -> Option<String> {
    match state {
        ClipboardState::Unavailable(error) => Some(format!(
            "Clipboard sharing is off for this session: {}",
            error_chain(&**error)
        )),
        ClipboardState::Active(BackendKind::Focus) => Some(
            "Clipboard sharing is focus based on this desktop: text you copy here is sent when \
             this window gains focus, so a copy made while it is focused is not seen until focus \
             returns; text from the host is written to your clipboard as it arrives"
                .to_owned(),
        ),
        ClipboardState::Active(_) | ClipboardState::Off => None,
    }
}

fn data_control(policy: ClipboardPolicy) -> Result<Box<dyn ClipboardBackend>, ClipboardError> {
    let options = DataControlOptions {
        max_bytes: policy.max_bytes,
        watch: policy.send,
    };

    Ok(Box::new(DataControl::open(options)?))
}

#[cfg(test)]
mod tests {
    use rusplay_client::MemoryClipboard;

    use super::*;

    fn policy() -> ClipboardPolicy {
        ClipboardPolicy {
            send: true,
            receive: true,
            max_bytes: 100,
        }
    }

    fn failing_primary() -> ClipboardOpener {
        Arc::new(|_| Err(ClipboardError::DataControlMissing))
    }

    fn memory_primary() -> ClipboardOpener {
        Arc::new(|_| Ok(Box::new(MemoryClipboard::default())))
    }

    #[test]
    fn data_control_is_used_when_it_opens() {
        let slot = FocusSlot::default();
        let opener = with_focus_fallback(memory_primary(), Arc::clone(&slot));

        let backend = opener(policy()).unwrap();

        assert_eq!(backend.kind(), BackendKind::Memory);
        assert!(slot.lock().is_none());
    }

    #[test]
    fn the_focus_fallback_replaces_a_missing_data_control() {
        let slot = FocusSlot::default();
        let opener = with_focus_fallback(failing_primary(), Arc::clone(&slot));

        let backend = opener(policy()).unwrap();

        assert_eq!(backend.kind(), BackendKind::Focus);
        assert!(slot.lock().is_some());
    }

    #[test]
    fn the_notice_says_the_fallback_is_focus_based() {
        let text = notice_text(&ClipboardState::Active(BackendKind::Focus)).unwrap();

        assert!(text.contains("focus"));
        assert!(text.contains("not seen until focus returns"));
    }

    #[test]
    fn only_unavailable_and_focus_states_raise_a_notice() {
        let unavailable = ClipboardState::Unavailable(Arc::new(ClipboardError::NotCompiled));

        assert!(notice_text(&unavailable).unwrap().contains("off"));
        assert_eq!(notice_text(&ClipboardState::Off), None);
        assert_eq!(
            notice_text(&ClipboardState::Active(BackendKind::ExtDataControl)),
            None
        );
    }
}
