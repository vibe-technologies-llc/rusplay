use std::os::fd::OwnedFd;

use ashpd::{
    desktop::{
        CreateSessionOptions, PersistMode, Session,
        remote_desktop::{
            ConnectToEISOptions, DeviceType, RemoteDesktop, SelectDevicesOptions, StartOptions,
        },
    },
    enumflags2::BitFlags,
};
use rusplay_config::InputConfig;

use crate::InputError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalStep {
    Connect,
    CreateSession,
    SelectDevices,
    Start,
    ConnectToEis,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct PortalInputOptions {
    pub keyboard: bool,
    pub pointer: bool,
    pub touch: bool,
    pub persist: bool,
    pub restore_token: Option<String>,
}

impl PortalInputOptions {
    pub fn from_config(config: &InputConfig) -> Self {
        Self {
            keyboard: config.keyboard,
            pointer: config.pointer,
            touch: config.touch,
            persist: false,
            restore_token: None,
        }
    }

    fn device_types(&self) -> BitFlags<DeviceType> {
        let mut types = BitFlags::empty();
        if self.keyboard {
            types |= DeviceType::Keyboard;
        }
        if self.pointer {
            types |= DeviceType::Pointer;
        }
        if self.touch {
            types |= DeviceType::Touchscreen;
        }
        types
    }

    fn persist_mode(&self) -> PersistMode {
        if self.persist {
            PersistMode::ExplicitlyRevoked
        } else {
            PersistMode::DoNot
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalStream {
    pub node_id: u32,
    pub size: Option<(i32, i32)>,
    pub mapping_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedPortal {
    pub keyboard: bool,
    pub pointer: bool,
    pub touch: bool,
    pub streams: Vec<PortalStream>,
    pub restore_token: Option<String>,
}

#[derive(Debug)]
pub struct RemoteDesktopSession {
    proxy: RemoteDesktop,
    session: Session<RemoteDesktop>,
}

impl RemoteDesktopSession {
    pub async fn create() -> Result<Self, InputError> {
        let proxy = RemoteDesktop::new()
            .await
            .map_err(portal(PortalStep::Connect))?;
        let session = proxy
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(portal(PortalStep::CreateSession))?;

        Ok(Self { proxy, session })
    }

    pub fn session(&self) -> &Session<RemoteDesktop> {
        &self.session
    }

    pub async fn select_devices(&self, options: &PortalInputOptions) -> Result<(), InputError> {
        let select = SelectDevicesOptions::default()
            .set_devices(options.device_types())
            .set_persist_mode(options.persist_mode())
            .set_restore_token(options.restore_token.as_deref());
        self.proxy
            .select_devices(&self.session, select)
            .await
            .map_err(portal(PortalStep::SelectDevices))?
            .response()
            .map_err(portal(PortalStep::SelectDevices))?;

        Ok(())
    }

    pub async fn start(&self, options: &PortalInputOptions) -> Result<StartedPortal, InputError> {
        self.select_devices(options).await?;
        self.start_selected().await
    }

    pub async fn start_selected(&self) -> Result<StartedPortal, InputError> {
        let selected = self
            .proxy
            .start(&self.session, None, StartOptions::default())
            .await
            .map_err(portal(PortalStep::Start))?
            .response()
            .map_err(portal(PortalStep::Start))?;

        let devices = selected.devices();
        let streams = selected
            .streams()
            .iter()
            .map(|stream| PortalStream {
                node_id: stream.pipe_wire_node_id(),
                size: stream.size(),
                mapping_id: stream.mapping_id().map(str::to_owned),
            })
            .collect();

        Ok(StartedPortal {
            keyboard: devices.contains(DeviceType::Keyboard),
            pointer: devices.contains(DeviceType::Pointer),
            touch: devices.contains(DeviceType::Touchscreen),
            streams,
            restore_token: selected.restore_token().map(str::to_owned),
        })
    }

    pub async fn connect_to_eis(&self) -> Result<OwnedFd, InputError> {
        self.proxy
            .connect_to_eis(&self.session, ConnectToEISOptions::default())
            .await
            .map_err(portal(PortalStep::ConnectToEis))
    }
}

fn portal(step: PortalStep) -> impl FnOnce(ashpd::Error) -> InputError {
    move |source| InputError::Portal {
        step,
        source: Box::new(source),
    }
}
