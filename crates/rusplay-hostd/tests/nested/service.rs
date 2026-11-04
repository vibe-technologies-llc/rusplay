use std::{process::Output, thread, time::Duration};

use rusplay_client::{Credentials, PeerRecord};

use super::{
    FIRST_FRAME_WAIT, Live, SERIAL, connect,
    daemon::Daemon,
    gpu, recorder,
    stack::{read_trimmed, wait_for},
};

const DISPLAY: (u32, u32, u32) = (1280, 720, 60_000);
const EVENT_WAIT: Duration = Duration::from_secs(5);
const STATS_WAIT: Duration = Duration::from_secs(6);
const ID_PREFIX_LEN: usize = 8;
const RESTART_WAIT: Duration = Duration::from_secs(15);

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_owned()
}

fn field(status: &str, name: &str) -> Option<String> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(name))
        .map(|rest| rest.trim().to_owned())
}

fn open_window_and_renew_pin(live: &Live) -> String {
    let daemon = &live.daemon;
    let pin_path = Daemon::pin_path(&live.stack);

    let status = stdout(&daemon.control(&["status"]));
    let first_pin = stdout(&daemon.control(&["pin"]));

    assert_eq!(field(&status, "state").as_deref(), Some("pairing"));
    assert_eq!(field(&status, "paired_clients").as_deref(), Some("0"));
    assert_eq!(read_trimmed(&pin_path).as_deref(), Some(first_pin.as_str()));
    assert!(
        daemon
            .find(&format!("pairing pin {first_pin} for the next"))
            .is_some()
    );

    let renewed = stdout(&daemon.control(&["new-pin"]));

    assert_ne!(renewed, first_pin);
    assert!(wait_for(EVENT_WAIT, || {
        read_trimmed(&pin_path).as_deref() == Some(renewed.as_str())
    }));
    assert!(
        daemon
            .find(&format!("pairing pin {renewed} for the next"))
            .is_some()
    );

    renewed
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn the_service_pairs_streams_disconnects_reconnects_and_forgets() {
    let _serial = SERIAL.lock();
    let Some(live) = Live::start(true, "") else {
        return;
    };
    let Some(gpu) = gpu() else {
        return;
    };
    let daemon = &live.daemon;
    let pin_path = Daemon::pin_path(&live.stack);

    let renewed = open_window_and_renew_pin(&live);

    let (seen, sink) = recorder();
    let stream = connect(
        &live,
        gpu.clone(),
        &Credentials::Pin(live.pin()),
        DISPLAY,
        sink,
    )
    .unwrap();
    let secret = stream
        .paired_secret()
        .cloned()
        .expect("pairing gave no secret");
    wait_for(FIRST_FRAME_WAIT, || !seen.lock().is_empty());

    let status = stdout(&daemon.control(&["status"]));
    let refused = daemon.control(&["new-pin"]);

    assert!(field(&status, "state").is_some_and(|state| state.starts_with("streaming 127.0.0.1:")));
    assert_eq!(field(&status, "paired_clients").as_deref(), Some("1"));
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("a client is streaming"));
    assert!(wait_for(EVENT_WAIT, || !pin_path.exists()));
    assert!(daemon.find("paired and streaming to").is_some());
    assert!(wait_for(STATS_WAIT, || daemon
        .find("rusplay-hostd: stats rtt")
        .is_some()));

    stream.stop().unwrap();

    assert!(
        daemon
            .wait_for_line("session summary", EVENT_WAIT)
            .is_some()
    );
    assert!(daemon.wait_for_line(" left", EVENT_WAIT).is_some());
    assert!(wait_for(EVENT_WAIT, || {
        read_trimmed(&pin_path).is_some_and(|pin| pin != renewed)
    }));
    let status = stdout(&daemon.control(&["status"]));
    assert_eq!(field(&status, "state").as_deref(), Some("pairing"));
    assert_eq!(field(&status, "paired_clients").as_deref(), Some("1"));

    let peers = stdout(&daemon.control(&["peers", "list"]));
    let id = peers.split_whitespace().next().expect("no paired peer");
    let name = peers
        .split_once(' ')
        .map(|(_, name)| name.to_owned())
        .expect("no peer name");
    let record = PeerRecord::new(&name, secret).unwrap();

    let (seen_again, sink) = recorder();
    let stream = connect(
        &live,
        gpu.clone(),
        &Credentials::Peer(record.clone()),
        DISPLAY,
        sink,
    )
    .unwrap();
    wait_for(FIRST_FRAME_WAIT, || !seen_again.lock().is_empty());

    assert!(!seen_again.lock().is_empty());
    assert_eq!(daemon.count("paired and streaming to"), 1);
    assert_eq!(daemon.count("rusplay-hostd: streaming to"), 1);
    let status = stdout(&daemon.control(&["status"]));
    assert_eq!(field(&status, "paired_clients").as_deref(), Some("1"));

    stream.stop().unwrap();
    assert!(wait_for(EVENT_WAIT, || daemon.count(" left") == 2));
    thread::sleep(Duration::from_millis(300));

    let forgotten = daemon.control(&["forget", &id[..ID_PREFIX_LEN]]);

    assert!(forgotten.status.success(), "{}", stderr(&forgotten));
    assert!(stdout(&forgotten).starts_with("forgot "));
    assert_eq!(
        stdout(&daemon.control(&["peers", "list"])),
        "no paired clients"
    );
    let (_, sink) = recorder();
    assert!(connect(&live, gpu, &Credentials::Peer(record), DISPLAY, sink).is_err());
}

#[test]
#[ignore = "needs kwin_wayland, xdg-desktop-portal, xdg-desktop-portal-kde, PipeWire and a GPU"]
fn a_restart_with_the_saved_token_skips_the_portal_dialog() {
    let _serial = SERIAL.lock();
    let Some(mut live) = Live::start(false, "") else {
        return;
    };
    live.daemon.stop();
    let token = read_trimmed(&live.stack.path("restore-token"));

    let restarted = Daemon::start(&live.stack, Daemon::config_path(&live.stack));
    let opened = restarted.wait_for_line("for the next", RESTART_WAIT);

    assert!(token.is_some_and(|token| !token.is_empty()));
    assert!(opened.is_some(), "hostd said: {:#?}", restarted.lines());
}
