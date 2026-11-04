use std::sync::Arc;

use rusplay_clipboard::{
    ClipboardBackend, ClipboardError, ClipboardLink, ClipboardPolicy, ClipboardState,
};

pub type ClipboardOpener =
    Arc<dyn Fn(ClipboardPolicy) -> Result<Box<dyn ClipboardBackend>, ClipboardError> + Send + Sync>;

#[cfg(feature = "window")]
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn default_clipboard_opener() -> Option<ClipboardOpener> {
    use rusplay_clipboard::{DataControl, DataControlOptions};

    Some(Arc::new(|policy| {
        let options = DataControlOptions {
            max_bytes: policy.max_bytes,
            watch: policy.send,
        };

        Ok(Box::new(DataControl::open(options)?))
    }))
}

#[cfg(not(feature = "window"))]
pub(crate) fn default_clipboard_opener() -> Option<ClipboardOpener> {
    None
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
