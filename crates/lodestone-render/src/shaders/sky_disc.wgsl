
struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

// The fog end used to be a module-level constant here, fixed at 512 blocks — the
// `EnvironmentAttributes.SKY_FOG_END_DISTANCE` *default*. Issue #399: vanilla
// clamps that attribute to the render distance in blocks before the shader ever
// reads it (`AtmosphericFogEnvironment.java:73`), so a constant is only right at
// render distance 32 and stretched the ramp 4x too far at the client default of
// 8. It now arrives per vertex on `@location(3)` — identical across all ten
// vertices of the fan, like the two colours beside it, so that this pass stays
// at one bind group. See `crate::sky::sky_fog_end_for_render_distance`.
//
// Do not reintroduce a constant fog end under any name: a shader-level constant
// shadowing this attribute would make the render distance inert while every
// Rust-side test still passed, which is what `sky_pipeline.rs`'s
// `the_disc_shader_takes_the_fog_end_as_a_vertex_input` exists to catch. That
// test greps this file, so the phrasing above deliberately avoids spelling the
// old declaration out.

// The floor on the divisor. A render distance of 0 chunks is a fog end of 0
// blocks, and `x / 0.0` here would be `inf` for a painted fragment but `0.0/0.0
// = NaN` at the disc centre, where WGSL does not specify what `clamp` returns.
// Flooring makes that degenerate case a fully-fogged disc, which is the correct
// limit, instead of an arbitrary colour.
const SKY_FOG_END_MIN: f32 = 1.0e-4;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) fog_color: vec4<f32>,
    @location(2) local_pos: vec3<f32>,
    @location(3) fog_end: f32,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) fog_color: vec4<f32>,
    @location(3) fog_end: f32,
) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(position, 1.0);
    out.color = color;
    out.fog_color = fog_color;
    out.local_pos = position;
    out.fog_end = fog_end;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // `linear_fog_value(sphericalVertexDistance, 0.0, FogSkyEnd)` — the 0.0
    // start means this is a plain normalised distance, so the disc centre (16
    // blocks up) is essentially pure sky colour and everything at or past
    // `fog_end` is pure fog colour. `fog_end` is vanilla's `fog.skyEnd`:
    // `min(renderDistanceInBlocks, 512)`, so the ramp shortens with the
    // player's render distance rather than always running to the disc's 512-block
    // rim.
    let fog_value = clamp(length(in.local_pos) / max(in.fog_end, SKY_FOG_END_MIN), 0.0, 1.0);
    // `apply_fog`: mix weighted by the fog colour's own alpha, and the
    // fragment keeps the sky colour's alpha rather than the fog colour's.
    let rgb = mix(in.color.rgb, in.fog_color.rgb, fog_value * in.fog_color.a);
    return vec4<f32>(rgb, in.color.a);
}
