use std::os::fd::OwnedFd;

use ashpd::{
    desktop::{
        CreateSessionOptions, PersistMode,
        screencast::{
            CursorMode as PortalCursorMode, OpenPipeWireRemoteOptions, Screencast,
            SelectSourcesOptions, SourceType, StartCastOptions,
        },
    },
    enumflags2::BitFlags,
};
use rusplay_config::{CaptureConfig, CursorMode, SourceKind};
use rusplay_core::Resolution;

use crate::CaptureError;

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
            .set_restore_token(config.restore_token.as_deref());
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
