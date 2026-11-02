#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_fuzz::stamped;
use rusplay_protocol::{ClientName, FecHeader, PacketHeader, StreamKind, VideoFrameHeader};

fuzz_target!(|data: &[u8]| {
    let _ = PacketHeader::parse(data);
    let _ = FecHeader::parse(data);
    let _ = VideoFrameHeader::parse(data);
    let _ = ClientName::from_bytes(data);

    if let Ok((_, payload)) = PacketHeader::parse(&stamped(data, StreamKind::Fec)) {
        let _ = FecHeader::parse(payload);
    }
    if let Ok((_, payload)) = PacketHeader::parse(&stamped(data, StreamKind::Video)) {
        let _ = VideoFrameHeader::parse(payload);
    }
});
