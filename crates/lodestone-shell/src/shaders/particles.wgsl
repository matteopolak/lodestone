
struct Camera {
    view_proj: mat4x4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
// Two stitches, two samplers. `Instance.atlas` picks between them per
// particle: 0 = the block-model atlas the terrain samples, 1 = the stitched
// particle sheet. Binding only the first is issue #45 — flame and smoke then
// sample block texels at particle-sheet coordinates.
@group(1) @binding(0) var block_atlas: texture_2d<f32>;
@group(1) @binding(1) var block_sampler: sampler;
@group(1) @binding(2) var sheet_atlas: texture_2d<f32>;
@group(1) @binding(3) var sheet_sampler: sampler;

struct Instance {
    @location(0) centre_size: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) colour: vec4<f32>,
    // `x` = roll about the view axis; `y` = the lightmap term at this
    // particle's block position, applied in `fs_main`. Location 5 (the
    // `Layer::Translucent` flag) is in the vertex layout but deliberately not
    // declared here: nothing in either stage reads it, because it selects
    // which of the two *draws* the instance lands in rather than anything
    // about the fragment.
    @location(3) roll_light: vec4<f32>,
    @location(4) atlas: u32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) colour: vec4<f32>,
    @location(2) @interpolate(flat) atlas: u32,
    @location(3) light: f32,
};

@vertex
fn vs_main(inst: Instance, @builtin(vertex_index) vi: u32) -> VsOut {
    // Triangle-strip corner order: (-1,-1) (-1,+1) (+1,-1) (+1,+1).
    let cx = select(-1.0, 1.0, vi >= 2u);
    let cy = select(-1.0, 1.0, (vi & 1u) == 1u);

    // Roll about the view axis, matching vanilla's `Particle.roll`.
    let s = sin(inst.roll_light.x);
    let c = cos(inst.roll_light.x);
    let rx = cx * c - cy * s;
    let ry = cx * s + cy * c;

    let size = inst.centre_size.w;
    let offset = camera.right.xyz * (rx * size) + camera.up.xyz * (ry * size);
    let world = inst.centre_size.xyz + offset;

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    // The atlas V axis grows downward, so the +Y corner takes v0.
    out.uv = vec2<f32>(
        select(inst.uv.x, inst.uv.z, cx > 0.0),
        select(inst.uv.w, inst.uv.y, cy > 0.0),
    );
    out.colour = inst.colour;
    out.atlas = inst.atlas;
    out.light = inst.roll_light.y;
    return out;
}

// Byte-for-byte `model.wgsl`'s pair, duplicated because WGSL has no `#include`
// and this crate's convention is to duplicate small helpers rather than
// generate them (see `lodestone_render::light`'s "How to change it").
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
    // Both taps are issued unconditionally and one is thrown away, rather than
    // branching: `textureSample` requires uniform control flow, and `select`
    // over two already-evaluated samples has it by construction. The discarded
    // tap costs one fetch per particle fragment — particles are a handful of
    // small billboards, so this is not a measurable cost.
    let from_block = textureSample(block_atlas, block_sampler, in.uv);
    let from_sheet = textureSample(sheet_atlas, sheet_sampler, in.uv);
    let texel = select(from_block, from_sheet, in.atlas == 1u);
    let alpha = texel.a * in.colour.a;
    // Terrain fragments come from opaque sprites; discarding near-zero alpha
    // keeps a cutout parent block (leaves, grass) from throwing square debris.
    if (alpha < 0.02) {
        discard;
    }
    // One gamma round-trip for the tint and the lightmap term together, exactly
    // as `model.wgsl` does it for tint and AO*light: vanilla is not
    // colour-managed, so both multiplies belong on gamma byte values. Doing
    // them against the linear texel pulls every factor toward 1.0 — which is
    // what made every particle look permanently full-bright.
    let lit_srgb = linear_to_srgb(texel.rgb) * in.colour.rgb * in.light;
    return vec4<f32>(srgb_to_linear(lit_srgb), alpha);
}
