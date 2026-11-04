use std::{thread, time::Duration};

use rusplay_client::{ClientStream, ColorVolume, Credentials, PeerRecord, announced_display};
use rusplay_core::{FrameRate, Resolution};

use super::{FIRST_FRAME_WAIT, Live, Recording, SERIAL, connect, gpu, recorder, stack::wait_for};

const DISPLAY: (u32, u32, u32) = (1280, 720, 60_000);
const RESIZE_WAIT: Duration = Duration::from_secs(10);
const SUMMARY_WAIT: Duration = Duration::from_secs(5);
const KWIN_MINIMUM: u32 = 200;
const FLOOD_STEPS: u32 = 120;

fn announce(stream: &ClientStream, width: u32, height: u32) {
    let display = announced_display(
        Resolution::new(width, height),
        FrameRate::integer(60),
        &ColorVolume::srgb_display(),
    )
    .unwrap();

    stream.announce_display(display);
}

fn reached(seen: &Recording, size: Resolution) -> bool {
    seen.lock().iter().any(|entry| entry.source == size)
}

fn sizes(seen: &Recording) -> Vec<Resolution> {
    let mut sizes: Vec<Resolution> = Vec::new();
    for entry in seen.lock().iter() {
        if sizes.last() != Some(&entry.source) {
            sizes.push(entry.source);
        }
    }

    sizes
}

fn counter(line: &str, name: &str) -> u64 {
    line.split(name)
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no {name} in {line:?}"))
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn a_live_resize_takes_the_stream_to_each_new_size_on_kwin() {
    let _serial = SERIAL.lock();
    let Some(live) = Live::start(true, "") else {
        return;
    };
    let Some(gpu) = gpu() else {
        return;
    };

    let (seen, sink) = recorder();
    let stream = connect(&live, gpu, &Credentials::Pin(live.pin()), DISPLAY, sink).unwrap();
    assert!(wait_for(FIRST_FRAME_WAIT, || reached(
        &seen,
        Resolution::new(1280, 720)
    )));

    let steps = [(1000, 700), (1600, 900), (800, 600)];
    for (width, height) in steps {
        announce(&stream, width, height);
        assert!(
            wait_for(RESIZE_WAIT, || reached(
                &seen,
                Resolution::new(width, height)
            )),
            "never reached {width}x{height}, saw {:?}",
            sizes(&seen)
        );
    }
    let client = stream.stats();
    let format = stream.stream_format().unwrap();
    stream.stop().unwrap();

    let summary = live
        .daemon
        .wait_for_line("session summary", SUMMARY_WAIT)
        .expect("no session summary");
    eprintln!("REPORT sizes {:?}", sizes(&seen));
    eprintln!("REPORT {summary}");

    assert_eq!(format.resolution, Resolution::new(800, 600));
    assert_eq!(client.display_updates_sent, 3);
    assert!(counter(&summary, "virtual resizes ") >= 3);
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn a_window_below_the_compositor_minimum_streams_at_the_minimum_on_kwin() {
    let _serial = SERIAL.lock();
    let Some(live) = Live::start(true, "") else {
        return;
    };
    let Some(gpu) = gpu() else {
        return;
    };

    let (seen, sink) = recorder();
    let stream = connect(&live, gpu, &Credentials::Pin(live.pin()), DISPLAY, sink).unwrap();
    assert!(wait_for(FIRST_FRAME_WAIT, || reached(
        &seen,
        Resolution::new(1280, 720)
    )));

    announce(&stream, 120, 90);
    let grown = Resolution::new(267, KWIN_MINIMUM);
    let arrived = wait_for(RESIZE_WAIT, || reached(&seen, grown));
    thread::sleep(Duration::from_millis(500));
    stream.stop().unwrap();

    eprintln!("REPORT sizes {:?}", sizes(&seen));
    assert!(arrived, "saw {:?}", sizes(&seen));
    assert!(
        sizes(&seen)
            .iter()
            .all(|size| { size.width >= KWIN_MINIMUM || *size == Resolution::new(1920, 1080) })
    );
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn what_kwin_does_with_an_offer_below_its_minimum() {
    let _serial = SERIAL.lock();
    let Some(live) = Live::start(true, "virtual_min_resolution = { width = 1, height = 1 }") else {
        return;
    };
    let Some(gpu) = gpu() else {
        return;
    };

    let (seen, sink) = recorder();
    let stream = connect(&live, gpu, &Credentials::Pin(live.pin()), DISPLAY, sink).unwrap();
    assert!(wait_for(FIRST_FRAME_WAIT, || reached(
        &seen,
        Resolution::new(1280, 720)
    )));

    announce(&stream, 120, 90);
    thread::sleep(Duration::from_secs(4));
    let finished = stream.is_finished();
    let client = stream.stats();
    let result = stream.stop();

    eprintln!("REPORT below-minimum sizes {:?}", sizes(&seen));
    eprintln!("REPORT below-minimum finished {finished} stop {result:?}");
    eprintln!(
        "REPORT below-minimum frames received {} decoded {}",
        client.frames_received, client.frames_decoded
    );
    for line in live.daemon.lines() {
        eprintln!("REPORT below-minimum host {}", &line[..line.len().min(200)]);
    }
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn a_flood_of_resizes_reaches_the_last_size_with_few_offers_on_kwin() {
    let _serial = SERIAL.lock();
    let Some(live) = Live::start(true, "virtual_resize_min_interval_ms = 300") else {
        return;
    };
    let Some(gpu) = gpu() else {
        return;
    };

    let (seen, sink) = recorder();
    let stream = connect(&live, gpu, &Credentials::Pin(live.pin()), DISPLAY, sink).unwrap();
    assert!(wait_for(FIRST_FRAME_WAIT, || reached(
        &seen,
        Resolution::new(1280, 720)
    )));

    for step in 0..FLOOD_STEPS {
        announce(&stream, 900 + step * 4, 600 + step * 2);
        thread::sleep(Duration::from_millis(10));
    }
    let last = Resolution::new(900 + (FLOOD_STEPS - 1) * 4, 600 + (FLOOD_STEPS - 1) * 2);
    let arrived = wait_for(RESIZE_WAIT, || reached(&seen, last));
    let sent = stream.stats().display_updates_sent;
    stream.stop().unwrap();

    let summary = live
        .daemon
        .wait_for_line("session summary", SUMMARY_WAIT)
        .expect("no session summary");
    eprintln!("REPORT sizes {:?}", sizes(&seen));
    eprintln!("REPORT {summary}");

    assert!(arrived, "saw {:?}", sizes(&seen));
    assert!(sent <= 3, "the debounce let {sent} announcements through");
    assert!(counter(&summary, "virtual resizes ") <= 4);
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn a_takeover_resizes_the_output_to_the_new_clients_display_on_kwin() {
    let _serial = SERIAL.lock();
    let Some(live) = Live::start(true, "") else {
        return;
    };
    let Some(gpu) = gpu() else {
        return;
    };

    let (first_seen, sink) = recorder();
    let first = connect(
        &live,
        gpu.clone(),
        &Credentials::Pin(live.pin()),
        DISPLAY,
        sink,
    )
    .unwrap();
    let secret = first.paired_secret().cloned().expect("no pairing secret");
    assert!(wait_for(FIRST_FRAME_WAIT, || reached(
        &first_seen,
        Resolution::new(1280, 720)
    )));

    let (second_seen, sink) = recorder();
    let record = PeerRecord::new("nested takeover", secret).unwrap();
    let second = connect(
        &live,
        gpu,
        &Credentials::Peer(record),
        (1000, 700, 60_000),
        sink,
    )
    .unwrap();
    let arrived = wait_for(RESIZE_WAIT, || {
        reached(&second_seen, Resolution::new(1000, 700))
    });
    let format = second.stream_format();
    second.stop().unwrap();
    first.stop().ok();

    eprintln!("REPORT takeover sizes {:?}", sizes(&second_seen));
    assert!(arrived, "saw {:?}", sizes(&second_seen));
    assert_eq!(
        format.map(|format| format.resolution),
        Some(Resolution::new(1000, 700))
    );
}
