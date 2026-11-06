//! Opt-in, bounded capture of the packets actually submitted to the decoder.
use std::{
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::Path,
    sync::mpsc::{self, SyncSender},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use rusplay_core::Codec;

use crate::receive::EncodedVideo;

const LIMIT: usize = 64 * 1024 * 1024;

pub(crate) struct PacketDump {
    queue: SyncSender<(Vec<u8>, String)>,
    remaining: usize,
    started: bool,
}

impl PacketDump {
    pub(crate) fn from_env() -> Option<Self> {
        let root = std::env::var_os("RUSPLAY_HEVC_DUMP_DIR")?;
        match Self::open(Path::new(&root)) {
            Ok(dump) => Some(dump),
            Err(error) => {
                eprintln!("HEVC capture could not start: {error}");
                None
            }
        }
    }

    fn open(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = root.join(format!("{}-{stamp}", std::process::id()));
        fs::create_dir(&directory)?;
        let mut video = BufWriter::new(File::create(directory.join("stream.hevc"))?);
        let mut index = BufWriter::new(File::create(directory.join("stream.index"))?);
        let (queue, packets) = mpsc::sync_channel::<(Vec<u8>, String)>(8);
        thread::Builder::new()
            .name("rusplay-packet-dump".into())
            .spawn(move || {
                for (bytes, record) in packets {
                    let result = video
                        .write_all(&bytes)
                        .and_then(|()| writeln!(index, "{record}"))
                        .and_then(|()| video.flush())
                        .and_then(|()| index.flush());
                    if let Err(error) = result {
                        eprintln!("HEVC capture stopped: {error}");
                        return;
                    }
                }
            })?;
        eprintln!("HEVC capture: {} (maximum 64 MiB)", directory.display());
        Ok(Self {
            queue,
            remaining: LIMIT,
            started: false,
        })
    }

    pub(crate) fn record(&mut self, video: &EncodedVideo, counters: &crate::stats::Counters) {
        if self.remaining == 0 || video.stream.codec != Codec::Hevc {
            return;
        }
        if !self.started && !video.frame.keyframe {
            return;
        }
        self.started = true;
        let length = video.frame.data.len();
        if length > self.remaining {
            self.remaining = 0;
            eprintln!("HEVC capture reached its size limit");
            return;
        }
        // First column is the access-unit size, as expected by the present
        // replay tests. Remaining fields preserve loss and stream metadata.
        let record = format!(
            "{length} {} {} {} {:?}",
            video.number, video.frame.keyframe, video.captured_micros, video.stream
        );
        let record = format!("{record} stats={:?}", counters.snapshot());
        if self
            .queue
            .try_send((video.frame.data.to_vec(), record))
            .is_err()
        {
            // Stop instead of silently creating an incomplete predictive stream.
            self.remaining = 0;
            eprintln!("HEVC capture stopped: writer could not keep up");
        } else {
            self.remaining -= length;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusplay_core::{
        ChromaSiting, CodecFormat, ColorSpace, EncodedFrame, PixelFormat, Resolution, Timestamp,
    };
    use rusplay_protocol::StreamDescription;
    use std::time::Instant;

    fn packet(keyframe: bool) -> EncodedVideo {
        EncodedVideo {
            frame: EncodedFrame {
                codec: Codec::Hevc,
                timestamp: Timestamp::ZERO,
                keyframe,
                data: vec![1, 2, 3].into(),
            },
            number: 7,
            stream: StreamDescription {
                generation: 0,
                codec: Codec::Hevc,
                resolution: Resolution::new(2560, 1440),
                format: PixelFormat::default(),
                color: ColorSpace::default(),
                configured: CodecFormat::new(Codec::Hevc, PixelFormat::default()),
                siting: ChromaSiting::Left,
            },
            recovers_from: None,
            captured_micros: 0,
            received_at: Instant::now(),
        }
    }

    #[test]
    fn capture_waits_for_a_keyframe_and_stops_at_the_byte_limit() {
        let (queue, received) = mpsc::sync_channel(8);
        let mut dump = PacketDump {
            queue,
            remaining: 5,
            started: false,
        };
        let counters = crate::stats::Counters::default();
        dump.record(&packet(false), &counters);
        assert!(received.try_recv().is_err());
        dump.record(&packet(true), &counters);
        let (bytes, record) = received.try_recv().unwrap();
        assert_eq!(bytes, [1, 2, 3]);
        assert!(record.starts_with("3 7 true 0 "));
        assert!(record.contains("stats="));
        dump.record(&packet(false), &counters);
        assert_eq!(dump.remaining, 0);
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn a_full_writer_queue_stops_capture_without_waiting() {
        let (queue, _received) = mpsc::sync_channel(0);
        let mut dump = PacketDump {
            queue,
            remaining: LIMIT,
            started: false,
        };
        dump.record(&packet(true), &crate::stats::Counters::default());
        assert_eq!(dump.remaining, 0);
    }
}
