#![cfg(feature = "pipewire")]

use std::{
    env,
    io::Cursor,
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::Duration,
};

use pipewire::{
    channel,
    context::ContextRc,
    keys,
    main_loop::MainLoopRc,
    properties::properties,
    spa::{
        param::{
            ParamType,
            format::{FormatProperties, MediaSubtype, MediaType},
            video::VideoFormat,
        },
        pod::{Object, Pod, Property, Value, serialize::PodSerializer},
        sys,
        utils::{Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Id, Rectangle, SpaTypes},
    },
    stream::{StreamFlags, StreamRc, StreamState},
};
use rusplay_capture::{
    CaptureSource, DrmFourcc, FrameBuffer, PipeWireCapture, PortalSession, StreamOptions,
};
use rusplay_config::Config;
use rusplay_core::Resolution;

const FRAME_WAIT: Duration = Duration::from_secs(5);
const SOURCE_SIZE: Resolution = Resolution::new(64, 48);
const SOURCE_STRIDE: u32 = SOURCE_SIZE.width * 4;
const SOURCE_FILL: u8 = 0xAB;

#[test]
#[ignore = "needs a Wayland session, the screencast portal and a click in its picker"]
fn portal_stream_delivers_a_frame() {
    let config = Config::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let session = runtime
        .block_on(PortalSession::open(&config.capture))
        .unwrap();
    let mut capture =
        PipeWireCapture::from_portal(session, StreamOptions::new(&config.video)).unwrap();
    let frame = capture.next_frame_timeout(FRAME_WAIT).unwrap().unwrap();

    assert!(frame.resolution.pixel_count() > 0);
}

#[test]
#[ignore = "needs a running PipeWire daemon"]
fn local_source_delivers_shared_memory_frames() {
    let source = TestSource::spawn();
    let options = StreamOptions::new(&Config::default().video);

    let mut capture = PipeWireCapture::connect(pipewire_socket(), source.node_id, options).unwrap();
    let frame = capture.next_frame().unwrap();

    assert_eq!(frame.resolution, SOURCE_SIZE);
    assert_eq!(frame.format, DrmFourcc::from_code(*b"XR24"));
    let FrameBuffer::Shm { data, stride } = frame.buffer else {
        panic!("expected a shared-memory frame");
    };
    assert_eq!(stride, SOURCE_STRIDE);
    assert!(data.iter().all(|byte| *byte == SOURCE_FILL));
}

fn pipewire_socket() -> OwnedFd {
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR").unwrap();
    let path = PathBuf::from(runtime_dir).join("pipewire-0");
    OwnedFd::from(UnixStream::connect(path).unwrap())
}

struct TestSource {
    node_id: u32,
    stop: channel::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TestSource {
    fn spawn() -> Self {
        let (stop, stop_rx) = channel::channel();
        let (ready_tx, ready_rx) = mpsc::channel();

        let thread = thread::spawn(move || run_source(&ready_tx, stop_rx));
        let node_id = ready_rx.recv_timeout(FRAME_WAIT).unwrap();

        Self {
            node_id,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for TestSource {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serialize(object: Object) -> Vec<u8> {
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .unwrap()
        .0
        .into_inner()
}

fn int(value: u32) -> Value {
    Value::Int(i32::try_from(value).unwrap())
}

fn source_format() -> Vec<u8> {
    serialize(Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: vec![
            Property::new(
                FormatProperties::MediaType.as_raw(),
                Value::Id(Id(MediaType::Video.as_raw())),
            ),
            Property::new(
                FormatProperties::MediaSubtype.as_raw(),
                Value::Id(Id(MediaSubtype::Raw.as_raw())),
            ),
            Property::new(
                FormatProperties::VideoFormat.as_raw(),
                Value::Id(Id(VideoFormat::BGRx.as_raw())),
            ),
            Property::new(
                FormatProperties::VideoSize.as_raw(),
                Value::Rectangle(Rectangle {
                    width: SOURCE_SIZE.width,
                    height: SOURCE_SIZE.height,
                }),
            ),
            Property::new(
                FormatProperties::VideoFramerate.as_raw(),
                Value::Fraction(Fraction { num: 60, denom: 1 }),
            ),
        ],
    })
}

fn source_buffers() -> Vec<u8> {
    let data_types = (1 << sys::SPA_DATA_MemFd) | (1 << sys::SPA_DATA_MemPtr);

    serialize(Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            Property::new(sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
            Property::new(
                sys::SPA_PARAM_BUFFERS_size,
                int(SOURCE_STRIDE * SOURCE_SIZE.height),
            ),
            Property::new(sys::SPA_PARAM_BUFFERS_stride, int(SOURCE_STRIDE)),
            Property::new(
                sys::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(pipewire::spa::pod::ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Flags {
                        default: data_types,
                        flags: Vec::new(),
                    },
                ))),
            ),
        ],
    })
}

fn run_source(ready: &mpsc::Sender<u32>, stop: channel::Receiver<()>) {
    pipewire::init();

    let mainloop = MainLoopRc::new(None).unwrap();
    let context = ContextRc::new(&mainloop, None).unwrap();
    let core = context.connect_rc(None).unwrap();
    let stream = StreamRc::new(
        core,
        "rusplay-test-source",
        properties! {
            *keys::MEDIA_CLASS => "Video/Source",
            *keys::MEDIA_TYPE => "Video",
            *keys::MEDIA_CATEGORY => "Source",
        },
    )
    .unwrap();

    let ready = ready.clone();
    let _listener = stream
        .add_local_listener_with_user_data(false)
        .state_changed(move |stream, announced, _, new| {
            if !*announced && new == StreamState::Paused {
                *announced = true;
                let _ = ready.send(stream.node_id());
            }
        })
        .param_changed(|stream, _, id, param| {
            if id == ParamType::Format.as_raw() && param.is_some() {
                let buffers = source_buffers();
                let mut params = [Pod::from_bytes(&buffers).unwrap()];
                stream.update_params(&mut params).unwrap();
            }
        })
        .process(|stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            if let Some(bytes) = data.data() {
                bytes.fill(SOURCE_FILL);
            }
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.size_mut() = SOURCE_STRIDE * SOURCE_SIZE.height;
            *chunk.stride_mut() = i32::try_from(SOURCE_STRIDE).unwrap();
        })
        .register()
        .unwrap();

    let format = source_format();
    stream
        .connect(
            Direction::Output,
            None,
            StreamFlags::MAP_BUFFERS,
            &mut [Pod::from_bytes(&format).unwrap()],
        )
        .unwrap();

    let _stop = stop.attach(mainloop.loop_(), {
        let mainloop = mainloop.downgrade();
        move |()| {
            if let Some(mainloop) = mainloop.upgrade() {
                mainloop.quit();
            }
        }
    });

    mainloop.run();
}
