// The lightning bolt: `core/rendertype_lightning`, ported.
//
// The simplest shader in the tree, and that is faithful rather than lazy — the
// real one is `POSITION_COLOR` with no sampler, no light and no overlay, so
// there is nothing to sample and nothing to attenuate. Every visible property
// of a bolt is in its geometry and in the pipeline's blend function
// (`SRC_ALPHA, ONE` — additive), not here.
//
// No fog term, the same deliberate gap `beacon_beam.wgsl` documents. Vanilla's
// LIGHTNING pipeline does carry `MATRICES_FOG_SNIPPET`; a bolt is meant to read
// as a bright, distance-visible flash, so an un-fogged one is the least visible
// of this pass's gaps.

struct CameraUniform {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
