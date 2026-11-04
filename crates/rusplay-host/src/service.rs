use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use parking_lot::{Mutex, RwLock};
use rusplay_config::Config;
use rusplay_net::{NetError, PeerStore, Pin, load_or_create_host_id};
use rusplay_protocol::PeerId;

use crate::{
    Admission, Client, Gpu, HostError, HostSession, HostStats, HostStream, NegotiatedFormat,
    Responder, open_portal,
};

const HOST_ID_FILE: &str = "host-id";
const SERVICE_THREAD: &str = "rusplay-host-service";
const WATCH_POLL: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub enum HostEvent {
    PairingOpened {
        pin: Pin,
        window: Duration,
    },
    PairingLocked,
    ClientAdmitted {
        address: SocketAddr,
        peer: PeerId,
        name: String,
        admission: Admission,
    },
    ClientLeft {
        address: SocketAddr,
    },
    SessionFailed(HostError),
    Stopped(Option<HostError>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSnapshot {
    pub client: SocketAddr,
    pub stats: HostStats,
    pub format: Option<NegotiatedFormat>,
}

#[derive(Debug, Default)]
struct Requests {
    streaming: bool,
    pin: Option<Pin>,
}

#[derive(Debug, Default)]
struct Control {
    stop: AtomicBool,
    cancel: AtomicBool,
    disconnect: AtomicBool,
    requests: Mutex<Requests>,
    snapshot: Mutex<Option<StreamSnapshot>>,
    ended: Mutex<Option<StreamSnapshot>>,
}

impl Control {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.cancel.store(true, Ordering::Release);
    }

    fn request_new_pin(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    fn request_disconnect(&self) {
        self.disconnect.store(true, Ordering::Release);
    }

    fn disconnecting(&self) -> bool {
        self.disconnect.load(Ordering::Acquire)
    }

    fn begin_window(&self) -> Option<Pin> {
        let mut requests = self.requests.lock();
        self.cancel.store(self.stopping(), Ordering::Release);

        requests.pin.take()
    }

    fn request_pin(&self, pin: Pin) -> Result<(), HostError> {
        let mut requests = self.requests.lock();
        if requests.streaming {
            return Err(HostError::StreamActive);
        }

        requests.pin = Some(pin);
        self.cancel.store(true, Ordering::Release);

        Ok(())
    }

    fn set_streaming(&self, streaming: bool) {
        self.requests.lock().streaming = streaming;
    }
}

struct Worker<S> {
    host: HostSession,
    serve: S,
    control: Arc<Control>,
    events: Sender<HostEvent>,
    _responder: Option<Responder>,
}

#[derive(Debug, Clone)]
pub struct PinRenewer {
    control: Arc<Control>,
}

impl PinRenewer {
    pub fn renew(&self, pin: Pin) -> Result<(), HostError> {
        self.control.request_pin(pin)
    }
}

#[derive(Debug)]
pub struct HostService {
    address: SocketAddr,
    peers: Arc<RwLock<PeerStore>>,
    control: Arc<Control>,
    events: Receiver<HostEvent>,
    thread: Option<JoinHandle<()>>,
}

impl HostService {
    pub fn start(config: Config, peers: &Arc<RwLock<PeerStore>>) -> Result<Self, HostError> {
        let host_id = load_or_create_host_id(&peers.read().path().with_file_name(HOST_ID_FILE))?;
        let gpu = Gpu::open()?;
        let portal = open_portal(&config, &gpu)?;
        let feed = portal.feed;
        let mut host = HostSession::bind_shared(config, Arc::clone(peers))?;
        if let Some(input) = portal.input {
            host = host.with_input(input);
        }

        let address = host.local_addr()?;
        let responder = Responder::start(
            &host.config().network,
            address.port(),
            host.config().peers.host_name.as_deref(),
            host_id,
            host.busy_flag(),
        )?;
        let serve =
            move |host: &HostSession, client: &Client| host.serve(client, gpu.clone(), &feed);

        Self::spawn(host, serve, Some(responder))
    }

    pub(crate) fn spawn<S>(
        host: HostSession,
        serve: S,
        responder: Option<Responder>,
    ) -> Result<Self, HostError>
    where
        S: Fn(&HostSession, &Client) -> Result<HostStream, HostError> + Send + 'static,
    {
        let address = host.local_addr()?;
        let peers = Arc::clone(host.peers());
        let control = Arc::new(Control::default());
        let (events, receiver) = mpsc::channel();
        let worker = Worker {
            host,
            serve,
            control: Arc::clone(&control),
            events,
            _responder: responder,
        };
        let thread = thread::Builder::new()
            .name(SERVICE_THREAD.to_owned())
            .spawn(move || worker.run())
            .map_err(HostError::SpawnService)?;

        Ok(Self {
            address,
            peers,
            control,
            events: receiver,
            thread: Some(thread),
        })
    }

    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn peers(&self) -> &Arc<RwLock<PeerStore>> {
        &self.peers
    }

    pub fn poll_events(&self) -> Vec<HostEvent> {
        self.events.try_iter().collect()
    }

    pub fn snapshot(&self) -> Option<StreamSnapshot> {
        *self.control.snapshot.lock()
    }

    pub fn renew_pin(&self) {
        self.control.request_new_pin();
    }

    pub fn pin_renewer(&self) -> PinRenewer {
        PinRenewer {
            control: Arc::clone(&self.control),
        }
    }

    pub fn ended_stream(&self) -> Option<StreamSnapshot> {
        *self.control.ended.lock()
    }

    pub fn disconnect(&self) {
        self.control.request_disconnect();
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub fn stop(mut self) -> Vec<HostEvent> {
        let panicked = self.shut_down();
        let mut events: Vec<HostEvent> = self.events.try_iter().collect();
        if panicked {
            events.push(HostEvent::Stopped(Some(HostError::ServicePanicked)));
        }

        events
    }

    fn shut_down(&mut self) -> bool {
        self.control.request_stop();
        let Some(thread) = self.thread.take() else {
            return false;
        };

        let panicked = thread.join().is_err();
        self.control.snapshot.lock().take();

        panicked
    }
}

impl Drop for HostService {
    fn drop(&mut self) {
        self.shut_down();
    }
}

impl<S> Worker<S>
where
    S: Fn(&HostSession, &Client) -> Result<HostStream, HostError>,
{
    fn run(self) {
        let outcome = self.serve_until_stopped();

        self.emit(HostEvent::Stopped(outcome.err()));
    }

    fn emit(&self, event: HostEvent) {
        self.events.send(event).ok();
    }

    fn serve_until_stopped(&self) -> Result<(), HostError> {
        while !self.control.stopping() {
            match self.serve_next() {
                Ok(()) => {}
                Err(error) if error.is_session_failure() => {
                    self.emit(HostEvent::SessionFailed(error));
                }
                Err(error) => return Err(error),
            }
        }

        Ok(())
    }

    fn serve_next(&self) -> Result<(), HostError> {
        let network = &self.host.config().network;
        let window = Duration::from_millis(u64::from(network.pairing_timeout_ms));
        let pin = match self.control.begin_window() {
            Some(pin) => pin,
            None => Pin::generate(network.pairing_pin_digits)?,
        };

        self.emit(HostEvent::PairingOpened {
            pin: pin.clone(),
            window,
        });

        let client = match self
            .host
            .accept_cancellable(Some(pin), &self.control.cancel)
        {
            Ok(client) => client,
            Err(HostError::AcceptTimeout(_) | HostError::AcceptCancelled) => return Ok(()),
            Err(HostError::Net(NetError::PairingLocked)) => {
                self.emit(HostEvent::PairingLocked);
                return Ok(());
            }
            Err(error) => return Err(error),
        };

        let address = client.address();
        let name = self
            .host
            .peers()
            .read()
            .get(&client.peer())
            .map(|record| record.name().to_owned())
            .unwrap_or_default();
        self.emit(HostEvent::ClientAdmitted {
            address,
            peer: client.peer(),
            name,
            admission: client.admission(),
        });

        self.control.ended.lock().take();
        self.control.set_streaming(true);
        let finished = self.stream(&client);
        self.control.set_streaming(false);

        finished
    }

    fn stream(&self, client: &Client) -> Result<(), HostError> {
        let stream = (self.serve)(&self.host, client)?;
        let finished = self.watch(stream);
        self.emit(HostEvent::ClientLeft {
            address: client.address(),
        });

        finished
    }

    fn watch(&self, stream: HostStream) -> Result<(), HostError> {
        self.control.disconnect.store(false, Ordering::Release);

        while !stream.is_finished() && !self.control.stopping() && !self.control.disconnecting() {
            *self.control.snapshot.lock() = Some(snapshot_of(&stream));
            thread::sleep(WATCH_POLL);
        }

        *self.control.ended.lock() = Some(snapshot_of(&stream));
        self.control.snapshot.lock().take();

        if self.control.stopping() || self.control.disconnecting() {
            self.control.disconnect.store(false, Ordering::Release);
            stream.stop()
        } else {
            stream.wait()
        }
    }
}

fn snapshot_of(stream: &HostStream) -> StreamSnapshot {
    StreamSnapshot {
        client: stream.client(),
        stats: stream.stats(),
        format: stream.format(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        time::Instant,
    };

    use rusplay_config::NetworkConfig;
    use tempfile::TempDir;

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);
    const POLL: Duration = Duration::from_millis(5);

    fn refuse(_: &HostSession, _: &Client) -> Result<HostStream, HostError> {
        Err(HostError::NoGpuAdapter)
    }

    fn service(pairing_timeout_ms: u32) -> (TempDir, HostService) {
        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            network: NetworkConfig {
                bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: 0,
                pairing_timeout_ms,
                ..NetworkConfig::default()
            },
            ..Config::default()
        };
        let peers = PeerStore::open(directory.path().join("paired-clients")).unwrap();
        let host = HostSession::bind(config, peers).unwrap();
        let service = HostService::spawn(host, refuse, None).unwrap();

        (directory, service)
    }

    fn next_event(service: &HostService) -> HostEvent {
        let deadline = Instant::now() + WAIT;

        loop {
            if let Ok(event) = service.events.try_recv() {
                return event;
            }
            assert!(Instant::now() < deadline, "no event within {WAIT:?}");
            thread::sleep(POLL);
        }
    }

    fn pin_of(event: HostEvent) -> (Pin, Duration) {
        match event {
            HostEvent::PairingOpened { pin, window } => (pin, window),
            other => panic!("expected a pairing window, got {other:?}"),
        }
    }

    #[test]
    fn a_pairing_window_opens_with_its_pin_and_length() {
        let (_directory, service) = service(5_000);

        let (pin, window) = pin_of(next_event(&service));

        assert_eq!(window, Duration::from_millis(5_000));
        assert_eq!(
            pin.as_str().len(),
            usize::from(NetworkConfig::default().pairing_pin_digits)
        );
        service.stop();
    }

    #[test]
    fn a_window_that_times_out_is_replaced_by_a_new_one() {
        let (_directory, service) = service(150);

        let (first, _) = pin_of(next_event(&service));
        let (second, _) = pin_of(next_event(&service));

        assert_ne!(first, second);
        service.stop();
    }

    #[test]
    fn renewing_the_pin_ends_the_open_window_at_once() {
        let (_directory, service) = service(60_000);

        let (first, _) = pin_of(next_event(&service));
        service.renew_pin();
        let (second, _) = pin_of(next_event(&service));

        assert_ne!(first, second);
        service.stop();
    }

    #[test]
    fn stopping_ends_the_loop_cleanly_and_says_so() {
        let (_directory, service) = service(60_000);
        pin_of(next_event(&service));

        let events = service.stop();

        assert!(matches!(events.last(), Some(HostEvent::Stopped(None))));
    }

    #[test]
    fn a_stopped_service_reports_finished_and_has_no_stream() {
        let (_directory, service) = service(60_000);

        assert!(!service.is_finished());
        assert!(service.snapshot().is_none());
        assert_eq!(service.address().ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        service.stop();
    }

    #[test]
    fn a_pending_renewal_does_not_outlive_a_stop() {
        let control = Control::default();

        control.request_stop();
        control.begin_window();

        assert!(control.cancel.load(Ordering::Acquire));
    }

    #[test]
    fn a_renewal_requested_before_a_window_is_dropped_when_it_opens() {
        let control = Control::default();

        control.request_new_pin();
        control.begin_window();

        assert!(!control.cancel.load(Ordering::Acquire));
    }

    #[test]
    fn a_requested_pin_is_taken_by_the_next_window_and_cancels_the_open_one() {
        let control = Control::default();
        let requested = Pin::parse("123456").unwrap();

        control.request_pin(requested.clone()).unwrap();
        let cancelled = control.cancel.load(Ordering::Acquire);
        let taken = control.begin_window();
        let again = control.begin_window();

        assert!(cancelled);
        assert_eq!(taken, Some(requested));
        assert_eq!(again, None);
        assert!(!control.cancel.load(Ordering::Acquire));
    }

    #[test]
    fn a_requested_pin_is_refused_while_a_client_streams() {
        let control = Control::default();
        control.set_streaming(true);

        let refused = control.request_pin(Pin::parse("123456").unwrap());
        control.set_streaming(false);

        assert!(matches!(refused, Err(HostError::StreamActive)));
        assert!(!control.cancel.load(Ordering::Acquire));
        assert_eq!(control.begin_window(), None);
    }
}
