// The beacon light beam: `rendertype_beacon_beam.vsh`/`.fsh`, ported.
//
// No lighting term at all, matching the real shaders exactly — vanilla submits
// beam geometry with `setLight(15728880)` (full-bright), so there is nothing
// for a light channel to attenuate. `fragColor = texture(Sampler0, uv) *
// vertexColor * ColorModulator` minus the fog term this pass does not carry
// (see `gpu/beacon_beam.rs`'s module doc for why fog is a deliberate gap
// here, the same one `gpu/sign_text.rs` already accepts for its own
// jar-sourced-texture pass).

struct CameraUniform {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

@group(1) @binding(0)
var t_beam: texture_2d<f32>;
@group(1) @binding(1)
var s_beam: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.color = in.color;
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let sampled = textureSample(t_beam, s_beam, in.uv);
    return sampled * in.color;
}
