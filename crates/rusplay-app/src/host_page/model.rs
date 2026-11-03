use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use crate::{
    hosting::{Admission, HostEvent, HostStats, LiveStream, NegotiatedFormat, PairedClient, Pin},
    notice::{Notice, error_chain},
    stats::{millis, per_second},
};

const SAMPLE_WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Stopped,
    Starting,
    Running,
    Stopping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    pub pin: Pin,
    expires_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub address: SocketAddr,
    pub name: String,
    pub admission: Admission,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostRates {
    pub sent_fps: f64,
    pub encoded_fps: f64,
    pub dropped: f64,
    pub lost: f64,
    pub rtt: Option<Duration>,
    pub bitrate_target_kbps: Option<u32>,
    pub keyframes: u64,
}

impl HostRates {
    fn between(previous: &Reading, current: &Reading) -> Option<Self> {
        let seconds = current
            .at
            .saturating_duration_since(previous.at)
            .as_secs_f64();
        if seconds <= 0.0 {
            return None;
        }

        let rate = |now: u64, before: u64| per_second(now.saturating_sub(before), seconds);
        let (now, before) = (&current.stats, &previous.stats);

        Some(Self {
            sent_fps: rate(now.frames_sent, before.frames_sent),
            encoded_fps: rate(now.frames_encoded, before.frames_encoded),
            dropped: rate(now.frames_dropped, before.frames_dropped),
            lost: rate(now.packets_reported_lost, before.packets_reported_lost),
            rtt: now.rtt,
            bitrate_target_kbps: now.bitrate_target_kbps,
            keyframes: now.keyframes_sent,
        })
    }

    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let mut rows = vec![
            ("Sent", format!("{:.0} fps", self.sent_fps)),
            ("Encoded", format!("{:.0} fps", self.encoded_fps)),
            (
                "Round trip",
                self.rtt.map_or_else(
                    || "not measured yet".to_owned(),
                    |rtt| format!("{:.1} ms", millis(rtt)),
                ),
            ),
            ("Dropped", format!("{:.1}/s", self.dropped)),
            ("Packets lost", format!("{:.1}/s", self.lost)),
            ("Keyframes", self.keyframes.to_string()),
        ];
        if let Some(kbps) = self.bitrate_target_kbps {
            rows.push((
                "Bitrate target",
                format!("{:.1} Mbit/s", f64::from(kbps) / 1_000.0),
            ));
        }

        rows
    }
}

#[derive(Debug, Clone, Copy)]
struct Reading {
    at: Instant,
    stats: HostStats,
}

#[derive(Debug)]
pub struct HostModel {
    phase: Phase,
    address: Option<SocketAddr>,
    pairing: Option<Pairing>,
    client: Option<Client>,
    format: Option<NegotiatedFormat>,
    rates: Option<HostRates>,
    last: Option<Reading>,
}

impl Default for HostModel {
    fn default() -> Self {
        Self {
            phase: Phase::Stopped,
            address: None,
            pairing: None,
            client: None,
            format: None,
            rates: None,
            last: None,
        }
    }
}

impl HostModel {
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    pub const fn address(&self) -> Option<SocketAddr> {
        self.address
    }

    pub const fn pairing(&self) -> Option<&Pairing> {
        self.pairing.as_ref()
    }

    pub const fn client(&self) -> Option<&Client> {
        self.client.as_ref()
    }

    pub const fn rates(&self) -> Option<&HostRates> {
        self.rates.as_ref()
    }

    pub fn format_line(&self) -> Option<String> {
        self.format.map(|format| {
            if format.is_fallback() {
                format!(
                    "{} instead of the configured {}, which the encoder or client lacks",
                    format.chosen, format.configured
                )
            } else {
                format.chosen.to_string()
            }
        })
    }

    pub fn begin_start(&mut self) -> bool {
        if self.phase != Phase::Stopped {
            return false;
        }

        self.phase = Phase::Starting;

        true
    }

    pub fn started(&mut self, address: SocketAddr) {
        self.reset();
        self.phase = Phase::Running;
        self.address = Some(address);
    }

    pub fn start_failed(&mut self) {
        self.reset();
        self.phase = Phase::Stopped;
    }

    pub fn begin_stop(&mut self) -> bool {
        if self.phase != Phase::Running {
            return false;
        }

        self.phase = Phase::Stopping;

        true
    }

    pub fn stopped(&mut self) {
        self.reset();
        self.phase = Phase::Stopped;
    }

    pub fn seconds_left(&self, now: Instant) -> Option<u64> {
        let pairing = self.pairing.as_ref()?;
        let left = pairing.expires_at.saturating_duration_since(now);

        Some(left.as_secs() + u64::from(left.subsec_nanos() > 0))
    }

    pub fn apply(&mut self, event: &HostEvent, now: Instant) -> Option<Notice> {
        if self.phase != Phase::Running && self.phase != Phase::Stopping {
            return None;
        }

        match event {
            HostEvent::PairingOpened { pin, window } => {
                self.pairing = Some(Pairing {
                    pin: pin.clone(),
                    expires_at: now + *window,
                });
                None
            }
            HostEvent::PairingLocked => {
                self.pairing = None;
                Some(Notice::Failure(
                    "Too many wrong PINs, a new PIN was issued".to_owned(),
                ))
            }
            HostEvent::ClientAdmitted {
                address,
                name,
                admission,
                ..
            } => {
                self.pairing = None;
                self.client = Some(Client {
                    address: *address,
                    name: name.clone(),
                    admission: *admission,
                });
                self.format = None;
                self.rates = None;
                self.last = None;

                Some(Notice::Info(match admission {
                    Admission::Paired => {
                        format!("Paired with {}, streaming to {address}", label(name))
                    }
                    Admission::Reconnected => format!("{} reconnected from {address}", label(name)),
                }))
            }
            HostEvent::ClientLeft { address } => {
                self.client = None;
                self.format = None;
                self.rates = None;
                self.last = None;

                Some(Notice::Info(format!("{address} left")))
            }
            HostEvent::SessionFailed(error) => Some(Notice::Failure(format!(
                "A client session failed: {}",
                error_chain(error)
            ))),
            HostEvent::Stopped(Some(error)) => Some(Notice::Failure(format!(
                "The host stopped: {}",
                error_chain(error)
            ))),
            HostEvent::Stopped(None) => None,
        }
    }

    pub fn observe(&mut self, stream: Option<&LiveStream>, now: Instant) -> bool {
        let Some(stream) = stream else {
            return false;
        };
        if self.client.as_ref().map(|client| client.address) != Some(stream.client) {
            return false;
        }

        let mut changed = self.format != stream.format;
        self.format = stream.format;

        let current = Reading {
            at: now,
            stats: stream.stats,
        };
        match self.last {
            None => self.last = Some(current),
            Some(previous) if now.saturating_duration_since(previous.at) >= SAMPLE_WINDOW => {
                if let Some(rates) = HostRates::between(&previous, &current) {
                    self.rates = Some(rates);
                    changed = true;
                }
                self.last = Some(current);
            }
            Some(_) => {}
        }

        changed
    }

    pub fn status(&self) -> String {
        match (self.phase, &self.client) {
            (Phase::Stopped, _) => "Not hosting".to_owned(),
            (Phase::Starting, _) => {
                "Starting. Approve the screen share request if one appears".to_owned()
            }
            (Phase::Stopping, _) => "Stopping".to_owned(),
            (Phase::Running, Some(client)) => {
                format!("Streaming to {} at {}", label(&client.name), client.address)
            }
            (Phase::Running, None) => match self.address {
                Some(address) => format!("Waiting for a client on port {}", address.port()),
                None => "Waiting for a client".to_owned(),
            },
        }
    }

    fn reset(&mut self) {
        self.address = None;
        self.pairing = None;
        self.client = None;
        self.format = None;
        self.rates = None;
        self.last = None;
    }
}

fn label(name: &str) -> &str {
    if name.is_empty() { "a client" } else { name }
}

pub fn grouped_pin(pin: &str) -> String {
    const GROUP: usize = 3;

    let digits: Vec<char> = pin.chars().collect();

    digits
        .chunks(GROUP)
        .map(|group| group.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn client_summary(clients: &[PairedClient]) -> String {
    match clients.len() {
        0 => "No paired clients".to_owned(),
        1 => "1 paired client".to_owned(),
        count => format!("{count} paired clients"),
    }
}

#[cfg(test)]
mod tests {
    use rusplay_host::HostError;

    use super::*;
    use crate::hosting::PeerId;

    const ADDRESS: &str = "192.168.1.9:50000";

    fn listening() -> SocketAddr {
        "0.0.0.0:47800".parse().unwrap()
    }

    fn client() -> SocketAddr {
        ADDRESS.parse().unwrap()
    }

    fn running() -> HostModel {
        let mut model = HostModel::default();
        model.begin_start();
        model.started(listening());

        model
    }

    fn pairing_event(seconds: u64) -> HostEvent {
        HostEvent::PairingOpened {
            pin: Pin::parse("123456").unwrap(),
            window: Duration::from_secs(seconds),
        }
    }

    fn admitted(admission: Admission) -> HostEvent {
        HostEvent::ClientAdmitted {
            address: client(),
            peer: PeerId([7; 16]),
            name: "laptop".to_owned(),
            admission,
        }
    }

    fn stream(frames_sent: u64, frames_encoded: u64) -> LiveStream {
        LiveStream {
            client: client(),
            stats: HostStats {
                frames_sent,
                frames_encoded,
                ..HostStats::default()
            },
            format: None,
        }
    }

    #[test]
    fn a_new_model_is_stopped_and_can_only_start() {
        let mut model = HostModel::default();

        assert_eq!(model.phase(), Phase::Stopped);
        assert!(!model.begin_stop());
        assert!(model.begin_start());
        assert!(!model.begin_start());
        assert_eq!(model.phase(), Phase::Starting);
    }

    #[test]
    fn starting_succeeds_into_running_with_the_listening_address() {
        let model = running();

        assert_eq!(model.phase(), Phase::Running);
        assert_eq!(model.address(), Some(listening()));
        assert_eq!(model.status(), "Waiting for a client on port 47800");
    }

    #[test]
    fn a_failed_start_returns_to_stopped_and_may_be_retried() {
        let mut model = HostModel::default();
        model.begin_start();

        model.start_failed();

        assert_eq!(model.phase(), Phase::Stopped);
        assert!(model.begin_start());
    }

    #[test]
    fn stopping_clears_the_pairing_and_the_client() {
        let mut model = running();
        let now = Instant::now();
        model.apply(&pairing_event(60), now);
        model.apply(&admitted(Admission::Paired), now);

        assert!(model.begin_stop());
        model.stopped();

        assert_eq!(model.phase(), Phase::Stopped);
        assert!(model.pairing().is_none());
        assert!(model.client().is_none());
        assert_eq!(model.address(), None);
    }

    #[test]
    fn an_opened_window_counts_down_and_rounds_partial_seconds_up() {
        let mut model = running();
        let start = Instant::now();

        model.apply(&pairing_event(60), start);

        assert_eq!(model.seconds_left(start), Some(60));
        assert_eq!(
            model.seconds_left(start + Duration::from_millis(100)),
            Some(60)
        );
        assert_eq!(model.seconds_left(start + Duration::from_secs(59)), Some(1));
        assert_eq!(model.seconds_left(start + Duration::from_secs(61)), Some(0));
        assert_eq!(model.pairing().unwrap().pin, Pin::parse("123456").unwrap());
    }

    #[test]
    fn a_new_window_replaces_the_previous_pin() {
        let mut model = running();
        let now = Instant::now();
        model.apply(&pairing_event(60), now);

        model.apply(
            &HostEvent::PairingOpened {
                pin: Pin::parse("654321").unwrap(),
                window: Duration::from_secs(30),
            },
            now,
        );

        assert_eq!(model.pairing().unwrap().pin, Pin::parse("654321").unwrap());
        assert_eq!(model.seconds_left(now), Some(30));
    }

    #[test]
    fn a_pairing_client_closes_the_window_and_is_announced() {
        let mut model = running();
        let now = Instant::now();
        model.apply(&pairing_event(60), now);

        let notice = model.apply(&admitted(Admission::Paired), now);

        assert!(model.pairing().is_none());
        assert_eq!(model.client().unwrap().name, "laptop");
        assert_eq!(
            notice,
            Some(Notice::Info(format!(
                "Paired with laptop, streaming to {ADDRESS}"
            )))
        );
        assert_eq!(model.status(), format!("Streaming to laptop at {ADDRESS}"));
    }

    #[test]
    fn a_reconnect_is_worded_differently_from_a_pairing() {
        let mut model = running();

        let notice = model.apply(&admitted(Admission::Reconnected), Instant::now());

        assert_eq!(
            notice,
            Some(Notice::Info(format!("laptop reconnected from {ADDRESS}")))
        );
    }

    #[test]
    fn a_nameless_client_gets_a_generic_label() {
        let mut model = running();
        let event = HostEvent::ClientAdmitted {
            address: client(),
            peer: PeerId([7; 16]),
            name: String::new(),
            admission: Admission::Reconnected,
        };

        let notice = model.apply(&event, Instant::now());

        assert_eq!(
            notice,
            Some(Notice::Info(format!("a client reconnected from {ADDRESS}")))
        );
    }

    #[test]
    fn a_client_leaving_clears_the_client_and_its_measurements() {
        let mut model = running();
        let start = Instant::now();
        model.apply(&admitted(Admission::Paired), start);
        model.observe(Some(&stream(0, 0)), start);
        model.observe(Some(&stream(60, 60)), start + Duration::from_secs(1));

        let notice = model.apply(&HostEvent::ClientLeft { address: client() }, start);

        assert!(model.client().is_none());
        assert!(model.rates().is_none());
        assert_eq!(notice, Some(Notice::Info(format!("{ADDRESS} left"))));
    }

    #[test]
    fn locked_pairing_and_failures_are_reported_as_failures() {
        let mut model = running();
        let now = Instant::now();
        model.apply(&pairing_event(60), now);

        let locked = model.apply(&HostEvent::PairingLocked, now);
        let failed = model.apply(&HostEvent::SessionFailed(HostError::NoGpuAdapter), now);
        let stopped = model.apply(&HostEvent::Stopped(Some(HostError::NoGpuAdapter)), now);

        assert!(model.pairing().is_none());
        assert!(matches!(locked, Some(Notice::Failure(_))));
        assert_eq!(
            failed,
            Some(Notice::Failure(
                "A client session failed: no gpu adapter is available".to_owned()
            ))
        );
        assert_eq!(
            stopped,
            Some(Notice::Failure(
                "The host stopped: no gpu adapter is available".to_owned()
            ))
        );
    }

    #[test]
    fn a_clean_stop_has_no_notice() {
        let mut model = running();

        assert_eq!(model.apply(&HostEvent::Stopped(None), Instant::now()), None);
    }

    #[test]
    fn events_after_the_host_stopped_are_ignored() {
        let mut model = HostModel::default();

        let notice = model.apply(&admitted(Admission::Paired), Instant::now());

        assert_eq!(notice, None);
        assert!(model.client().is_none());
    }

    #[test]
    fn rates_come_from_the_difference_between_two_samples() {
        let mut model = running();
        let start = Instant::now();
        model.apply(&admitted(Admission::Paired), start);

        let first = model.observe(Some(&stream(100, 100)), start);
        let second = model.observe(Some(&stream(160, 158)), start + Duration::from_secs(1));

        let rates = model.rates().unwrap();
        assert!(!first);
        assert!(second);
        assert!((rates.sent_fps - 60.0).abs() < 1e-9);
        assert!((rates.encoded_fps - 58.0).abs() < 1e-9);
    }

    #[test]
    fn samples_closer_than_the_window_do_not_produce_rates() {
        let mut model = running();
        let start = Instant::now();
        model.apply(&admitted(Admission::Paired), start);
        model.observe(Some(&stream(0, 0)), start);

        let changed = model.observe(Some(&stream(5, 5)), start + Duration::from_millis(100));

        assert!(!changed);
        assert!(model.rates().is_none());
    }

    #[test]
    fn a_stream_of_another_client_is_not_measured() {
        let mut model = running();
        let start = Instant::now();
        model.apply(&admitted(Admission::Paired), start);
        let mut other = stream(1, 1);
        other.client = "10.0.0.2:1".parse().unwrap();

        assert!(!model.observe(Some(&other), start));
        assert!(!model.observe(None, start));
    }

    #[test]
    fn rows_list_the_rates_and_say_when_the_round_trip_is_unknown() {
        let rates = HostRates {
            sent_fps: 59.6,
            encoded_fps: 60.0,
            dropped: 0.0,
            lost: 1.25,
            rtt: None,
            bitrate_target_kbps: Some(25_000),
            keyframes: 3,
        };

        let rows = rates.rows();

        assert_eq!(rows[0], ("Sent", "60 fps".to_owned()));
        assert_eq!(rows[2], ("Round trip", "not measured yet".to_owned()));
        assert_eq!(rows[4], ("Packets lost", "1.2/s".to_owned()));
        assert_eq!(rows[6], ("Bitrate target", "25.0 Mbit/s".to_owned()));
    }

    #[test]
    fn a_pin_is_shown_in_groups_of_three() {
        assert_eq!(grouped_pin("123456"), "123 456");
        assert_eq!(grouped_pin("1234567"), "123 456 7");
        assert_eq!(grouped_pin("1234"), "123 4");
        assert_eq!(grouped_pin(""), "");
    }

    #[test]
    fn the_paired_client_count_is_worded_for_zero_one_and_many() {
        let one = PairedClient {
            id: PeerId([1; 16]),
            name: "a".to_owned(),
        };

        assert_eq!(client_summary(&[]), "No paired clients");
        assert_eq!(
            client_summary(std::slice::from_ref(&one)),
            "1 paired client"
        );
        assert_eq!(client_summary(&[one.clone(), one]), "2 paired clients");
    }
}
