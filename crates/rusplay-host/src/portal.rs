use std::sync::Arc;

use rusplay_config::Config;

use crate::{CaptureFeed, Gpu, HostError, InputFeed, open_portal_capture};

#[derive(Debug)]
pub struct PortalHost {
    pub feed: Arc<CaptureFeed>,
    pub input: Option<Arc<InputFeed>>,
}

pub fn open_portal(config: &Config, gpu: &Gpu) -> Result<PortalHost, HostError> {
    if config.input.accept_remote {
        shared::open(config, gpu)
    } else {
        Ok(PortalHost {
            feed: open_portal_capture(config, gpu)?,
            input: None,
        })
    }
}

#[cfg(all(feature = "pipewire", feature = "input"))]
mod shared {
    use std::{os::fd::OwnedFd, time::Duration};

    use rusplay_capture::{
        PortalSession, PortalStream, RestoreTokenStore, SharedScreencast, portal_resolution,
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
    use crate::{Gpu, HostError, InputFeed, InputTarget, source::feed_from_session};

    const READY_TIMEOUT: Duration = Duration::from_secs(2);

    type DesktopTarget = SplitSink<LibeiInjector, UinputGamepads>;

    impl InputTarget for DesktopTarget {
        fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
            InputSink::inject(self, event)
        }

        fn service(&mut self, rumble: &mut Vec<RumbleCommand>) -> Result<(), InputError> {
            self.desktop.service()?;
            self.gamepads.poll_rumble(rumble)
        }
    }

    struct InputSession {
        session: RemoteDesktopSession,
        eis: OwnedFd,
        target: AbsoluteTarget,
    }

    struct Opened {
        capture: PortalSession,
        input: InputSession,
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

        let opened = pollster::block_on(open_session(config, stored))?;
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
        })
    }

    async fn open_session(
        config: &Config,
        restore_token: Option<String>,
    ) -> Result<Opened, HostError> {
        let session = RemoteDesktopSession::create().await?;
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
