use rusplay_core::{ColorPrimaries, Resolution, TransferFunction};

use super::{SERIAL, hardware, observe, observe_with, stack::Stack};

const VIRTUAL_SOURCE_BIT: u32 = 4;
const OUTPUT_REFRESH_HZ: usize = 60;
const REFRESH_SLACK: usize = 5;
const OBSERVED_SECONDS: u64 = 4;

fn captured_per_interval(host: &[String]) -> Vec<usize> {
    let totals: Vec<usize> = host
        .iter()
        .filter(|line| line.contains("rusplay-hostd: stats "))
        .filter_map(|line| line.split("frames captured ").nth(1))
        .filter_map(|rest| rest.split_whitespace().next()?.parse().ok())
        .collect();

    totals.windows(2).map(|pair| pair[1] - pair[0]).collect()
}

fn offered(width: u32, height: u32) -> Resolution {
    Resolution::new(width, height)
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde and PipeWire"]
fn the_portal_lists_the_virtual_source_type() {
    let _serial = SERIAL.lock();
    let Some(stack) = Stack::start() else {
        return;
    };

    let Ok(output) = stack
        .command("busctl")
        .args([
            "--user",
            "get-property",
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.ScreenCast",
            "AvailableSourceTypes",
        ])
        .output()
    else {
        hardware::missing("busctl");
        return;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let types: u32 = text
        .trim()
        .strip_prefix("u ")
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("unexpected property {text:?}"));

    assert_ne!(types & VIRTUAL_SOURCE_BIT, 0);
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn the_first_keyframe_has_the_offered_size_because_the_first_frame_is_held() {
    for (width, height, millihertz) in [
        (1280, 720, 60_000),
        (1000, 700, 90_000),
        (2560, 1440, 144_000),
    ] {
        let Some(observation) = observe((width, height, millihertz), true, OBSERVED_SECONDS) else {
            return;
        };
        observation.report();

        assert_eq!(observation.sizes, [offered(width, height)]);
        assert!(observation.first.is_some_and(|first| first.keyframe));
        assert_eq!(observation.keyframe_times.len(), 1);
        assert_eq!(observation.client.stream_formats_received, 1);
    }
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn without_the_hold_kwin_sends_a_keyframe_at_its_own_size_first() {
    let Some(observation) = observe_with(
        (1280, 720, 60_000),
        true,
        OBSERVED_SECONDS,
        "virtual_first_frame_timeout_ms = 0",
    ) else {
        return;
    };
    observation.report();

    assert_eq!(observation.sizes.first(), Some(&offered(1920, 1080)));
    assert_eq!(observation.sizes.last(), Some(&offered(1280, 720)));
    assert!(observation.keyframe_times.len() >= 2);
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn an_offer_kwin_ignores_is_streamed_at_its_own_size_after_the_timeout() {
    let Some(observation) = observe_with(
        (120, 90, 60_000),
        true,
        OBSERVED_SECONDS,
        "virtual_min_resolution = { width = 1, height = 1 }",
    ) else {
        return;
    };
    observation.report();

    assert_eq!(observation.sizes, [offered(1920, 1080)]);
    assert!(observation.first.is_some_and(|first| first.keyframe));
    assert!(observation.frames > 30);
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn kwin_does_not_deliver_more_frames_than_its_output_refreshes() {
    for (width, height, millihertz) in [(1920, 1080, 30_000), (2560, 1440, 144_000)] {
        let Some(observation) = observe((width, height, millihertz), true, OBSERVED_SECONDS) else {
            return;
        };
        observation.report();

        let captured = captured_per_interval(&observation.host);
        assert!(captured.len() >= 2, "{captured:?}");
        assert!(
            captured
                .iter()
                .all(|count| *count <= OUTPUT_REFRESH_HZ + REFRESH_SLACK),
            "{captured:?}"
        );
        assert!(
            captured
                .iter()
                .any(|count| *count >= OUTPUT_REFRESH_HZ - REFRESH_SLACK)
        );
    }
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn a_static_virtual_monitor_delivers_a_frame_at_the_new_size_after_renegotiation() {
    let Some(observation) = observe((1280, 720, 60_000), false, OBSERVED_SECONDS) else {
        return;
    };
    observation.report();

    assert_eq!(observation.sizes, [offered(1280, 720)]);
    assert!(observation.first.is_some_and(|first| first.keyframe));
    assert!(observation.frames >= 1);
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn kwin_describes_no_colour_so_the_stream_is_taken_as_srgb() {
    let Some(observation) = observe((1280, 720, 60_000), true, OBSERVED_SECONDS) else {
        return;
    };
    observation.report();

    assert_eq!(observation.colors.len(), 1);
    assert_eq!(observation.colors[0].space.primaries, ColorPrimaries::Bt709);
    assert_eq!(observation.colors[0].space.transfer, TransferFunction::Srgb);
}
