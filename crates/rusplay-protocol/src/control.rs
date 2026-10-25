use crate::ProtocolVersion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMessage {
    Hello { version: ProtocolVersion },
    RequestKeyframe { frame: u32 },
    ReportLoss { first_sequence: u32, count: u16 },
    BitrateHint { kbps: u32 },
    Goodbye,
}
