use std::{io::Cursor, rc::Rc};

use cros_codecs::codec::{
    av1::parser::{
        FrameType, ObuAction, ObuType, ParsedObu, Parser as Av1Parser, SequenceHeaderObu,
    },
    h264::parser::{NaluHeader as H264NaluHeader, NaluType as H264NaluType, Parser as H264Parser},
    h265::parser::{NaluHeader as HevcNaluHeader, NaluType as HevcNaluType, Parser as HevcParser},
};
use rusplay_core::{Codec, SignalledColor};

use crate::{
    BitstreamUnit, DecodeError,
    annexb::{malformed, next_nalu},
    signal::{self, HdrTracker},
};

const H264_NAL_HEADER_LEN: usize = 1;
const HEVC_NAL_HEADER_LEN: usize = 2;
const FIRST_SLICE_SEGMENT_IN_PIC: u8 = 0x80;

pub(super) struct ColorReader {
    parsers: Parsers,
    hdr: HdrTracker,
    current: SignalledColor,
}

enum Parsers {
    H264(Box<H264Parser>),
    Hevc(Box<HevcParser>),
    Av1 {
        parser: Box<Av1Parser>,
        sequence: Option<Rc<SequenceHeaderObu>>,
    },
}

impl ColorReader {
    pub(super) fn new(codec: Codec) -> Self {
        let parsers = match codec {
            Codec::H264 => Parsers::H264(Box::default()),
            Codec::Hevc => Parsers::Hevc(Box::default()),
            Codec::Av1 => Parsers::Av1 {
                parser: Box::default(),
                sequence: None,
            },
        };

        Self {
            parsers,
            hdr: HdrTracker::default(),
            current: SignalledColor::default(),
        }
    }

    pub(super) fn current(&self) -> SignalledColor {
        self.current
    }

    pub(super) fn read(&mut self, data: &[u8]) -> Result<(), DecodeError> {
        let picture = match &mut self.parsers {
            Parsers::H264(parser) => read_h264(parser, &mut self.hdr, data)?,
            Parsers::Hevc(parser) => read_hevc(parser, &mut self.hdr, data)?,
            Parsers::Av1 { parser, sequence } => read_av1(parser, sequence, &mut self.hdr, data)?,
        };

        if let Some((base, random_access)) = picture {
            self.current = self.hdr.resolve(base, random_access);
        }

        Ok(())
    }
}

type Picture = Option<(SignalledColor, bool)>;

fn read_h264(
    parser: &mut H264Parser,
    hdr: &mut HdrTracker,
    data: &[u8],
) -> Result<Picture, DecodeError> {
    const CODEC: Codec = Codec::H264;
    let mut cursor = Cursor::new(data);
    let mut picture = None;

    while let Some(nalu) = next_nalu::<H264NaluHeader>(&mut cursor, CODEC)? {
        match nalu.header.type_ {
            H264NaluType::Sps => {
                parser
                    .parse_sps(&nalu)
                    .map_err(malformed(CODEC, BitstreamUnit::SequenceParameterSet))?;
            }
            H264NaluType::Pps => {
                parser
                    .parse_pps(&nalu)
                    .map_err(malformed(CODEC, BitstreamUnit::PictureParameterSet))?;
            }
            H264NaluType::Sei => {
                if let Some(payload) = nalu.as_ref().get(H264_NAL_HEADER_LEN..) {
                    hdr.read_sei(payload);
                }
            }
            H264NaluType::Slice | H264NaluType::SliceIdr if picture.is_none() => {
                let idr = nalu.header.idr_pic_flag;
                let slice = parser
                    .parse_slice_header(nalu)
                    .map_err(malformed(CODEC, BitstreamUnit::SliceHeader))?;
                let id = slice.header.pic_parameter_set_id;
                let pps = parser.get_pps(id).ok_or(DecodeError::MissingParameterSet {
                    codec: CODEC,
                    unit: BitstreamUnit::PictureParameterSet,
                    id,
                })?;
                picture = Some((signal::h264_color(&pps.sps), idr));
            }
            _ => {}
        }
    }

    Ok(picture)
}

fn read_hevc(
    parser: &mut HevcParser,
    hdr: &mut HdrTracker,
    data: &[u8],
) -> Result<Picture, DecodeError> {
    const CODEC: Codec = Codec::Hevc;
    let mut cursor = Cursor::new(data);
    let mut picture = None;

    while let Some(nalu) = next_nalu::<HevcNaluHeader>(&mut cursor, CODEC)? {
        let kind = nalu.header.type_;
        match kind {
            HevcNaluType::VpsNut => {
                parser
                    .parse_vps(&nalu)
                    .map_err(malformed(CODEC, BitstreamUnit::VideoParameterSet))?;
            }
            HevcNaluType::SpsNut => {
                parser
                    .parse_sps(&nalu)
                    .map_err(malformed(CODEC, BitstreamUnit::SequenceParameterSet))?;
            }
            HevcNaluType::PpsNut => {
                parser
                    .parse_pps(&nalu)
                    .map_err(malformed(CODEC, BitstreamUnit::PictureParameterSet))?;
            }
            HevcNaluType::PrefixSeiNut => {
                if let Some(payload) = nalu.as_ref().get(HEVC_NAL_HEADER_LEN..) {
                    hdr.read_sei(payload);
                }
            }
            _ if picture.is_none() && is_first_slice_segment(kind, nalu.as_ref()) => {
                let slice = parser
                    .parse_slice_header(nalu)
                    .map_err(malformed(CODEC, BitstreamUnit::SliceHeader))?;
                let id = slice.header.pic_parameter_set_id;
                let pps = parser.get_pps(id).ok_or(DecodeError::MissingParameterSet {
                    codec: CODEC,
                    unit: BitstreamUnit::PictureParameterSet,
                    id,
                })?;
                picture = Some((signal::hevc_color(&pps.sps), kind.is_irap()));
            }
            _ => {}
        }
    }

    Ok(picture)
}

fn is_first_slice_segment(kind: HevcNaluType, nalu: &[u8]) -> bool {
    let slice = matches!(
        kind,
        HevcNaluType::TrailN
            | HevcNaluType::TrailR
            | HevcNaluType::TsaN
            | HevcNaluType::TsaR
            | HevcNaluType::StsaN
            | HevcNaluType::StsaR
            | HevcNaluType::RadlN
            | HevcNaluType::RadlR
            | HevcNaluType::RaslN
            | HevcNaluType::RaslR
            | HevcNaluType::BlaWLp
            | HevcNaluType::BlaWRadl
            | HevcNaluType::BlaNLp
            | HevcNaluType::IdrWRadl
            | HevcNaluType::IdrNLp
            | HevcNaluType::CraNut
    );

    slice
        && nalu
            .get(HEVC_NAL_HEADER_LEN)
            .is_some_and(|byte| byte & FIRST_SLICE_SEGMENT_IN_PIC != 0)
}

fn read_av1(
    parser: &mut Av1Parser,
    sequence: &mut Option<Rc<SequenceHeaderObu>>,
    hdr: &mut HdrTracker,
    mut data: &[u8],
) -> Result<Picture, DecodeError> {
    const CODEC: Codec = Codec::Av1;
    let mut picture: Picture = None;

    while !data.is_empty() {
        let obu = match parser
            .read_obu(data)
            .map_err(malformed(CODEC, BitstreamUnit::Obu))?
        {
            ObuAction::Process(obu) => obu,
            ObuAction::Drop(length) => {
                data = data
                    .get(usize::try_from(length).unwrap_or(usize::MAX)..)
                    .unwrap_or_default();
                continue;
            }
        };
        let length = obu.bytes_used;
        if obu.header.obu_type == ObuType::Metadata {
            hdr.read_av1_metadata(obu.as_ref());
        }

        let header = match parser
            .parse_obu(obu)
            .map_err(malformed(CODEC, BitstreamUnit::Obu))?
        {
            ParsedObu::SequenceHeader(parsed) => {
                *sequence = Some(parsed);
                None
            }
            ParsedObu::FrameHeader(header) => Some(header),
            ParsedObu::Frame(frame) => Some(frame.header),
            _ => None,
        };

        if let Some(header) = header {
            if !header.show_existing_frame {
                let current = sequence.as_ref().ok_or(DecodeError::MissingParameterSet {
                    codec: CODEC,
                    unit: BitstreamUnit::SequenceHeader,
                    id: 0,
                })?;
                let key = header.frame_type == FrameType::KeyFrame;
                picture = Some(match picture {
                    Some((_, earlier_key)) => (signal::av1_color(current), earlier_key || key),
                    None => (signal::av1_color(current), key),
                });
            }
            parser
                .ref_frame_update(&header)
                .map_err(malformed(CODEC, BitstreamUnit::FrameHeader))?;
        }

        data = data.get(length..).unwrap_or_default();
    }

    Ok(picture)
}

#[cfg(test)]
mod tests;
