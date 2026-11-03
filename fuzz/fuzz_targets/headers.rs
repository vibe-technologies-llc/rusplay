#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_fuzz::stamped;
use rusplay_protocol::{ClientName, FecHeader, PacketHeader, StreamKind, VideoFrameHeader};

fuzz_target!(|data: &[u8]| {
    let _ = PacketHeader::parse(data);
    let _ = FecHeader::parse(data);
    if let Ok((header, body)) = VideoFrameHeader::parse(data) {
        let mut written = Vec::with_capacity(VideoFrameHeader::SIZE);
        header.write(&mut written);

        assert_eq!(written, data[..VideoFrameHeader::SIZE]);
        assert_eq!(body, &data[VideoFrameHeader::SIZE..]);
    }
    let _ = ClientName::from_bytes(data);

    if let Ok((_, payload)) = PacketHeader::parse(&stamped(data, StreamKind::Fec)) {
        let _ = FecHeader::parse(payload);
    }
    if let Ok((_, payload)) = PacketHeader::parse(&stamped(data, StreamKind::Video)) {
        let _ = VideoFrameHeader::parse(payload);
    }
});
