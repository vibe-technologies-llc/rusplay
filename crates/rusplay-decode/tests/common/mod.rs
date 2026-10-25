#![allow(dead_code)]

use std::{fs, path::PathBuf};

use rusplay_core::{Codec, EncodedFrame, Timestamp};

const IVF_HEADER_LEN: usize = 32;
const IVF_FRAME_HEADER_LEN: usize = 12;
const FRAME_INTERVAL_MICROS: u64 = 33_333;

pub fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    fs::read(&path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

pub fn ivf_frames(codec: Codec, file: &[u8]) -> Vec<EncodedFrame> {
    assert_eq!(&file[..4], b"DKIF");

    let mut frames = Vec::new();
    let mut cursor = IVF_HEADER_LEN;

    while cursor + IVF_FRAME_HEADER_LEN <= file.len() {
        let size = u32::from_le_bytes(file[cursor..cursor + 4].try_into().unwrap());
        let start = cursor + IVF_FRAME_HEADER_LEN;
        let end = start + usize::try_from(size).unwrap();
        frames.push(encoded(codec, frames.len(), file[start..end].to_vec()));
        cursor = end;
    }

    frames
}

pub fn annexb_access_units(codec: Codec, file: &[u8]) -> Vec<EncodedFrame> {
    let delimiters: Vec<usize> = start_codes(file)
        .into_iter()
        .filter(|&(_, header)| is_access_unit_delimiter(codec, file[header]))
        .map(|(start, _)| start)
        .collect();

    delimiters
        .iter()
        .enumerate()
        .map(|(index, &start)| {
            let end = delimiters.get(index + 1).copied().unwrap_or(file.len());
            encoded(codec, index, file[start..end].to_vec())
        })
        .collect()
}

fn start_codes(file: &[u8]) -> Vec<(usize, usize)> {
    let mut codes = Vec::new();
    let mut index = 0;

    while index + 3 < file.len() {
        if file[index..index + 3] == [0, 0, 1] {
            codes.push((index, index + 3));
            index += 3;
        } else if file[index..index + 4] == [0, 0, 0, 1] {
            codes.push((index, index + 4));
            index += 4;
        } else {
            index += 1;
        }
    }

    codes
}

fn is_access_unit_delimiter(codec: Codec, header: u8) -> bool {
    match codec {
        Codec::H264 => header & 0x1f == 9,
        Codec::Hevc => (header >> 1) & 0x3f == 35,
        Codec::Av1 => false,
    }
}

fn encoded(codec: Codec, index: usize, data: Vec<u8>) -> EncodedFrame {
    EncodedFrame {
        codec,
        timestamp: Timestamp::from_micros(u64::try_from(index).unwrap() * FRAME_INTERVAL_MICROS),
        keyframe: index == 0,
        data,
    }
}
