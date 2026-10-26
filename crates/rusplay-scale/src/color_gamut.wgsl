override BOUNDARY_COARSE_STEPS: u32 = 4u;
override BOUNDARY_APPROACH_STEPS: u32 = 2u;
override BOUNDARY_WINDOW_STEPS: u32 = 4u;
override BOUNDARY_WINDOW_START: f32 = 0.75;
override BOUNDARY_SUBSTEPS: u32 = 4u;
override BOUNDARY_LOWER_TOLERANCE: f32 = 0.02;
override BOUNDARY_UPPER_TOLERANCE: f32 = 0.002;
override BOUNDARY_RAMP_CENTRE: f32 = 0.5;
override BOUNDARY_REACH: f32 = 3.0;
override BOUNDARY_HEADROOM: f32 = 1.25;
override COMPRESSION_POWER: f32 = 1.2;

const PATH_MATRIX: u32 = 0u;
const PATH_PERCEPTUAL: u32 = 1u;
const PATH_CLIP: u32 = 2u;
const PATH_DESATURATE: u32 = 3u;
const PATH_HIGHLIGHT: u32 = 4u;

const GAMUT_EPSILON: f32 = 1.0e-5;
const MIN_BOUND_DISTANCE: f32 = 1.0e-6;
const ACHROMATIC_CHROMA: f32 = 1.0e-6;
const MIN_COMPRESSION_LIMIT: f32 = 1.0001;
const UNREACHABLE: f32 = 3.0e38;

struct RaySample {
    value: vec3<f32>,
    slope: vec3<f32>,
}

struct Ray {
    origin: vec3<f32>,
    direction: vec3<f32>,
    lms_to_rgb: Rows,
}

struct Bounds {
    peak: f32,
    below: vec3<f32>,
    above: vec3<f32>,
}

struct Cubic {
    constant: vec3<f32>,
    linear: vec3<f32>,
    quadratic: vec3<f32>,
    cubic: vec3<f32>,
}

struct Breakpoints {
    approach_end: f32,
    approach_steps: u32,
    window_end: f32,
    window_steps: u32,
}

fn inside(rgb: vec3<f32>, peak: f32) -> bool {
    let margin = GAMUT_EPSILON * peak;
    return all(rgb >= vec3<f32>(-margin)) && all(rgb <= vec3<f32>(peak + margin));
}

fn compose_row(row: vec4<f32>, inner: Rows) -> vec4<f32> {
    return vec4<f32>(row.x * inner.x.xyz + row.y * inner.y.xyz + row.z * inner.z.xyz, 0.0);
}

fn compose(outer: Rows, inner: Rows) -> Rows {
    return Rows(compose_row(outer.x, inner), compose_row(outer.y, inner), compose_row(outer.z, inner));
}

fn ray_sample(ray: Ray, chroma: f32) -> RaySample {
    let shaped = ray.origin + chroma * ray.direction;
    var lms: vec3<f32>;
    var rate: vec3<f32>;
    if params.space == SPACE_ICTCP {
        let code = abs(shaped);
        let clamped = min(code, vec3<f32>(1.0));
        let power = pow(clamped, vec3<f32>(1.0 / COLOR_PQ_M2));
        let numerator = power - COLOR_PQ_C1;
        let denominator = COLOR_PQ_C2 - COLOR_PQ_C3 * power;
        let live = numerator > vec3<f32>(0.0);
        let sloped = live & code < vec3<f32>(1.0);
        let safe_numerator = select(vec3<f32>(1.0), numerator, live);
        let decoded = select(vec3<f32>(0.0), pow(safe_numerator / denominator, vec3<f32>(1.0 / COLOR_PQ_M1)), live);
        let safe_code = select(vec3<f32>(1.0), clamped, sloped);
        let derivative = decoded * (COLOR_PQ_C2 - COLOR_PQ_C3 * COLOR_PQ_C1) * power
            / (COLOR_PQ_M1 * COLOR_PQ_M2 * safe_numerator * denominator * safe_code);
        lms = sign(shaped) * decoded / params.levels.z;
        rate = select(vec3<f32>(0.0), derivative, sloped) / params.levels.z;
    } else {
        lms = shaped * shaped * shaped;
        rate = 3.0 * shaped * shaped;
    }
    return RaySample(apply(ray.lms_to_rgb, lms), apply(ray.lms_to_rgb, rate * ray.direction));
}

fn bound_excess(bounds: Bounds, rgb: vec3<f32>) -> f32 {
    let worst = max(-rgb * bounds.below, (rgb - vec3<f32>(bounds.peak)) * bounds.above);
    return max(worst.x, max(worst.y, worst.z)) + BOUNDARY_RAMP_CENTRE;
}

fn bound_reach(peak: f32, grey: RaySample) -> f32 {
    let toward = select(
        select(vec3<f32>(UNREACHABLE), (vec3<f32>(peak) - grey.value) / grey.slope, grey.slope > vec3<f32>(0.0)),
        grey.value / -grey.slope,
        grey.slope < vec3<f32>(0.0),
    );
    return min(toward.x, min(toward.y, toward.z));
}

fn weight(level: f32) -> f32 {
    return 1.0 - min(level, 1.0);
}

fn share(previous: f32, excess: f32, level: f32) -> f32 {
    if excess <= level {
        return weight(level);
    }
    var crossing = 0.0;
    if previous < level {
        crossing = (level - previous) / (excess - previous);
    }
    let low = min(level, 1.0);
    let high = min(excess, 1.0);
    let soft = (high - low) * (1.0 - 0.5 * (low + high)) / (excess - level);
    return crossing * weight(level) + (1.0 - crossing) * soft;
}

fn hull_excess(bounds: Bounds, near: RaySample, far: RaySample, width: f32) -> f32 {
    let leaving = near.value + (width / 3.0) * near.slope;
    let arriving = far.value - (width / 3.0) * far.slope;
    return max(bound_excess(bounds, leaving), max(bound_excess(bounds, arriving), bound_excess(bounds, far.value)));
}

fn hermite_cubic(near: RaySample, far: RaySample, width: f32) -> Cubic {
    let rise = far.value - near.value;
    let near_slope = width * near.slope;
    let far_slope = width * far.slope;
    return Cubic(
        near.value,
        near_slope,
        3.0 * rise - 2.0 * near_slope - far_slope,
        near_slope + far_slope - 2.0 * rise,
    );
}

fn cubic_at(curve: Cubic, fraction: f32) -> vec3<f32> {
    return curve.constant + fraction * (curve.linear + fraction * (curve.quadratic + fraction * curve.cubic));
}

fn breakpoint(breakpoints: Breakpoints, step: u32) -> f32 {
    if step <= breakpoints.approach_steps {
        return breakpoints.approach_end * f32(step) / f32(breakpoints.approach_steps);
    }
    let into = f32(step - breakpoints.approach_steps) / f32(breakpoints.window_steps);
    return breakpoints.approach_end + into * (breakpoints.window_end - breakpoints.approach_end);
}

fn soft_exit(ray: Ray, bounds: Bounds, grey: RaySample, breakpoints: Breakpoints) -> f32 {
    var previous = bound_excess(bounds, grey.value);
    var level = 0.0;
    var near = grey;
    var start = 0.0;
    var total = 0.0;
    let steps = breakpoints.approach_steps + breakpoints.window_steps;
    for (var step = 1u; step <= steps; step++) {
        let end = breakpoint(breakpoints, step);
        let width = end - start;
        let far = ray_sample(ray, end);
        if level <= 0.0 && hull_excess(bounds, near, far, width) <= 0.0 {
            total += width;
            previous = bound_excess(bounds, far.value);
        } else {
            let substep = width / f32(BOUNDARY_SUBSTEPS);
            let curve = hermite_cubic(near, far, width);
            for (var index = 1u; index <= BOUNDARY_SUBSTEPS; index++) {
                var rgb = far.value;
                if index < BOUNDARY_SUBSTEPS {
                    rgb = cubic_at(curve, f32(index) / f32(BOUNDARY_SUBSTEPS));
                }
                let excess = bound_excess(bounds, rgb);
                total += substep * share(previous, excess, level);
                level = max(level, excess);
                previous = excess;
                if level >= 1.0 {
                    return total;
                }
            }
        }
        near = far;
        start = end;
    }
    return total;
}

fn chroma_boundary(lightness: f32, hue: vec2<f32>, to_rgb: Rows, peak: f32, limit: f32) -> f32 {
    let ray = Ray(
        apply(params.lab_to_lms, vec3<f32>(lightness, 0.0, 0.0)),
        apply(params.lab_to_lms, vec3<f32>(0.0, hue)),
        compose(to_rgb, params.lms_to_xyz),
    );
    let grey = ray_sample(ray, 0.0);
    if !inside(grey.value, peak) {
        return 0.0;
    }

    let nearest = vec3<f32>(MIN_BOUND_DISTANCE * peak);
    let below = 1.0 / (BOUNDARY_LOWER_TOLERANCE * max(grey.value, nearest));
    let above = 1.0 / (BOUNDARY_UPPER_TOLERANCE * max(vec3<f32>(peak) - grey.value, nearest));
    let bounds = Bounds(peak, below, above);
    let coarse_range = min(BOUNDARY_REACH * bound_reach(peak, grey), limit);
    let coarse = soft_exit(ray, bounds, grey, Breakpoints(coarse_range, BOUNDARY_COARSE_STEPS, coarse_range, 0u));
    let window = Breakpoints(BOUNDARY_WINDOW_START * coarse, BOUNDARY_APPROACH_STEPS, BOUNDARY_HEADROOM * coarse, BOUNDARY_WINDOW_STEPS);
    return soft_exit(ray, bounds, grey, window);
}

fn compress_chroma(chroma: f32, target_limit: f32, source_limit: f32, knee: f32) -> f32 {
    if target_limit <= 0.0 {
        return 0.0;
    }

    let distance = chroma / target_limit;
    let limit = source_limit / target_limit;
    if limit <= MIN_COMPRESSION_LIMIT || distance <= knee {
        return min(distance, 1.0) * target_limit;
    }

    let power = COMPRESSION_POWER;
    let scale = (limit - knee) / pow(pow((1.0 - knee) / (limit - knee), -power) - 1.0, 1.0 / power);
    let excess = (distance - knee) / scale;
    let compressed = knee + (distance - knee) / pow(1.0 + pow(excess, power), 1.0 / power);
    return min(compressed, 1.0) * target_limit;
}

fn perceptual(work: vec3<f32>, peak: f32) -> vec3<f32> {
    let lab = xyz_to_lab(work);
    let chroma = length(lab.yz);
    if chroma < ACHROMATIC_CHROMA {
        return clamp(apply(params.work_to_target, work), vec3<f32>(0.0), vec3<f32>(peak));
    }

    let hue = lab.yz / chroma;
    let target_limit = chroma_boundary(lab.x, hue, params.work_to_target, peak, params.chroma_limits.x);
    let knee = params.levels.y;
    if chroma <= knee * target_limit {
        return clamp(apply(params.work_to_target, work), vec3<f32>(0.0), vec3<f32>(peak));
    }
    let source_limit = chroma_boundary(lab.x, hue, params.work_to_source, peak, params.chroma_limits.y);
    let mapped = compress_chroma(chroma, target_limit, source_limit, knee);
    let rgb = apply(params.work_to_target, lab_to_xyz(vec3<f32>(lab.x, hue * mapped)));
    return clamp(rgb, vec3<f32>(0.0), vec3<f32>(peak));
}

fn desaturate(rgb: vec3<f32>, peak: f32) -> vec3<f32> {
    let grey = clamp(dot(params.target_luma.xyz, rgb), 0.0, peak);
    var amount = 1.0;
    for (var channel = 0u; channel < 3u; channel++) {
        let value = rgb[channel];
        if value < 0.0 {
            amount = min(amount, grey / (grey - value));
        } else if value > peak {
            amount = min(amount, (peak - grey) / (value - grey));
        }
    }
    return clamp(vec3<f32>(grey) + amount * (rgb - vec3<f32>(grey)), vec3<f32>(0.0), vec3<f32>(peak));
}

fn highlight(rgb: vec3<f32>, peak: f32) -> vec3<f32> {
    let clipped = clamp(rgb, vec3<f32>(0.0), vec3<f32>(peak));
    if inside(rgb, peak) {
        return clipped;
    }
    return vec3<f32>(peak) - clipped;
}

fn map_gamut(source: vec3<f32>) -> vec3<f32> {
    let peak = params.levels.x;
    switch params.path {
        case PATH_PERCEPTUAL: {
            return perceptual(apply(params.source_to_work, source), peak);
        }
        case PATH_CLIP: {
            return clamp(apply(params.source_to_target, source), vec3<f32>(0.0), vec3<f32>(peak));
        }
        case PATH_DESATURATE: {
            return desaturate(apply(params.source_to_target, source), peak);
        }
        case PATH_HIGHLIGHT: {
            return highlight(apply(params.source_to_target, source), peak);
        }
        default: {
            return apply(params.source_to_target, source);
        }
    }
}
