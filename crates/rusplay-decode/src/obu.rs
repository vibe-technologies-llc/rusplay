use cros_codecs::codec::av1::parser::{
    FrameHeaderObu, FrameType, Obu, ObuAction, ObuType, ParsedObu, Parser, SEG_LVL_ALT_Q,
};
use rusplay_core::Codec;

use crate::{
    Av1SyntaxError, BitstreamUnit, DecodeError,
    av1::{self, Av1Syntax, Layer, ObuKind, Progress, Refresh, SlotFrame, Unit},
};

const CODEC: Codec = Codec::Av1;
const KEY_FRAME: u8 = 0;
const TEMPORAL_DELIMITER: [u8; 2] = [0x12, 0x00];
const SEQUENCE_HEADER_WITH_SIZE: u8 = 0x0a;
const LEB128_PAYLOAD_BITS: u32 = 7;
const LEB128_PAYLOAD_MASK: usize = 0x7f;
const LEB128_CONTINUATION: u8 = 0x80;

const ALL_REFERENCE_SLOTS: u32 = 0xff;
const SHOW_EXISTING_FRAME: u8 = 0x80;
const FRAME_TYPE_SHIFT: u8 = 5;
const FRAME_TYPE_MASK: u8 = 0x03;
const SHOW_FRAME: u8 = 0x10;

pub(crate) struct ObuParser {
    parser: Parser,
    syntax: Av1Syntax,
    sequence_payload: Option<Vec<u8>>,
    references_valid: bool,
}

impl ObuParser {
    pub(crate) fn new() -> Self {
        Self {
            parser: primed_parser(),
            syntax: Av1Syntax::default(),
            sequence_payload: None,
            references_valid: false,
        }
    }

    pub(crate) fn read_obu<'a>(&mut self, data: &'a [u8]) -> Result<ObuAction<'a>, DecodeError> {
        match self.parser.read_obu(data).map_err(obu_error)? {
            ObuAction::Drop(_) => {
                let length = av1::obu_length(data)?;
                Ok(ObuAction::Drop(u32::try_from(length).unwrap_or(u32::MAX)))
            }
            process @ ObuAction::Process(_) => Ok(process),
        }
    }

    pub(crate) fn parse<'a>(&mut self, obu: Obu<'a>) -> Result<ParsedObu<'a>, DecodeError> {
        let kind = obu.header.obu_type;

        if matches!(kind, ObuType::FrameHeader | ObuType::Frame) {
            self.admit_frame_header(obu.as_ref())?;
        }
        if kind == ObuType::SequenceHeader && av1::decoder_model_present(obu.as_ref())? {
            return Err(Av1SyntaxError::DecoderModelUnsupported.into());
        }

        let unit = Unit {
            kind: obu_kind(kind),
            layer: obu.header.extension_flag.then(|| Layer {
                temporal_id: u8::try_from(obu.header.temporal_id).unwrap_or_default(),
                spatial_id: u8::try_from(obu.header.spatial_id).unwrap_or_default(),
            }),
            payload: obu.as_ref(),
        };
        if self.syntax.obu(&unit)? == Progress::SequenceChanged {
            self.parser = primed_parser();
            self.references_valid = false;
        }
        let payload = (kind == ObuType::SequenceHeader).then(|| obu.as_ref().to_vec());

        let parsed = self.parser.parse_obu(obu).map_err(obu_error);
        match &parsed {
            Ok(ParsedObu::SequenceHeader(_)) => self.sequence_payload = payload,
            Err(_) => self.recover(),
            Ok(_) => {}
        }

        parsed
    }

    pub(crate) fn ref_frame_update(&mut self, header: &FrameHeaderObu) -> Result<(), DecodeError> {
        self.parser
            .ref_frame_update(header)
            .map_err(|reason| DecodeError::Malformed {
                codec: CODEC,
                unit: BitstreamUnit::FrameHeader,
                reason,
            })?;

        if let Some(refresh) = refresh_of(header) {
            self.syntax.refresh(&refresh);
        }

        let refreshes_every_slot = header.frame_type == FrameType::KeyFrame
            && header.refresh_frame_flags == ALL_REFERENCE_SLOTS;
        self.references_valid |= refreshes_every_slot;

        Ok(())
    }

    fn recover(&mut self) {
        self.references_valid = false;
        self.parser = primed_parser();
        self.syntax = Av1Syntax::default();

        let Some(payload) = &self.sequence_payload else {
            return;
        };
        let unit = Unit {
            kind: ObuKind::SequenceHeader,
            layer: None,
            payload,
        };
        let _ = self.syntax.obu(&unit);

        let bytes = sequence_obu(payload);
        if let Ok(ObuAction::Process(obu)) = self.parser.read_obu(&bytes) {
            let _ = self.parser.parse_obu(obu);
        }
    }

    fn admit_frame_header(&mut self, payload: &[u8]) -> Result<(), DecodeError> {
        let Some(sequence) = &self.parser.sequence_header else {
            return Err(malformed("frame header without a sequence header"));
        };
        if sequence.reduced_still_picture_header {
            return Ok(());
        }

        let first = payload.first().copied().unwrap_or_default();
        let key_frame = first & SHOW_EXISTING_FRAME == 0
            && (first >> FRAME_TYPE_SHIFT) & FRAME_TYPE_MASK == KEY_FRAME;

        if !key_frame && !self.references_valid {
            return Err(malformed(
                "inter frame before a key frame refreshed the references",
            ));
        }
        if key_frame && first & SHOW_FRAME != 0 {
            self.references_valid = false;
        }

        Ok(())
    }
}

fn primed_parser() -> Parser {
    let mut parser = Parser::default();
    let _ = parser.read_obu(&TEMPORAL_DELIMITER);
    parser
}

fn sequence_obu(payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![SEQUENCE_HEADER_WITH_SIZE];
    let mut size = payload.len();
    loop {
        let byte = u8::try_from(size & LEB128_PAYLOAD_MASK).unwrap_or_default();
        size >>= LEB128_PAYLOAD_BITS;
        if size == 0 {
            bytes.push(byte);
            break;
        }
        bytes.push(byte | LEB128_CONTINUATION);
    }
    bytes.extend_from_slice(payload);
    bytes
}

fn obu_kind(kind: ObuType) -> ObuKind {
    match kind {
        ObuType::SequenceHeader => ObuKind::SequenceHeader,
        ObuType::TemporalDelimiter => ObuKind::TemporalDelimiter,
        ObuType::FrameHeader => ObuKind::FrameHeader,
        ObuType::TileGroup => ObuKind::TileGroup,
        ObuType::Frame => ObuKind::Frame,
        ObuType::RedundantFrameHeader => ObuKind::RedundantFrameHeader,
        _ => ObuKind::Other,
    }
}

fn refresh_of(header: &FrameHeaderObu) -> Option<Refresh> {
    let key = header.frame_type == FrameType::KeyFrame;
    if header.show_existing_frame && !key {
        return None;
    }

    let segmentation = &header.segmentation_params;
    Some(Refresh {
        flags: u8::try_from(header.refresh_frame_flags & ALL_REFERENCE_SLOTS).unwrap_or_default(),
        order_hint: header.order_hint,
        frame: SlotFrame {
            key,
            upscaled_width: header.upscaled_width,
            frame_height: header.frame_height,
            alt_q: std::array::from_fn(|segment| {
                segmentation.feature_enabled[segment][SEG_LVL_ALT_Q]
                    .then(|| i32::from(segmentation.feature_data[segment][SEG_LVL_ALT_Q]))
            }),
        },
    })
}

fn obu_error(reason: String) -> DecodeError {
    DecodeError::Malformed {
        codec: CODEC,
        unit: BitstreamUnit::Obu,
        reason,
    }
}

fn malformed(reason: &str) -> DecodeError {
    DecodeError::Malformed {
        codec: CODEC,
        unit: BitstreamUnit::Obu,
        reason: String::from(reason),
    }
}

#[cfg(test)]
mod tests;
