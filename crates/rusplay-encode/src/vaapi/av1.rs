use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat, Resolution};

use crate::{
    EncodeError,
    vaapi::{
        CodecBuffers, INFINITE_INTRA_PERIOD, PackedKind, Picture, StreamInfo,
        bits::{BitWriter, leb128, packed},
        codec_buffers, ffi,
        format::{colour_primaries, full_range, matrix_coefficients, transfer_characteristics},
        hdr::metadata_obus,
        level::{self, Demand, Level},
        packed_buffers, sequence_buffer,
        va::{Context, INVALID_SURFACE, Profile},
    },
};

const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_FRAME_HEADER: u8 = 3;
const FRAME_TYPE_KEY: u8 = 0;
const FRAME_TYPE_INTER: u8 = 1;
const PRIMARY_REF_NONE: u8 = 7;
const REFS_PER_FRAME: usize = 7;
const ALL_FRAMES: u8 = 0xff;
const ORDER_HINT_BITS: u32 = 8;
const SUPERBLOCK_LOG2: u32 = 6;
const MAX_TILE_WIDTH: u32 = 4096;
const MAX_TILE_AREA: u32 = 4096 * 2304;
const MAX_TILE_COLS: u32 = 64;
const MAX_TILE_ROWS: u32 = 64;
const TILE_SIZE_BYTES: u64 = 4;
const TX_MODE_SELECT: u32 = 2;
const DEFAULT_QINDEX: u8 = 128;
const QINDEX_PER_QP: u32 = 5;
const MIN_QINDEX: u8 = 1;
const MAX_QINDEX: u8 = 255;
const FRAME_HEADER_SIZE_FIELD_BYTES: usize = 2;
const LAST_FRAME_SEARCH: u32 = 1;

const fn level(
    id: u8,
    max_picture_samples: u64,
    max_sample_rate: u64,
    max_kbps: u64,
    max_kbps_high_tier: Option<u64>,
) -> Level {
    Level {
        id,
        max_picture_samples,
        max_sample_rate,
        max_kbps,
        max_kbps_high_tier,
    }
}

const LEVELS: [Level; 14] = [
    level(0, 147_456, 4_423_680, 1_500, None),
    level(1, 278_784, 8_363_520, 3_000, None),
    level(4, 665_856, 19_975_680, 6_000, None),
    level(5, 1_065_024, 31_950_720, 10_000, None),
    level(8, 2_359_296, 70_778_880, 12_000, Some(30_000)),
    level(9, 2_359_296, 141_557_760, 20_000, Some(50_000)),
    level(12, 8_912_896, 267_386_880, 30_000, Some(100_000)),
    level(13, 8_912_896, 534_773_760, 40_000, Some(160_000)),
    level(14, 8_912_896, 1_069_547_520, 60_000, Some(240_000)),
    level(15, 8_912_896, 1_069_547_520, 60_000, Some(240_000)),
    level(16, 35_651_584, 1_069_547_520, 60_000, Some(240_000)),
    level(17, 35_651_584, 2_139_095_040, 100_000, Some(480_000)),
    level(18, 35_651_584, 4_278_190_080, 160_000, Some(800_000)),
    level(19, 35_651_584, 4_278_190_080, 160_000, Some(800_000)),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Av1Profile {
    Main,
    High,
}

impl Av1Profile {
    pub(super) const fn for_format(format: PixelFormat) -> Option<Self> {
        match (format.chroma, format.depth) {
            (_, BitDepth::Twelve) | (ChromaSubsampling::Yuv422, _) => None,
            (ChromaSubsampling::Yuv420, _) => Some(Self::Main),
            (ChromaSubsampling::Yuv444, _) => Some(Self::High),
        }
    }

    pub(super) const fn va(self) -> Profile {
        match self {
            Self::Main => ffi::VAProfile_VAProfileAV1Profile0,
            Self::High => ffi::VAProfile_VAProfileAV1Profile1,
        }
    }

    const fn seq_profile(self) -> u8 {
        match self {
            Self::Main => 0,
            Self::High => 1,
        }
    }

    const fn bitrate_percent(self) -> u64 {
        match self {
            Self::Main => 100,
            Self::High => 200,
        }
    }
}

fn tile_log2(block: u32, target: u32) -> u32 {
    let mut k = 0;
    while (block << k) < target {
        k += 1;
    }
    k
}

fn bit_length(value: u32) -> u32 {
    value.bit_width().max(1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TileLayout {
    sb_cols: u32,
    sb_rows: u32,
    cols_log2: u32,
    rows_log2: u32,
    max_cols_log2: u32,
    max_rows_log2: u32,
    widths: Vec<u32>,
    heights: Vec<u32>,
}

impl TileLayout {
    pub(super) fn new(resolution: Resolution) -> Self {
        let mi_cols = 2 * resolution.width.div_ceil(8);
        let mi_rows = 2 * resolution.height.div_ceil(8);
        let sb_cols = mi_cols.div_ceil(16);
        let sb_rows = mi_rows.div_ceil(16);
        let max_tile_width_sb = MAX_TILE_WIDTH >> SUPERBLOCK_LOG2;
        let max_tile_area_sb = MAX_TILE_AREA >> (2 * SUPERBLOCK_LOG2);

        let min_cols_log2 = tile_log2(max_tile_width_sb, sb_cols);
        let max_cols_log2 = tile_log2(1, sb_cols.min(MAX_TILE_COLS));
        let max_rows_log2 = tile_log2(1, sb_rows.min(MAX_TILE_ROWS));
        let min_tiles_log2 = min_cols_log2.max(tile_log2(max_tile_area_sb, sb_rows * sb_cols));

        let cols_log2 = min_cols_log2;
        let rows_log2 = min_tiles_log2.saturating_sub(cols_log2);

        Self {
            sb_cols,
            sb_rows,
            cols_log2,
            rows_log2,
            max_cols_log2,
            max_rows_log2,
            widths: Self::split(sb_cols, cols_log2),
            heights: Self::split(sb_rows, rows_log2),
        }
    }

    fn split(total: u32, log2: u32) -> Vec<u32> {
        let size = total.div_ceil(1 << log2);
        let count = total.div_ceil(size);

        (0..count)
            .map(|index| size.min(total - index * size))
            .collect()
    }

    pub(super) fn tile_count(&self) -> usize {
        self.widths.len() * self.heights.len()
    }

    fn write(&self, header: &mut BitWriter) {
        header.flag(true);
        if self.cols_log2 < self.max_cols_log2 {
            header.flag(false);
        }
        if self.rows_log2 < self.max_rows_log2 {
            header.flag(false);
        }
        if self.cols_log2 > 0 || self.rows_log2 > 0 {
            header.bits(0, self.cols_log2 + self.rows_log2);
            header.bits(TILE_SIZE_BYTES - 1, 2);
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct FrameHeaderOffsets {
    pub(super) qindex: u32,
    pub(super) segmentation: u32,
    pub(super) loop_filter: u32,
    pub(super) cdef: u32,
    pub(super) cdef_bits: u32,
    pub(super) total_bits: u32,
}

pub(super) fn obu(kind: u8, payload: Vec<u8>, size_field_bytes: usize) -> Vec<u8> {
    let mut bytes = vec![(kind << 3) | 0b010];
    bytes.extend(leb128(payload.len(), size_field_bytes));
    bytes.extend(payload);
    bytes
}

pub(super) fn temporal_delimiter() -> Vec<u8> {
    obu(OBU_TEMPORAL_DELIMITER, Vec::new(), 1)
}

pub(super) fn starts_with_temporal_delimiter(data: &[u8]) -> bool {
    data.first()
        .is_some_and(|&byte| (byte >> 3) & 0x0f == OBU_TEMPORAL_DELIMITER)
}

#[derive(Debug, Clone, Copy)]
struct ReferencePicture {
    surface: u32,
}

pub(super) struct Av1Stream {
    profile: Av1Profile,
    level: level::Selected,
    tiles: TileLayout,
    info: StreamInfo,
    reference: Option<ReferencePicture>,
}

impl Av1Stream {
    pub(super) fn new(profile: Av1Profile, info: StreamInfo) -> Self {
        let demand = Demand::new(info.resolution, info.frame_rate, info.peak_kbps);

        Self {
            profile,
            level: level::select(&LEVELS, demand, profile.bitrate_percent()),
            tiles: TileLayout::new(info.resolution),
            info,
            reference: None,
        }
    }

    pub(super) const fn refresh_block_size() -> u32 {
        1 << SUPERBLOCK_LOG2
    }

    fn high_bitdepth(&self) -> bool {
        self.info.format.depth != BitDepth::Eight
    }

    fn qindex(&self) -> u8 {
        self.info.qp.map_or(DEFAULT_QINDEX, |qp| {
            u8::try_from(u32::from(qp) * QINDEX_PER_QP)
                .unwrap_or(MAX_QINDEX)
                .max(MIN_QINDEX)
        })
    }

    pub(super) fn sequence_header(&self) -> Vec<u8> {
        let mut header = BitWriter::new();
        let Resolution { width, height } = self.info.resolution;
        let width_bits = bit_length(width - 1);
        let height_bits = bit_length(height - 1);
        let color = self.info.color;

        header.bits(u64::from(self.profile.seq_profile()), 3);
        header.flag(false);
        header.flag(false);
        header.flag(false);
        header.flag(false);
        header.bits(0, 5);
        header.bits(0, 12);
        header.bits(u64::from(self.level.id), 5);
        if self.level.id > 7 {
            header.flag(self.level.high_tier);
        }
        header.bits(u64::from(width_bits - 1), 4);
        header.bits(u64::from(height_bits - 1), 4);
        header.bits(u64::from(width - 1), width_bits);
        header.bits(u64::from(height - 1), height_bits);
        header.flag(false);
        header.flag(false);
        header.flag(false);
        header.flag(false);
        header.flag(false);
        header.flag(false);
        header.flag(false);
        header.flag(false);
        header.flag(true);
        header.flag(false);
        header.flag(false);
        header.flag(false);
        header.flag(false);
        header.bits(u64::from(ORDER_HINT_BITS - 1), 3);
        header.flag(false);
        header.flag(true);
        header.flag(false);

        header.flag(self.high_bitdepth());
        if self.profile == Av1Profile::Main {
            header.flag(false);
        }
        header.flag(true);
        header.bits(u64::from(colour_primaries(color.primaries)), 8);
        header.bits(u64::from(transfer_characteristics(color.transfer)), 8);
        header.bits(
            u64::from(matrix_coefficients(color.matrix_coefficients())),
            8,
        );
        header.flag(full_range(color.range));
        if self.profile == Av1Profile::Main {
            header.bits(0, 2);
        }
        header.flag(false);
        header.flag(false);
        header.trailing_bits();

        obu(OBU_SEQUENCE_HEADER, header.into_bytes(), 1)
    }

    fn render_size(&self, header: &mut BitWriter) {
        let Resolution { width, height } = self.info.resolution;

        header.flag(true);
        header.bits(u64::from(width - 1), 16);
        header.bits(u64::from(height - 1), 16);
    }

    pub(super) fn frame_header(
        &self,
        keyframe: bool,
        order_hint: u8,
    ) -> (Vec<u8>, FrameHeaderOffsets) {
        let mut header = BitWriter::new();
        let mut offsets = FrameHeaderOffsets::default();
        let prefix_bits = 8 * (1 + FRAME_HEADER_SIZE_FIELD_BYTES);
        let at = |header: &BitWriter| u32::try_from(prefix_bits + header.position()).unwrap_or(0);

        header.flag(false);
        header.bits(
            u64::from(if keyframe {
                FRAME_TYPE_KEY
            } else {
                FRAME_TYPE_INTER
            }),
            2,
        );
        header.flag(true);
        if !keyframe {
            header.flag(false);
        }
        header.flag(false);
        header.flag(false);
        header.bits(u64::from(order_hint), ORDER_HINT_BITS);
        if keyframe {
            self.render_size(&mut header);
        } else {
            header.bits(0, 3);
            header.bits(1, 8);
            header.flag(false);
            for _ in 0..REFS_PER_FRAME {
                header.bits(0, 3);
            }
            self.render_size(&mut header);
            header.flag(false);
            header.flag(false);
            header.bits(0, 2);
            header.flag(false);
        }
        header.flag(false);

        self.tiles.write(&mut header);

        offsets.qindex = at(&header);
        header.bits(u64::from(self.qindex()), 8);
        header.flag(false);
        header.flag(false);
        header.flag(false);
        header.flag(false);

        offsets.segmentation = at(&header);
        header.flag(false);
        header.flag(false);

        offsets.loop_filter = at(&header);
        header.bits(0, 6);
        header.bits(0, 6);
        header.bits(0, 3);
        header.flag(false);

        offsets.cdef = at(&header);
        header.bits(0, 2);
        header.bits(0, 2);
        header.bits(0, 4);
        header.bits(0, 2);
        header.bits(0, 4);
        header.bits(0, 2);
        offsets.cdef_bits = at(&header) - offsets.cdef;

        header.flag(true);
        if !keyframe {
            header.flag(false);
        }
        header.flag(false);
        if !keyframe {
            for _ in 0..REFS_PER_FRAME {
                header.flag(false);
            }
        }
        header.trailing_bits();

        let bytes = obu(
            OBU_FRAME_HEADER,
            header.into_bytes(),
            FRAME_HEADER_SIZE_FIELD_BYTES,
        );
        offsets.total_bits = u32::try_from(bytes.len() * 8).unwrap_or(0);

        (bytes, offsets)
    }

    fn sequence_parameters(&self, bits_per_second: u32) -> ffi::VAEncSequenceParameterBufferAV1 {
        let mut parameters = ffi::VAEncSequenceParameterBufferAV1 {
            seq_profile: self.profile.seq_profile(),
            seq_level_idx: self.level.id,
            seq_tier: u8::from(self.level.high_tier),
            intra_period: INFINITE_INTRA_PERIOD,
            ip_period: 1,
            bits_per_second,
            order_hint_bits_minus_1: u8::try_from(ORDER_HINT_BITS - 1).unwrap_or(7),
            ..Default::default()
        };

        let subsampled = u32::from(self.info.format.chroma == ChromaSubsampling::Yuv420);
        let fields = unsafe { &mut parameters.seq_fields.bits };
        fields.set_enable_order_hint(1);
        fields.set_enable_cdef(1);
        fields.set_bit_depth_minus8(u32::from(self.info.format.depth.bits()) - 8);
        fields.set_subsampling_x(subsampled);
        fields.set_subsampling_y(subsampled);

        parameters
    }

    fn picture_parameters(
        &self,
        picture: &Picture,
        order_hint: u8,
        offsets: FrameHeaderOffsets,
    ) -> ffi::VAEncPictureParameterBufferAV1 {
        let Resolution { width, height } = self.info.resolution;
        let mut parameters = ffi::VAEncPictureParameterBufferAV1 {
            frame_width_minus_1: u16::try_from(width - 1).unwrap_or(u16::MAX),
            frame_height_minus_1: u16::try_from(height - 1).unwrap_or(u16::MAX),
            reconstructed_frame: picture.recon,
            coded_buf: picture.coded,
            reference_frames: [INVALID_SURFACE; 8],
            primary_ref_frame: if picture.keyframe {
                PRIMARY_REF_NONE
            } else {
                0
            },
            order_hint,
            refresh_frame_flags: if picture.keyframe { ALL_FRAMES } else { 1 },
            base_qindex: self.qindex(),
            min_base_qindex: MIN_QINDEX,
            max_base_qindex: MAX_QINDEX,
            tile_cols: u8::try_from(self.tiles.widths.len()).unwrap_or(1),
            tile_rows: u8::try_from(self.tiles.heights.len()).unwrap_or(1),
            bit_offset_qindex: offsets.qindex,
            bit_offset_segmentation: offsets.segmentation,
            bit_offset_loopfilter_params: offsets.loop_filter,
            bit_offset_cdef_params: offsets.cdef,
            size_in_bits_cdef_params: offsets.cdef_bits,
            byte_offset_frame_hdr_obu_size: 1,
            size_in_bits_frame_hdr_obu: offsets.total_bits,
            ..Default::default()
        };
        if let Some(reference) = self.reference {
            parameters.reference_frames[0] = reference.surface;
            parameters.ref_frame_ctrl_l0.value = LAST_FRAME_SEARCH;
        }
        for (slot, width) in parameters
            .width_in_sbs_minus_1
            .iter_mut()
            .zip(&self.tiles.widths)
        {
            *slot = u16::try_from(width - 1).unwrap_or(0);
        }
        for (slot, height) in parameters
            .height_in_sbs_minus_1
            .iter_mut()
            .zip(&self.tiles.heights)
        {
            *slot = u16::try_from(height - 1).unwrap_or(0);
        }

        let flags = unsafe { &mut parameters.picture_flags.bits };
        flags.set_frame_type(u32::from(if picture.keyframe {
            FRAME_TYPE_KEY
        } else {
            FRAME_TYPE_INTER
        }));
        flags.set_error_resilient_mode(u32::from(picture.keyframe));
        let modes = unsafe { &mut parameters.mode_control_flags.bits };
        modes.set_tx_mode(TX_MODE_SELECT);
        let tile_group_header = unsafe { &mut parameters.tile_group_obu_hdr_info.bits };
        tile_group_header.set_obu_has_size_field(1);

        parameters
    }

    pub(super) fn buffers(
        &mut self,
        context: &Context,
        picture: &Picture,
    ) -> Result<CodecBuffers, EncodeError> {
        if picture.keyframe {
            self.reference = None;
        }

        let order_hint = u8::try_from(picture.index % (1 << ORDER_HINT_BITS)).unwrap_or(0);
        let (frame_header, offsets) = self.frame_header(picture.keyframe, order_hint);
        let tile_group = ffi::VAEncTileGroupBufferAV1 {
            tg_start: 0,
            tg_end: u8::try_from(self.tiles.tile_count() - 1).unwrap_or(0),
            ..Default::default()
        };

        let sequence = sequence_buffer(context, picture, || {
            self.sequence_parameters(picture.bits_per_second)
        })?;
        let headers = packed_buffers(
            context,
            picture,
            [
                (
                    PackedKind::Sequence,
                    picture.keyframe.then(|| packed(self.sequence_header())),
                ),
                (
                    PackedKind::Raw,
                    picture
                        .keyframe
                        .then(|| metadata_obus(&self.info.color.hdr))
                        .flatten(),
                ),
                (PackedKind::Picture, Some(packed(frame_header))),
            ],
        )?;
        let buffers = codec_buffers(
            context,
            sequence,
            &self.picture_parameters(picture, order_hint, offsets),
            headers,
            &tile_group,
        )?;

        self.reference = Some(ReferencePicture {
            surface: picture.recon,
        });

        Ok(buffers)
    }
}
