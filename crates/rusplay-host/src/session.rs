use std::{
    net::SocketAddr,
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
};

use parking_lot::RwLock;
use rusplay_config::Config;
use rusplay_net::{
    ControlChannel, ControlConfig, FecScheme, PeerStore, Pin, Transport, TransportOptions,
};

use crate::{
    CaptureFeed, Client, Gpu, HostError, HostStats, Stage,
    accept::accept,
    capture,
    control::{ControlLinks, ControlStage},
    encode::{self, EncodeLinks, PipelineFactory},
    outbox::ControlOutbox,
    pipeline::{GpuPipeline, VideoPipeline},
    send::{self, SendLinks, SendOptions},
    shared::Shared,
};

const FEC_BLOCK_DATA_SHARDS: u8 = 32;
const SEND_QUEUE_FRAMES: usize = 2;
const PERCENT: u32 = 100;

#[derive(Debug)]
pub struct HostSession {
    config: Config,
    transport: Arc<Transport>,
    peers: Arc<RwLock<PeerStore>>,
}

impl HostSession {
    pub fn bind(config: Config, peers: PeerStore) -> Result<Self, HostError> {
        let network = &config.network;
        let transport = Transport::bind(
            SocketAddr::new(network.bind_address, network.port),
            TransportOptions::from(network),
        )?;

        Ok(Self {
            config,
            transport: Arc::new(transport),
            peers: Arc::new(RwLock::new(peers)),
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn local_addr(&self) -> Result<SocketAddr, HostError> {
        Ok(self.transport.local_addr()?)
    }

    pub fn peers(&self) -> &Arc<RwLock<PeerStore>> {
        &self.peers
    }

    pub fn accept(&self, pin: Option<Pin>) -> Result<Client, HostError> {
        accept(&self.transport, &self.peers, pin, &self.config.network)
    }

    pub fn serve(
        &self,
        client: &Client,
        gpu: Gpu,
        feed: &Arc<CaptureFeed>,
    ) -> Result<HostStream, HostError> {
        let config = self.config.clone();
        let factory: PipelineFactory =
            Box::new(move || Ok(Box::new(GpuPipeline::new(gpu, config)) as Box<dyn VideoPipeline>));

        self.serve_with(client, feed, factory)
    }

    pub(crate) fn serve_with(
        &self,
        client: &Client,
        feed: &Arc<CaptureFeed>,
        factory: PipelineFactory,
    ) -> Result<HostStream, HostError> {
        let network = &self.config.network;
        let shared = Arc::new(Shared::new());
        let mut stream = HostStream {
            stages: Vec::new(),
            shared: Arc::clone(&shared),
            client: client.address(),
        };

        let outbox = Arc::new(ControlOutbox::new(
            ControlChannel::new(ControlConfig::from(network)),
            Arc::clone(&self.transport),
            client.address(),
        ));
        let control = ControlStage::new(
            network,
            ControlLinks {
                shared: Arc::clone(&shared),
                transport: Arc::clone(&self.transport),
                client: client.address(),
                replay: client.replay().clone(),
                outbox: Arc::clone(&outbox),
            },
        );
        stream.spawn(Stage::Control, move || control.run())?;

        let (output, queue) = mpsc::sync_channel(SEND_QUEUE_FRAMES);
        let send_links = SendLinks {
            shared: Arc::clone(&shared),
            queue,
            transport: Arc::clone(&self.transport),
            client: client.address(),
        };
        let options = send_options(&self.config);
        stream.spawn(Stage::Send, move || send::run(options, &send_links))?;

        let encode_links = EncodeLinks {
            shared: Arc::clone(&shared),
            feed: Arc::clone(feed),
            output,
            outbox,
        };
        stream.spawn(Stage::Encode, move || encode::run(factory, &encode_links))?;

        let feed = Arc::clone(feed);
        stream.spawn(Stage::Capture, move || capture::run(&feed, &shared))?;

        Ok(stream)
    }
}

fn send_options(config: &Config) -> SendOptions {
    let network = &config.network;
    let pacing_window = network
        .pacing
        .then(|| config.video.frame_rate.frame_interval())
        .flatten()
        .map(|interval| interval * u32::from(network.pacing_window_percent) / PERCENT);

    SendOptions {
        fec: FecScheme::for_overhead(FEC_BLOCK_DATA_SHARDS, network.fec_overhead_percent),
        pacing_window,
    }
}

#[derive(Debug)]
pub struct HostStream {
    stages: Vec<(Stage, JoinHandle<Result<(), HostError>>)>,
    shared: Arc<Shared>,
    client: SocketAddr,
}

impl HostStream {
    pub fn client(&self) -> SocketAddr {
        self.client
    }

    pub fn stats(&self) -> HostStats {
        self.shared.counters.snapshot()
    }

    pub fn is_finished(&self) -> bool {
        self.stages.iter().any(|(_, handle)| handle.is_finished())
    }

    pub fn stop(mut self) -> Result<(), HostError> {
        self.shared.stop();
        self.join()
    }

    pub fn wait(mut self) -> Result<(), HostError> {
        self.join()
    }

    fn join(&mut self) -> Result<(), HostError> {
        let mut first_error = None;

        for (stage, handle) in self.stages.drain(..) {
            let result = handle
                .join()
                .unwrap_or(Err(HostError::StagePanicked { stage }));
            self.shared.stop();
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    fn spawn(
        &mut self,
        stage: Stage,
        body: impl FnOnce() -> Result<(), HostError> + Send + 'static,
    ) -> Result<(), HostError> {
        let spawned = thread::Builder::new()
            .name(format!("rusplay-host-{stage}"))
            .spawn(body);

        match spawned {
            Ok(handle) => {
                self.stages.push((stage, handle));
                Ok(())
            }
            Err(source) => {
                self.shared.stop();
                self.join().ok();
                Err(HostError::SpawnStage { stage, source })
            }
        }
    }
}

impl Drop for HostStream {
    fn drop(&mut self) {
        self.shared.stop();
        self.join().ok();
    }
}
