struct ToneCurve {
    source_peak_nits: f32,
    target_peak_nits: f32,
    pq_source_peak: f32,
    pq_source_black: f32,
    min_lum: f32,
    max_lum: f32,
    knee_start: f32,
    padding: f32,
}

struct ToneState {
    curve: ToneCurve,
    smoothed_peak_pq: f32,
    previous_average_pq: f32,
    frames: u32,
    padding: u32,
}

const TONE_BT2390: u32 = 1u;
const TONE_BT2446A: u32 = 2u;
const TONE_SPLINE: u32 = 3u;
const TONE_REINHARD: u32 = 4u;

const MAX_NITS: f32 = 10000.0;
const MIN_PQ_RANGE: f32 = 1.0e-6;
const BT2446A_GAMMA: f32 = 2.4;
const BT2446A_RHO_GAIN: f32 = 32.0;
const BT2446A_LINEAR_END: f32 = 0.7399;
const BT2446A_QUADRATIC_END: f32 = 0.9909;
const SPLINE_PIVOT: f32 = 0.7;

fn pq_from_nits(nits: f32) -> f32 {
    return pq_encode(vec3<f32>(nits / MAX_NITS)).x;
}

fn pq_to_nits(code: f32) -> f32 {
    return pq_decode(vec3<f32>(code)).x * MAX_NITS;
}

fn make_curve(source_peak_nits: f32) -> ToneCurve {
    let display_peak = params.peak_limits.x;
    let pq_source_peak = pq_from_nits(source_peak_nits);
    let pq_source_black = pq_from_nits(params.peak_limits.y);
    let range = max(pq_source_peak - pq_source_black, MIN_PQ_RANGE);
    let max_lum = (pq_from_nits(display_peak) - pq_source_black) / range;
    let min_lum = max((pq_from_nits(params.peak_limits.z) - pq_source_black) / range, 0.0);

    return ToneCurve(
        source_peak_nits,
        display_peak,
        pq_source_peak,
        pq_source_black,
        min_lum,
        max_lum,
        1.5 * max_lum - 0.5,
        0.0,
    );
}

fn hermite(t: f32, start: f32, start_tangent: f32, end: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    return (2.0 * t3 - 3.0 * t2 + 1.0) * start + (t3 - 2.0 * t2 + t) * start_tangent + (-2.0 * t3 + 3.0 * t2) * end;
}

fn bt2390(normalized: f32, curve: ToneCurve) -> f32 {
    let knee = curve.knee_start;
    var rolled = normalized;
    if normalized >= knee && knee < 1.0 {
        rolled = hermite((normalized - knee) / (1.0 - knee), knee, 1.0 - knee, curve.max_lum);
    }
    let lift = 1.0 - rolled;
    return rolled + curve.min_lum * lift * lift * lift * lift;
}

fn spline(normalized: f32, curve: ToneCurve) -> f32 {
    let max_lum = curve.max_lum;
    let pivot = SPLINE_PIVOT * max_lum;
    if max_lum >= 1.0 || normalized <= pivot {
        return normalized;
    }

    let span = 1.0 - pivot;
    let start_slope = min(3.0 * (max_lum - pivot) / span, 1.0);
    return hermite((normalized - pivot) / span, pivot, span * start_slope, max_lum);
}

fn bt2446a(nits: f32, curve: ToneCurve) -> f32 {
    let source = curve.source_peak_nits;
    let display_peak = curve.target_peak_nits;
    if source <= display_peak {
        return min(nits, display_peak);
    }

    let rho_hdr = 1.0 + BT2446A_RHO_GAIN * pow(source / MAX_NITS, 1.0 / BT2446A_GAMMA);
    let rho_sdr = 1.0 + BT2446A_RHO_GAIN * pow(display_peak / MAX_NITS, 1.0 / BT2446A_GAMMA);
    let encoded = pow(clamp(nits / source, 0.0, 1.0), 1.0 / BT2446A_GAMMA);
    let perceptual = log(1.0 + (rho_hdr - 1.0) * encoded) / log(rho_hdr);
    var toned: f32;
    if perceptual <= BT2446A_LINEAR_END {
        toned = 1.077 * perceptual;
    } else if perceptual < BT2446A_QUADRATIC_END {
        toned = (-1.151 * perceptual + 2.7811) * perceptual - 0.6302;
    } else {
        toned = 0.5 * perceptual + 0.5;
    }
    let sdr = (pow(rho_sdr, toned) - 1.0) / (rho_sdr - 1.0);
    return display_peak * pow(clamp(sdr, 0.0, 1.0), BT2446A_GAMMA);
}

fn reinhard(nits: f32, curve: ToneCurve) -> f32 {
    let display_peak = curve.target_peak_nits;
    let white = curve.source_peak_nits / display_peak;
    if white <= 1.0 {
        return min(nits, display_peak);
    }

    let x = max(nits, 0.0) / display_peak;
    return min(x * (1.0 + x / (white * white)) / (1.0 + x), 1.0) * display_peak;
}

fn tone_curve_pq(code: f32, curve: ToneCurve, curve_kind: u32) -> f32 {
    let range = max(curve.pq_source_peak - curve.pq_source_black, MIN_PQ_RANGE);
    let normalized = clamp((code - curve.pq_source_black) / range, 0.0, 1.0);
    switch curve_kind {
        case TONE_BT2390: {
            return bt2390(normalized, curve) * range + curve.pq_source_black;
        }
        case TONE_SPLINE: {
            return spline(normalized, curve) * range + curve.pq_source_black;
        }
        case TONE_BT2446A: {
            return pq_from_nits(bt2446a(pq_to_nits(code), curve));
        }
        case TONE_REINHARD: {
            return pq_from_nits(reinhard(pq_to_nits(code), curve));
        }
        default: {
            return pq_from_nits(min(pq_to_nits(code), curve.target_peak_nits));
        }
    }
}

fn tone_map(rgb: vec3<f32>, curve: ToneCurve) -> vec3<f32> {
    let xyz = apply(params.source_to_work, rgb) * params.tone_levels.x;
    let lms = apply(params.tone_xyz_to_lms, xyz);
    let itp = apply(params.tone_lms_to_itp, signed_pq_encode(lms / MAX_NITS));

    let intensity = itp.x;
    let mapped = tone_curve_pq(max(intensity, 0.0), curve, params.tone);
    var ratio = 1.0;
    if intensity > 0.0 && mapped > 0.0 {
        ratio = min(mapped / intensity, intensity / mapped);
    }

    let toned = vec3<f32>(mapped, itp.yz * ratio);
    let toned_lms = signed_pq_decode(apply(params.tone_itp_to_lms, toned)) * MAX_NITS;
    let toned_xyz = apply(params.tone_lms_to_xyz, toned_lms);
    return apply(params.work_to_source, toned_xyz) / params.tone_levels.y;
}
