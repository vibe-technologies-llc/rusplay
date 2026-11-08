#[cfg(test)]
mod tests;

use std::{
    collections::VecDeque,
    io,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rusplay_core::{FrameRate, QualityScore, SamplePicture, SamplePlane, TemporalOrder, measure};
use rusplay_protocol::QualityUnavailableReason;

const QUALITY_THREAD: &str = "rusplay-host-quality";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SampleKind {
    Luma,
    Picture,
}

#[derive(Debug)]
pub(crate) enum Sample {
    Luma(SamplePlane),
    Picture(Box<PictureSample>),
    Unsupported,
    Failed,
}

#[derive(Debug)]
pub(crate) struct PictureSample {
    pub source: SamplePicture,
    pub reconstruction: Option<SamplePicture>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum QualityEvent {
    Report { frame: u32, score: QualityScore },
    Unavailable(QualityUnavailableReason),
}

pub(crate) trait QualitySink: Send + Sync {
    fn deliver(&self, event: QualityEvent);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QualitySettings {
    pub interval: Option<Duration>,
    pub order: TemporalOrder,
}

impl QualitySettings {
    pub(crate) fn new(interval_ms: u32, rate: FrameRate) -> Self {
        Self {
            interval: (interval_ms > 0).then(|| Duration::from_millis(u64::from(interval_ms))),
            order: TemporalOrder::for_rate(rate),
        }
    }
}

struct Job {
    frame: u32,
    source: SamplePicture,
    reconstruction: SamplePicture,
    history: Vec<SamplePlane>,
    order: TemporalOrder,
}

struct QualityWorker {
    jobs: Option<SyncSender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl QualityWorker {
    fn spawn(sink: Arc<dyn QualitySink>) -> io::Result<Self> {
        let (jobs, queue) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name(QUALITY_THREAD.into())
            .spawn(move || work(&queue, &*sink))?;

        Ok(Self {
            jobs: Some(jobs),
            thread: Some(thread),
        })
    }

    fn submit(&self, job: Job) -> bool {
        self.jobs
            .as_ref()
            .is_some_and(|jobs| match jobs.try_send(job) {
                Ok(()) => true,
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
            })
    }
}

impl Drop for QualityWorker {
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

fn work(queue: &Receiver<Job>, sink: &dyn QualitySink) {
    while let Ok(job) = queue.recv() {
        let history: Vec<&SamplePlane> = job.history.iter().collect();
        let event = match measure(&job.source, &job.reconstruction, &history, job.order) {
            Ok(score) => QualityEvent::Report {
                frame: job.frame,
                score,
            },
            Err(_) => QualityEvent::Unavailable(QualityUnavailableReason::MeasurementFailed),
        };
        sink.deliver(event);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    None,
    HostDisabled,
    EncoderUnsupported,
}

pub(crate) struct QualitySampler {
    settings: QualitySettings,
    sink: Arc<dyn QualitySink>,
    worker: Option<QualityWorker>,
    due: Option<Instant>,
    collecting: bool,
    history: VecDeque<SamplePlane>,
    refused: Refusal,
    was_requested: bool,
}

impl QualitySampler {
    pub(crate) fn new(settings: QualitySettings, sink: Arc<dyn QualitySink>) -> Self {
        Self {
            settings,
            sink,
            worker: None,
            due: None,
            collecting: false,
            history: VecDeque::new(),
            refused: Refusal::None,
            was_requested: false,
        }
    }

    pub(crate) fn plan(&mut self, requested: bool, now: Instant) -> Option<SampleKind> {
        if requested != self.was_requested {
            self.was_requested = requested;
            self.restart();
            self.refused = Refusal::None;
            self.due = None;
        }
        if !requested {
            return None;
        }

        let Some(interval) = self.settings.interval else {
            self.refuse(Refusal::HostDisabled);
            return None;
        };
        if self.refused != Refusal::None {
            return None;
        }
        if !self.collecting && self.due.is_some_and(|due| now < due) {
            return None;
        }
        if self.worker.is_none() {
            let Ok(worker) = QualityWorker::spawn(Arc::clone(&self.sink)) else {
                self.sink.deliver(QualityEvent::Unavailable(
                    QualityUnavailableReason::MeasurementFailed,
                ));
                self.due = Some(now + interval);
                return None;
            };
            self.worker = Some(worker);
        }

        self.collecting = true;
        if self.history.len() < self.settings.order.history_frames() {
            Some(SampleKind::Luma)
        } else {
            Some(SampleKind::Picture)
        }
    }

    pub(crate) fn record(&mut self, sample: Option<Sample>, frame: Option<u32>, now: Instant) {
        if !self.collecting {
            return;
        }

        match (sample, frame) {
            (Some(Sample::Luma(luma)), Some(_)) => self.remember(luma),
            (Some(Sample::Picture(picture)), Some(frame)) => {
                let PictureSample {
                    source,
                    reconstruction,
                } = *picture;
                match reconstruction {
                    Some(reconstruction) => self.measure(frame, source, reconstruction, now),
                    None => self.restart(),
                }
            }
            (Some(Sample::Unsupported), _) => self.refuse(Refusal::EncoderUnsupported),
            (Some(Sample::Failed), _) => {
                self.sink.deliver(QualityEvent::Unavailable(
                    QualityUnavailableReason::MeasurementFailed,
                ));
                self.finish(now);
            }
            _ => self.restart(),
        }
    }

    pub(crate) fn interrupt(&mut self) {
        self.restart();
    }

    fn remember(&mut self, luma: SamplePlane) {
        let same_size = |earlier: &SamplePlane| {
            earlier.width() == luma.width() && earlier.height() == luma.height()
        };
        if !self.history.iter().all(same_size) {
            self.history.clear();
        }
        self.history.push_back(luma);
        while self.history.len() > self.settings.order.history_frames() {
            self.history.pop_front();
        }
    }

    fn measure(
        &mut self,
        frame: u32,
        source: SamplePicture,
        reconstruction: SamplePicture,
        now: Instant,
    ) {
        let luma = source.luma();
        let history_fits = self
            .history
            .iter()
            .all(|earlier| earlier.width() == luma.width() && earlier.height() == luma.height());
        if !history_fits {
            self.restart();
            return;
        }

        let job = Job {
            frame,
            source,
            reconstruction,
            history: self.history.drain(..).collect(),
            order: self.settings.order,
        };
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.submit(job))
        {
            self.finish(now);
        } else {
            self.restart();
        }
    }

    fn finish(&mut self, now: Instant) {
        self.restart();
        self.due = self.settings.interval.map(|interval| now + interval);
    }

    fn restart(&mut self) {
        self.collecting = false;
        self.history.clear();
    }

    fn refuse(&mut self, refusal: Refusal) {
        self.restart();
        if self.refused == refusal {
            return;
        }

        self.refused = refusal;
        let reason = match refusal {
            Refusal::None => return,
            Refusal::HostDisabled => QualityUnavailableReason::HostDisabled,
            Refusal::EncoderUnsupported => QualityUnavailableReason::EncoderUnsupported,
        };
        self.sink.deliver(QualityEvent::Unavailable(reason));
    }
}
