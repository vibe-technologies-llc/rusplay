mod bits;
mod frame;
mod framing;
mod sequence;
mod tiles;

#[cfg(any(feature = "vaapi", feature = "nvdec"))]
pub(crate) use self::framing::{Layer, check_framing, obu_length};
pub(crate) use self::framing::{ObuKind, Unit};
use self::{bits::BitReader, frame::Header, sequence::Sequence, tiles::TileLayout};
use crate::{Av1SyntaxError, BitstreamUnit, DecodeError};

pub(crate) const REFERENCE_SLOTS: usize = 8;
const SEGMENTS: usize = 8;
const ALL_SLOTS: u8 = 0xff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SlotFrame {
    pub(crate) key: bool,
    pub(crate) upscaled_width: u32,
    pub(crate) frame_height: u32,
    pub(crate) alt_q: [Option<i32>; SEGMENTS],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Slot {
    order_hint: u32,
    frame: Option<SlotFrame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Refresh {
    pub(crate) flags: u8,
    pub(crate) order_hint: u32,
    pub(crate) frame: SlotFrame,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Progress {
    Nothing,
    SequenceChanged,
    FramePending,
    FrameComplete(Refresh),
    ShowExisting(Option<Refresh>),
}

#[cfg(any(test, feature = "software"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PacketSummary {
    pub(crate) sequence_header: bool,
}

#[derive(Debug, Clone)]
struct KnownSequence {
    payload: Vec<u8>,
    header: Sequence,
}

#[derive(Debug, Clone)]
struct FrameInProgress {
    header_payload: Option<Vec<u8>>,
    layout: TileLayout,
    next_tile: u32,
    refresh: Refresh,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Av1Syntax {
    sequence: Option<KnownSequence>,
    slots: [Slot; REFERENCE_SLOTS],
    frame: Option<FrameInProgress>,
}

impl Av1Syntax {
    pub(crate) fn obu(&mut self, unit: &Unit<'_>) -> Result<Progress, DecodeError> {
        Ok(match unit.kind {
            ObuKind::SequenceHeader => self.sequence_header(unit.payload)?,
            ObuKind::TemporalDelimiter => {
                self.frame = None;
                Progress::Nothing
            }
            ObuKind::RedundantFrameHeader if self.frame.is_some() => Progress::Nothing,
            ObuKind::RedundantFrameHeader => {
                return Err(Av1SyntaxError::RedundantFrameHeaderWithoutFrame.into());
            }
            ObuKind::FrameHeader | ObuKind::Frame => self.frame_header(unit)?,
            ObuKind::TileGroup => self.tile_group(unit.payload)?,
            ObuKind::Other => Progress::Nothing,
        })
    }

    pub(crate) fn refresh(&mut self, refresh: &Refresh) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if refresh.flags & (1 << index) != 0 {
                *slot = Slot {
                    order_hint: refresh.order_hint,
                    frame: Some(refresh.frame),
                };
            }
        }
    }

    fn sequence_header(&mut self, payload: &[u8]) -> Result<Progress, DecodeError> {
        if self
            .sequence
            .as_ref()
            .is_some_and(|known| known.payload == payload)
        {
            return Ok(Progress::Nothing);
        }

        let header = sequence::parse(payload)?;
        *self = Self {
            sequence: Some(KnownSequence {
                payload: payload.to_vec(),
                header,
            }),
            ..Self::default()
        };

        Ok(Progress::SequenceChanged)
    }

    fn frame_header(&mut self, unit: &Unit<'_>) -> Result<Progress, DecodeError> {
        if let Some(frame) = &self.frame {
            if unit.kind == ObuKind::FrameHeader
                && frame.header_payload.as_deref() == Some(unit.payload)
            {
                return Ok(Progress::Nothing);
            }
            return Err(Av1SyntaxError::FrameHeaderDuringFrame.into());
        }

        let sequence = &self
            .sequence
            .as_ref()
            .ok_or(Av1SyntaxError::FrameBeforeSequenceHeader)?
            .header;
        let mut r = BitReader::new(unit.payload, BitstreamUnit::FrameHeader);

        let header = match frame::parse(sequence, &self.slots, unit, &mut r)? {
            Header::ShowExisting { slot } => return Ok(self.show_existing(slot)?),
            Header::Frame(header) => header,
        };

        let mut frame = FrameInProgress {
            header_payload: (unit.kind == ObuKind::FrameHeader).then(|| unit.payload.to_vec()),
            layout: header.layout,
            next_tile: 0,
            refresh: header.refresh,
        };

        let complete = if unit.kind == ObuKind::Frame {
            r.byte_align();
            r.set_unit(BitstreamUnit::TileGroup);
            let end = tiles::tile_group(&mut r, &frame.layout, 0)?;
            frame.next_tile = end + 1;
            frame.next_tile == frame.layout.tiles()
        } else {
            false
        };

        for (index, slot) in self.slots.iter_mut().enumerate() {
            if header.invalidated & (1 << index) != 0 {
                slot.frame = None;
            }
        }

        if complete {
            return Ok(Progress::FrameComplete(frame.refresh));
        }
        self.frame = Some(frame);
        Ok(Progress::FramePending)
    }

    fn show_existing(&self, slot: u8) -> Result<Progress, Av1SyntaxError> {
        let shown = &self.slots[usize::from(slot)];
        let frame = shown
            .frame
            .ok_or(Av1SyntaxError::EmptyReferenceSlot { slot })?;

        Ok(Progress::ShowExisting(frame.key.then_some(Refresh {
            flags: ALL_SLOTS,
            order_hint: shown.order_hint,
            frame,
        })))
    }

    fn tile_group(&mut self, payload: &[u8]) -> Result<Progress, DecodeError> {
        let frame = self
            .frame
            .as_mut()
            .ok_or(Av1SyntaxError::TileGroupWithoutFrameHeader)?;

        let mut r = BitReader::new(payload, BitstreamUnit::TileGroup);
        let end = tiles::tile_group(&mut r, &frame.layout, frame.next_tile)?;
        frame.next_tile = end + 1;

        if frame.next_tile < frame.layout.tiles() {
            return Ok(Progress::Nothing);
        }

        let refresh = frame.refresh;
        self.frame = None;
        Ok(Progress::FrameComplete(refresh))
    }
}

#[cfg(any(test, feature = "software"))]
impl Av1Syntax {
    pub(crate) fn check_packet(&mut self, data: &[u8]) -> Result<PacketSummary, DecodeError> {
        let units = framing::units(data)?;
        let mut next = self.clone();
        let mut summary = PacketSummary {
            sequence_header: false,
        };

        for unit in &units {
            if next.dropped(unit) {
                continue;
            }
            summary.sequence_header |= unit.kind == ObuKind::SequenceHeader;
            match next.obu(unit)? {
                Progress::FrameComplete(refresh) | Progress::ShowExisting(Some(refresh)) => {
                    next.refresh(&refresh);
                }
                _ => {}
            }
        }

        *self = next;
        Ok(summary)
    }

    pub(crate) fn forget_after_decode_error(&mut self, packet: PacketSummary) {
        self.slots = Default::default();
        self.frame = None;
        if packet.sequence_header {
            self.sequence = None;
        }
    }

    fn dropped(&self, unit: &Unit<'_>) -> bool {
        let idc = self
            .sequence
            .as_ref()
            .and_then(|sequence| sequence.header.operating_points.first())
            .map_or(0, |point| point.idc);

        match unit.layer {
            Some(layer)
                if idc != 0
                    && !matches!(
                        unit.kind,
                        ObuKind::SequenceHeader | ObuKind::TemporalDelimiter
                    ) =>
            {
                let temporal = (idc >> layer.temporal_id) & 1 == 1;
                let spatial = (idc >> (u32::from(layer.spatial_id) + 8)) & 1 == 1;
                !(temporal && spatial)
            }
            _ => false,
        }
    }
}

#[cfg(any(feature = "vaapi", feature = "nvdec"))]
pub(crate) fn decoder_model_present(payload: &[u8]) -> Result<bool, DecodeError> {
    Ok(sequence::parse(payload)?.decoder_model.is_some())
}

#[cfg(test)]
pub(crate) mod tests;
