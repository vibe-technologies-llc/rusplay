#![cfg(feature = "pipewire")]

use std::{
    env,
    fs::File,
    io::Cursor,
    mem,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::net::UnixStream,
    },
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
        pod::{
            ChoiceValue, Object, Pod, Property, PropertyFlags, Value, deserialize::PodDeserializer,
            serialize::PodSerializer,
        },
        sys,
        utils::{Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Id, Rectangle, SpaTypes},
    },
    stream::{Stream, StreamFlags, StreamRc, StreamState},
    sys as pw_sys,
};
use rusplay_capture::{
    CaptureSource, DRM_FORMAT_MOD_LINEAR, DrmFourcc, FrameBuffer, PipeWireCapture, PortalSession,
    Received, RestoreTokenStore, StreamOptions,
};
use rusplay_config::Config;
use rusplay_core::Resolution;

const FRAME_WAIT: Duration = Duration::from_secs(5);
const SOURCE_SIZE: Resolution = Resolution::new(64, 48);
const SOURCE_STRIDE: u32 = SOURCE_SIZE.width * 4;
const SOURCE_BYTES: u32 = SOURCE_STRIDE * SOURCE_SIZE.height;
const SOURCE_FILL: u8 = 0xAB;
const AMD_TILED: u64 = 0x0200_0000_0000_0001;
const FRAMES_TO_CYCLE: usize = 12;
const CURSOR_EDGE: u32 = 2;
const CURSOR_PIXELS: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

#[test]
#[ignore = "needs a Wayland session, the screencast portal and a click in its picker"]
fn portal_stream_delivers_a_frame() {
    let dir = tempfile::tempdir().unwrap();
    let store = RestoreTokenStore::new(dir.path().join("restore-token"));
    let config = Config::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let session = runtime
        .block_on(PortalSession::open_persistent(&config.capture, &store))
        .unwrap();
    let session_token = session.restore_token.clone();
    let mut capture =
        PipeWireCapture::from_portal(session, StreamOptions::new(&config.capture, &config.video))
            .unwrap();
    let frame = capture.next_frame_timeout(FRAME_WAIT).unwrap().unwrap();

    assert!(frame.resolution.pixel_count() > 0);
    assert_eq!(store.load().unwrap(), session_token);
}

#[test]
#[ignore = "needs a running PipeWire daemon"]
fn local_source_delivers_shared_memory_frames() {
    let source = TestSource::spawn(SourceMemory::Shared);
    let config = Config::default();
    let options = StreamOptions::new(&config.capture, &config.video);

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

#[test]
#[ignore = "needs a running PipeWire daemon"]
fn local_source_negotiates_a_dmabuf_modifier() {
    let source = TestSource::spawn(SourceMemory::DmaBuf);
    let config = Config::default();
    let options = StreamOptions::new(&config.capture, &config.video);

    let mut capture = PipeWireCapture::connect(pipewire_socket(), source.node_id, options).unwrap();

    for _ in 0..FRAMES_TO_CYCLE {
        let frame = capture.next_frame_timeout(FRAME_WAIT).unwrap().unwrap();
        let FrameBuffer::DmaBuf {
            modifier, planes, ..
        } = frame.buffer
        else {
            panic!("expected a dma-buf frame");
        };
        assert_eq!(modifier, DRM_FORMAT_MOD_LINEAR);
        assert_eq!(planes.len(), 1);
        assert_eq!(planes[0].stride, SOURCE_STRIDE);
    }
}

#[test]
#[ignore = "needs a running PipeWire daemon"]
fn dmabuf_off_falls_back_to_shared_memory() {
    let source = TestSource::spawn(SourceMemory::Shared);
    let config = Config::default();
    let mut options = StreamOptions::new(&config.capture, &config.video);
    options.prefer_dmabuf = false;

    let mut capture = PipeWireCapture::connect(pipewire_socket(), source.node_id, options).unwrap();
    let frame = capture.next_frame().unwrap();

    assert!(matches!(frame.buffer, FrameBuffer::Shm { .. }));
}

#[test]
#[ignore = "needs a running PipeWire daemon"]
fn local_source_forwards_cursor_metadata_once_per_bitmap() {
    let source = TestSource::spawn(SourceMemory::Shared);
    let config = Config::default();
    let options = StreamOptions::new(&config.capture, &config.video);

    let mut capture = PipeWireCapture::connect(pipewire_socket(), source.node_id, options).unwrap();
    let feed = capture.cursor_feed().unwrap();
    for _ in 0..FRAMES_TO_CYCLE {
        capture.next_frame().unwrap();
    }
    let first = feed.recv_timeout(FRAME_WAIT);
    capture.next_frame().unwrap();
    let later = feed.recv_timeout(FRAME_WAIT);

    let Received::Value(first) = first else {
        panic!("expected a cursor update, got {first:?}");
    };
    let bitmap = first.bitmap.unwrap();
    assert_eq!((bitmap.width, bitmap.height), (CURSOR_EDGE, CURSOR_EDGE));
    assert_eq!((bitmap.hotspot_x, bitmap.hotspot_y), (1, 0));
    assert_eq!(bitmap.rgba, CURSOR_PIXELS);
    assert!(first.visible);
    assert_eq!(first.y, 10);
    let Received::Value(later) = later else {
        panic!("expected a cursor update, got {later:?}");
    };
    assert!(later.x > first.x);
    assert_eq!(later.bitmap, None);
}

fn pipewire_socket() -> OwnedFd {
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR").unwrap();
    let path = PathBuf::from(runtime_dir).join("pipewire-0");
    OwnedFd::from(UnixStream::connect(path).unwrap())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceMemory {
    Shared,
    DmaBuf,
}

struct SourceState {
    memory: SourceMemory,
    announced: bool,
    ready: mpsc::Sender<u32>,
    backing: Vec<File>,
    produced: i32,
}

struct TestSource {
    node_id: u32,
    stop: channel::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TestSource {
    fn spawn(memory: SourceMemory) -> Self {
        let (stop, stop_rx) = channel::channel();
        let (ready, ready_rx) = mpsc::channel();

        let thread = thread::spawn(move || run_source(memory, ready, stop_rx));
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

fn source_format(memory: SourceMemory) -> Vec<u8> {
    let mut properties = vec![
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
    ];
    if memory == SourceMemory::DmaBuf {
        let modifiers = vec![DRM_FORMAT_MOD_LINEAR.cast_signed(), AMD_TILED.cast_signed()];
        properties.push(Property {
            key: FormatProperties::VideoModifier.as_raw(),
            flags: PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE,
            value: Value::Choice(ChoiceValue::Long(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: modifiers[0],
                    alternatives: modifiers,
                },
            ))),
        });
    }

    serialize(Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties,
    })
}

fn source_buffers(memory: SourceMemory) -> Vec<u8> {
    let data_types = match memory {
        SourceMemory::Shared => (1 << sys::SPA_DATA_MemFd) | (1 << sys::SPA_DATA_MemPtr),
        SourceMemory::DmaBuf => 1 << sys::SPA_DATA_DmaBuf,
    };

    serialize(Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            Property::new(sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
            Property::new(sys::SPA_PARAM_BUFFERS_size, int(SOURCE_BYTES)),
            Property::new(sys::SPA_PARAM_BUFFERS_stride, int(SOURCE_STRIDE)),
            Property::new(
                sys::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(ChoiceValue::Int(Choice(
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

fn modifier_still_open(format: &Pod) -> bool {
    let Ok((_, Value::Object(object))) = PodDeserializer::deserialize_any_from(format.as_bytes())
    else {
        return false;
    };

    object.properties.iter().any(|property| {
        property.key == FormatProperties::VideoModifier.as_raw()
            && matches!(
                property.value,
                Value::Choice(ChoiceValue::Long(Choice(_, ChoiceEnum::Enum { .. })))
            )
    })
}

fn on_source_format(stream: &Stream, state: &SourceState, format: &Pod) {
    if modifier_still_open(format) {
        return;
    }

    let buffers = source_buffers(state.memory);
    let cursor = source_cursor_meta();
    stream
        .update_params(&mut [
            Pod::from_bytes(&buffers).unwrap(),
            Pod::from_bytes(&cursor).unwrap(),
        ])
        .unwrap();
}

fn back_with_memory(state: &mut SourceState, buffer: *mut pw_sys::pw_buffer) {
    if state.memory != SourceMemory::DmaBuf {
        return;
    }
    let file = tempfile::tempfile().unwrap();
    file.set_len(u64::from(SOURCE_BYTES)).unwrap();

    let data = unsafe { &mut *(*(*buffer).buffer).datas };
    data.type_ = sys::SPA_DATA_DmaBuf;
    data.flags = sys::SPA_DATA_FLAG_READABLE;
    data.fd = i64::from(file.as_raw_fd());
    data.mapoffset = 0;
    data.maxsize = SOURCE_BYTES;
    data.data = std::ptr::null_mut();
    state.backing.push(file);
}

fn cursor_meta_size() -> usize {
    mem::size_of::<sys::spa_meta_cursor>()
        + mem::size_of::<sys::spa_meta_bitmap>()
        + CURSOR_PIXELS.len()
}

fn source_cursor_meta() -> Vec<u8> {
    serialize(Object {
        type_: SpaTypes::ObjectParamMeta.as_raw(),
        id: ParamType::Meta.as_raw(),
        properties: vec![
            Property::new(
                sys::SPA_PARAM_META_type,
                Value::Id(Id(sys::SPA_META_Cursor)),
            ),
            Property::new(
                sys::SPA_PARAM_META_size,
                Value::Int(i32::try_from(cursor_meta_size()).unwrap()),
            ),
        ],
    })
}

fn write_cursor(buffer: *mut sys::spa_buffer, x: i32) {
    let cursor =
        unsafe { sys::spa_buffer_find_meta_data(buffer, sys::SPA_META_Cursor, cursor_meta_size()) }
            .cast::<sys::spa_meta_cursor>();
    if cursor.is_null() {
        return;
    }

    let cursor_size = mem::size_of::<sys::spa_meta_cursor>();
    let bitmap_size = mem::size_of::<sys::spa_meta_bitmap>();
    unsafe {
        (*cursor).id = 1;
        (*cursor).flags = 0;
        (*cursor).position = sys::spa_point { x, y: 10 };
        (*cursor).hotspot = sys::spa_point { x: 1, y: 0 };
        (*cursor).bitmap_offset = u32::try_from(cursor_size).unwrap();

        let bitmap = cursor.add(1).cast::<sys::spa_meta_bitmap>();
        (*bitmap).format = VideoFormat::RGBA.as_raw();
        (*bitmap).size = sys::spa_rectangle {
            width: CURSOR_EDGE,
            height: CURSOR_EDGE,
        };
        (*bitmap).stride = i32::try_from(CURSOR_EDGE * 4).unwrap();
        (*bitmap).offset = u32::try_from(bitmap_size).unwrap();
        bitmap
            .add(1)
            .cast::<u8>()
            .copy_from_nonoverlapping(CURSOR_PIXELS.as_ptr(), CURSOR_PIXELS.len());
    }
}

fn produce(stream: &Stream, state: &mut SourceState) {
    let raw = unsafe { stream.dequeue_raw_buffer() };
    if raw.is_null() {
        return;
    }

    let buffer = unsafe { (*raw).buffer };
    let data = unsafe { &mut *(*buffer).datas };
    if !data.data.is_null() {
        unsafe {
            data.data
                .cast::<u8>()
                .write_bytes(SOURCE_FILL, usize::try_from(SOURCE_BYTES).unwrap());
        }
    }
    let chunk = unsafe { &mut *data.chunk };
    chunk.offset = 0;
    chunk.size = SOURCE_BYTES;
    chunk.stride = i32::try_from(SOURCE_STRIDE).unwrap();

    state.produced += 1;
    write_cursor(buffer, state.produced);

    unsafe { stream.queue_raw_buffer(raw) };
}

fn run_source(memory: SourceMemory, ready: mpsc::Sender<u32>, stop: channel::Receiver<()>) {
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

    let state = SourceState {
        memory,
        announced: false,
        ready,
        backing: Vec::new(),
        produced: 0,
    };
    let _listener = stream
        .add_local_listener_with_user_data(state)
        .state_changed(|stream, state, _, new| {
            if !state.announced && new == StreamState::Paused {
                state.announced = true;
                let _ = state.ready.send(stream.node_id());
            }
        })
        .param_changed(|stream, state, id, param| {
            if let (true, Some(format)) = (id == ParamType::Format.as_raw(), param) {
                on_source_format(stream, state, format);
            }
        })
        .add_buffer(|_, state, buffer| back_with_memory(state, buffer))
        .process(produce)
        .register()
        .unwrap();

    let flags = match memory {
        SourceMemory::Shared => StreamFlags::MAP_BUFFERS,
        SourceMemory::DmaBuf => StreamFlags::ALLOC_BUFFERS,
    };
    let format = source_format(memory);
    stream
        .connect(
            Direction::Output,
            None,
            flags,
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
