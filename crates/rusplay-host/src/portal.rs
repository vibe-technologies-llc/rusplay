use std::{any::Any, sync::Arc};

use rusplay_config::Config;

use crate::{CaptureFeed, ClipboardOpener, Gpu, HostError, InputFeed, open_portal_capture};

pub struct PortalHost {
    pub feed: Arc<CaptureFeed>,
    pub input: Option<Arc<InputFeed>>,
    pub clipboard: Option<ClipboardOpener>,
    pub input_unavailable: bool,
    pub keepalive: Option<Box<dyn Any + Send>>,
}

impl std::fmt::Debug for PortalHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PortalHost")
            .field("feed", &self.feed)
            .field("input", &self.input)
            .field("clipboard", &self.clipboard.is_some())
            .field("input_unavailable", &self.input_unavailable)
            .field("keepalive", &self.keepalive.is_some())
            .finish()
    }
}

pub fn open_portal(config: &Config, gpu: &Gpu) -> Result<PortalHost, HostError> {
    if !config.input.accept_remote {
        return capture_only(config, gpu, false);
    }

    shared::open(config, gpu)
}

fn restore_hid_the_prompt(used_token: bool, wanted_input: bool, input_missing: bool) -> bool {
    used_token && wanted_input && input_missing
}

fn capture_only(
    config: &Config,
    gpu: &Gpu,
    input_unavailable: bool,
) -> Result<PortalHost, HostError> {
    Ok(PortalHost {
        feed: open_portal_capture(config, gpu)?,
        input: None,
        clipboard: None,
        input_unavailable,
        keepalive: None,
    })
}

#[cfg(all(feature = "pipewire", feature = "input"))]
mod shared {
    use std::{os::fd::OwnedFd, sync::Arc, time::Duration};

    use rusplay_capture::RestoreTokenStore;
    use rusplay_clipboard::{
        Clipboard, ClipboardError, ClipboardPolicy, PortalClipboard, probe_data_control,
        request_clipboard,
    };
    use rusplay_config::Config;
    use rusplay_input::{
        InputError, InputEvent, InputSink, PadFeedback, SplitSink,
        gamepad::VirtualGamepads,
        libei::{
            AbsoluteTarget, LibeiInjector, LibeiOptions, PortalInputOptions, RemoteDesktopSession,
        },
    };

    use super::PortalHost;
    use crate::{
        ClipboardOpener, Gpu, HostError, InputFeed, InputTarget,
        source::{feed_from_session, open_capture_session},
    };

    const READY_TIMEOUT: Duration = Duration::from_secs(2);
    const INPUT_TOKEN_FILE: &str = "input-restore-token";

    type DesktopTarget = SplitSink<LibeiInjector, VirtualGamepads>;

    impl InputTarget for DesktopTarget {
        fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
            InputSink::inject(self, event)
        }

        fn inject_batch(
            &mut self,
            events: &[InputEvent],
            results: &mut Vec<Result<(), InputError>>,
        ) {
            InputSink::inject_batch(self, events, results);
        }

        fn service(&mut self, feedback: &mut Vec<PadFeedback>) -> Result<(), InputError> {
            let desktop = self.desktop.service();
            let gamepads = self.gamepads.poll_feedback(feedback);

            desktop.and(gamepads)
        }
    }

    struct Opened {
        session: Arc<RemoteDesktopSession>,
        eis: Option<OwnedFd>,
        clipboard: Option<ClipboardOpener>,
        restore_token: Option<String>,
    }

    pub(super) fn open(config: &Config, gpu: &Gpu) -> Result<PortalHost, HostError> {
        let capture = open_capture_session(config)?;
        let target = capture
            .streams
            .first()
            .and_then(|stream| stream.mapping_id.clone())
            .map_or(AbsoluteTarget::Bounds, AbsoluteTarget::Mapping);
        let opened = match open_input(config) {
            Ok(opened) => Some(opened),
            Err(error) if error.denies_input_permission() => None,
            Err(error) => return Err(error),
        };

        let requested = config.input.keyboard || config.input.pointer || config.input.touch;
        let Some(opened) = opened else {
            return Ok(PortalHost {
                feed: feed_from_session(capture, config, gpu)?,
                input: None,
                clipboard: None,
                input_unavailable: requested,
                keepalive: None,
            });
        };

        let input = match opened.eis {
            Some(eis) => {
                let input_config = config.input;
                let session = Arc::clone(&opened.session);
                match InputFeed::spawn(&config.network, move || {
                    let options = LibeiOptions {
                        target,
                        ..LibeiOptions::default()
                    };
                    let mut injector = LibeiInjector::connect(eis, &input_config, options)?;
                    injector.attach_session(session);
                    match injector.wait_ready(READY_TIMEOUT) {
                        Ok(()) | Err(InputError::DevicesNotResumed) => {}
                        Err(error) => return Err(error.into()),
                    }

                    Ok(SplitSink::new(
                        injector,
                        VirtualGamepads::from_config(input_config),
                    ))
                }) {
                    Ok(feed) => Some(feed),
                    Err(error) if error.denies_input_permission() => None,
                    Err(error) => return Err(error),
                }
            }
            None => None,
        };

        let input_unavailable = requested && input.is_none();

        Ok(PortalHost {
            feed: feed_from_session(capture, config, gpu)?,
            input,
            clipboard: opened.clipboard,
            input_unavailable,
            keepalive: Some(Box::new(opened.session)),
        })
    }

    fn open_input(config: &Config) -> Result<Opened, HostError> {
        let store = config
            .capture
            .persist_session
            .then(|| RestoreTokenStore::from_config(&config.capture))
            .transpose()?
            .map(|store| store.sibling(INPUT_TOKEN_FILE));
        let stored = match &store {
            Some(store) => store.load()?,
            None => None,
        };

        let portal_clipboard =
            ClipboardPolicy::host(&config.input).is_enabled() && probe_data_control().is_err();
        let wanted_input = config.input.keyboard || config.input.pointer || config.input.touch;
        let used_token = stored.is_some();
        let first = pollster::block_on(open_session(config, stored, portal_clipboard));
        let restored_without_input = match &first {
            Ok(opened) => opened.eis.is_none(),
            Err(error) => error.denies_input_permission(),
        };
        let opened =
            if super::restore_hid_the_prompt(used_token, wanted_input, restored_without_input) {
                drop(first);
                if let Some(store) = &store {
                    store.clear()?;
                }
                pollster::block_on(open_session(config, None, portal_clipboard))?
            } else {
                first?
            };
        if let (Some(store), Some(token)) = (&store, &opened.restore_token) {
            store.store(token)?;
        }

        Ok(opened)
    }

    async fn open_session(
        config: &Config,
        restore_token: Option<String>,
        portal_clipboard: bool,
    ) -> Result<Opened, HostError> {
        let session = RemoteDesktopSession::create().await?;
        let requested = if portal_clipboard {
            Some(request_clipboard(&session).await)
        } else {
            None
        };
        let options = PortalInputOptions {
            persist: config.capture.persist_session,
            restore_token,
            ..PortalInputOptions::from_config(&config.input)
        };

        session.select_devices(&options).await?;
        let started = session.start_selected().await?;

        let session = Arc::new(session);
        let clipboard = requested.map(|requested| portal_opener(&session, requested));
        let granted = started.keyboard || started.pointer || started.touch;
        let eis = if granted {
            match session.connect_to_eis().await {
                Ok(eis) => Some(eis),
                Err(error) if error.denies_input_permission() => None,
                Err(error) => return Err(error.into()),
            }
        } else {
            None
        };

        Ok(Opened {
            session,
            eis,
            clipboard,
            restore_token: started.restore_token,
        })
    }

    fn portal_opener(
        session: &Arc<RemoteDesktopSession>,
        requested: Result<Clipboard, ClipboardError>,
    ) -> ClipboardOpener {
        let portal = requested
            .and_then(|clipboard| PortalClipboard::spawn(Arc::clone(session), clipboard))
            .map_err(Arc::new);

        Arc::new(move |policy| match &portal {
            Ok(portal) => Ok(portal.link(policy.max_bytes, policy.send)),
            Err(error) => Err(ClipboardError::PortalSetup(Arc::clone(error))),
        })
    }
}

#[cfg(not(all(feature = "pipewire", feature = "input")))]
mod shared {
    use rusplay_config::Config;

    use super::PortalHost;
    use crate::{Gpu, HostError};

    pub(super) fn open(_config: &Config, _gpu: &Gpu) -> Result<PortalHost, HostError> {
        if cfg!(feature = "pipewire") {
            Err(HostError::InputNotCompiled)
        } else {
            Err(HostError::CaptureNotCompiled)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::restore_hid_the_prompt;

    #[test]
    fn a_restore_token_that_skipped_input_asks_again() {
        assert!(restore_hid_the_prompt(true, true, true));
        assert!(!restore_hid_the_prompt(true, true, false));
        assert!(!restore_hid_the_prompt(false, true, true));
        assert!(!restore_hid_the_prompt(true, false, true));
    }
}
