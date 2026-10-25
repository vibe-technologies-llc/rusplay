use rusplay_core::Resolution;

pub const WIDTH: usize = 128;
pub const HEIGHT: usize = 64;
pub const MBS_PER_FIELD: usize = MBS_WIDE * MBS_HIGH;

const MB: usize = 16;
const FIELD_HEIGHT: usize = HEIGHT / 2;
const MBS_WIDE: usize = WIDTH / MB;
const MBS_HIGH: usize = FIELD_HEIGHT / MB;
const CHROMA_MB: usize = MB / 2;

const NAL_SLICE: u8 = 1;
const NAL_IDR: u8 = 5;
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const PCM_MB_TYPE_IN_I_SLICE: u32 = 25;
const SLICE_TYPE_P: u32 = 5;
const SLICE_TYPE_B: u32 = 6;
const SLICE_TYPE_I: u32 = 7;
const MAIN_PROFILE: u32 = 77;
const LEVEL_3_0: u32 = 30;

struct Bits {
    bytes: Vec<u8>,
    pending: u8,
    filled: u32,
}

impl Bits {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            pending: 0,
            filled: 0,
        }
    }

    fn bit(&mut self, value: bool) {
        self.pending = (self.pending << 1) | u8::from(value);
        self.filled += 1;
        if self.filled == 8 {
            self.bytes.push(self.pending);
            self.pending = 0;
            self.filled = 0;
        }
    }

    fn bits(&mut self, value: u32, width: u32) {
        for shift in (0..width).rev() {
            self.bit((value >> shift) & 1 == 1);
        }
    }

    fn ue(&mut self, value: u32) {
        let code = value + 1;
        let length = 32 - code.leading_zeros();
        self.bits(0, length - 1);
        self.bits(code, length);
    }

    fn se(&mut self, value: i32) {
        self.ue(if value > 0 {
            value.unsigned_abs() * 2 - 1
        } else {
            value.unsigned_abs() * 2
        });
    }

    fn align(&mut self) {
        while self.filled != 0 {
            self.bit(false);
        }
    }

    fn trailing(mut self) -> Vec<u8> {
        self.bit(true);
        self.align();
        self.bytes
    }
}

fn nal(reference_idc: u8, kind: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut unit = vec![0, 0, 0, 1, (reference_idc << 5) | kind];
    let mut zeros = 0;

    for &byte in rbsp {
        if zeros >= 2 && byte <= 3 {
            unit.push(3);
            zeros = 0;
        }
        unit.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }

    unit
}

pub fn resolution() -> Resolution {
    Resolution::new(
        u32::try_from(WIDTH).unwrap(),
        u32::try_from(HEIGHT).unwrap(),
    )
}

pub fn parameter_sets(max_num_ref_frames: u32, reorder_frames: u32) -> Vec<u8> {
    let mut sps = Bits::new();
    sps.bits(MAIN_PROFILE, 8);
    sps.bits(0, 8);
    sps.bits(LEVEL_3_0, 8);
    sps.ue(0);
    sps.ue(0);
    sps.ue(0);
    sps.ue(2);
    sps.ue(max_num_ref_frames);
    sps.bit(false);
    sps.ue(u32::try_from(MBS_WIDE).unwrap() - 1);
    sps.ue(u32::try_from(MBS_HIGH).unwrap() - 1);
    sps.bit(false);
    sps.bit(false);
    sps.bit(true);
    sps.bit(false);
    sps.bit(true);
    for _ in 0..8 {
        sps.bit(false);
    }
    sps.bit(true);
    sps.bit(true);
    sps.ue(0);
    sps.ue(0);
    sps.ue(16);
    sps.ue(16);
    sps.ue(reorder_frames);
    sps.ue(max_num_ref_frames.max(reorder_frames));

    let mut pps = Bits::new();
    pps.ue(0);
    pps.ue(0);
    pps.bit(false);
    pps.bit(false);
    pps.ue(0);
    pps.ue(0);
    pps.ue(0);
    pps.bit(false);
    pps.bits(0, 2);
    pps.se(0);
    pps.se(0);
    pps.se(0);
    pps.bit(true);
    pps.bit(false);
    pps.bit(false);

    let mut stream = nal(3, NAL_SPS, &sps.trailing());
    stream.extend(nal(3, NAL_PPS, &pps.trailing()));
    stream
}

#[derive(Debug, Clone, Copy)]
pub enum Mmco {
    UnmarkShortTerm { difference_minus1: u32 },
    UnmarkLongTerm { long_term_pic_num: u32 },
    ShortToLongTerm { difference_minus1: u32, index: u32 },
    MaxLongTermIndexPlus1(u32),
}

#[derive(Debug, Clone)]
pub enum Marking {
    Sliding,
    Adaptive(Vec<Mmco>),
}

#[derive(Debug, Clone, Copy)]
pub struct Modification {
    pub idc: u32,
    pub value: u32,
}

#[derive(Debug, Clone)]
pub enum Content {
    Intra {
        seed: u8,
    },
    Probe {
        active: usize,
        modifications: Vec<Modification>,
    },
    BidirectionalProbe {
        active0: usize,
        active1: usize,
    },
}

#[derive(Debug, Clone)]
pub struct FieldPicture {
    pub bottom: bool,
    pub idr: bool,
    pub reference: bool,
    pub frame_num: u32,
    pub poc_lsb: u32,
    pub content: Content,
    pub marking: Marking,
}

impl FieldPicture {
    pub fn intra(frame: u32, frame_num: u32, bottom: bool, seed: u8) -> Self {
        Self {
            bottom,
            idr: frame == 0 && !bottom,
            reference: true,
            frame_num,
            poc_lsb: frame * 2 + u32::from(bottom),
            content: Content::Intra { seed },
            marking: Marking::Sliding,
        }
    }

    pub fn probe(frame: u32, frame_num: u32, bottom: bool, active: usize) -> Self {
        Self {
            bottom,
            idr: false,
            reference: false,
            frame_num,
            poc_lsb: frame * 2 + u32::from(bottom),
            content: Content::Probe {
                active,
                modifications: Vec::new(),
            },
            marking: Marking::Sliding,
        }
    }

    pub fn reference_probe(frame: u32, frame_num: u32, bottom: bool, active: usize) -> Self {
        Self {
            reference: true,
            ..Self::probe(frame, frame_num, bottom, active)
        }
    }

    pub fn bidirectional_probe(
        frame: u32,
        frame_num: u32,
        bottom: bool,
        active0: usize,
        active1: usize,
    ) -> Self {
        Self {
            content: Content::BidirectionalProbe { active0, active1 },
            ..Self::probe(frame, frame_num, bottom, 1)
        }
    }

    pub fn with_marking(mut self, operations: Vec<Mmco>) -> Self {
        self.marking = Marking::Adaptive(operations);
        self
    }

    pub fn with_modifications(mut self, list: Vec<Modification>) -> Self {
        if let Content::Probe { modifications, .. } = &mut self.content {
            *modifications = list;
        }
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bits = Bits::new();
        bits.ue(0);
        bits.ue(match self.content {
            Content::Intra { .. } => SLICE_TYPE_I,
            Content::Probe { .. } => SLICE_TYPE_P,
            Content::BidirectionalProbe { .. } => SLICE_TYPE_B,
        });
        bits.ue(0);
        bits.bits(self.frame_num, 4);
        bits.bit(true);
        bits.bit(self.bottom);
        if self.idr {
            bits.ue(0);
        }
        bits.bits(self.poc_lsb, 6);

        self.write_list_setup(&mut bits);
        self.write_marking(&mut bits);

        bits.se(0);
        bits.ue(1);

        self.write_macroblocks(&mut bits);

        let kind = if self.idr { NAL_IDR } else { NAL_SLICE };
        nal(u8::from(self.reference) * 2, kind, &bits.trailing())
    }

    fn write_list_setup(&self, bits: &mut Bits) {
        match &self.content {
            Content::Probe {
                active,
                modifications,
            } => {
                bits.bit(true);
                bits.ue(u32::try_from(*active).unwrap() - 1);
                bits.bit(!modifications.is_empty());
                if !modifications.is_empty() {
                    for modification in modifications {
                        bits.ue(modification.idc);
                        bits.ue(modification.value);
                    }
                    bits.ue(3);
                }
            }
            Content::BidirectionalProbe { active0, active1 } => {
                bits.bit(true);
                bits.bit(true);
                bits.ue(u32::try_from(*active0).unwrap() - 1);
                bits.ue(u32::try_from(*active1).unwrap() - 1);
                bits.bit(false);
                bits.bit(false);
            }
            Content::Intra { .. } => {}
        }
    }

    fn write_marking(&self, bits: &mut Bits) {
        if self.reference {
            if self.idr {
                bits.bit(false);
                bits.bit(false);
            } else {
                match &self.marking {
                    Marking::Sliding => bits.bit(false),
                    Marking::Adaptive(operations) => {
                        bits.bit(true);
                        for operation in operations {
                            match *operation {
                                Mmco::UnmarkShortTerm { difference_minus1 } => {
                                    bits.ue(1);
                                    bits.ue(difference_minus1);
                                }
                                Mmco::UnmarkLongTerm { long_term_pic_num } => {
                                    bits.ue(2);
                                    bits.ue(long_term_pic_num);
                                }
                                Mmco::ShortToLongTerm {
                                    difference_minus1,
                                    index,
                                } => {
                                    bits.ue(3);
                                    bits.ue(difference_minus1);
                                    bits.ue(index);
                                }
                                Mmco::MaxLongTermIndexPlus1(plus1) => {
                                    bits.ue(4);
                                    bits.ue(plus1);
                                }
                            }
                        }
                        bits.ue(0);
                    }
                }
            }
        }
    }

    fn write_macroblocks(&self, bits: &mut Bits) {
        match &self.content {
            Content::Intra { seed } => {
                for macroblock in 0..MBS_PER_FIELD {
                    bits.ue(PCM_MB_TYPE_IN_I_SLICE);
                    bits.align();
                    write_pcm(bits, *seed, macroblock);
                }
            }
            Content::Probe { active, .. } => {
                for macroblock in 0..MBS_PER_FIELD {
                    bits.ue(0);
                    bits.ue(0);
                    write_reference_index(bits, *active, macroblock % active);
                    bits.se(0);
                    bits.se(0);
                    bits.ue(0);
                }
            }
            Content::BidirectionalProbe { active0, active1 } => {
                for macroblock in 0..MBS_PER_FIELD {
                    let backward = macroblock % 2 == 1;
                    let active = if backward { active1 } else { active0 };
                    bits.ue(0);
                    bits.ue(if backward { 2 } else { 1 });
                    write_reference_index(bits, *active, (macroblock / 2) % active);
                    bits.se(0);
                    bits.se(0);
                    bits.ue(0);
                }
            }
        }
    }
}

fn write_reference_index(bits: &mut Bits, active: usize, reference: usize) {
    match active {
        1 => {}
        2 => bits.bit(reference == 0),
        _ => bits.ue(u32::try_from(reference).unwrap()),
    }
}

fn write_pcm(bits: &mut Bits, seed: u8, macroblock: usize) {
    let (x0, y0) = macroblock_origin(macroblock);

    for y in 0..MB {
        for x in 0..MB {
            bits.bits(u32::from(luma(seed, x0 + x, y0 + y)), 8);
        }
    }
    for _ in 0..CHROMA_MB * CHROMA_MB {
        bits.bits(u32::from(cb(seed)), 8);
    }
    for _ in 0..CHROMA_MB * CHROMA_MB {
        bits.bits(u32::from(cr(seed)), 8);
    }
}

fn macroblock_origin(macroblock: usize) -> (usize, usize) {
    ((macroblock % MBS_WIDE) * MB, (macroblock / MBS_WIDE) * MB)
}

fn luma(seed: u8, x: usize, y: usize) -> u8 {
    let value = (usize::from(seed) * 53 + x * 7 + y * 13) % 200;
    u8::try_from(value + 16).unwrap()
}

fn cb(seed: u8) -> u8 {
    100 + seed
}

fn cr(seed: u8) -> u8 {
    220 - seed
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    luma: Vec<u8>,
    cb: Vec<u8>,
    cr: Vec<u8>,
}

impl Field {
    pub fn intra(seed: u8) -> Self {
        Self::of_sources(|_| seed)
    }

    pub fn probe(list: &[u8]) -> Self {
        Self::of_sources(|macroblock| list[macroblock % list.len()])
    }

    pub fn bidirectional_probe(list0: &[u8], list1: &[u8]) -> Self {
        Self::of_sources(|macroblock| {
            let list = if macroblock % 2 == 1 { list1 } else { list0 };
            list[(macroblock / 2) % list.len()]
        })
    }

    fn of_sources(seed_of: impl Fn(usize) -> u8) -> Self {
        let mut luma_plane = vec![0; WIDTH * FIELD_HEIGHT];
        let mut blue_plane = vec![0; WIDTH / 2 * FIELD_HEIGHT / 2];
        let mut red_plane = vec![0; WIDTH / 2 * FIELD_HEIGHT / 2];

        for macroblock in 0..MBS_PER_FIELD {
            let seed = seed_of(macroblock);
            let (x0, y0) = macroblock_origin(macroblock);

            for y in 0..MB {
                for x in 0..MB {
                    luma_plane[(y0 + y) * WIDTH + x0 + x] = luma(seed, x0 + x, y0 + y);
                }
            }
            for y in 0..CHROMA_MB {
                for x in 0..CHROMA_MB {
                    let index = (y0 / 2 + y) * (WIDTH / 2) + x0 / 2 + x;
                    blue_plane[index] = cb(seed);
                    red_plane[index] = cr(seed);
                }
            }
        }

        Self {
            luma: luma_plane,
            cb: blue_plane,
            cr: red_plane,
        }
    }
}

pub fn nv12(top: &Field, bottom: &Field) -> Vec<Vec<u8>> {
    let fields = [top, bottom];

    let mut luma_plane = Vec::with_capacity(WIDTH * HEIGHT);
    for row in 0..HEIGHT {
        let field = fields[row % 2];
        let start = (row / 2) * WIDTH;
        luma_plane.extend_from_slice(&field.luma[start..start + WIDTH]);
    }

    let mut chroma_plane = Vec::with_capacity(WIDTH * HEIGHT / 2);
    for row in 0..HEIGHT / 2 {
        let field = fields[row % 2];
        let start = (row / 2) * (WIDTH / 2);
        for column in 0..WIDTH / 2 {
            chroma_plane.push(field.cb[start + column]);
            chroma_plane.push(field.cr[start + column]);
        }
    }

    vec![luma_plane, chroma_plane]
}

pub fn describe(planes: &[Vec<u8>]) -> String {
    ["top", "bottom"]
        .iter()
        .enumerate()
        .map(|(parity, name)| {
            let seeds: Vec<String> = (0..MBS_PER_FIELD)
                .map(|macroblock| {
                    let (x0, y0) = macroblock_origin(macroblock);
                    let sample = planes[0][(y0 * 2 + parity) * WIDTH + x0];
                    (1..=32u8)
                        .find(|&seed| luma(seed, x0, y0) == sample)
                        .map_or_else(|| String::from("?"), |seed| seed.to_string())
                })
                .collect();
            format!("{name}: {}", seeds.join(" "))
        })
        .collect::<Vec<_>>()
        .join(" | ")
}
