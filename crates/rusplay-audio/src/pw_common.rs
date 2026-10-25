use std::{
    io::Cursor,
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};

use pipewire::{
    channel,
    spa::{
        param::{
            ParamType,
            audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS},
            format::{MediaSubtype, MediaType},
            format_utils,
        },
        pod::{Object, Pod, Value, serialize::PodSerializer},
        sys,
        utils::SpaTypes,
    },
};
use rusplay_core::ChannelLayout;

use crate::AudioError;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NegotiatedFormat {
    pub rate: u32,
    pub channels: usize,
}

pub(crate) type StartupSender = mpsc::Sender<Result<NegotiatedFormat, AudioError>>;

pub(crate) fn channel_positions(layout: ChannelLayout) -> [u32; MAX_CHANNELS] {
    let order: &[u32] = match layout {
        ChannelLayout::Mono => &[sys::SPA_AUDIO_CHANNEL_MONO],
        ChannelLayout::Stereo => &[sys::SPA_AUDIO_CHANNEL_FL, sys::SPA_AUDIO_CHANNEL_FR],
        ChannelLayout::Surround51 => &[
            sys::SPA_AUDIO_CHANNEL_FL,
            sys::SPA_AUDIO_CHANNEL_FR,
            sys::SPA_AUDIO_CHANNEL_FC,
            sys::SPA_AUDIO_CHANNEL_LFE,
            sys::SPA_AUDIO_CHANNEL_SL,
            sys::SPA_AUDIO_CHANNEL_SR,
        ],
        ChannelLayout::Surround71 => &[
            sys::SPA_AUDIO_CHANNEL_FL,
            sys::SPA_AUDIO_CHANNEL_FR,
            sys::SPA_AUDIO_CHANNEL_FC,
            sys::SPA_AUDIO_CHANNEL_LFE,
            sys::SPA_AUDIO_CHANNEL_RL,
            sys::SPA_AUDIO_CHANNEL_RR,
            sys::SPA_AUDIO_CHANNEL_SL,
            sys::SPA_AUDIO_CHANNEL_SR,
        ],
    };

    let mut positions = [0; MAX_CHANNELS];
    positions[..order.len()].copy_from_slice(order);
    positions
}

pub(crate) fn format_pod_bytes(layout: ChannelLayout, rate: u32) -> Result<Vec<u8>, AudioError> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(rate);
    info.set_channels(u32::from(layout.channel_count()));
    info.set_position(channel_positions(layout));

    let object = Value::Object(Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    });

    let (cursor, _) = PodSerializer::serialize(Cursor::new(Vec::new()), &object)
        .map_err(|_| AudioError::PipeWireFormatEncode)?;
    Ok(cursor.into_inner())
}

pub(crate) fn negotiated_format(id: u32, param: Option<&Pod>) -> Option<NegotiatedFormat> {
    let param = param?;
    if id != ParamType::Format.as_raw() {
        return None;
    }

    let (media_type, media_subtype) = format_utils::parse_format(param).ok()?;
    if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
        return None;
    }

    let mut info = AudioInfoRaw::new();
    info.parse(param).ok()?;
    if info.format() != AudioFormat::F32LE {
        return None;
    }

    Some(NegotiatedFormat {
        rate: info.rate(),
        channels: usize::try_from(info.channels()).ok()?,
    })
}

pub(crate) fn node_latency(frames: usize, rate: u32) -> String {
    format!("{frames}/{rate}")
}

pub(crate) struct PipewireThread {
    quit: channel::Sender<()>,
    join: Option<JoinHandle<()>>,
}

impl PipewireThread {
    pub(crate) fn spawn<Body>(
        name: &str,
        body: Body,
    ) -> Result<(Self, NegotiatedFormat), AudioError>
    where
        Body:
            FnOnce(channel::Receiver<()>, StartupSender) -> Result<(), AudioError> + Send + 'static,
    {
        let (quit, quit_receiver) = channel::channel::<()>();
        let (startup, startup_receiver) = mpsc::channel();
        let failure_sender = startup.clone();

        let join = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                if let Err(error) = body(quit_receiver, startup) {
                    let _ = failure_sender.send(Err(error));
                }
            })
            .map_err(AudioError::ThreadSpawn)?;

        let mut running = Self {
            quit,
            join: Some(join),
        };

        match startup_receiver.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(format)) => Ok((running, format)),
            Ok(Err(error)) => {
                running.stop();
                Err(error)
            }
            Err(_) => {
                running.stop();
                Err(AudioError::PipeWireStartupTimeout {
                    waited: STARTUP_TIMEOUT,
                })
            }
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.join.as_ref().is_some_and(|join| !join.is_finished())
    }

    fn stop(&mut self) {
        let _ = self.quit.send(());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for PipewireThread {
    fn drop(&mut self) {
        self.stop();
    }
}
