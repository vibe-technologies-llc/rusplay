use rtrb::{Consumer, Producer, RingBuffer};

const RING_MILLISECONDS: usize = 250;

pub(crate) fn frames_per_ring(rate: u32) -> usize {
    rate as usize * RING_MILLISECONDS / 1000
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

    pub(crate) fn buffered_frames(&self) -> usize {
        (self.capacity - self.producer.slots()) / self.channels
    }
}

pub(crate) fn sample_ring(rate: u32, channels: usize) -> (RingWriter, Consumer<f32>) {
    let capacity = frames_per_ring(rate) * channels;
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
