// The three order-independent transparency stages of a translucent surface,
// appended to `model.wgsl` or `fluid.wgsl` (which supply `VsOut` and
// `shade_fragment`) together with `oit_common.wgsl`.
//
// Run in this order over the same geometry, none of them depth-writing:
//   1. bounds         nearest and farthest translucent distance per pixel (max blend)
//   2. transmittance  every fragment adds its absorbance to the coefficients
//   3. accumulate     every fragment is weighted by the transmittance in front of it
// `oit_composite.wgsl` then resolves the accumulated colour over the scene.
//
// The distance is the clip-space w, which the position builtin hands the
// fragment stage as its reciprocal.

@fragment
fn fs_oit_bounds(in: VsOut) -> @location(0) vec4<f32> {
    let alpha = shade_fragment(in).a;
    if (alpha < 0.01) {
        discard;
    }
    let distance = 1.0 / in.clip.w;
    return vec4<f32>(-distance, distance, 0.0, 0.0);
}

struct OitTransmittanceOut {
    @location(0) c0: vec4<f32>,
    @location(1) c1: vec4<f32>,
};

@fragment
fn fs_oit_transmittance(in: VsOut) -> OitTransmittanceOut {
    let alpha = shade_fragment(in).a;
    let pos = vec2<i32>(in.clip.xy);
    let depth = oit_normalize_depth(1.0 / in.clip.w, pos);
    let coefficients = oit_fragment_coefficients(1.0 - alpha, depth);
    var out: OitTransmittanceOut;
    out.c0 = coefficients.c0;
    out.c1 = coefficients.c1;
    return out;
}

@fragment
fn fs_oit_accumulate(in: VsOut) -> @location(0) vec4<f32> {
    let colour = shade_fragment(in);
    let pos = vec2<i32>(in.clip.xy);
    let depth = oit_normalize_depth(1.0 / in.clip.w, pos);
    let in_front = oit_sample_transmittance(pos, depth, 1.0 - colour.a);
    return vec4<f32>(colour.rgb * colour.a, colour.a) * in_front;
}
