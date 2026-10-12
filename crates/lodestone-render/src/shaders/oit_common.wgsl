// Order-independent transparency: the wavelet transmittance function shared by
// the translucent-terrain stages (`oit_stages.wgsl`) and the composite
// (`oit_composite.wgsl`).
//
// Each translucent fragment adds its absorbance, as a depth-resolved wavelet
// series, to eight coefficients per pixel (two RGBA16F targets). A later pass
// reads the series back to learn how much light the translucent surfaces in
// front of a fragment have already absorbed, so the fragment can be weighted
// without ever sorting. Depth is normalised to the per-pixel [nearest, farthest]
// translucent range from the bounds target and binned into eight bins.
//
// Group 0 carries the texture bindings; bindings 0 and 1 are the camera and the
// section origin of the surface shaders that append this file.

const OIT_COEFF_COUNT: i32 = 8;
const OIT_WAVELET_RANK: i32 = 2;
const OIT_DEPTH_BINS: i32 = 8;
// Past this many blocks of depth range the bins stop being spread evenly: the
// first ten blocks keep most of them, so surfaces near the camera stay distinct
// when a distant translucent fragment stretches the range.
const OIT_HIGH_PRECISION_DEPTH: f32 = 10.0;
const OIT_LOW_PRECISION_BINS: i32 = 2;
// A pixel whose surfaces together pass less than this fraction of the light is
// treated as fully covered.
const OIT_FULLY_OPAQUE_TRANSMITTANCE: f32 = 0.02;

@group(0) @binding(2) var oit_bounds: texture_2d<f32>;
@group(0) @binding(3) var oit_coeff0: texture_2d<f32>;
@group(0) @binding(4) var oit_coeff1: texture_2d<f32>;

fn oit_to_absorbance(transmittance: f32) -> f32 {
    return clamp(-log(max(transmittance, 0.0001)), 0.0, 4.0);
}

fn oit_to_transmittance(absorbance: f32) -> f32 {
    return clamp(exp(-absorbance), 0.0001, 1.0);
}

// Maps a fragment's view-space distance onto [0, 1] within this pixel's
// translucent depth range. The bounds target holds (-nearest, farthest).
fn oit_normalize_depth(fragment_distance: f32, pos: vec2<i32>) -> f32 {
    let bounds = textureLoad(oit_bounds, pos, 0);
    let nearest = -bounds.r;
    let farthest = bounds.g;
    let range = max(farthest - nearest, 0.0001);
    let within = clamp(fragment_distance - nearest, 0.0, range);

    let high_fraction = f32(OIT_DEPTH_BINS - OIT_LOW_PRECISION_BINS) / f32(OIT_DEPTH_BINS);
    if (high_fraction * range <= OIT_HIGH_PRECISION_DEPTH) {
        return within / range;
    }
    if (within <= OIT_HIGH_PRECISION_DEPTH) {
        return (within / OIT_HIGH_PRECISION_DEPTH) * high_fraction;
    }
    return high_fraction
        + ((within - OIT_HIGH_PRECISION_DEPTH) / (range - OIT_HIGH_PRECISION_DEPTH)) * (1.0 - high_fraction);
}

struct OitCoefficients {
    c0: vec4<f32>,
    c1: vec4<f32>,
};

// The coefficients one fragment contributes: coefficient 0 is the absorbance
// averaged over the range, 1..7 the Haar difference coefficients along the path
// from the fragment's bin to the root.
fn oit_fragment_coefficients(transmittance: f32, normalized_depth: f32) -> OitCoefficients {
    var c: array<f32, 8>;
    let absorbance = oit_to_absorbance(transmittance);
    let depth = normalized_depth * f32(OIT_DEPTH_BINS - 1) / f32(OIT_DEPTH_BINS);
    c[0] = absorbance * (1.0 - depth);

    let bin = i32(floor(normalized_depth * f32(OIT_DEPTH_BINS - 1)));
    var tree = bin + OIT_COEFF_COUNT;
    for (var level = 0; level <= OIT_WAVELET_RANK; level++) {
        let power = OIT_WAVELET_RANK - level;
        let parent = tree >> 1u;
        let index_at_level = f32(parent & ((1 << u32(power)) - 1));
        let right = tree & 1;
        let sign = 1 - (right << 1u);
        let width = exp2(-f32(power));
        let height_scale = exp2(f32(power) * 0.5);
        let offset = depth - width * index_at_level;
        let edge_distance = f32(right) * width + f32(sign) * offset;
        c[parent] = edge_distance * height_scale * absorbance;
        tree = parent;
    }
    var out: OitCoefficients;
    out.c0 = vec4<f32>(c[0], c[1], c[2], c[3]);
    out.c1 = vec4<f32>(c[4], c[5], c[6], c[7]);
    return out;
}

// Absorbance accumulated strictly in front of `normalized_depth`, with this
// fragment's own contribution (`own_absorbance`) removed from the series. The
// result interpolates between the bin before the fragment and the fragment's
// own bin.
fn oit_sample_absorbance(coefficients: array<f32, 8>, normalized_depth: f32, own_absorbance: f32) -> f32 {
    var co = coefficients;
    var average = co[0];
    if (average == 0.0) {
        return 0.0;
    }

    let in_bins = normalized_depth * f32(OIT_DEPTH_BINS - 1);
    let depth = in_bins / f32(OIT_DEPTH_BINS);
    average -= own_absorbance * (1.0 - depth);

    var index_b = clamp(i32(floor(in_bins)), 0, OIT_DEPTH_BINS - 1);
    let sample_a_valid = index_b >= 1;
    var index_a = select(index_b, index_b - 1, sample_a_valid);
    index_b += OIT_COEFF_COUNT;
    index_a += OIT_COEFF_COUNT;

    var sample_b = average;
    var sample_a = select(0.0, average, sample_a_valid);

    for (var level = 0; level <= OIT_WAVELET_RANK; level++) {
        let power = OIT_WAVELET_RANK - level;
        let width = exp2(-f32(power));
        let height_scale = exp2(f32(power) * 0.5);

        let parent_b = index_b >> 1u;
        let right_b = index_b & 1;
        let sign_b = 1 - (right_b << 1u);
        let index_at_level = f32(parent_b & ((1 << u32(power)) - 1));
        let offset = depth - width * index_at_level;
        let own_difference = (f32(right_b) * width + f32(sign_b) * offset) * height_scale * own_absorbance;

        let difference_b = co[parent_b] - own_difference;
        sample_b -= height_scale * difference_b * f32(sign_b);
        index_b = parent_b;

        if (sample_a_valid) {
            let parent_a = index_a >> 1u;
            let difference_a = select(co[parent_a], difference_b, parent_a == parent_b);
            let right_a = index_a & 1;
            let sign_a = 1 - (right_a << 1u);
            sample_a -= height_scale * difference_a * f32(sign_a);
            index_a = parent_a;
        }
    }

    let blend = select(fract(in_bins), 1.0, in_bins >= f32(OIT_DEPTH_BINS));
    return mix(sample_a, sample_b, blend);
}

fn oit_sample_transmittance(pos: vec2<i32>, normalized_depth: f32, own_transmittance: f32) -> f32 {
    let a = textureLoad(oit_coeff0, pos, 0);
    let b = textureLoad(oit_coeff1, pos, 0);
    var coefficients: array<f32, 8>;
    coefficients[0] = a.x;
    coefficients[1] = a.y;
    coefficients[2] = a.z;
    coefficients[3] = a.w;
    coefficients[4] = b.x;
    coefficients[5] = b.y;
    coefficients[6] = b.z;
    coefficients[7] = b.w;
    return oit_to_transmittance(
        oit_sample_absorbance(coefficients, normalized_depth, oit_to_absorbance(own_transmittance)),
    );
}
