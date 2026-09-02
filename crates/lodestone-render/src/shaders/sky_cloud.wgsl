
struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var cloud_tex: texture_2d<f32>;
@group(1) @binding(1) var cloud_smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(position, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let sampled = textureSample(cloud_tex, cloud_smp, in.uv);
    // Vanilla's own empty-cell rule: alpha under 10/255 is an empty cell in
    // `clouds.png` — discarding it here is what turns one flat textured quad
    // into the right cloud silhouette with no CPU-side cell meshing.
    if sampled.a < 0.04 {
        discard;
    }
    // `in.color` is the real `CLOUD_COLOR` attribute times its timeline track:
    // pure white by day, `#191926` at night, alpha 0.8 throughout. The alpha is
    // load-bearing and only reaches pixels because `CloudPipeline` blends
    // (`CLOUD_BLEND`); with an opaque pipeline it was written to the target and
    // weighted nothing, and clouds painted solid.
    //
    // The multiply is nominally in the wrong space — both operands are linear
    // light, where vanilla multiplies gamma bytes — but it is exactly the
    // identity here and so cannot be wrong: `clouds.png` in the real jar is a
    // hard binary mask (see `load_cloud_texture`), so every texel that survives
    // the discard above is opaque **pure white** and `sampled.rgb` is 1.0. If a
    // resource pack ever ships a non-white clouds.png, this needs the same gamma
    // round-trip the terrain shaders use.
    return vec4<f32>(sampled.rgb * in.color.rgb, in.color.a);
}
