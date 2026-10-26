use std::os::fd::OwnedFd;

use ashpd::{
    desktop::{
        CreateSessionOptions, PersistMode, Session,
        screencast::{
            CursorMode as PortalCursorMode, IsScreencastSession, OpenPipeWireRemoteOptions,
            Screencast, SelectSourcesOptions, SourceType, StartCastOptions,
        },
    },
    enumflags2::BitFlags,
};
use rusplay_config::{CaptureConfig, CursorMode, SourceKind};
use rusplay_core::Resolution;

use crate::{CaptureError, RestoreTokenStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalStream {
    pub node_id: u32,
    pub size: Option<Resolution>,
}

#[derive(Debug)]
pub struct PortalSession {
    pub remote: OwnedFd,
    pub streams: Vec<PortalStream>,
    pub restore_token: Option<String>,
}

impl PortalSession {
    pub async fn open(config: &CaptureConfig) -> Result<Self, CaptureError> {
        Self::open_with_token(config, config.restore_token.as_deref()).await
    }

    pub async fn open_persistent(
        config: &CaptureConfig,
        store: &RestoreTokenStore,
    ) -> Result<Self, CaptureError> {
        if !config.persist_session {
            return Self::open(config).await;
        }

        let stored = match &config.restore_token {
            Some(token) => Some(token.clone()),
            None => store.load()?,
        };
        let session = Self::open_with_token(config, stored.as_deref()).await?;

        if let Some(token) = &session.restore_token {
            store.store(token)?;
        }
        Ok(session)
    }

    async fn open_with_token(
        config: &CaptureConfig,
        restore_token: Option<&str>,
    ) -> Result<Self, CaptureError> {
        let proxy = Screencast::new().await?;
        let session = proxy
            .create_session(CreateSessionOptions::default())
            .await?;

        let persist = if config.persist_session {
            PersistMode::ExplicitlyRevoked
        } else {
            PersistMode::DoNot
        };
        let sources = SelectSourcesOptions::default()
            .set_cursor_mode(portal_cursor(config.cursor))
            .set_sources(portal_sources(config.source))
            .set_multiple(false)
            .set_persist_mode(persist)
            .set_restore_token(restore_token);
        proxy.select_sources(&session, sources).await?;

        let response = proxy
            .start(&session, None, StartCastOptions::default())
            .await?
            .response()?;
        let streams: Vec<PortalStream> = response
            .streams()
            .iter()
            .map(|stream| PortalStream {
                node_id: stream.pipe_wire_node_id(),
                size: stream.size().and_then(resolution_from_portal),
            })
            .collect();
        if streams.is_empty() {
            return Err(CaptureError::NoStreams);
        }

        let remote = proxy
            .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
            .await?;

        Ok(Self {
            remote,
            streams,
            restore_token: response.restore_token().map(str::to_owned),
        })
    }
}

#[derive(Debug)]
pub struct SharedScreencast {
    proxy: Screencast,
}

impl SharedScreencast {
    pub async fn select_sources(
        config: &CaptureConfig,
        session: &Session<impl IsScreencastSession>,
    ) -> Result<Self, CaptureError> {
        let proxy = Screencast::new().await?;
        let sources = SelectSourcesOptions::default()
            .set_cursor_mode(portal_cursor(config.cursor))
            .set_sources(portal_sources(config.source))
            .set_multiple(false);
        proxy.select_sources(session, sources).await?;

        Ok(Self { proxy })
    }

    pub async fn open_remote(
        &self,
        session: &Session<impl IsScreencastSession>,
        streams: Vec<PortalStream>,
        restore_token: Option<String>,
    ) -> Result<PortalSession, CaptureError> {
        if streams.is_empty() {
            return Err(CaptureError::NoStreams);
        }

        let remote = self
            .proxy
            .open_pipe_wire_remote(session, OpenPipeWireRemoteOptions::default())
            .await?;

        Ok(PortalSession {
            remote,
            streams,
            restore_token,
        })
    }
}

pub fn portal_resolution(width: i32, height: i32) -> Option<Resolution> {
    resolution_from_portal((width, height))
}

fn portal_cursor(mode: CursorMode) -> PortalCursorMode {
    match mode {
        CursorMode::Hidden => PortalCursorMode::Hidden,
        CursorMode::Embedded => PortalCursorMode::Embedded,
        CursorMode::Metadata => PortalCursorMode::Metadata,
    }
}

fn portal_sources(kind: SourceKind) -> BitFlags<SourceType> {
    match kind {
        SourceKind::Monitor => SourceType::Monitor.into(),
        SourceKind::Window => SourceType::Window.into(),
        SourceKind::Any => SourceType::Monitor | SourceType::Window,
    }
}

fn resolution_from_portal((width, height): (i32, i32)) -> Option<Resolution> {
    Some(Resolution::new(
        u32::try_from(width).ok()?,
        u32::try_from(height).ok()?,
    ))
}
