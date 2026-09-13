
struct Camera {
    view_proj: mat4x4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
// The one texture this pass binds: the same block atlas the terrain pass
// samples (`RenderState::new`'s `atlas` field), borrowed rather than
// re-uploaded — see `gpu/plugin_billboards.rs`'s module doc.
@group(1) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;

struct Instance {
    // World-space centre (`xyz`); `w` unused.
    @location(0) position: vec4<f32>,
    // Width/height in blocks (`xy`); `z` is `1.0` when this instance samples
    // `atlas`, `0.0` for a flat tint; `w` unused.
    @location(1) size_textured: vec4<f32>,
    // Atlas UV rect (`uv_min.xy`, `uv_max.xy`); ignored when untextured.
    @location(2) uv: vec4<f32>,
    @location(3) colour: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) colour: vec4<f32>,
    @location(2) @interpolate(flat) textured: f32,
};

@vertex
fn vs_main(inst: Instance, @builtin(vertex_index) vi: u32) -> VsOut {
    // Triangle-strip corner order: (-1,-1) (-1,+1) (+1,-1) (+1,+1) — same
    // scheme `particles.wgsl` uses, minus the roll this pass has no field
    // for: a plugin billboard is always camera-facing with no spin.
    let cx = select(-1.0, 1.0, vi >= 2u);
    let cy = select(-1.0, 1.0, (vi & 1u) == 1u);

    let half = inst.size_textured.xy * 0.5;
    let offset = camera.right.xyz * (cx * half.x) + camera.up.xyz * (cy * half.y);
    let world = inst.position.xyz + offset;

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    // The atlas V axis grows downward, so the +Y corner takes v0 — the same
    // convention `particles.wgsl` uses.
    out.uv = vec2<f32>(
        select(inst.uv.x, inst.uv.z, cx > 0.0),
        select(inst.uv.w, inst.uv.y, cy > 0.0),
    );
    out.colour = inst.colour;
    out.textured = inst.size_textured.z;
    return out;
}

// Byte-for-byte `particles.wgsl`'s pair (itself `model.wgsl`'s), duplicated
// per this crate's convention of duplicating small helpers rather than
// generating them (see `lodestone_render::light`'s "How to change it").
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
    // Always sample, then `select` away the result for an untextured
    // instance — `textureSample` requires uniform control flow, so this
    // cannot branch on `in.textured` (identical reasoning to
    // `particles.wgsl`'s two-atlas selector).
    let sampled = textureSample(atlas, atlas_sampler, in.uv);
    let base = select(vec4<f32>(1.0, 1.0, 1.0, 1.0), sampled, in.textured > 0.5);
    let alpha = base.a * in.colour.a;
    if (alpha < 0.02) {
        discard;
    }
    // Vanilla is not colour-managed: a tint multiplies the sampled texel in
    // **gamma space** (`CLAUDE.md`, `PluginBillboard::color`'s doc), so
    // recover the raw byte value, multiply, and re-linearise for a
    // possibly-sRGB render target — identical to `particles.wgsl`'s
    // treatment of `texel * tint`.
    let lit_srgb = linear_to_srgb(base.rgb) * in.colour.rgb;
    return vec4<f32>(srgb_to_linear(lit_srgb), alpha);
}
