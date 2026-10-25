override HISTOGRAM_BINS: u32;
const MIN_AVERAGE_NITS: f32 = 1.0e-4;
const DECIBELS_PER_DECADE: f32 = 10.0;

@group(0) @binding(0) var<uniform> params: ColorParams;
@group(0) @binding(1) var<storage, read_write> state: ToneState;
@group(0) @binding(2) var<storage, read_write> histogram: array<atomic<u32>>;

fn decibels(numerator: f32, denominator: f32) -> f32 {
    return DECIBELS_PER_DECADE * log2(numerator / denominator) / log2(10.0);
}

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn detect_peak() {
    var total = 0u;
    for (var bin = 0u; bin < HISTOGRAM_BINS; bin++) {
        total += atomicLoad(&histogram[bin]);
    }
    if total == 0u {
        return;
    }

    let wanted = params.dynamic.x * f32(total);
    var cumulative = 0u;
    var peak_bin = HISTOGRAM_BINS - 1u;
    var found = false;
    var weighted = 0.0;
    for (var bin = 0u; bin < HISTOGRAM_BINS; bin++) {
        let count = atomicLoad(&histogram[bin]);
        cumulative += count;
        weighted += f32(count) * (f32(bin) + 0.5);
        if !found && f32(cumulative) >= wanted {
            peak_bin = bin;
            found = true;
        }
        atomicStore(&histogram[bin], 0u);
    }

    let frame_peak = f32(peak_bin + 1u) / f32(HISTOGRAM_BINS);
    let average = weighted / f32(total) / f32(HISTOGRAM_BINS);
    var smoothed = frame_peak;
    if state.frames > 0u {
        let change = abs(decibels(
            max(pq_to_nits(average), MIN_AVERAGE_NITS),
            max(pq_to_nits(state.previous_average_pq), MIN_AVERAGE_NITS),
        ));
        if change <= params.dynamic.z {
            smoothed = state.smoothed_peak_pq + (frame_peak - state.smoothed_peak_pq) / params.dynamic.y;
        }
    }

    state.smoothed_peak_pq = smoothed;
    state.previous_average_pq = average;
    state.frames = min(state.frames, 0xfffffffeu) + 1u;
    let peak_nits = max(min(pq_to_nits(smoothed), params.peak_limits.w), params.peak_limits.x);
    state.curve = make_curve(peak_nits);
}
