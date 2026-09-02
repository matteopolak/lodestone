// The entity ground-shadow decal, matching vanilla's own shadow feature
// pass. Every quad is built fresh
// on the CPU each frame (`lodestone_shell`'s `prepare_shadows`) with its own
// world-space corners and UV already resolved, so unlike `entity.wgsl` this
// shader carries no per-instance transform or bone logic at all — it is the
// same shape as a debug-line or outline draw: upload, bind, draw.
//
// Reuses the entity pipeline's own group-0 camera uniform and group-1
// texture+sampler layout (`EntityPipeline::camera_layout`/`texture_layout`)
// so this pass spends the same two bind groups every other entity pass does,
// nowhere near CLAUDE.md's 4-bind-group floor. Only `view_proj` is read from
// the camera uniform — the shadow render type inherits vanilla's fog snippet,
// which this port does not reproduce; see `shadow_pipeline`'s own doc for
// why that is a disclosed simplification rather than an oversight.

struct Camera {
    view_proj: mat4x4<f32>,
    section_origin: vec4<f32>,
    fog_eye: vec4<f32>,
    fog_color_start: vec4<f32>,
    fog_end_enabled: vec4<f32>,
    fog_ambient_light: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var smp: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    // Vanilla's own per-piece colour is white with only the alpha varying,
    // so this carries just the scalar rather than a full vec4.
    @location(2) alpha: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.uv = in.uv;
    out.alpha = in.alpha;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // `shadow.png` is a radial gradient sprite (opaque-ish centre fading to
    // fully transparent at the rim); the per-piece alpha further scales it,
    // exactly white-at-that-alpha multiplying the sampled texel, as vanilla's
    // own entity-shadow blend does.
    let texel = textureSample(tex, smp, in.uv);
    return vec4<f32>(texel.rgb, texel.a * in.alpha);
}
