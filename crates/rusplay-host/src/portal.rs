use std::sync::Arc;

use rusplay_config::Config;

use crate::{CaptureFeed, ClipboardOpener, Gpu, HostError, InputFeed, open_portal_capture};

pub struct PortalHost {
    pub feed: Arc<CaptureFeed>,
    pub input: Option<Arc<InputFeed>>,
    pub clipboard: Option<ClipboardOpener>,
}

impl std::fmt::Debug for PortalHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PortalHost")
            .field("feed", &self.feed)
            .field("input", &self.input)
            .field("clipboard", &self.clipboard.is_some())
            .finish()
    }
}

pub fn open_portal(config: &Config, gpu: &Gpu) -> Result<PortalHost, HostError> {
    if config.input.accept_remote {
        shared::open(config, gpu)
    } else {
        Ok(PortalHost {
            feed: open_portal_capture(config, gpu)?,
            input: None,
            clipboard: None,
        })
    }
}

#[cfg(all(feature = "pipewire", feature = "input"))]
mod shared {
    use std::{os::fd::OwnedFd, sync::Arc, time::Duration};

    use rusplay_capture::{
        PortalSession, PortalStream, RestoreTokenStore, SharedScreencast, portal_resolution,
    };
    use rusplay_clipboard::{
        Clipboard, ClipboardError, ClipboardPolicy, PortalClipboard, probe_data_control,
        request_clipboard,
    };
    use rusplay_config::Config;
    use rusplay_input::{
        InputError, InputEvent, InputSink, RumbleCommand, SplitSink,
        gamepad::UinputGamepads,
        libei::{
            AbsoluteTarget, LibeiInjector, LibeiOptions, PortalInputOptions, RemoteDesktopSession,
        },
    };

    use super::PortalHost;
    use crate::{
        ClipboardOpener, Gpu, HostError, InputFeed, InputTarget, source::feed_from_session,
    };

    const READY_TIMEOUT: Duration = Duration::from_secs(2);

    type DesktopTarget = SplitSink<LibeiInjector, UinputGamepads>;

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

        fn service(&mut self, rumble: &mut Vec<RumbleCommand>) -> Result<(), InputError> {
            let desktop = self.desktop.service();
            let gamepads = self.gamepads.poll_rumble(rumble);

            desktop.and(gamepads)
        }
    }

    struct InputSession {
        session: Arc<RemoteDesktopSession>,
        eis: OwnedFd,
        target: AbsoluteTarget,
    }

    struct Opened {
        capture: PortalSession,
        input: InputSession,
        clipboard: Option<ClipboardOpener>,
    }

    pub(super) fn open(config: &Config, gpu: &Gpu) -> Result<PortalHost, HostError> {
        let capture = &config.capture;
        let store = capture
            .persist_session
            .then(|| RestoreTokenStore::from_config(capture))
            .transpose()?;
        let stored = match (&capture.restore_token, &store) {
            (Some(token), _) => Some(token.clone()),
            (None, Some(store)) => store.load()?,
            (None, None) => None,
        };

        let portal_clipboard =
            ClipboardPolicy::host(&config.input).is_enabled() && probe_data_control().is_err();
        let opened = pollster::block_on(open_session(config, stored, portal_clipboard))?;
        if let (Some(store), Some(token)) = (&store, &opened.capture.restore_token) {
            store.store(token)?;
        }

        let input = opened.input;
        let input_config = config.input;
        let feed = InputFeed::spawn(move || {
            let options = LibeiOptions {
                target: input.target,
                ..LibeiOptions::default()
            };
            let mut injector = LibeiInjector::connect(input.eis, &input_config, options)?;
            injector.attach_session(input.session);
            match injector.wait_ready(READY_TIMEOUT) {
                Ok(()) | Err(InputError::DevicesNotResumed) => {}
                Err(error) => return Err(error.into()),
            }

            Ok(SplitSink::new(
                injector,
                UinputGamepads::from_config(input_config),
            ))
        })?;

        Ok(PortalHost {
            feed: feed_from_session(opened.capture, config, gpu)?,
            input: Some(feed),
            clipboard: opened.clipboard,
        })
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
        let screencast =
            SharedScreencast::select_sources(&config.capture, session.session()).await?;
        let started = session.start_selected().await?;

        let streams = started
            .streams
            .iter()
            .map(|stream| PortalStream {
                node_id: stream.node_id,
                size: stream
                    .size
                    .and_then(|(width, height)| portal_resolution(width, height)),
            })
            .collect();
        let session = Arc::new(session);
        let clipboard = requested.map(|requested| portal_opener(&session, requested));
        let capture = screencast
            .open_remote(session.session(), streams, started.restore_token.clone())
            .await?;
        let eis = session.connect_to_eis().await?;
        let target = started
            .streams
            .first()
            .and_then(|stream| stream.mapping_id.clone())
            .map_or(AbsoluteTarget::Bounds, AbsoluteTarget::Mapping);

        Ok(Opened {
            capture,
            input: InputSession {
                session,
                eis,
                target,
            },
            clipboard,
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
