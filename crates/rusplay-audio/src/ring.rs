use std::mem::size_of;

use rtrb::{Consumer, Producer, RingBuffer};

const BYTES_PER_SAMPLE: usize = size_of::<f32>();
pub(crate) const MIN_RING_MILLISECONDS: u32 = 250;
const JITTER_RING_MULTIPLE: u32 = 3;
const MAX_DEVICE_RATE: u32 = 192_000;

pub(crate) fn ring_milliseconds(jitter_ms: u32) -> u32 {
    MIN_RING_MILLISECONDS.max(jitter_ms.saturating_mul(JITTER_RING_MULTIPLE))
}

pub(crate) fn playback_ring_rate(configured: u32) -> u32 {
    configured.max(MAX_DEVICE_RATE)
}

pub(crate) fn frames_per_ring(rate: u32, milliseconds: u32) -> usize {
    rate as usize * milliseconds as usize / 1000
}

pub(crate) struct RingWriter {
    producer: Producer<f32>,
    channels: usize,
    capacity: usize,
}

impl RingWriter {
    pub(crate) fn write(&mut self, samples: &[f32]) -> usize {
        let writable = self.producer.slots().min(samples.len()) / self.channels * self.channels;
        match self.producer.push_entire_slice(&samples[..writable]) {
            Ok(()) => samples.len() - writable,
            Err(_) => samples.len(),
        }
    }

    pub(crate) fn write_le_bytes(&mut self, bytes: &[u8]) -> usize {
        let (chunks, _) = bytes.as_chunks::<BYTES_PER_SAMPLE>();
        let writable = self.producer.slots().min(chunks.len()) / self.channels * self.channels;
        let Ok(chunk) = self.producer.write_chunk_uninit(writable) else {
            return chunks.len();
        };

        let filled = chunk.fill_from_iter(chunks.iter().map(|bytes| f32::from_le_bytes(*bytes)));
        chunks.len() - filled
    }

    pub(crate) fn dropped_frames(&self, samples: usize) -> usize {
        samples / self.channels
    }

    pub(crate) fn buffered_frames(&self) -> usize {
        (self.capacity - self.producer.slots()) / self.channels
    }
}

pub(crate) fn sample_ring(
    rate: u32,
    channels: usize,
    milliseconds: u32,
) -> (RingWriter, Consumer<f32>) {
    let capacity = frames_per_ring(rate, milliseconds) * channels;
    let (producer, consumer) = RingBuffer::new(capacity);
    (
        RingWriter {
            producer,
            channels,
            capacity,
        },
        consumer,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le_bytes(samples: &[f32]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect()
    }

    #[test]
    fn bytes_are_converted_straight_into_the_ring() {
        let (mut writer, mut consumer) = sample_ring(48_000, 2, 250);
        let samples: Vec<f32> = (0..20u8).map(f32::from).collect();

        let dropped = writer.write_le_bytes(&le_bytes(&samples));
        let mut read = vec![0.0; samples.len()];
        consumer.pop_entire_slice(&mut read).unwrap();

        assert_eq!(dropped, 0);
        assert_eq!(read, samples);
    }

    #[test]
    fn a_trailing_partial_frame_or_sample_is_dropped() {
        let (mut writer, consumer) = sample_ring(48_000, 2, 250);
        let mut bytes = le_bytes(&[1.0, 2.0, 3.0]);
        bytes.push(7);

        let dropped = writer.write_le_bytes(&bytes);

        assert_eq!(dropped, 1);
        assert_eq!(consumer.slots(), 2);
    }

    #[test]
    fn a_full_ring_drops_the_surplus_in_whole_frames() {
        let (mut writer, _consumer) = sample_ring(1000, 2, 1000);
        let flood = le_bytes(&[0.5; 2003]);

        let dropped = writer.write_le_bytes(&flood);

        assert_eq!(dropped, 3);
        assert_eq!(writer.buffered_frames(), 1000);
    }

    #[test]
    fn the_ring_grows_with_the_jitter_buffer() {
        assert_eq!(ring_milliseconds(10), 250);
        assert_eq!(ring_milliseconds(500), 1500);
    }
}
