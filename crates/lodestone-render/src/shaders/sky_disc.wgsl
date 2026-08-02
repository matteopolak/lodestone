
struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

// `EnvironmentAttributes.SKY_FOG_END_DISTANCE`'s default, in blocks. Kept in
// step with `crate::sky::SKY_FOG_END_DISTANCE` by a unit test rather than by a
// comment.
const SKY_FOG_END: f32 = 512.0;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) fog_color: vec4<f32>,
    @location(2) local_pos: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) fog_color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(position, 1.0);
    out.color = color;
    out.fog_color = fog_color;
    out.local_pos = position;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // `linear_fog_value(sphericalVertexDistance, 0.0, FogSkyEnd)` — the 0.0
    // start means this is a plain normalised distance, so the disc centre (16
    // blocks up) is essentially pure sky colour and its rim (512 blocks out)
    // is pure fog colour.
    let fog_value = clamp(length(in.local_pos) / SKY_FOG_END, 0.0, 1.0);
    // `apply_fog`: mix weighted by the fog colour's own alpha, and the
    // fragment keeps the sky colour's alpha rather than the fog colour's.
    let rgb = mix(in.color.rgb, in.fog_color.rgb, fog_value * in.fog_color.a);
    return vec4<f32>(rgb, in.color.a);
}
