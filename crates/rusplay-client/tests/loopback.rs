#![recursion_limit = "256"]
#![cfg(feature = "software")]

mod common;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use common::{FIXTURE_HEIGHT, FIXTURE_WIDTH, HostOptions, Outgoing, TestHost};
use parking_lot::Mutex;
use rusplay_audio::AudioError;
use rusplay_client::{
    AudioInput, ClientError, ClientSession, ColorVolume, Credentials, FrameColor, LatestFrame,
    OutputLight, Pin, PresentedFrame, announced_display,
};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, CodecFormat, ColorPrimaries, ColorRange, ColorSpace,
    FormatSet, FrameRate, PixelFormat, Resolution,
};
use rusplay_input::{InputEvent, StateRecord};
use rusplay_net::NetError;
use rusplay_protocol::{ClientDisplay, ControlMessage, CursorImage, CursorMessage};

const STREAM_TIMEOUT: Duration = Duration::from_secs(10);
const UNARMED: u32 = u32::MAX;
const FRAMES_BEFORE_LOSS: usize = 3;
const FRAMES_AFTER_LOSS: usize = 2;
const SECOND_KEYFRAME: u32 = 8;
const HELD_KEY: u32 = 30;
const SNAPSHOT_INTERVAL_MS: u16 = 30;
const AUDIO_PACKETS: u32 = 40;
const LOST_AUDIO_PACKET: u32 = 12;
const SAMPLES_PER_PACKET: usize = 240 * 2;
const MICROPHONE_PACKETS: usize = 20;
const MICROPHONE_LEVEL: f32 = 0.25;
const MICROPHONE_INTERVAL: Duration = Duration::from_millis(10);

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

const NICE_FIELD_AFTER_COMM: usize = 16;
const TEST_NICE: i8 = 7;

fn current_thread_nice() -> i32 {
    let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap();
    let after_comm = &stat[stat.rfind(')').unwrap() + 1..];

    after_comm
        .split_whitespace()
        .nth(NICE_FIELD_AFTER_COMM)
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn the_stage_threads_take_the_configured_nice_value() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions {
        goodbye: false,
        ..HostOptions::default()
    });
    let mut config = common::client_config();
    config.network.frame_thread_nice = TEST_NICE;
    let session = ClientSession::new(config);
    let present_nice = Arc::new(Mutex::new(None));
    let (seen, mut recording) = recorder();
    let sink = {
        let present_nice = Arc::clone(&present_nice);
        move |frame: PresentedFrame| {
            present_nice.lock().get_or_insert_with(current_thread_nice);
            recording(frame);
        }
    };

    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        sink,
    )
    .unwrap();
    let stats = settle(stream, &seen, common::DEFAULT_FRAMES - 1);

    assert_eq!(*present_nice.lock(), Some(i32::from(TEST_NICE)));
    assert_eq!(stats.thread_priority_failures, 0);
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
    assert_eq!(stats.recovery_requests_sent, 0);
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
        .mapped_to(&ColorVolume::srgb_display())
    );
    assert_eq!(seen[0].color.light, OutputLight::Linear);
    assert_eq!(seen[0].color.space.primaries, ColorPrimaries::Bt709);
    assert!(
        !host
            .messages()
            .iter()
            .any(|message| matches!(message, ControlMessage::RequestKeyframe { .. }))
    );
}

#[test]
fn latency_is_measured_from_the_host_capture_time_across_a_skewed_clock() {
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
    let stats = settle(stream, &seen, common::DEFAULT_FRAMES - 1);

    let ceiling = Duration::from_secs(2);
    let offset = stats.clock_offset_micros.unwrap();
    let skew = i64::try_from(common::HOST_CLOCK_SKEW.as_micros()).unwrap();

    assert!((skew - 2_000_000..=skew + 2_000_000).contains(&offset));
    assert!(stats.capture_to_receive.unwrap() < ceiling);
    assert!(stats.capture_to_decode.unwrap() < ceiling);
    assert!(stats.capture_to_present.unwrap() < ceiling);
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
fn a_target_change_rescales_the_last_frame_without_new_video() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let frames = 8;
    let host = TestHost::start(HostOptions {
        frames,
        goodbye: false,
        ..HostOptions::default()
    });
    let session = ClientSession::new(common::client_config());
    let latest = LatestFrame::new();
    let native = Resolution::new(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    let upscaled = Resolution::new(2 * FIXTURE_WIDTH, 2 * FIXTURE_HEIGHT);
    let downscaled = Resolution::new(FIXTURE_WIDTH / 2, FIXTURE_HEIGHT / 2);

    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu.clone(),
        latest.clone(),
    )
    .unwrap();
    let last = wait_for_frame(&latest, |frame| frame.frame == frames - 1);
    let presented = latest.presented();

    stream.set_target(Some(upscaled));
    let up = wait_for_frame(&latest, |frame| frame.resolution() == upscaled);
    let texels = common::download(&gpu, &up.texture);
    let distinct: std::collections::BTreeSet<[u16; 4]> = texels.iter().copied().collect();

    stream.set_target(Some(downscaled));
    let down = wait_for_frame(&latest, |frame| frame.resolution() == downscaled);

    stream.set_target(Some(native));
    let back = wait_for_frame(&latest, |frame| frame.resolution() == native);
    let restored = common::download(&gpu, &back.texture);
    let stats = stream.stats();
    stream.stop().unwrap();

    assert_eq!(last.resolution(), native);
    assert_eq!(up.frame, frames - 1);
    assert_eq!(down.frame, frames - 1);
    assert_eq!(back.frame, frames - 1);
    assert!(distinct.len() > 100);
    assert_eq!(restored, common::download(&gpu, &last.texture));
    assert_eq!(stats.frames_presented, presented);
}

fn wait_for_frame(
    latest: &LatestFrame,
    wanted: impl Fn(&PresentedFrame) -> bool,
) -> PresentedFrame {
    let deadline = std::time::Instant::now() + STREAM_TIMEOUT;

    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let frame = latest
            .wait(remaining)
            .expect("the wanted frame was never presented");
        if wanted(&frame) {
            return frame;
        }
    }
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
fn an_unrecoverable_frame_requests_a_recovery_and_the_stream_resumes() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let frames = 120;
    let armed = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicU32::new(UNARMED));
    let drop_rule = {
        let armed = Arc::clone(&armed);
        let lost = Arc::clone(&lost);
        move |outgoing: Outgoing| {
            if armed.load(Ordering::Acquire) && outgoing.frames_to_keyframe > FRAMES_AFTER_LOSS {
                lost.compare_exchange(UNARMED, outgoing.frame, Ordering::AcqRel, Ordering::Acquire)
                    .ok();
            }
            lost.load(Ordering::Acquire) == outgoing.frame
        }
    };
    let host = TestHost::start(HostOptions {
        frames,
        drop: Box::new(drop_rule),
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
    let deadline = std::time::Instant::now() + STREAM_TIMEOUT;
    while seen.lock().len() < FRAMES_BEFORE_LOSS && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    armed.store(true, Ordering::Release);
    let stats = settle(stream, &seen, frames - 1);

    let seen = seen.lock().clone();
    let ceiling_kbps = common::client_config().video.bitrate_kbps;
    let lost_frame = lost.load(Ordering::Acquire);

    assert_ne!(lost_frame, UNARMED);
    assert!(stats.frames_lost >= 1);
    assert!(stats.recovery_requests_sent >= 1);
    assert!(stats.frame_acks_sent >= 1);
    assert_eq!(stats.decode_errors, 0);
    assert!(seen.iter().any(|frame| frame.frame > lost_frame));
    assert!(host.wait_for(STREAM_TIMEOUT, |message| matches!(
        message,
        ControlMessage::RequestRecovery { frame, last_good }
            if *frame == lost_frame && *last_good < lost_frame
    )));
    assert!(host.wait_for(STREAM_TIMEOUT, |message| matches!(
        message,
        ControlMessage::AcknowledgeFrame { frame } if *frame < lost_frame
    )));
    assert!(stats.bitrate_hints_sent >= 1);
    assert!(host.wait_for(STREAM_TIMEOUT, |message| matches!(
        message,
        ControlMessage::BitrateHint { kbps } if *kbps < ceiling_kbps
    )));
}

#[test]
fn an_unrecoverable_last_frame_requests_a_recovery_while_no_frame_follows_it() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions {
        frames: SECOND_KEYFRAME + 1,
        drop: Box::new(|outgoing: Outgoing| outgoing.frame == SECOND_KEYFRAME && !outgoing.parity),
        goodbye: false,
        ..HostOptions::default()
    });
    let mut config = common::client_config();
    config.network.reassembly_timeout_ms = u16::MAX;
    let session = ClientSession::new(config);

    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        LatestFrame::new(),
    )
    .unwrap();
    let requested = host.wait_for(STREAM_TIMEOUT, |message| {
        matches!(
            message,
            ControlMessage::RequestRecovery { frame, .. } | ControlMessage::RequestKeyframe { frame }
                if *frame == SECOND_KEYFRAME
        )
    });
    let stats = stream.stats();
    stream.stop().unwrap();

    assert!(requested);
    assert!(stats.frames_lost >= 1);
    assert_eq!(stats.decode_errors, 0);
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

fn software_decodable() -> FormatSet {
    rusplay_decode::capabilities(&common::client_config().video.decoder)
        .expect("software decoder capabilities")
        .formats()
}

fn nothing_the_client_decodes() -> FormatSet {
    let decodable = software_decodable();
    let outside: FormatSet = CodecFormat::all()
        .filter(|format| !decodable.contains(*format))
        .collect();

    if outside.is_empty() {
        FormatSet::EMPTY
    } else {
        outside
    }
}

#[test]
fn the_hello_names_what_the_client_decodes_and_a_fallback_is_visible() {
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
    let deadline = std::time::Instant::now() + STREAM_TIMEOUT;
    while stream.stream_format().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let format = stream.stream_format();
    stream.stop().unwrap();

    let format = format.expect("the host's stream format never arrived");
    assert!(format.is_fallback());
    assert_eq!(format.coded().codec, Codec::Av1);
    assert_eq!(
        format.configured,
        CodecFormat::new(
            Codec::Hevc,
            PixelFormat {
                chroma: ChromaSubsampling::Yuv422,
                depth: BitDepth::Eight,
            },
        )
    );
    assert!(host.messages().contains(&ControlMessage::Hello {
        version: rusplay_protocol::ProtocolVersion::CURRENT,
        formats: software_decodable(),
    }));
}

#[test]
fn the_hello_is_followed_by_the_display_the_client_was_given() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions {
        goodbye: false,
        ..HostOptions::default()
    });
    let display = announced_display(
        Resolution::new(3440, 1440),
        FrameRate::new(143_981, 1000),
        &ColorVolume::srgb_display(),
    )
    .unwrap();
    let session = ClientSession::new(common::client_config()).with_display(display);

    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        LatestFrame::new(),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + STREAM_TIMEOUT;
    while !host.messages().contains(&ControlMessage::Display(display))
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    stream.stop().unwrap();

    assert!(host.messages().contains(&ControlMessage::Display(display)));
}

fn displays(host: &TestHost) -> Vec<ClientDisplay> {
    host.messages()
        .into_iter()
        .filter_map(|message| match message {
            ControlMessage::Display(display) => Some(display),
            _ => None,
        })
        .collect()
}

#[test]
fn a_resize_drag_announces_only_the_size_it_settles_on() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions {
        goodbye: false,
        ..HostOptions::default()
    });
    let announce = |width, height| {
        announced_display(
            Resolution::new(width, height),
            FrameRate::integer(60),
            &ColorVolume::srgb_display(),
        )
        .unwrap()
    };
    let mut config = common::client_config();
    config.video.display_debounce_ms = 1000;
    let session = ClientSession::new(config).with_display(announce(1280, 720));

    let stream = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        LatestFrame::new(),
    )
    .unwrap();
    assert!(host.wait_for(STREAM_TIMEOUT, |message| matches!(
        message,
        ControlMessage::Display(_)
    )));

    for step in 0..30 {
        stream.announce_display(announce(1300 + step, 720));
    }
    let settled = announce(1329, 720);
    let settled_arrived = host.wait_for(STREAM_TIMEOUT, |message| {
        *message == ControlMessage::Display(settled)
    });
    let marker = announce(1600, 900);
    stream.announce_display(marker);
    let marker_arrived = host.wait_for(STREAM_TIMEOUT, |message| {
        *message == ControlMessage::Display(marker)
    });
    let sent = stream.stats().display_updates_sent;
    stream.stop().unwrap();

    assert!(settled_arrived);
    assert!(marker_arrived);
    assert_eq!(displays(&host), [announce(1280, 720), settled, marker]);
    assert_eq!(sent, 2);
}

#[test]
fn a_host_encoding_nothing_the_client_decodes_is_a_specific_error() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let encodable = nothing_the_client_decodes();
    let host = TestHost::start(HostOptions {
        encodable,
        ..HostOptions::default()
    });
    let session = ClientSession::new(common::client_config());

    let result = common::connect_silent(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        LatestFrame::new(),
    );

    assert!(matches!(
        result,
        Err(ClientError::NoCommonFormat { decodable, encodable })
            if decodable == software_decodable() && encodable == nothing_the_client_decodes()
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

struct ConstantMicrophone {
    next: std::time::Instant,
}

impl AudioInput for ConstantMicrophone {
    fn read_frame(&mut self, frame: &mut [f32], timeout: Duration) -> Result<bool, AudioError> {
        let wait = self
            .next
            .saturating_duration_since(std::time::Instant::now());
        if wait > timeout {
            std::thread::sleep(timeout);
            return Ok(false);
        }
        std::thread::sleep(wait);

        frame.fill(MICROPHONE_LEVEL);
        self.next += MICROPHONE_INTERVAL;

        Ok(true)
    }
}

fn microphone_config(enabled: bool) -> rusplay_config::Config {
    let mut config = common::client_config();
    config.audio.microphone.enabled = enabled;
    config
}

fn wait_until(timeout: Duration, condition: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn the_microphone_is_encoded_numbered_and_sent_to_the_host() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions {
        goodbye: false,
        ..HostOptions::default()
    });
    let session = ClientSession::new(microphone_config(true));

    let stream = common::connect_with_microphone(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        LatestFrame::new(),
        Box::new(ConstantMicrophone {
            next: std::time::Instant::now(),
        }),
    )
    .unwrap();
    let arrived = host.wait_for_microphone(STREAM_TIMEOUT, MICROPHONE_PACKETS);
    let counted = wait_until(STREAM_TIMEOUT, || {
        stream.stats().microphone_packets_sent >= MICROPHONE_PACKETS as u64
    });
    stream.stop().unwrap();

    let frames = host.microphone_frames();
    assert!(arrived);
    assert!(counted);
    assert!(
        frames
            .iter()
            .zip(0..)
            .all(|(frame, number)| frame.frame == number)
    );
    assert!(frames.len() >= MICROPHONE_PACKETS - 1);
}

#[test]
fn a_disabled_microphone_sends_nothing() {
    let Some(gpu) = common::gpu() else {
        return;
    };
    let host = TestHost::start(HostOptions {
        goodbye: false,
        ..HostOptions::default()
    });
    let session = ClientSession::new(microphone_config(false));

    let stream = common::connect_with_microphone(
        &session,
        host.address,
        &Credentials::Pin(common::pin()),
        gpu,
        LatestFrame::new(),
        Box::new(ConstantMicrophone {
            next: std::time::Instant::now(),
        }),
    )
    .unwrap();
    let arrived = host.wait_for_microphone(Duration::from_millis(300), 1);
    let stats = stream.stats();
    stream.stop().unwrap();

    assert!(!arrived);
    assert_eq!(stats.microphone_packets_sent, 0);
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
    let deadline = std::time::Instant::now() + STREAM_TIMEOUT;
    while stream.stats().input_packets_sent == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
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
