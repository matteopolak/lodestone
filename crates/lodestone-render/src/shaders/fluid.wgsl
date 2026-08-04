
// Camera plus this frame's distance fog (see the model shader); folded into
// group 0 so the fluid shader stays within four bind groups. Shared by every
// section this frame — written once per frame, not once per section.
struct Camera {
    view_proj: mat4x4<f32>,
    fog_eye: vec4<f32>,
    fog_color_start: vec4<f32>,
    fog_end_enabled: vec4<f32>,
};

// A section's world-space origin (see the model shader's `Origin`); bound at
// group 0 binding 1 with a dynamic offset.
struct Origin {
    section_origin: vec4<f32>,
};

// The factor the *sky* half of the lightmap is scaled by, so terrain darkens at
// night. Rides `fog_end_enabled.z`, the same spare lane the entity pass uses, so
// terrain and mobs cannot disagree about what time it is.
//
// `0.0` is the `not wired yet` sentinel and reads as full daylight: every caller
// builds this uniform from a `FogUniform` that zeroes the lane, and taking 0.0
// literally would render all sky-lit water pure black. Vanilla's real range is
// [0.24, 1.0], so 0.0 is never legitimate.
//
// Only the sky half is scaled -- see `lightmap_term` below.
fn sky_darken() -> f32 {
    let raw = camera.fog_end_enabled.z;
    return select(raw, 1.0, raw <= 0.0);
}

// Vanilla's lightmap, byte-for-byte the model shader's copy -- see
// `model.wgsl`'s comments and `crate::light`'s module docs for the derivation
// from `lightmap.fsh`. Duplicated because WGSL has no include; water sharing
// terrain's exact curve is the point, since a fluid surface sits flush against
// the blocks around it and any drift reads as a seam.
fn light_brightness(level: f32) -> f32 {
    return level / (4.0 - 3.0 * level);
}

fn not_gamma_grey(c: f32) -> f32 {
    let inverted = 1.0 - c;
    return 1.0 - inverted * inverted * inverted * inverted;
}

const BRIGHTNESS_FACTOR: f32 = 0.5;

// The overworld's `AMBIENT_LIGHT_COLOR`, 0x0A0A0A. See the model shader for the
// derivation; the two must move together or water and terrain disagree about
// what an unlit surface looks like.
const AMBIENT_LIGHT: f32 = 0.039215688;

fn lightmap_term(sky_level: f32, block_level: f32) -> f32 {
    let sky = light_brightness(sky_level) * sky_darken();
    let block = light_brightness(block_level);
    let c = clamp(AMBIENT_LIGHT + max(sky, block), 0.0, 1.0);
    return mix(c, not_gamma_grey(c), BRIGHTNESS_FACTOR);
}

@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<uniform> origin: Origin;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_smp: sampler;

// Per-slot animation offsets for the current tick (see the model shader). The
// fluid pipeline has no palette, so this is group 2.
struct AnimSlot {
    v_off_a: f32,
    v_off_b: f32,
    blend: f32,
    pad: f32,
};
struct AnimSlots {
    slots: array<AnimSlot, 256>,
};
@group(2) @binding(0) var<uniform> anim: AnimSlots;

// Identical to the model shader's `linear_fog`/`fog_amount` and to
// `crate::fog::fog_factor`/`total_fog_factor`. `fog_eye.w` /
// `fog_end_enabled.w` are vanilla's environmental term's start/end (measured
// spherically); `fog_color_start.w` / `fog_end_enabled.x` are the
// render-distance term's (measured cylindrically) — see the model shader's
// `Camera` doc for the full lane layout.
fn linear_fog(dist: f32, start: f32, end: f32) -> f32 {
    if (end <= start) {
        return 0.0;
    }
    return clamp((dist - start) / (end - start), 0.0, 1.0);
}

fn fog_amount(rel: vec3<f32>) -> f32 {
    let sph = length(rel);
    let cyl = max(length(rel.xz), abs(rel.y));
    let env = linear_fog(sph, camera.fog_eye.w, camera.fog_end_enabled.w);
    let rd = linear_fog(cyl, camera.fog_color_start.w, camera.fog_end_enabled.x);
    return max(env, rd) * camera.fog_end_enabled.y;
}

// sRGB transfer functions (component-wise); see the model shader for why the
// water tint and the shade multiply both need to happen in gamma space.
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

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) shade: f32,
    @location(2) tinted: f32,
    @location(3) @interpolate(flat) anim_idx: u32,
    @location(4) world: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) ao: f32,
    @location(3) packed: vec4<u32>,
) -> VsOut {
    let light_byte = packed.x;
    let sky = f32((light_byte >> 4u) & 15u) / 15.0;
    let block = f32(light_byte & 15u) / 15.0;
    let tint_idx = packed.y;

    let world = position + origin.section_origin.xyz;

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.uv = uv;
    out.shade = ao * lightmap_term(sky, block);
    out.tinted = select(0.0, 1.0, tint_idx != 255u);
    out.anim_idx = packed.z;
    out.world = world;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var tex = textureSample(atlas_tex, atlas_smp, in.uv);
    if (in.anim_idx != 0u) {
        let slot = anim.slots[in.anim_idx];
        let a = textureSampleLevel(atlas_tex, atlas_smp, in.uv + vec2<f32>(0.0, slot.v_off_a), 0.0);
        let b = textureSampleLevel(atlas_tex, atlas_smp, in.uv + vec2<f32>(0.0, slot.v_off_b), 0.0);
        tex = mix(a, b, slot.blend);
    }
    // Default water colour (#3F76E4), a straight sRGB byte-space constant;
    // untinted quads keep their own colour. Tint and shade both go through a
    // single gamma round-trip together (see the model shader) rather than
    // multiplying them into the linear texel directly, which is the same bug
    // fixed there, on this shader's own multiply.
    let water = vec3<f32>(0.247, 0.463, 0.894);
    let tint_col = mix(vec3<f32>(1.0, 1.0, 1.0), water, in.tinted);
    let lit_srgb = linear_to_srgb(tex.rgb) * tint_col * in.shade;
    // Fog mixes in gamma space, inside the same round-trip — see the model
    // shader for the derivation from `fog.glsl` and for the measured size of the
    // linear-space error this replaced. All three fogged shaders must agree, or
    // water and the terrain it sits in dissolve at visibly different rates.
    let amount = fog_amount(in.world - camera.fog_eye.xyz);
    let fogged_srgb = mix(lit_srgb, linear_to_srgb(camera.fog_color_start.rgb), amount);
    return vec4<f32>(srgb_to_linear(fogged_srgb), tex.a);
}
