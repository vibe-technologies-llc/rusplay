use std::sync::Arc;

use rusplay_clipboard::{
    ClipboardBackend, ClipboardError, ClipboardLink, ClipboardPolicy, ClipboardState,
};

pub type ClipboardOpener =
    Arc<dyn Fn(ClipboardPolicy) -> Result<Box<dyn ClipboardBackend>, ClipboardError> + Send + Sync>;

#[cfg(feature = "input")]
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn default_clipboard_opener() -> Option<ClipboardOpener> {
    Some(data_control_opener())
}

#[cfg(not(feature = "input"))]
pub(crate) fn default_clipboard_opener() -> Option<ClipboardOpener> {
    None
}

#[cfg(feature = "input")]
pub fn data_control_opener() -> ClipboardOpener {
    use rusplay_clipboard::{DataControl, DataControlOptions};

    Arc::new(|policy| {
        let options = DataControlOptions {
            max_bytes: policy.max_bytes,
            watch: policy.send,
        };

        Ok(Box::new(DataControl::open(options)?))
    })
}

pub(crate) fn open_link(
    opener: Option<&ClipboardOpener>,
    policy: ClipboardPolicy,
) -> (Option<ClipboardLink>, ClipboardState) {
    if !policy.is_enabled() {
        return (None, ClipboardState::Off);
    }

    let Some(opener) = opener else {
        return (
            None,
            ClipboardState::Unavailable(Arc::new(ClipboardError::NotCompiled)),
        );
    };

    match opener(policy) {
        Ok(backend) => {
            let kind = backend.kind();
            (
                Some(ClipboardLink::new(policy, backend)),
                ClipboardState::Active(kind),
            )
        }
        Err(error) => (None, ClipboardState::Unavailable(Arc::new(error))),
    }
}

#[cfg(test)]
mod tests {
    use rusplay_clipboard::{BackendKind, MemoryClipboard};

    use super::*;

    fn policy(send: bool, receive: bool) -> ClipboardPolicy {
        ClipboardPolicy {
            send,
            receive,
            max_bytes: 1024,
        }
    }

    fn memory_opener(clipboard: &MemoryClipboard) -> ClipboardOpener {
        let clipboard = clipboard.clone();

        Arc::new(move |_| Ok(Box::new(clipboard.clone()) as Box<dyn ClipboardBackend>))
    }

    #[test]
    fn a_disabled_policy_opens_nothing() {
        let clipboard = MemoryClipboard::default();
        let opener = memory_opener(&clipboard);

        let (link, state) = open_link(Some(&opener), policy(false, false));

        assert!(link.is_none());
        assert!(matches!(state, ClipboardState::Off));
    }

    #[test]
    fn an_enabled_policy_opens_the_backend_and_reports_it() {
        let clipboard = MemoryClipboard::default();
        let opener = memory_opener(&clipboard);

        let (link, state) = open_link(Some(&opener), policy(true, true));

        assert!(link.is_some());
        assert!(matches!(state, ClipboardState::Active(BackendKind::Memory)));
    }

    #[test]
    fn a_failing_backend_is_reported_unavailable_and_the_session_goes_on_without_it() {
        let opener: ClipboardOpener = Arc::new(|_| Err(ClipboardError::DataControlMissing));

        let (link, state) = open_link(Some(&opener), policy(true, true));

        assert!(link.is_none());
        let ClipboardState::Unavailable(error) = state else {
            panic!("the backend failed to open");
        };
        assert!(matches!(*error, ClipboardError::DataControlMissing));
    }

    #[test]
    fn a_host_without_an_opener_reports_the_backends_missing() {
        let (link, state) = open_link(None, policy(true, false));

        assert!(link.is_none());
        let ClipboardState::Unavailable(error) = state else {
            panic!("no opener was given");
        };
        assert!(matches!(*error, ClipboardError::NotCompiled));
    }
}
