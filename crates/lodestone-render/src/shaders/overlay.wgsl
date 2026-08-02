
@group(0) @binding(0) var overlay_tex: texture_2d<f32>;
@group(0) @binding(1) var overlay_smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(position, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

// Same gamma round-trip every tint in this codebase uses (see
// `model_pipeline.rs`): vanilla multiplies a gamma-space colour by a
// gamma-space tint on gamma-space bytes, never in linear light. Only the
// tint's RGB goes through the round-trip — alpha is coverage, not colour, and
// is not gamma-encoded.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let tex = textureSample(overlay_tex, overlay_smp, in.uv);
    let tinted = srgb_to_linear(linear_to_srgb(tex.rgb) * in.color.rgb);
    return vec4<f32>(tinted, tex.a * in.color.a);
}
