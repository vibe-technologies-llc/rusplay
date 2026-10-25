override BOUNDARY_STEPS: u32 = 16u;
override COMPRESSION_POWER: f32 = 1.2;

const PATH_MATRIX: u32 = 0u;
const PATH_PERCEPTUAL: u32 = 1u;
const PATH_CLIP: u32 = 2u;
const PATH_DESATURATE: u32 = 3u;
const PATH_HIGHLIGHT: u32 = 4u;

const GAMUT_EPSILON: f32 = 1.0e-5;
const MIN_SEARCH_CHROMA: f32 = 1.0;
const ACHROMATIC_CHROMA: f32 = 1.0e-6;
const MIN_COMPRESSION_LIMIT: f32 = 1.0001;

fn inside(rgb: vec3<f32>, peak: f32) -> bool {
    let margin = GAMUT_EPSILON * peak;
    return all(rgb >= vec3<f32>(-margin)) && all(rgb <= vec3<f32>(peak + margin));
}

fn chroma_boundary(lightness: f32, hue: vec2<f32>, to_rgb: Rows, peak: f32, ceiling: f32) -> f32 {
    let at = vec3<f32>(lightness, 0.0, 0.0);
    if !inside(apply(to_rgb, lab_to_xyz(at)), peak) {
        return 0.0;
    }

    var low = 0.0;
    var high = ceiling;
    if inside(apply(to_rgb, lab_to_xyz(vec3<f32>(lightness, hue * high))), peak) {
        return high;
    }
    for (var step = 0u; step < BOUNDARY_STEPS; step++) {
        let middle = 0.5 * (low + high);
        if inside(apply(to_rgb, lab_to_xyz(vec3<f32>(lightness, hue * middle))), peak) {
            low = middle;
        } else {
            high = middle;
        }
    }
    return low;
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
    let ceiling = max(MIN_SEARCH_CHROMA, chroma);
    let target_limit = chroma_boundary(lab.x, hue, params.work_to_target, peak, ceiling);
    let knee = params.levels.y;
    if chroma <= knee * target_limit {
        return clamp(apply(params.work_to_target, work), vec3<f32>(0.0), vec3<f32>(peak));
    }
    let source_limit = chroma_boundary(lab.x, hue, params.work_to_source, peak, ceiling);
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
