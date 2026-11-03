use super::{
    REFERENCE_SLOTS, Refresh, Slot, SlotFrame,
    bits::BitReader,
    framing::{Layer, ObuKind, Unit},
    sequence::Sequence,
    tiles::{self, TileLayout},
};
use crate::Av1SyntaxError;

const KEY_FRAME: u32 = 0;
const INTER_FRAME: u32 = 1;
const INTRA_ONLY_FRAME: u32 = 2;
const SWITCH_FRAME: u32 = 3;
const ALL_FRAMES: u8 = 0xff;
const PRIMARY_REF_NONE: u32 = 7;
const SELECT: u8 = 2;
const REFS_PER_FRAME: usize = 7;
const GOLDEN: usize = 3;
const BACKWARD: usize = 4;
const ALTREF2: usize = 5;
const ALTREF: usize = 6;
const FORWARD_FILL_ORDER: [usize; 5] = [1, 2, BACKWARD, ALTREF2, ALTREF];
const SUPERRES_NUM: u32 = 8;
const SUPERRES_DENOM_MIN: u32 = 9;
const SEGMENTS: usize = 8;
const SEGMENT_FEATURE_BITS: [u32; 8] = [8, 6, 6, 6, 6, 3, 0, 0];
const SEGMENT_FEATURE_MAX: [i32; 8] = [255, 63, 63, 63, 63, 7, 0, 0];
const SIGNED_SEGMENT_FEATURES: usize = 5;
const ALT_Q: usize = 0;
const MAX_Q_INDEX: i32 = 255;
const LOOP_FILTER_REF_DELTAS: usize = 8;
const LOOP_FILTER_MODE_DELTAS: usize = 2;
const DELTA_BITS: u32 = 7;
const TRANSLATION: u32 = 1;
const ROTZOOM: u32 = 2;
const AFFINE: u32 = 3;
const GM_ABS_ALPHA_BITS: u32 = 12;
const GM_ABS_TRANS_ONLY_BITS: u32 = 9;
const GM_ABS_TRANS_BITS: u32 = 12;
const SUBEXP_K: u32 = 3;

pub(super) enum Header {
    ShowExisting { slot: u8 },
    Frame(FrameHeader),
}

pub(super) struct FrameHeader {
    pub(super) layout: TileLayout,
    pub(super) refresh: Refresh,
    pub(super) invalidated: u8,
}

pub(super) struct FrameSize {
    frame_width: u32,
    upscaled_width: u32,
    frame_height: u32,
}

impl FrameSize {
    pub(super) fn mi_cols(&self) -> u32 {
        2 * self.frame_width.div_ceil(8)
    }

    pub(super) fn mi_rows(&self) -> u32 {
        2 * self.frame_height.div_ceil(8)
    }
}

struct Context<'s> {
    sequence: &'s Sequence,
    slots: &'s [Slot; REFERENCE_SLOTS],
}

#[allow(clippy::too_many_lines)]
pub(super) fn parse(
    sequence: &Sequence,
    slots: &[Slot; REFERENCE_SLOTS],
    unit: &Unit<'_>,
    r: &mut BitReader<'_>,
) -> Result<Header, Av1SyntaxError> {
    let context = Context { sequence, slots };
    let presentation_time_length = sequence
        .decoder_model
        .filter(|_| !sequence.equal_picture_interval)
        .map(|model| model.frame_presentation_time_length);

    let frame_type;
    let show_frame;
    let showable_frame;
    let error_resilient_mode;

    if sequence.reduced_still_picture_header {
        frame_type = KEY_FRAME;
        show_frame = true;
        showable_frame = false;
        error_resilient_mode = true;
    } else {
        if r.bit()? {
            if unit.kind == ObuKind::Frame {
                return Err(Av1SyntaxError::ShowExistingFrameInFrameObu);
            }
            let slot = r.bits(3)?;
            if let Some(length) = presentation_time_length {
                r.bits(length)?;
            }
            if let Some(lengths) = sequence.frame_id_lengths {
                r.bits(lengths.frame_id)?;
            }
            return Ok(Header::ShowExisting {
                slot: u8::try_from(slot).unwrap_or_default(),
            });
        }

        frame_type = r.bits(2)?;
        show_frame = r.bit()?;
        if show_frame && let Some(length) = presentation_time_length {
            r.bits(length)?;
        }
        showable_frame = if show_frame {
            frame_type != KEY_FRAME
        } else {
            r.bit()?
        };
        error_resilient_mode =
            frame_type == SWITCH_FRAME || (frame_type == KEY_FRAME && show_frame) || r.bit()?;
    }

    let intra = frame_type == KEY_FRAME || frame_type == INTRA_ONLY_FRAME;
    let disable_cdf_update = r.bit()?;
    let allow_screen_content_tools = adaptive(r, sequence.force_screen_content_tools)?;
    let signalled_integer_mv =
        allow_screen_content_tools && adaptive(r, sequence.force_integer_mv)?;
    let force_integer_mv = intra || signalled_integer_mv;
    if let Some(lengths) = sequence.frame_id_lengths {
        r.bits(lengths.frame_id)?;
    }

    let frame_size_override =
        frame_type == SWITCH_FRAME || (!sequence.reduced_still_picture_header && r.bit()?);
    let order_hint = r.bits(sequence.order_hint_bits)?;
    let primary_ref_frame = if intra || error_resilient_mode {
        PRIMARY_REF_NONE
    } else {
        r.bits(3)?
    };

    if let Some(model) = sequence.decoder_model
        && r.bit()?
    {
        let layer = unit.layer.unwrap_or_default();
        for point in &sequence.operating_points {
            if point.decoder_model_present && in_layer(point.idc, layer) {
                r.bits(model.buffer_removal_time_length)?;
            }
        }
    }

    let refresh_frame_flags =
        if frame_type == SWITCH_FRAME || (frame_type == KEY_FRAME && show_frame) {
            ALL_FRAMES
        } else {
            u8::try_from(r.bits(8)?).unwrap_or(ALL_FRAMES)
        };

    let mut invalidated = 0u8;
    if (!intra || refresh_frame_flags != ALL_FRAMES)
        && error_resilient_mode
        && sequence.enable_order_hint
    {
        for (index, slot) in slots.iter().enumerate() {
            if r.bits(sequence.order_hint_bits)? != slot.order_hint {
                invalidated |= 1 << index;
            }
        }
    }

    let mut allow_intrabc = false;
    let mut allow_high_precision_mv = false;
    let mut ref_frame_idx = [0u8; REFS_PER_FRAME];
    let size;

    if intra {
        size = frame_size(r, sequence, frame_size_override)?;
        render_size(r)?;
        if allow_screen_content_tools && size.upscaled_width == size.frame_width {
            allow_intrabc = r.bit()?;
        }
    } else {
        let short_signaling = sequence.enable_order_hint && r.bit()?;
        if short_signaling {
            let last = r.bits(3)?;
            let golden = r.bits(3)?;
            ref_frame_idx = context.short_signalled_refs(last, golden, order_hint)?;
        }
        for index in &mut ref_frame_idx {
            if !short_signaling {
                *index = u8::try_from(r.bits(3)?).unwrap_or_default();
            }
            if let Some(lengths) = sequence.frame_id_lengths {
                r.bits(lengths.delta)?;
            }
        }

        size = if frame_size_override && !error_resilient_mode {
            context.frame_size_with_refs(r, ref_frame_idx)?
        } else {
            let size = frame_size(r, sequence, frame_size_override)?;
            render_size(r)?;
            size
        };

        if !force_integer_mv {
            allow_high_precision_mv = r.bit()?;
        }
        if !r.bit()? {
            r.bits(2)?;
        }
        r.bit()?;
        if !error_resilient_mode && sequence.enable_ref_frame_mvs {
            r.bit()?;
        }
    }

    if !sequence.reduced_still_picture_header && !disable_cdf_update {
        r.bit()?;
    }

    let layout = tiles::tile_info(r, sequence, &size)?;
    let quantizer = quantization_params(r, sequence)?;
    let alt_q = context.segmentation_params(r, primary_ref_frame, ref_frame_idx)?;

    let delta_q_present = quantizer.base_q_idx > 0 && r.bit()?;
    if delta_q_present {
        r.bits(2)?;
        if !allow_intrabc && r.bit()? {
            r.bits(2)?;
            r.bit()?;
        }
    }

    let coded_lossless = quantizer.deltas_zero
        && alt_q
            .iter()
            .all(|segment| segment_q_index(quantizer.base_q_idx, *segment) == 0);
    let all_lossless = coded_lossless && size.frame_width == size.upscaled_width;

    if !coded_lossless && !allow_intrabc {
        loop_filter_params(r, sequence)?;
    }
    if !coded_lossless && !allow_intrabc && sequence.enable_cdef {
        cdef_params(r, sequence)?;
    }
    if !all_lossless && !allow_intrabc && sequence.enable_restoration {
        loop_restoration_params(r, sequence)?;
    }
    if !coded_lossless {
        r.bit()?;
    }

    let reference_select = !intra && r.bit()?;
    if reference_select
        && sequence.enable_order_hint
        && context.skip_mode_allowed(ref_frame_idx, order_hint)
    {
        r.bit()?;
    }
    if !intra && !error_resilient_mode && sequence.enable_warped_motion {
        r.bit()?;
    }
    r.bit()?;

    if !intra {
        global_motion_params(r, allow_high_precision_mv)?;
    }
    if sequence.film_grain_params_present && (show_frame || showable_frame) {
        film_grain_params(r, sequence, frame_type)?;
    }

    Ok(Header::Frame(FrameHeader {
        layout,
        refresh: Refresh {
            flags: refresh_frame_flags,
            order_hint,
            frame: SlotFrame {
                key: frame_type == KEY_FRAME,
                upscaled_width: size.upscaled_width,
                frame_height: size.frame_height,
                alt_q,
            },
        },
        invalidated,
    }))
}

fn adaptive(r: &mut BitReader<'_>, forced: u8) -> Result<bool, Av1SyntaxError> {
    if forced == SELECT {
        r.bit()
    } else {
        Ok(forced == 1)
    }
}

fn in_layer(idc: u32, layer: Layer) -> bool {
    let temporal = (idc >> layer.temporal_id) & 1 == 1;
    let spatial = (idc >> (u32::from(layer.spatial_id) + 8)) & 1 == 1;
    idc == 0 || (temporal && spatial)
}

fn frame_size(
    r: &mut BitReader<'_>,
    sequence: &Sequence,
    frame_size_override: bool,
) -> Result<FrameSize, Av1SyntaxError> {
    let (width, height) = if frame_size_override {
        (
            r.bits(sequence.frame_width_bits)? + 1,
            r.bits(sequence.frame_height_bits)? + 1,
        )
    } else {
        (sequence.max_frame_width, sequence.max_frame_height)
    };

    superres_params(r, sequence, width, height)
}

fn superres_params(
    r: &mut BitReader<'_>,
    sequence: &Sequence,
    upscaled_width: u32,
    frame_height: u32,
) -> Result<FrameSize, Av1SyntaxError> {
    if !(sequence.enable_superres && r.bit()?) {
        return Ok(FrameSize {
            frame_width: upscaled_width,
            upscaled_width,
            frame_height,
        });
    }

    let denominator = r.bits(3)? + SUPERRES_DENOM_MIN;
    let frame_width = (upscaled_width * SUPERRES_NUM + denominator / 2) / denominator;
    if frame_width == upscaled_width {
        return Err(Av1SyntaxError::SuperresWithoutDownscale {
            width: upscaled_width,
        });
    }

    Ok(FrameSize {
        frame_width,
        upscaled_width,
        frame_height,
    })
}

fn render_size(r: &mut BitReader<'_>) -> Result<(), Av1SyntaxError> {
    if r.bit()? {
        r.bits(16)?;
        r.bits(16)?;
    }
    Ok(())
}

impl Context<'_> {
    fn known(&self, slot: u8) -> Result<&SlotFrame, Av1SyntaxError> {
        self.slots[usize::from(slot)]
            .frame
            .as_ref()
            .ok_or(Av1SyntaxError::EmptyReferenceSlot { slot })
    }

    fn frame_size_with_refs(
        &self,
        r: &mut BitReader<'_>,
        ref_frame_idx: [u8; REFS_PER_FRAME],
    ) -> Result<FrameSize, Av1SyntaxError> {
        for slot in ref_frame_idx {
            if r.bit()? {
                let frame = self.known(slot)?;
                return superres_params(r, self.sequence, frame.upscaled_width, frame.frame_height);
            }
        }

        let size = frame_size(r, self.sequence, true)?;
        render_size(r)?;
        Ok(size)
    }

    fn relative_distance(&self, a: u32, b: u32) -> i32 {
        relative_distance(self.sequence, a, b)
    }

    fn short_signalled_refs(
        &self,
        last: u32,
        golden: u32,
        order_hint: u32,
    ) -> Result<[u8; REFS_PER_FRAME], Av1SyntaxError> {
        let current = 1i32 << (self.sequence.order_hint_bits - 1);
        let shifted: [i32; REFERENCE_SLOTS] = std::array::from_fn(|index| {
            current + self.relative_distance(self.slots[index].order_hint, order_hint)
        });

        let last = usize::try_from(last).unwrap_or_default();
        let golden = usize::try_from(golden).unwrap_or_default();
        for slot in [last, golden] {
            if shifted[slot] >= current {
                return Err(Av1SyntaxError::ShortSignalledReferenceNotPast {
                    slot: u8::try_from(slot).unwrap_or_default(),
                });
            }
        }

        let mut refs: [Option<usize>; REFS_PER_FRAME] = [None; REFS_PER_FRAME];
        let mut used = [false; REFERENCE_SLOTS];
        refs[0] = Some(last);
        refs[GOLDEN] = Some(golden);
        used[last] = true;
        used[golden] = true;

        let pick = |used: &[bool; REFERENCE_SLOTS], backward: bool, latest: bool| {
            let mut chosen: Option<(usize, i32)> = None;
            for (index, &hint) in shifted.iter().enumerate() {
                let side = if backward {
                    hint >= current
                } else {
                    hint < current
                };
                let better =
                    chosen.is_none_or(|(_, best)| if latest { hint >= best } else { hint < best });
                if !used[index] && side && better {
                    chosen = Some((index, hint));
                }
            }
            chosen.map(|(index, _)| index)
        };

        for (target, latest) in [(ALTREF, true), (BACKWARD, false), (ALTREF2, false)] {
            if let Some(index) = pick(&used, true, latest) {
                refs[target] = Some(index);
                used[index] = true;
            }
        }
        for target in FORWARD_FILL_ORDER {
            if refs[target].is_none()
                && let Some(index) = pick(&used, false, true)
            {
                refs[target] = Some(index);
                used[index] = true;
            }
        }

        let mut earliest = 0;
        for (index, &hint) in shifted.iter().enumerate().skip(1) {
            if hint < shifted[earliest] {
                earliest = index;
            }
        }

        Ok(refs.map(|slot| u8::try_from(slot.unwrap_or(earliest)).unwrap_or_default()))
    }

    fn segmentation_params(
        &self,
        r: &mut BitReader<'_>,
        primary_ref_frame: u32,
        ref_frame_idx: [u8; REFS_PER_FRAME],
    ) -> Result<[Option<i32>; SEGMENTS], Av1SyntaxError> {
        if !r.bit()? {
            return Ok([None; SEGMENTS]);
        }

        let update_data = if primary_ref_frame == PRIMARY_REF_NONE {
            true
        } else {
            if r.bit()? {
                r.bit()?;
            }
            r.bit()?
        };

        if !update_data {
            let primary = usize::try_from(primary_ref_frame).unwrap_or_default();
            return Ok(self.known(ref_frame_idx[primary])?.alt_q);
        }

        let mut alt_q = [None; SEGMENTS];
        for segment in &mut alt_q {
            for (feature, &bits) in SEGMENT_FEATURE_BITS.iter().enumerate() {
                if !r.bit()? {
                    continue;
                }
                let limit = SEGMENT_FEATURE_MAX[feature];
                let value = if feature < SIGNED_SEGMENT_FEATURES {
                    r.signed(bits + 1)?.clamp(-limit, limit)
                } else {
                    i32::try_from(r.bits(bits)?)
                        .unwrap_or(limit)
                        .clamp(0, limit)
                };
                if feature == ALT_Q {
                    *segment = Some(value);
                }
            }
        }

        Ok(alt_q)
    }

    fn skip_mode_allowed(&self, ref_frame_idx: [u8; REFS_PER_FRAME], order_hint: u32) -> bool {
        let hints = ref_frame_idx.map(|slot| self.slots[usize::from(slot)].order_hint);

        let mut forward: Option<u32> = None;
        let mut backward: Option<u32> = None;
        for &hint in &hints {
            let distance = self.relative_distance(hint, order_hint);
            if distance < 0 {
                if forward.is_none_or(|best| self.relative_distance(hint, best) > 0) {
                    forward = Some(hint);
                }
            } else if distance > 0
                && backward.is_none_or(|best| self.relative_distance(hint, best) < 0)
            {
                backward = Some(hint);
            }
        }

        let Some(forward) = forward else {
            return false;
        };
        if backward.is_some() {
            return true;
        }

        hints
            .iter()
            .any(|&hint| self.relative_distance(hint, forward) < 0)
    }
}

fn relative_distance(sequence: &Sequence, a: u32, b: u32) -> i32 {
    if !sequence.enable_order_hint {
        return 0;
    }

    let half = 1i32 << (sequence.order_hint_bits - 1);
    let difference = i32::try_from(a).unwrap_or_default() - i32::try_from(b).unwrap_or_default();
    (difference & (half - 1)) - (difference & half)
}

struct Quantizer {
    base_q_idx: u32,
    deltas_zero: bool,
}

fn quantization_params(
    r: &mut BitReader<'_>,
    sequence: &Sequence,
) -> Result<Quantizer, Av1SyntaxError> {
    let base_q_idx = r.bits(8)?;
    let mut deltas_zero = delta_q(r)? == 0;

    if sequence.planes() > 1 {
        let separate = sequence.separate_uv_delta_q && r.bit()?;
        deltas_zero &= delta_q(r)? == 0;
        deltas_zero &= delta_q(r)? == 0;
        if separate {
            deltas_zero &= delta_q(r)? == 0;
            deltas_zero &= delta_q(r)? == 0;
        }
    }

    if r.bit()? {
        r.bits(4)?;
        r.bits(4)?;
        if sequence.separate_uv_delta_q {
            r.bits(4)?;
        }
    }

    Ok(Quantizer {
        base_q_idx,
        deltas_zero,
    })
}

fn delta_q(r: &mut BitReader<'_>) -> Result<i32, Av1SyntaxError> {
    if r.bit()? {
        r.signed(DELTA_BITS)
    } else {
        Ok(0)
    }
}

fn segment_q_index(base_q_idx: u32, alt_q: Option<i32>) -> i32 {
    let base = i32::try_from(base_q_idx).unwrap_or(MAX_Q_INDEX);
    alt_q.map_or(base, |delta| (base + delta).clamp(0, MAX_Q_INDEX))
}

fn loop_filter_params(r: &mut BitReader<'_>, sequence: &Sequence) -> Result<(), Av1SyntaxError> {
    let level_y_vertical = r.bits(6)?;
    let level_y_horizontal = r.bits(6)?;
    if sequence.planes() > 1 && (level_y_vertical != 0 || level_y_horizontal != 0) {
        r.bits(6)?;
        r.bits(6)?;
    }
    r.bits(3)?;

    if r.bit()? && r.bit()? {
        for _ in 0..LOOP_FILTER_REF_DELTAS + LOOP_FILTER_MODE_DELTAS {
            if r.bit()? {
                r.signed(DELTA_BITS)?;
            }
        }
    }
    Ok(())
}

fn cdef_params(r: &mut BitReader<'_>, sequence: &Sequence) -> Result<(), Av1SyntaxError> {
    r.bits(2)?;
    let strengths = 1 << r.bits(2)?;
    let planes_with_strength = if sequence.planes() > 1 { 2 } else { 1 };
    for _ in 0..strengths * planes_with_strength {
        r.bits(4)?;
        r.bits(2)?;
    }
    Ok(())
}

fn loop_restoration_params(
    r: &mut BitReader<'_>,
    sequence: &Sequence,
) -> Result<(), Av1SyntaxError> {
    let mut uses_lr = false;
    let mut uses_chroma_lr = false;
    for plane in 0..sequence.planes() {
        if r.bits(2)? != 0 {
            uses_lr = true;
            uses_chroma_lr |= plane > 0;
        }
    }

    if !uses_lr {
        return Ok(());
    }

    let unit_shift = r.bit()?;
    if unit_shift && !sequence.use_128x128_superblock {
        r.bit()?;
    }
    if sequence.subsampling_x && sequence.subsampling_y && uses_chroma_lr {
        r.bit()?;
    }
    Ok(())
}

fn global_motion_params(
    r: &mut BitReader<'_>,
    allow_high_precision_mv: bool,
) -> Result<(), Av1SyntaxError> {
    for _ in 0..REFS_PER_FRAME {
        let kind = warp_model(r)?;

        if kind >= ROTZOOM {
            let params = if kind == AFFINE { 4 } else { 2 };
            for _ in 0..params {
                subexp(r, 2 * (1 << GM_ABS_ALPHA_BITS) + 1)?;
            }
        }
        if kind >= TRANSLATION {
            let abs_bits = if kind == TRANSLATION {
                GM_ABS_TRANS_ONLY_BITS - u32::from(!allow_high_precision_mv)
            } else {
                GM_ABS_TRANS_BITS
            };
            for _ in 0..2 {
                subexp(r, 2 * (1 << abs_bits) + 1)?;
            }
        }
    }
    Ok(())
}

fn warp_model(r: &mut BitReader<'_>) -> Result<u32, Av1SyntaxError> {
    let is_global = r.bit()?;
    if !is_global {
        return Ok(0);
    }

    let is_rot_zoom = r.bit()?;
    if is_rot_zoom {
        return Ok(ROTZOOM);
    }

    let is_translation = r.bit()?;
    Ok(if is_translation { TRANSLATION } else { AFFINE })
}

fn subexp(r: &mut BitReader<'_>, symbols: u32) -> Result<(), Av1SyntaxError> {
    let mut index = 0;
    let mut covered = 0;

    loop {
        let bits = if index == 0 {
            SUBEXP_K
        } else {
            SUBEXP_K + index - 1
        };
        let span = 1 << bits;
        if symbols <= covered + 3 * span {
            r.non_symmetric(symbols - covered)?;
            return Ok(());
        }
        if !r.bit()? {
            r.bits(bits)?;
            return Ok(());
        }
        index += 1;
        covered += span;
    }
}

fn film_grain_params(
    r: &mut BitReader<'_>,
    sequence: &Sequence,
    frame_type: u32,
) -> Result<(), Av1SyntaxError> {
    if !r.bit()? {
        return Ok(());
    }

    r.bits(16)?;
    let update_grain = frame_type != INTER_FRAME || r.bit()?;
    if !update_grain {
        r.bits(3)?;
        return Ok(());
    }

    let luma_points = r.bits(4)?;
    for _ in 0..luma_points {
        r.bits(16)?;
    }

    let chroma_from_luma = !sequence.mono_chrome && r.bit()?;
    let (blue_points, red_points) = if sequence.mono_chrome
        || chroma_from_luma
        || (sequence.subsampling_x && sequence.subsampling_y && luma_points == 0)
    {
        (0, 0)
    } else {
        let cb = r.bits(4)?;
        for _ in 0..cb {
            r.bits(16)?;
        }
        let cr = r.bits(4)?;
        for _ in 0..cr {
            r.bits(16)?;
        }
        (cb, cr)
    };

    r.bits(2)?;
    let lag = r.bits(2)?;
    let luma_positions = 2 * lag * (lag + 1);
    let chroma_positions = if luma_points > 0 {
        for _ in 0..luma_positions {
            r.bits(8)?;
        }
        luma_positions + 1
    } else {
        luma_positions
    };
    for points in [blue_points, red_points] {
        if chroma_from_luma || points > 0 {
            for _ in 0..chroma_positions {
                r.bits(8)?;
            }
        }
    }

    r.bits(2)?;
    r.bits(2)?;
    for points in [blue_points, red_points] {
        if points > 0 {
            r.bits(8)?;
            r.bits(8)?;
            r.bits(9)?;
        }
    }
    r.bit()?;
    r.bit()?;
    Ok(())
}
