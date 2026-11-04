use std::{
    thread,
    time::{Duration, Instant},
};

use rusplay_config::NetworkConfig;
use rusplay_host::{Admission, HostError, HostEvent, HostService, StreamSnapshot};

use crate::{
    control_server::{Phase, SharedPhase},
    pin_file::PinFile,
    report,
    stats::StreamLog,
};

const POLL: Duration = Duration::from_millis(50);
const MILLIS_PER_SECOND: u128 = 1_000;

pub struct Supervisor {
    pin_file: Option<PinFile>,
    phase: SharedPhase,
    stats_interval_ms: u32,
    stats_json: bool,
    stream: Option<StreamLog>,
}

impl Supervisor {
    pub fn new(pin_file: Option<PinFile>, phase: SharedPhase, network: &NetworkConfig) -> Self {
        Self {
            pin_file,
            phase,
            stats_interval_ms: network.stats_log_interval_ms,
            stats_json: network.stats_json,
            stream: None,
        }
    }

    pub fn run(mut self, service: &HostService) -> Result<(), HostError> {
        loop {
            let finished = service.is_finished();
            for event in service.poll_events() {
                self.handle(event, service.ended_stream().as_ref())?;
            }
            if finished {
                return Ok(());
            }

            self.observe(service.snapshot().as_ref(), Instant::now());
            thread::sleep(POLL);
        }
    }

    pub fn handle(
        &mut self,
        event: HostEvent,
        ended: Option<&StreamSnapshot>,
    ) -> Result<(), HostError> {
        match event {
            HostEvent::PairingOpened { pin, window } => {
                self.phase.set(Phase::Pairing {
                    opened: Instant::now(),
                    timeout: window,
                });
                self.publish(pin.as_str());
                eprintln!(
                    "rusplay-hostd: pairing pin {} for the next {} s",
                    pin.as_str(),
                    window.as_millis() / MILLIS_PER_SECOND
                );
            }
            HostEvent::PairingLocked => {
                self.clear_pin_file();
                eprintln!("rusplay-hostd: too many wrong pins, issuing a new one");
            }
            HostEvent::ClientAdmitted {
                address, admission, ..
            } => {
                self.clear_pin_file();
                self.phase.set(Phase::Streaming { client: address });
                self.stream = Some(StreamLog::new(
                    self.stats_interval_ms,
                    self.stats_json,
                    Instant::now(),
                ));
                match admission {
                    Admission::Paired => {
                        eprintln!("rusplay-hostd: paired and streaming to {address}");
                    }
                    Admission::Reconnected => eprintln!("rusplay-hostd: streaming to {address}"),
                }
            }
            HostEvent::ClientLeft { address } => {
                if let (Some(log), Some(ended)) = (self.stream.take(), ended) {
                    log.finish(&ended.stats);
                }
                eprintln!("rusplay-hostd: {address} left");
            }
            HostEvent::ClipboardUnavailable(error) => {
                eprintln!("rusplay-hostd: clipboard sharing is off for this session");
                report(&*error);
            }
            HostEvent::SessionFailed(error) => {
                self.clear_pin_file();
                self.stream = None;
                report(&error);
            }
            HostEvent::Stopped(error) => {
                self.clear_pin_file();
                self.stream = None;

                return error.map_or(Ok(()), Err);
            }
        }

        Ok(())
    }

    pub fn observe(&mut self, snapshot: Option<&StreamSnapshot>, now: Instant) {
        if let (Some(log), Some(snapshot)) = (&mut self.stream, snapshot) {
            log.observe(snapshot, now);
        }
    }

    fn publish(&self, pin: &str) {
        if let Some(file) = &self.pin_file
            && let Err(error) = file.publish(pin)
        {
            report(&error);
        }
    }

    fn clear_pin_file(&self) {
        if let Some(file) = &self.pin_file {
            file.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, net::SocketAddr};

    use rusplay_config::PeersConfig;
    use rusplay_host::{Pin, StreamSnapshot};
    use rusplay_net::PeerSecret;
    use tempfile::TempDir;

    use super::*;
    use crate::pin_file;

    const PIN: &str = "135790";

    struct Fixture {
        directory: TempDir,
        supervisor: Supervisor,
        phase: SharedPhase,
    }

    impl Fixture {
        fn new(write_pin_file: bool) -> Self {
            let directory = TempDir::new().unwrap();
            let peers = PeersConfig {
                write_pin_file,
                pin_file: Some(directory.path().join("run").join("pin")),
                ..PeersConfig::default()
            };
            let pin_file = PinFile::open(&peers, None).unwrap();
            let phase = SharedPhase::new();
            let supervisor = Supervisor::new(pin_file, phase.clone(), &NetworkConfig::default());

            Self {
                directory,
                supervisor,
                phase,
            }
        }

        fn pin_path(&self) -> std::path::PathBuf {
            self.directory.path().join("run").join("pin")
        }

        fn handle(&mut self, event: HostEvent) -> Result<(), HostError> {
            self.supervisor.handle(event, None)
        }
    }

    fn address() -> SocketAddr {
        "10.0.0.2:5000".parse().unwrap()
    }

    fn opened(seconds: u64) -> HostEvent {
        HostEvent::PairingOpened {
            pin: Pin::parse(PIN).unwrap(),
            window: Duration::from_secs(seconds),
        }
    }

    fn admitted(admission: Admission) -> HostEvent {
        HostEvent::ClientAdmitted {
            address: address(),
            peer: PeerSecret::from_bytes([1; 32]).id(),
            name: "laptop".to_owned(),
            admission,
        }
    }

    #[test]
    fn an_opened_window_publishes_its_pin_and_the_pairing_phase() {
        let mut fixture = Fixture::new(true);

        fixture.handle(opened(60)).unwrap();

        assert_eq!(pin_file::read(&fixture.pin_path()).unwrap(), PIN);
        assert!(matches!(
            fixture.phase.get(),
            Phase::Pairing { timeout, .. } if timeout == Duration::from_secs(60)
        ));
    }

    #[test]
    fn the_pin_file_goes_when_a_client_is_admitted_or_the_pin_is_locked() {
        let mut fixture = Fixture::new(true);

        fixture.handle(opened(60)).unwrap();
        fixture.handle(HostEvent::PairingLocked).unwrap();
        let locked = fixture.pin_path().exists();
        fixture.handle(opened(60)).unwrap();
        fixture.handle(admitted(Admission::Paired)).unwrap();

        assert!(!locked);
        assert!(!fixture.pin_path().exists());
        assert_eq!(fixture.phase.get(), Phase::Streaming { client: address() });
    }

    #[test]
    fn the_pin_file_goes_when_a_session_fails_or_the_service_stops() {
        let mut fixture = Fixture::new(true);

        fixture.handle(opened(60)).unwrap();
        fixture
            .handle(HostEvent::SessionFailed(HostError::AcceptCancelled))
            .unwrap();
        let failed = fixture.pin_path().exists();
        fixture.handle(opened(60)).unwrap();
        fixture.handle(HostEvent::Stopped(None)).unwrap();

        assert!(!failed);
        assert!(!fixture.pin_path().exists());
    }

    #[test]
    fn no_pin_file_is_written_unless_asked_for() {
        let mut fixture = Fixture::new(false);

        fixture.handle(opened(60)).unwrap();

        assert!(!fixture.directory.path().join("run").exists());
    }

    #[test]
    fn a_renewed_window_replaces_the_published_pin() {
        let mut fixture = Fixture::new(true);
        fixture.handle(opened(60)).unwrap();
        fs::write(fixture.pin_path(), "stale\n").unwrap();

        fixture.handle(opened(60)).unwrap();

        assert_eq!(pin_file::read(&fixture.pin_path()).unwrap(), PIN);
    }

    #[test]
    fn a_stop_with_an_error_ends_the_daemon_with_it() {
        let mut fixture = Fixture::new(false);

        let outcome = fixture.handle(HostEvent::Stopped(Some(HostError::NoGpuAdapter)));

        assert!(matches!(outcome, Err(HostError::NoGpuAdapter)));
    }

    #[test]
    fn a_stream_is_logged_from_admission_to_departure() {
        let mut fixture = Fixture::new(false);

        fixture.handle(admitted(Admission::Reconnected)).unwrap();
        let tracking = fixture.supervisor.stream.is_some();
        fixture
            .supervisor
            .handle(
                HostEvent::ClientLeft { address: address() },
                Some(&StreamSnapshot {
                    client: address(),
                    stats: rusplay_host::HostStats::default(),
                    format: None,
                }),
            )
            .unwrap();

        assert!(tracking);
        assert!(fixture.supervisor.stream.is_none());
    }
}
