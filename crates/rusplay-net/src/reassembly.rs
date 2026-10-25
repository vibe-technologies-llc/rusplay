use std::time::{Duration, Instant};

use rusplay_config::NetworkConfig;
use rusplay_protocol::{FecHeader, PacketFlags, PacketHeader, StreamKind};

use crate::{FecBlock, NetError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReassemblyLimits {
    pub max_pending_frames: usize,
    pub max_fragments: u16,
    pub frame_timeout: Duration,
}

impl Default for ReassemblyLimits {
    fn default() -> Self {
        Self {
            max_pending_frames: 8,
            max_fragments: 8192,
            frame_timeout: Duration::from_millis(100),
        }
    }
}

impl From<&NetworkConfig> for ReassemblyLimits {
    fn from(config: &NetworkConfig) -> Self {
        Self {
            frame_timeout: Duration::from_millis(u64::from(config.reassembly_timeout_ms)),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReassembledFrame {
    pub frame: u32,
    pub keyframe: bool,
    pub data: Vec<u8>,
    pub recovered_fragments: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReassemblyEvent {
    Frame(ReassembledFrame),
    FrameLost { frame: u32 },
    PacketsMissing { first_sequence: u32, count: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReassemblyStats {
    pub frames_completed: u64,
    pub frames_recovered: u64,
    pub frames_lost: u64,
    pub packets_missing: u64,
    pub stale_packets: u64,
    pub duplicate_packets: u64,
}

#[derive(Debug)]
pub struct Reassembler {
    stream: StreamKind,
    limits: ReassemblyLimits,
    pending: Vec<PendingFrame>,
    floor: Option<u32>,
    next_sequence: Option<u32>,
    stats: ReassemblyStats,
}

impl Reassembler {
    pub fn new(stream: StreamKind, limits: ReassemblyLimits) -> Self {
        Self {
            stream,
            limits,
            pending: Vec::new(),
            floor: None,
            next_sequence: None,
            stats: ReassemblyStats::default(),
        }
    }

    pub const fn stats(&self) -> ReassemblyStats {
        self.stats
    }

    pub fn pending_frames(&self) -> usize {
        self.pending.len()
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.floor = None;
        self.next_sequence = None;
    }

    pub fn next_timeout(&self) -> Option<Instant> {
        self.pending
            .iter()
            .map(|pending| pending.first_seen + self.limits.frame_timeout)
            .min()
    }

    pub fn push(
        &mut self,
        datagram: &[u8],
        now: Instant,
        events: &mut Vec<ReassemblyEvent>,
    ) -> Result<(), NetError> {
        let (header, payload) = PacketHeader::parse(datagram)?;
        let packet = self.classify(header.stream, payload)?;

        self.track_sequence(header.sequence, events);

        if self.is_stale(header.frame) {
            self.stats.stale_packets += 1;
            return Ok(());
        }

        let existing = self.take_pending(header.frame);
        let is_new = existing.is_none();
        let mut pending = existing.unwrap_or_else(|| PendingFrame::new(header.frame, now));

        let inserted = match packet {
            Packet::Data(payload) => pending.insert_data(&header, payload, &self.limits),
            Packet::Parity(fec, parity) => {
                pending.insert_parity(&header, &fec, parity, &self.limits)
            }
        };
        let outcome = inserted.and_then(|inserted| {
            pending.try_recover()?;
            Ok(inserted)
        });

        match outcome {
            Err(error) => {
                if !is_new {
                    self.insert_pending(pending);
                }
                Err(error)
            }
            Ok(Inserted::Duplicate) => {
                self.stats.duplicate_packets += 1;
                self.insert_pending(pending);
                Ok(())
            }
            Ok(Inserted::New) if pending.is_complete() => self.complete(pending, events),
            Ok(Inserted::New) => {
                self.insert_pending(pending);
                self.enforce_window(events);
                Ok(())
            }
        }
    }

    pub fn poll_timeouts(&mut self, now: Instant, events: &mut Vec<ReassemblyEvent>) {
        while let Some(expired) = self
            .pending
            .iter()
            .filter(|pending| {
                now.saturating_duration_since(pending.first_seen) >= self.limits.frame_timeout
            })
            .map(|pending| pending.frame)
            .reduce(|newest, frame| {
                if is_newer(frame, newest) {
                    frame
                } else {
                    newest
                }
            })
        {
            self.drop_lost_through(expired, events);
        }
    }

    fn classify<'a>(&self, stream: StreamKind, payload: &'a [u8]) -> Result<Packet<'a>, NetError> {
        if stream == self.stream {
            return Ok(Packet::Data(payload));
        }

        if stream == StreamKind::Fec {
            let (fec, parity) = FecHeader::parse(payload)?;
            if fec.protects == self.stream {
                return Ok(Packet::Parity(fec, parity));
            }
        }

        Err(NetError::StreamMismatch {
            expected: self.stream,
            found: stream,
        })
    }

    fn track_sequence(&mut self, sequence: u32, events: &mut Vec<ReassemblyEvent>) {
        let Some(expected) = self.next_sequence else {
            self.next_sequence = Some(sequence.wrapping_add(1));
            return;
        };

        let ahead = i64::from(sequence.wrapping_sub(expected).cast_signed());
        if ahead < 0 {
            return;
        }

        if ahead > 0 {
            let count = u16::try_from(ahead).unwrap_or(u16::MAX);
            self.stats.packets_missing += u64::from(count);
            events.push(ReassemblyEvent::PacketsMissing {
                first_sequence: expected,
                count,
            });
        }
        self.next_sequence = Some(sequence.wrapping_add(1));
    }

    fn is_stale(&self, frame: u32) -> bool {
        self.floor.is_some_and(|floor| !is_newer(frame, floor))
    }

    fn take_pending(&mut self, frame: u32) -> Option<PendingFrame> {
        let position = self
            .pending
            .iter()
            .position(|pending| pending.frame == frame)?;
        Some(self.pending.remove(position))
    }

    fn insert_pending(&mut self, pending: PendingFrame) {
        let position = self
            .pending
            .partition_point(|other| is_newer(pending.frame, other.frame));
        self.pending.insert(position, pending);
    }

    fn enforce_window(&mut self, events: &mut Vec<ReassemblyEvent>) {
        while self.pending.len() > self.limits.max_pending_frames {
            let oldest = self.pending[0].frame;
            self.drop_lost_through(oldest, events);
        }
    }

    fn drop_lost_through(&mut self, frame: u32, events: &mut Vec<ReassemblyEvent>) {
        while self
            .pending
            .first()
            .is_some_and(|first| !is_newer(first.frame, frame))
        {
            let lost = self.pending.remove(0);
            self.stats.frames_lost += 1;
            events.push(ReassemblyEvent::FrameLost { frame: lost.frame });
        }

        if self.floor.is_none_or(|floor| is_newer(frame, floor)) {
            self.floor = Some(frame);
        }
    }

    fn complete(
        &mut self,
        pending: PendingFrame,
        events: &mut Vec<ReassemblyEvent>,
    ) -> Result<(), NetError> {
        let frame = pending.frame;
        self.drop_lost_through(frame, events);

        match pending.assemble() {
            Ok(assembled) => {
                self.stats.frames_completed += 1;
                if assembled.recovered_fragments > 0 {
                    self.stats.frames_recovered += 1;
                }
                events.push(ReassemblyEvent::Frame(assembled));
                Ok(())
            }
            Err(error) => {
                self.stats.frames_lost += 1;
                events.push(ReassemblyEvent::FrameLost { frame });
                Err(error)
            }
        }
    }
}

enum Packet<'a> {
    Data(&'a [u8]),
    Parity(FecHeader, &'a [u8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inserted {
    New,
    Duplicate,
}

#[derive(Debug)]
struct ParityBlock {
    block: FecBlock,
    shards: Vec<Option<Vec<u8>>>,
    received: usize,
    decoded_with: usize,
}

#[derive(Debug)]
struct PendingFrame {
    frame: u32,
    first_seen: Instant,
    keyframe: bool,
    fragment_count: Option<u16>,
    shard_len: Option<usize>,
    frame_bytes: Option<u32>,
    fragments: Vec<Option<Vec<u8>>>,
    present: usize,
    recovered: u16,
    blocks: Vec<ParityBlock>,
}

impl PendingFrame {
    fn new(frame: u32, first_seen: Instant) -> Self {
        Self {
            frame,
            first_seen,
            keyframe: false,
            fragment_count: None,
            shard_len: None,
            frame_bytes: None,
            fragments: Vec::new(),
            present: 0,
            recovered: 0,
            blocks: Vec::new(),
        }
    }

    fn is_complete(&self) -> bool {
        self.fragment_count
            .is_some_and(|count| self.present == usize::from(count))
    }

    fn insert_data(
        &mut self,
        header: &PacketHeader,
        payload: &[u8],
        limits: &ReassemblyLimits,
    ) -> Result<Inserted, NetError> {
        let count = header.fragment_count;
        let index = header.fragment_index;

        if count > limits.max_fragments {
            return Err(NetError::FragmentCountTooLarge {
                count: u32::from(count),
                max: limits.max_fragments,
            });
        }

        if let Some(expected) = self.fragment_count
            && expected != count
        {
            return Err(NetError::FragmentCountMismatch {
                frame: self.frame,
                expected,
                found: count,
            });
        }

        let is_last = index + 1 == count;
        let learned_shard_len = (!is_last && self.shard_len.is_none()).then_some(payload.len());
        if let Some(len) = learned_shard_len
            && (len == 0 || !len.is_multiple_of(2))
        {
            return Err(NetError::InvalidShardLength(len));
        }
        let shard_len = self.shard_len.or(learned_shard_len);

        let length_error = || NetError::FragmentLengthMismatch {
            frame: self.frame,
            index,
            len: payload.len(),
        };
        if !fragment_fits(index, payload.len(), count, shard_len, self.frame_bytes) {
            return Err(length_error());
        }

        if learned_shard_len.is_some() {
            self.check_stored(count, shard_len, self.frame_bytes)?;
        }

        self.fragment_count = Some(count);
        self.shard_len = shard_len;
        self.keyframe |= header.flags.contains(PacketFlags::KEYFRAME);
        if self.fragments.len() != usize::from(count) {
            self.fragments.resize(usize::from(count), None);
        }

        let slot = &mut self.fragments[usize::from(index)];
        if slot.is_some() {
            return Ok(Inserted::Duplicate);
        }

        *slot = Some(payload.to_vec());
        self.present += 1;
        Ok(Inserted::New)
    }

    fn insert_parity(
        &mut self,
        header: &PacketHeader,
        fec: &FecHeader,
        parity: &[u8],
        limits: &ReassemblyLimits,
    ) -> Result<Inserted, NetError> {
        let shard_len = parity.len();
        if shard_len == 0 || !shard_len.is_multiple_of(2) {
            return Err(NetError::InvalidShardLength(shard_len));
        }

        if fec.frame_bytes == 0 {
            return Err(NetError::ZeroFrameBytes);
        }

        let fragments = (fec.frame_bytes as usize).div_ceil(shard_len);
        let count = u16::try_from(fragments)
            .ok()
            .filter(|count| *count <= limits.max_fragments)
            .ok_or(NetError::FragmentCountTooLarge {
                count: u32::try_from(fragments).unwrap_or(u32::MAX),
                max: limits.max_fragments,
            })?;

        if let Some(expected) = self.fragment_count
            && expected != count
        {
            return Err(NetError::FragmentCountMismatch {
                frame: self.frame,
                expected,
                found: count,
            });
        }

        if let Some(expected) = self.shard_len
            && expected != shard_len
        {
            return Err(NetError::ShardLengthMismatch {
                expected,
                found: shard_len,
            });
        }

        if let Some(expected) = self.frame_bytes
            && expected != fec.frame_bytes
        {
            return Err(NetError::FrameBytesMismatch {
                expected,
                found: fec.frame_bytes,
            });
        }

        let known = self.locate_block(fec, count)?;

        if self.shard_len.is_none() || self.frame_bytes.is_none() {
            self.check_stored(count, Some(shard_len), Some(fec.frame_bytes))?;
        }

        self.fragment_count = Some(count);
        self.shard_len = Some(shard_len);
        self.frame_bytes = Some(fec.frame_bytes);
        self.keyframe |= header.flags.contains(PacketFlags::KEYFRAME);
        if self.fragments.len() != usize::from(count) {
            self.fragments.resize(usize::from(count), None);
        }

        let position = known.unwrap_or_else(|| {
            self.blocks.push(ParityBlock {
                block: FecBlock {
                    first_fragment: fec.first_fragment,
                    data_shards: fec.data_shards,
                    parity_shards: fec.parity_shards,
                },
                shards: vec![None; usize::from(fec.parity_shards)],
                received: 0,
                decoded_with: 0,
            });
            self.blocks.len() - 1
        });

        let slot = &mut self.blocks[position].shards[usize::from(fec.parity_index)];
        if slot.is_some() {
            return Ok(Inserted::Duplicate);
        }

        *slot = Some(parity.to_vec());
        self.blocks[position].received += 1;
        Ok(Inserted::New)
    }

    fn locate_block(&self, fec: &FecHeader, count: u16) -> Result<Option<usize>, NetError> {
        let start = usize::from(fec.first_fragment);
        let end = start + usize::from(fec.data_shards);
        if end > usize::from(count) {
            return Err(NetError::FecBlockOutOfRange {
                first_fragment: fec.first_fragment,
                data_shards: fec.data_shards,
                fragment_count: count,
            });
        }

        let conflict = NetError::FecBlockConflict {
            first_fragment: fec.first_fragment,
        };
        let same_start = self
            .blocks
            .iter()
            .position(|known| known.block.first_fragment == fec.first_fragment);
        if let Some(position) = same_start {
            let known = &self.blocks[position].block;
            let same_shape =
                known.data_shards == fec.data_shards && known.parity_shards == fec.parity_shards;
            return if same_shape {
                Ok(Some(position))
            } else {
                Err(conflict)
            };
        }

        let overlaps = self.blocks.iter().any(|known| {
            let known_start = usize::from(known.block.first_fragment);
            let known_end = known_start + usize::from(known.block.data_shards);
            start < known_end && known_start < end
        });
        if overlaps { Err(conflict) } else { Ok(None) }
    }

    fn check_stored(
        &self,
        count: u16,
        shard_len: Option<usize>,
        frame_bytes: Option<u32>,
    ) -> Result<(), NetError> {
        for (index, fragment) in self.fragments.iter().enumerate() {
            let Some(fragment) = fragment else { continue };
            let index = u16::try_from(index).unwrap_or(u16::MAX);

            if !fragment_fits(index, fragment.len(), count, shard_len, frame_bytes) {
                return Err(NetError::FragmentLengthMismatch {
                    frame: self.frame,
                    index,
                    len: fragment.len(),
                });
            }
        }

        Ok(())
    }

    fn try_recover(&mut self) -> Result<(), NetError> {
        let (Some(count), Some(shard_len), Some(frame_bytes)) =
            (self.fragment_count, self.shard_len, self.frame_bytes)
        else {
            return Ok(());
        };

        let last_len = frame_bytes as usize - (usize::from(count) - 1) * shard_len;

        for position in 0..self.blocks.len() {
            let block = self.blocks[position].block;
            let first = usize::from(block.first_fragment);
            let range = first..first + usize::from(block.data_shards);

            let missing = range
                .clone()
                .filter(|index| self.fragments[*index].is_none())
                .count();
            let received = self.blocks[position].received;
            if missing == 0 || received < missing || self.blocks[position].decoded_with == received
            {
                continue;
            }
            self.blocks[position].decoded_with = received;

            let padded: Vec<(usize, Vec<u8>)> = range
                .clone()
                .filter_map(|index| {
                    let fragment = self.fragments[index].as_ref()?;
                    let mut shard = fragment.clone();
                    shard.resize(shard_len, 0);
                    Some((index - first, shard))
                })
                .collect();
            let parity = self.blocks[position]
                .shards
                .iter()
                .enumerate()
                .filter_map(|(index, shard)| Some((index, shard.as_deref()?)));

            let restored = block.recover(
                padded
                    .iter()
                    .map(|(index, shard)| (*index, shard.as_slice())),
                parity,
            )?;

            for (relative, mut shard) in restored {
                let index = first + relative;
                if index + 1 == usize::from(count) {
                    shard.truncate(last_len);
                }
                self.fragments[index] = Some(shard);
                self.present += 1;
                self.recovered += 1;
            }
        }

        Ok(())
    }

    fn assemble(self) -> Result<ReassembledFrame, NetError> {
        let data: Vec<u8> = self.fragments.into_iter().flatten().flatten().collect();

        if let Some(expected) = self.frame_bytes
            && data.len() != expected as usize
        {
            return Err(NetError::FrameLengthMismatch {
                frame: self.frame,
                expected,
                assembled: data.len(),
            });
        }

        Ok(ReassembledFrame {
            frame: self.frame,
            keyframe: self.keyframe,
            data,
            recovered_fragments: self.recovered,
        })
    }
}

fn fragment_fits(
    index: u16,
    len: usize,
    count: u16,
    shard_len: Option<usize>,
    frame_bytes: Option<u32>,
) -> bool {
    if len == 0 {
        return false;
    }

    let is_last = index + 1 == count;

    if let Some(shard_len) = shard_len {
        if len > shard_len || (!is_last && len != shard_len) {
            return false;
        }

        if is_last && let Some(frame_bytes) = frame_bytes {
            let tail = frame_bytes as usize - (usize::from(count) - 1) * shard_len;
            return len == tail;
        }
    }

    true
}

fn is_newer(a: u32, b: u32) -> bool {
    a.wrapping_sub(b).cast_signed() > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FecScheme, Packetizer};

    const MAX_DATAGRAM: usize = 100;
    const SHARD_LEN: usize = 74;

    fn frame_data(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|index| u8::try_from(index % 251).unwrap().wrapping_add(seed))
            .collect()
    }

    fn packetizer(stream: StreamKind, scheme: FecScheme) -> Packetizer {
        Packetizer::new(stream, MAX_DATAGRAM, scheme).unwrap()
    }

    fn reassembler(stream: StreamKind) -> Reassembler {
        Reassembler::new(stream, ReassemblyLimits::default())
    }

    fn feed<'a>(
        reassembler: &mut Reassembler,
        now: Instant,
        datagrams: impl IntoIterator<Item = &'a Vec<u8>>,
    ) -> Vec<ReassemblyEvent> {
        let mut events = Vec::new();
        for datagram in datagrams {
            reassembler.push(datagram, now, &mut events).unwrap();
        }
        events
    }

    fn frames(events: &[ReassemblyEvent]) -> Vec<&ReassembledFrame> {
        events
            .iter()
            .filter_map(|event| match event {
                ReassemblyEvent::Frame(frame) => Some(frame),
                _ => None,
            })
            .collect()
    }

    fn lost(events: &[ReassemblyEvent]) -> Vec<u32> {
        events
            .iter()
            .filter_map(|event| match event {
                ReassemblyEvent::FrameLost { frame } => Some(*frame),
                _ => None,
            })
            .collect()
    }

    fn parity_of(datagram: &[u8]) -> bool {
        PacketHeader::read(datagram).unwrap().stream == StreamKind::Fec
    }

    #[test]
    fn frame_in_order_round_trips() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let data = frame_data(SHARD_LEN * 3 + 10, 1);

        let datagrams = tx.packetize(0, true, &data).unwrap();
        let events = feed(&mut rx, Instant::now(), &datagrams);

        let received = frames(&events);
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].data, data);
        assert!(received[0].keyframe);
        assert_eq!(received[0].recovered_fragments, 0);
        assert_eq!(rx.pending_frames(), 0);
    }

    #[test]
    fn single_fragment_frame_round_trips() {
        let mut tx = packetizer(StreamKind::Audio, FecScheme::None);
        let mut rx = reassembler(StreamKind::Audio);

        let datagrams = tx.packetize(3, false, &[1, 2, 3]).unwrap();
        let events = feed(&mut rx, Instant::now(), &datagrams);

        assert_eq!(frames(&events)[0].data, [1, 2, 3]);
        assert!(!frames(&events)[0].keyframe);
    }

    #[test]
    fn reordered_fragments_assemble() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let data = frame_data(SHARD_LEN * 5, 2);

        let mut datagrams = tx.packetize(0, false, &data).unwrap();
        datagrams.reverse();
        let events = feed(&mut rx, Instant::now(), &datagrams);

        assert_eq!(frames(&events)[0].data, data);
    }

    #[test]
    fn duplicate_fragments_are_counted_and_ignored() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let data = frame_data(SHARD_LEN * 2, 3);

        let datagrams = tx.packetize(0, false, &data).unwrap();
        let events = feed(
            &mut rx,
            Instant::now(),
            [&datagrams[0], &datagrams[0], &datagrams[1]],
        );

        assert_eq!(frames(&events).len(), 1);
        assert_eq!(rx.stats().duplicate_packets, 1);
    }

    #[test]
    fn lost_fragment_without_fec_times_out_as_lost() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let data = frame_data(SHARD_LEN * 3, 4);
        let start = Instant::now();

        let datagrams = tx.packetize(10, false, &data).unwrap();
        let mut events = feed(&mut rx, start, [&datagrams[0], &datagrams[2]]);
        rx.poll_timeouts(start + Duration::from_millis(99), &mut events);
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, ReassemblyEvent::FrameLost { .. }))
        );

        rx.poll_timeouts(start + Duration::from_millis(100), &mut events);

        assert_eq!(lost(&events), [10]);
        assert_eq!(rx.stats().frames_lost, 1);
        assert_eq!(rx.pending_frames(), 0);
    }

    #[test]
    fn next_timeout_tracks_the_oldest_pending_frame() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let start = Instant::now();

        assert_eq!(rx.next_timeout(), None);

        let datagrams = tx
            .packetize(0, false, &frame_data(SHARD_LEN * 2, 0))
            .unwrap();
        feed(&mut rx, start, [&datagrams[0]]);

        assert_eq!(rx.next_timeout(), Some(start + Duration::from_millis(100)));
    }

    #[test]
    fn fec_recovers_lost_data_fragments() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::for_overhead(8, 50));
        let mut rx = reassembler(StreamKind::Video);
        let data = frame_data(SHARD_LEN * 8 + 20, 5);

        let datagrams = tx.packetize(0, false, &data).unwrap();
        let surviving: Vec<_> = datagrams
            .iter()
            .enumerate()
            .filter(|(index, _)| ![1, 4, 8].contains(index))
            .map(|(_, datagram)| datagram)
            .collect();
        let events = feed(&mut rx, Instant::now(), surviving);

        let received = frames(&events);
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].data, data);
        assert_eq!(received[0].recovered_fragments, 3);
        assert_eq!(rx.stats().frames_recovered, 1);
    }

    #[test]
    fn fec_recovers_a_lost_short_tail_fragment_with_its_true_length() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::for_overhead(4, 50));
        let mut rx = reassembler(StreamKind::Video);
        let data = frame_data(SHARD_LEN * 2 + 7, 6);

        let datagrams = tx.packetize(0, false, &data).unwrap();
        let surviving = datagrams
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != 2);
        let events = feed(
            &mut rx,
            Instant::now(),
            surviving.map(|(_, datagram)| datagram),
        );

        assert_eq!(frames(&events)[0].data, data);
    }

    #[test]
    fn fec_alone_recovers_a_frame_whose_data_all_vanished() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::for_overhead(4, 100));
        let mut rx = reassembler(StreamKind::Video);
        let data = frame_data(SHARD_LEN * 3 + 1, 7);

        let datagrams = tx.packetize(0, true, &data).unwrap();
        let parity = datagrams.iter().filter(|datagram| parity_of(datagram));
        let events = feed(&mut rx, Instant::now(), parity);

        let received = frames(&events);
        assert_eq!(received[0].data, data);
        assert!(received[0].keyframe);
        assert_eq!(received[0].recovered_fragments, 4);
    }

    #[test]
    fn parity_arriving_first_does_not_hinder_assembly() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::for_overhead(4, 50));
        let mut rx = reassembler(StreamKind::Video);
        let data = frame_data(SHARD_LEN * 4, 8);

        let mut datagrams = tx.packetize(0, false, &data).unwrap();
        datagrams.rotate_left(4);
        let events = feed(&mut rx, Instant::now(), &datagrams);

        assert_eq!(frames(&events).len(), 1);
        assert_eq!(frames(&events)[0].data, data);
    }

    #[test]
    fn large_frame_with_many_blocks_recovers_scattered_losses() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::for_overhead(16, 25));
        let mut rx = reassembler(StreamKind::Video);
        let data = frame_data(SHARD_LEN * 200 + 3, 9);

        let datagrams = tx.packetize(0, false, &data).unwrap();
        let surviving = datagrams
            .iter()
            .enumerate()
            .filter(|(index, _)| index % 17 != 3)
            .map(|(_, datagram)| datagram);
        let events = feed(&mut rx, Instant::now(), surviving);

        assert_eq!(frames(&events)[0].data, data);
        assert!(frames(&events)[0].recovered_fragments > 5);
    }

    #[test]
    fn losses_beyond_parity_leave_the_frame_incomplete() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::for_overhead(4, 25));
        let mut rx = reassembler(StreamKind::Video);
        let data = frame_data(SHARD_LEN * 4, 10);
        let start = Instant::now();

        let datagrams = tx.packetize(0, false, &data).unwrap();
        let surviving = datagrams
            .iter()
            .enumerate()
            .filter(|(index, _)| *index > 1)
            .map(|(_, datagram)| datagram);
        let mut events = feed(&mut rx, start, surviving);

        assert_eq!(frames(&events).len(), 0);

        rx.poll_timeouts(start + Duration::from_secs(1), &mut events);
        assert_eq!(lost(&events), [0]);
    }

    #[test]
    fn newer_completed_frame_declares_older_ones_lost() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let now = Instant::now();

        let first = tx
            .packetize(1, false, &frame_data(SHARD_LEN * 2, 0))
            .unwrap();
        let second = tx.packetize(2, false, &frame_data(SHARD_LEN, 1)).unwrap();
        let third = tx.packetize(3, false, &frame_data(SHARD_LEN, 2)).unwrap();

        let mut events = feed(&mut rx, now, [&first[0]]);
        events.extend(feed(&mut rx, now, &third));
        events.extend(feed(&mut rx, now, &second));

        assert_eq!(lost(&events), [1]);
        assert_eq!(frames(&events).len(), 1);
        assert_eq!(frames(&events)[0].frame, 3);
        assert_eq!(rx.stats().stale_packets, 1);
        assert_eq!(rx.pending_frames(), 0);
    }

    #[test]
    fn late_fragments_of_a_resolved_frame_are_stale() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let now = Instant::now();

        let datagrams = tx
            .packetize(5, false, &frame_data(SHARD_LEN * 2, 0))
            .unwrap();
        feed(&mut rx, now, &datagrams);
        let events = feed(&mut rx, now, &datagrams);

        assert_eq!(events.len(), 0);
        assert_eq!(rx.stats().stale_packets, 2);
    }

    #[test]
    fn pending_window_overflow_drops_the_oldest_frame() {
        let limits = ReassemblyLimits {
            max_pending_frames: 2,
            ..ReassemblyLimits::default()
        };
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = Reassembler::new(StreamKind::Video, limits);
        let now = Instant::now();

        let mut events = Vec::new();
        for frame in 0..4 {
            let datagrams = tx
                .packetize(frame, false, &frame_data(SHARD_LEN * 2, 0))
                .unwrap();
            events.extend(feed(&mut rx, now, [&datagrams[0]]));
        }

        assert_eq!(lost(&events), [0, 1]);
        assert_eq!(rx.pending_frames(), 2);
    }

    #[test]
    fn frame_numbers_wrap_around() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let now = Instant::now();

        let mut events = Vec::new();
        for frame in [u32::MAX - 1, u32::MAX, 0, 1] {
            let datagrams = tx
                .packetize(frame, false, &frame_data(SHARD_LEN + 1, 0))
                .unwrap();
            events.extend(feed(&mut rx, now, &datagrams));
        }

        let numbers: Vec<u32> = frames(&events).iter().map(|frame| frame.frame).collect();
        assert_eq!(numbers, [u32::MAX - 1, u32::MAX, 0, 1]);
        assert_eq!(lost(&events).len(), 0);
        assert_eq!(rx.stats().stale_packets, 0);
    }

    #[test]
    fn sequence_gaps_are_reported_across_the_wrap() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let now = Instant::now();

        let sequence_start = u32::MAX - 1;
        let mut datagrams = Vec::new();
        for frame in 0..4 {
            let mut header_bytes = tx.packetize(frame, false, &[1, 2, 3]).unwrap();
            datagrams.append(&mut header_bytes);
        }
        for (offset, datagram) in datagrams.iter_mut().enumerate() {
            let mut header = PacketHeader::read(datagram).unwrap();
            header.sequence = sequence_start.wrapping_add(u32::try_from(offset).unwrap());
            header.write(datagram).unwrap();
        }

        let events = feed(&mut rx, now, [&datagrams[0], &datagrams[3]]);

        assert!(events.contains(&ReassemblyEvent::PacketsMissing {
            first_sequence: u32::MAX,
            count: 2,
        }));
        assert_eq!(rx.stats().packets_missing, 2);
    }

    #[test]
    fn fragment_count_conflict_is_rejected_and_the_frame_survives() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let now = Instant::now();
        let mut events = Vec::new();

        let datagrams = tx
            .packetize(0, false, &frame_data(SHARD_LEN * 3, 0))
            .unwrap();
        rx.push(&datagrams[0], now, &mut events).unwrap();

        let mut forged = datagrams[1].clone();
        let mut header = PacketHeader::read(&forged).unwrap();
        header.fragment_count = 4;
        header.write(&mut forged).unwrap();

        assert!(matches!(
            rx.push(&forged, now, &mut events),
            Err(NetError::FragmentCountMismatch {
                expected: 3,
                found: 4,
                ..
            })
        ));

        rx.push(&datagrams[1], now, &mut events).unwrap();
        rx.push(&datagrams[2], now, &mut events).unwrap();
        assert_eq!(frames(&events).len(), 1);
    }

    #[test]
    fn short_middle_fragment_is_rejected() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let now = Instant::now();
        let mut events = Vec::new();

        let datagrams = tx
            .packetize(0, false, &frame_data(SHARD_LEN * 3, 0))
            .unwrap();
        rx.push(&datagrams[0], now, &mut events).unwrap();

        let truncated = &datagrams[1][..datagrams[1].len() - 2];

        assert!(matches!(
            rx.push(truncated, now, &mut events),
            Err(NetError::FragmentLengthMismatch { index: 1, .. })
        ));
    }

    #[test]
    fn rejected_packet_does_not_leave_an_empty_pending_frame() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let mut events = Vec::new();

        let mut datagram = tx
            .packetize(0, false, &frame_data(SHARD_LEN * 2, 0))
            .unwrap()
            .remove(0);
        datagram.pop();

        assert!(rx.push(&datagram, Instant::now(), &mut events).is_err());
        assert_eq!(rx.pending_frames(), 0);
    }

    #[test]
    fn oversized_fragment_count_is_rejected() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let limits = ReassemblyLimits {
            max_fragments: 2,
            ..ReassemblyLimits::default()
        };
        let mut rx = Reassembler::new(StreamKind::Video, limits);
        let datagrams = tx
            .packetize(0, false, &frame_data(SHARD_LEN * 3, 0))
            .unwrap();

        let result = rx.push(&datagrams[0], Instant::now(), &mut Vec::new());

        assert!(matches!(
            result,
            Err(NetError::FragmentCountTooLarge { count: 3, max: 2 })
        ));
    }

    #[test]
    fn other_streams_are_not_accepted() {
        let mut tx = packetizer(StreamKind::Audio, FecScheme::for_overhead(2, 50));
        let mut rx = reassembler(StreamKind::Video);
        let datagrams = tx.packetize(0, false, &[1, 2, 3, 4]).unwrap();

        for datagram in &datagrams {
            assert!(matches!(
                rx.push(datagram, Instant::now(), &mut Vec::new()),
                Err(NetError::StreamMismatch { .. })
            ));
        }
    }

    #[test]
    fn conflicting_parity_geometry_is_rejected() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::for_overhead(4, 50));
        let mut rx = reassembler(StreamKind::Video);
        let now = Instant::now();
        let mut events = Vec::new();

        let datagrams = tx
            .packetize(0, false, &frame_data(SHARD_LEN * 4, 0))
            .unwrap();
        let parity: Vec<&Vec<u8>> = datagrams
            .iter()
            .filter(|datagram| parity_of(datagram))
            .collect();
        rx.push(parity[0], now, &mut events).unwrap();

        let mut forged = parity[1].clone();
        let (_, payload) = PacketHeader::parse(&forged).unwrap();
        let (mut fec, _) = FecHeader::parse(payload).unwrap();
        fec.data_shards = 3;
        fec.write(&mut forged[PacketHeader::SIZE..]).unwrap();

        assert!(matches!(
            rx.push(&forged, now, &mut events),
            Err(NetError::FecBlockConflict { .. })
        ));
    }

    #[test]
    fn parity_contradicting_stored_fragments_is_rejected() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::for_overhead(4, 50));
        let mut rx = reassembler(StreamKind::Video);
        let now = Instant::now();
        let mut events = Vec::new();

        let datagrams = tx
            .packetize(0, false, &frame_data(SHARD_LEN * 4, 0))
            .unwrap();
        rx.push(&datagrams[0], now, &mut events).unwrap();

        let mut forged = datagrams
            .iter()
            .find(|datagram| parity_of(datagram))
            .unwrap()
            .clone();
        let (_, payload) = PacketHeader::parse(&forged).unwrap();
        let (mut fec, _) = FecHeader::parse(payload).unwrap();
        fec.frame_bytes = u32::try_from(SHARD_LEN * 8).unwrap();
        fec.write(&mut forged[PacketHeader::SIZE..]).unwrap();

        assert!(matches!(
            rx.push(&forged, now, &mut events),
            Err(NetError::FragmentCountMismatch { .. })
        ));
    }

    #[test]
    fn reset_forgets_floor_and_pending() {
        let mut tx = packetizer(StreamKind::Video, FecScheme::None);
        let mut rx = reassembler(StreamKind::Video);
        let now = Instant::now();

        let datagrams = tx.packetize(100, false, &frame_data(SHARD_LEN, 0)).unwrap();
        feed(&mut rx, now, &datagrams);
        rx.reset();

        let restarted = packetizer(StreamKind::Video, FecScheme::None)
            .packetize(0, false, &frame_data(SHARD_LEN, 0))
            .unwrap();
        let events = feed(&mut rx, now, &restarted);

        assert_eq!(frames(&events).len(), 1);
    }

    #[test]
    fn limits_follow_the_configured_timeout() {
        let config = NetworkConfig {
            reassembly_timeout_ms: 40,
            ..NetworkConfig::default()
        };

        assert_eq!(
            ReassemblyLimits::from(&config).frame_timeout,
            Duration::from_millis(40)
        );
    }
}
