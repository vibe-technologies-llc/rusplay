#![cfg(feature = "software")]

mod common;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use common::{FIXTURE_HEIGHT, FIXTURE_WIDTH, HostOptions, Outgoing, TestHost};
use parking_lot::Mutex;
use rusplay_client::{
    ClientError, ClientSession, Credentials, FrameColor, LatestFrame, OutputLight, Pin,
    PresentedFrame,
};
use rusplay_core::{ColorRange, ColorSpace, Resolution};
use rusplay_input::{InputEvent, StateRecord};
use rusplay_net::NetError;
use rusplay_protocol::{ControlMessage, CursorImage, CursorMessage};

const STREAM_TIMEOUT: Duration = Duration::from_secs(10);
const LOST_FRAME: u32 = 5;
const HELD_KEY: u32 = 30;
const SNAPSHOT_INTERVAL_MS: u16 = 30;
const AUDIO_PACKETS: u32 = 40;
const LOST_AUDIO_PACKET: u32 = 12;
const SAMPLES_PER_PACKET: usize = 240 * 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    frame: u32,
    keyframe: bool,
    resolution: Resolution,
    color: FrameColor,
}

fn recorder() -> (
    Arc<Mutex<Vec<Seen>>>,
    impl FnMut(PresentedFrame) + Send + 'static,
) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let seen = Arc::clone(&seen);
        move |frame: PresentedFrame| {
            seen.lock().push(Seen {
                frame: frame.frame,
                keyframe: frame.keyframe,
                resolution: frame.resolution(),
                color: frame.color,
            });
        }
    };

    (seen, sink)
}

#[test]
fn frames_arrive_decoded_at_the_source_resolution() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions {
        goodbye: false,
        ..HostOptions::default()
    });
    let session = ClientSession::new(common::client_config());
    let (seen, sink) = recorder();

    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        sink,
    )
    .unwrap();
    let paired = stream.paired_secret().is_some();
    let stats = settle(stream, &seen, common::DEFAULT_FRAMES - 1);

    let seen = seen.lock().clone();
    let source = Resolution::new(FIXTURE_WIDTH, FIXTURE_HEIGHT);

    assert!(paired);
    assert_eq!(stats.keyframe_requests_sent, 0);
    assert_eq!(stats.frames_dropped, 0);
    assert_eq!(stats.frames_decoded, u64::from(common::DEFAULT_FRAMES));
    assert!(first_is_keyframe_or_was_superseded(&seen, &stats));
    assert!(seen.iter().all(|frame| frame.resolution == source));
    assert!(seen.windows(2).all(|pair| pair[0].frame < pair[1].frame));
    let described = common::stream_description();
    assert_eq!(
        seen[0].color,
        FrameColor::of_stream(
            ColorSpace {
                range: ColorRange::Limited,
                ..described.color
            },
            described.format,
        )
    );
    assert_eq!(seen[0].color.light, OutputLight::Linear);
    assert!(
        !host
            .messages()
            .iter()
            .any(|message| matches!(message, ControlMessage::RequestKeyframe { .. }))
    );
}

#[test]
fn presented_texture_holds_the_picture() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions {
        goodbye: false,
        ..HostOptions::default()
    });
    let session = ClientSession::new(common::client_config());
    let latest = LatestFrame::new();

    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu.clone(),
        latest.clone(),
    )
    .unwrap();
    let frame = latest.wait(STREAM_TIMEOUT).unwrap();
    let texels = common::download(&gpu, &frame.texture);
    stream.stop().unwrap();

    let distinct: std::collections::BTreeSet<[u16; 4]> = texels.iter().copied().collect();

    assert_eq!(texels.len(), (FIXTURE_WIDTH * FIXTURE_HEIGHT) as usize);
    assert!(distinct.len() > 100);
    assert!(host.wait_for(STREAM_TIMEOUT, |message| *message
        == ControlMessage::Goodbye));
}

#[test]
fn frames_are_scaled_to_the_requested_target() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let frames = 48;
    let gate = Arc::new(AtomicBool::new(false));
    let host = TestHost::start(HostOptions {
        frames,
        goodbye: false,
        gate: Some(Arc::clone(&gate)),
        ..HostOptions::default()
    });
    let session = ClientSession::new(common::client_config());
    let (seen, sink) = recorder();
    let upscaled = Resolution::new(2 * FIXTURE_WIDTH, 2 * FIXTURE_HEIGHT);

    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        sink,
    )
    .unwrap();
    stream.set_target(Some(upscaled));
    gate.store(true, Ordering::Release);
    settle(stream, &seen, frames - 1);

    let seen = seen.lock().clone();

    assert!(seen.iter().all(|frame| frame.resolution == upscaled));
}

#[test]
fn fec_recovers_a_lost_fragment_and_loss_is_reported() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions {
        drop: Box::new(|outgoing: Outgoing| {
            outgoing.frame % 4 == 2 && !outgoing.parity && outgoing.index == 0
        }),
        goodbye: false,
        ..HostOptions::default()
    });
    let session = ClientSession::new(common::client_config());
    let (seen, sink) = recorder();

    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        sink,
    )
    .unwrap();
    let stats = settle(stream, &seen, common::DEFAULT_FRAMES - 1);

    assert!(stats.frames_recovered > 0);
    assert_eq!(stats.frames_lost, 0);
    assert_eq!(stats.frames_dropped, 0);
    assert_eq!(stats.decode_errors, 0);
    assert_eq!(stats.frames_decoded, u64::from(common::DEFAULT_FRAMES));
    assert!(stats.loss_reports_sent > 0);
    assert!(host.wait_for(STREAM_TIMEOUT, |message| matches!(
        message,
        ControlMessage::ReportLoss { .. }
    )));
}

#[test]
fn an_unrecoverable_frame_requests_a_keyframe_and_the_stream_resumes() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let frames = 120;
    let host = TestHost::start(HostOptions {
        frames,
        drop: Box::new(|outgoing: Outgoing| outgoing.frame == LOST_FRAME),
        goodbye: false,
        ..HostOptions::default()
    });
    let session = ClientSession::new(common::client_config());
    let (seen, sink) = recorder();

    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        sink,
    )
    .unwrap();
    let stats = settle(stream, &seen, frames - 1);

    let seen = seen.lock().clone();

    assert!(stats.frames_lost >= 1);
    assert!(stats.keyframe_requests_sent >= 1);
    assert!(stats.frames_skipped >= 1);
    assert_eq!(stats.decode_errors, 0);
    assert!(seen.iter().any(|frame| frame.frame > LOST_FRAME));
    assert!(host.wait_for(STREAM_TIMEOUT, |message| matches!(
        message,
        ControlMessage::RequestKeyframe { frame } if *frame == LOST_FRAME
    )));
}

#[test]
fn a_wrong_pin_is_refused_before_streaming() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions::default());
    let session = ClientSession::new(common::client_config());

    let result = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(Pin::parse("000000").unwrap()),
        gpu,
        LatestFrame::new(),
    );

    assert!(matches!(
        result,
        Err(ClientError::Net(NetError::PairingAuthentication))
    ));
}

#[test]
fn audio_is_decoded_and_a_lost_packet_is_concealed() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions {
        audio: common::opus_packets(AUDIO_PACKETS),
        audio_drop: Box::new(|number| number == LOST_AUDIO_PACKET),
        goodbye: false,
        ..HostOptions::default()
    });
    let session = ClientSession::new(common::client_config());
    let captured = common::CapturedAudio::default();

    let stream = session
        .connect_with_audio(
            host.address,
            &Credentials::Pin(common::pin()),
            gpu,
            LatestFrame::new(),
            Some(Box::new(captured.clone())),
        )
        .unwrap();
    let expected = AUDIO_PACKETS as usize * SAMPLES_PER_PACKET;
    let deadline = std::time::Instant::now() + STREAM_TIMEOUT;
    while captured.samples.lock().len() < expected && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let stats = stream.stats();
    stream.stop().unwrap();

    let samples = captured.samples.lock();
    assert_eq!(stats.audio_packets_received, u64::from(AUDIO_PACKETS - 1));
    assert_eq!(stats.audio_packets_lost, 1);
    assert_eq!(stats.audio_frames_recovered, 1);
    assert_eq!(stats.audio_decode_errors, 0);
    assert_eq!(samples.len(), expected);
    assert!(samples.iter().any(|sample| *sample != 0.0));
}

#[test]
fn cursor_position_and_image_reach_the_app() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let image = CursorImage {
        width: 32,
        height: 32,
        hotspot_x: 3,
        hotspot_y: 4,
        rgba: vec![200; 32 * 32 * 4],
    };
    let mut cursor: Vec<CursorMessage> = (0..10)
        .map(|step| CursorMessage {
            x: step * 10,
            y: step * 5,
            space: Resolution::new(1920, 1080),
            visible: true,
            image: None,
        })
        .collect();
    cursor[0].image = Some(image.clone());
    let host = TestHost::start(HostOptions {
        cursor,
        goodbye: false,
        ..HostOptions::default()
    });
    let session = ClientSession::new(common::client_config());

    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        LatestFrame::new(),
    )
    .unwrap();
    let latest = stream.cursor();
    let mut seen = 0;
    while seen < 10 {
        let Some(_) = latest.wait_after(seen, STREAM_TIMEOUT) else {
            break;
        };
        seen = latest.updates();
    }
    let cursor_now = latest.latest().unwrap();
    let stats = stream.stats();
    stream.stop().unwrap();

    assert_eq!((cursor_now.x, cursor_now.y), (90, 45));
    assert_eq!(*cursor_now.image.unwrap(), image);
    assert_eq!(stats.cursor_updates, 10);
}

#[test]
fn input_events_reach_the_host_in_order() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions {
        goodbye: false,
        ..HostOptions::default()
    });
    let session = ClientSession::new(common::client_config());
    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        LatestFrame::new(),
    )
    .unwrap();
    let input = stream.input();
    let key = |pressed| InputEvent::Key {
        keycode: 30,
        pressed,
    };

    input.send(key(true));
    input.send(InputEvent::PointerMotion { dx: 3.0, dy: -2.0 });
    input.send(key(false));
    let delivered = host.wait_for_input(STREAM_TIMEOUT, 3);
    let stats = stream.stats();
    stream.stop().unwrap();

    assert!(delivered);
    assert_eq!(
        host.input_events(),
        vec![
            key(true),
            InputEvent::PointerMotion { dx: 3.0, dy: -2.0 },
            key(false)
        ]
    );
    assert!(stats.input_packets_sent >= 1);
}

#[test]
fn held_state_snapshots_reach_the_host() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions {
        goodbye: false,
        ..HostOptions::default()
    });
    let mut config = common::client_config();
    config.input.snapshot_interval_ms = SNAPSHOT_INTERVAL_MS;
    let session = ClientSession::new(config);
    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        LatestFrame::new(),
    )
    .unwrap();

    stream.input().send(InputEvent::Key {
        keycode: HELD_KEY,
        pressed: true,
    });
    let held = host.wait_for_state(
        STREAM_TIMEOUT,
        |record| matches!(record, StateRecord::Keys(keys) if keys.contains(HELD_KEY)),
    );
    let stats = stream.stats();
    stream.stop().unwrap();

    assert!(held);
    assert!(stats.input_snapshots_sent >= 1);
}

fn settle(
    stream: rusplay_client::ClientStream,
    seen: &Mutex<Vec<Seen>>,
    last_frame: u32,
) -> rusplay_client::ClientStats {
    let deadline = std::time::Instant::now() + STREAM_TIMEOUT;
    while !seen.lock().iter().any(|frame| frame.frame == last_frame)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    let stats = stream.stats();
    stream.stop().unwrap();

    assert!(
        seen.lock().iter().any(|frame| frame.frame == last_frame),
        "frame {last_frame} was never presented"
    );

    stats
}

fn first_is_keyframe_or_was_superseded(seen: &[Seen], stats: &rusplay_client::ClientStats) -> bool {
    let first = seen[0];

    first.keyframe || stats.frames_superseded + stats.frames_unpresented >= u64::from(first.frame)
}
