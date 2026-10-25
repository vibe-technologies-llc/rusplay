use rusplay_core::{HdrLevels, Matrix3, pq_from_nits, pq_to_nits};

use crate::color::{BOUNDARY_STEPS, COMPRESSION_POWER, ColorPlan, MappingPath, WorkingSpace};

const GAMUT_EPSILON: f64 = 1.0e-5;
const MIN_SEARCH_CHROMA: f64 = 1.0;
const ACHROMATIC_CHROMA: f64 = 1.0e-6;
const MIN_COMPRESSION_LIMIT: f64 = 1.0001;
const MAX_NITS: f64 = HdrLevels::MAX_NITS as f64;

fn signed(value: f64, curve: impl Fn(f64) -> f64) -> f64 {
    value.signum() * curve(value.abs())
}

pub(crate) fn xyz_to_lab(plan: &ColorPlan, xyz: [f64; 3]) -> [f64; 3] {
    let lms = plan.xyz_to_lms.apply(xyz);
    let shaped = match plan.space {
        WorkingSpace::Ictcp => {
            lms.map(|value| signed(value * plan.pq_scale, |v| pq_from_nits(v * MAX_NITS)))
        }
        WorkingSpace::Oklab => lms.map(f64::cbrt),
    };

    plan.lms_to_lab.apply(shaped)
}

pub(crate) fn lab_to_xyz(plan: &ColorPlan, lab: [f64; 3]) -> [f64; 3] {
    let shaped = plan.lab_to_lms.apply(lab);
    let lms = match plan.space {
        WorkingSpace::Ictcp => shaped.map(|value| {
            signed(value, |code| pq_to_nits(code.min(1.0)) / MAX_NITS) / plan.pq_scale
        }),
        WorkingSpace::Oklab => shaped.map(|value| value * value * value),
    };

    plan.lms_to_xyz.apply(lms)
}

pub(crate) fn target_lab(plan: &ColorPlan, rgb: [f64; 3]) -> [f64; 3] {
    let target_to_work = plan.work_to_target.inverse().unwrap_or(Matrix3::IDENTITY);

    xyz_to_lab(plan, target_to_work.apply(rgb))
}

pub(crate) fn source_lab(plan: &ColorPlan, rgb: [f64; 3]) -> [f64; 3] {
    xyz_to_lab(
        plan,
        plan.source_to_work
            .apply(rgb.map(|value| value * plan.gain)),
    )
}

fn inside(rgb: [f64; 3], peak: f64) -> bool {
    let margin = GAMUT_EPSILON * peak;

    rgb.iter()
        .all(|value| (-margin..=peak + margin).contains(value))
}

fn at_chroma(plan: &ColorPlan, lightness: f64, hue: [f64; 2], chroma: f64) -> [f64; 3] {
    lab_to_xyz(plan, [lightness, hue[0] * chroma, hue[1] * chroma])
}

pub(crate) fn chroma_boundary(
    plan: &ColorPlan,
    lightness: f64,
    hue: [f64; 2],
    to_rgb: &Matrix3,
    ceiling: f64,
) -> f64 {
    let peak = plan.target_peak;
    let fits = |chroma: f64| inside(to_rgb.apply(at_chroma(plan, lightness, hue, chroma)), peak);

    if !fits(0.0) {
        return 0.0;
    }
    if fits(ceiling) {
        return ceiling;
    }

    let (mut low, mut high) = (0.0, ceiling);
    for _ in 0..BOUNDARY_STEPS {
        let middle = f64::midpoint(low, high);
        if fits(middle) {
            low = middle;
        } else {
            high = middle;
        }
    }
    low
}

pub(crate) fn compress_chroma(chroma: f64, target_limit: f64, source_limit: f64, knee: f64) -> f64 {
    if target_limit <= 0.0 {
        return 0.0;
    }

    let distance = chroma / target_limit;
    let limit = source_limit / target_limit;
    if limit <= MIN_COMPRESSION_LIMIT || distance <= knee {
        return distance.min(1.0) * target_limit;
    }

    let power = COMPRESSION_POWER;
    let scale =
        (limit - knee) / (((1.0 - knee) / (limit - knee)).powf(-power) - 1.0).powf(1.0 / power);
    let excess = (distance - knee) / scale;
    let compressed = knee + (distance - knee) / (1.0 + excess.powf(power)).powf(1.0 / power);

    compressed.min(1.0) * target_limit
}

fn clamp(rgb: [f64; 3], peak: f64) -> [f64; 3] {
    rgb.map(|value| value.clamp(0.0, peak))
}

fn perceptual(plan: &ColorPlan, work: [f64; 3]) -> [f64; 3] {
    let peak = plan.target_peak;
    let [lightness, a, b] = xyz_to_lab(plan, work);
    let chroma = a.hypot(b);
    if chroma < ACHROMATIC_CHROMA {
        return clamp(plan.work_to_target.apply(work), peak);
    }

    let hue = [a / chroma, b / chroma];
    let ceiling = MIN_SEARCH_CHROMA.max(chroma);
    let target_limit = chroma_boundary(plan, lightness, hue, &plan.work_to_target, ceiling);
    if chroma <= plan.knee * target_limit {
        return clamp(plan.work_to_target.apply(work), peak);
    }
    let source_limit = chroma_boundary(plan, lightness, hue, &plan.work_to_source, ceiling);
    let mapped = compress_chroma(chroma, target_limit, source_limit, plan.knee);

    clamp(
        plan.work_to_target
            .apply(at_chroma(plan, lightness, hue, mapped)),
        peak,
    )
}

fn desaturate(plan: &ColorPlan, rgb: [f64; 3]) -> [f64; 3] {
    let peak = plan.target_peak;
    let grey = plan
        .target_luma
        .iter()
        .zip(rgb)
        .map(|(weight, value)| weight * value)
        .sum::<f64>()
        .clamp(0.0, peak);
    let amount = rgb.iter().fold(1.0_f64, |amount, &value| {
        if value < 0.0 {
            amount.min(grey / (grey - value))
        } else if value > peak {
            amount.min((peak - grey) / (value - grey))
        } else {
            amount
        }
    });

    clamp(rgb.map(|value| grey + amount * (value - grey)), peak)
}

fn highlight(plan: &ColorPlan, rgb: [f64; 3]) -> [f64; 3] {
    let peak = plan.target_peak;
    let clipped = clamp(rgb, peak);

    if inside(rgb, peak) {
        clipped
    } else {
        clipped.map(|value| peak - value)
    }
}

pub(crate) fn map(plan: &ColorPlan, rgb: [f64; 3]) -> [f64; 3] {
    let source = rgb.map(|value| value * plan.gain);
    let direct = plan.source_to_target.apply(source);

    match plan.path {
        MappingPath::Matrix => direct,
        MappingPath::Perceptual => perceptual(plan, plan.source_to_work.apply(source)),
        MappingPath::Clip => clamp(direct, plan.target_peak),
        MappingPath::Desaturate => desaturate(plan, direct),
        MappingPath::Highlight => highlight(plan, direct),
    }
}
