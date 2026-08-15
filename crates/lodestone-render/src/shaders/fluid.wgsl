
// Camera plus this frame's distance fog (see the model shader); folded into
// group 0 so the fluid shader stays within four bind groups. Shared by every
// section this frame — written once per frame, not once per section.
// `fog_ambient_light.rgb` is this frame's dimension `AMBIENT_LIGHT_COLOR` —
// see `ambient_light()` below and the model shader's matching field comment.
struct Camera {
    view_proj: mat4x4<f32>,
    fog_eye: vec4<f32>,
    fog_color_start: vec4<f32>,
    fog_end_enabled: vec4<f32>,
    fog_ambient_light: vec4<f32>,
};

// A section's world-space origin (see the model shader's `Origin`); bound at
// group 0 binding 1 with a dynamic offset. `section_origin.w` is this
// section's fade `build_time` -- see the model shader's `Origin` doc and
// `section_visibility` below. Water shares its section's own origin slot (one
// `ModelSectionGpu::origin_alloc` for opaque, water and translucent alike), so
// a section's water surface fades in on exactly the same clock as its blocks
// rather than drifting from them.
struct Origin {
    section_origin: vec4<f32>,
};

// Byte-for-byte the model shader's own constant and function -- see that
// shader's comments for the vanilla derivation and the cross-check that keeps
// the two from drifting.
const SECTION_FADE_DURATION_SECS: f32 = 0.75;

fn section_visibility(now: f32, build_time: f32) -> f32 {
    return clamp((now - build_time) / SECTION_FADE_DURATION_SECS, 0.0, 1.0);
}

// The factor the *sky* half of the lightmap is scaled by, so terrain darkens at
// night. Rides `fog_end_enabled.z`, the same spare lane the entity pass uses, so
// terrain and mobs cannot disagree about what time it is.
//
// `0.0` is the `not wired yet` sentinel and reads as full daylight: every caller
// builds this uniform from a `FogUniform` that zeroes the lane, and taking 0.0
// literally would render all sky-lit water pure black. Vanilla's real range is
// [0.24, 1.0], so 0.0 is never legitimate.
//
// Only the sky half is scaled -- see `lightmap_color` below.
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

// Vanilla's real `notGamma` -- see the model shader's `not_gamma_vec3` for the
// full derivation. Byte-for-byte the same function.
fn not_gamma_vec3(c: vec3<f32>) -> vec3<f32> {
    let max_component = max(c.r, max(c.g, c.b));
    if (max_component <= 0.0) {
        return vec3<f32>(0.0, 0.0, 0.0);
    }
    let inv = 1.0 - max_component;
    let max_scaled = 1.0 - inv * inv * inv * inv;
    return c * (max_scaled / max_component);
}

// Anti-z-fight depth nudge, in **window-depth units** (`z / w`), applied to
// every fluid fragment in `vs_main`.
//
// `bake_fluid` already insets each fluid side face 0.001 blocks off its block
// boundary, exactly where vanilla's `FluidRenderer.tesselate` does, so that a
// waterlogged block's water face on a *partially* covered side -- a stair front,
// where the stair itself fills only the bottom half -- sits behind the block's
// own coplanar face and loses the depth test cleanly. Vanilla spends that inset
// in a reversed-Z depth buffer, where relative precision barely changes with
// distance. Ours is `[0,1]` DirectX-style `Depth32Float` (see `camera.rs` and
// `DESIGN.md` §7), which spends nearly the whole float32 mantissa within a few
// blocks of the near plane. Measured through the real `Camera::view_projection`
// (`fluid_coplanar_depth_gate.rs`), 0.001 blocks is worth 210 float32 ULPs of
// depth at 2 blocks, 4 at 16, 1 at 32, **0 at 64 and -1 at 128** -- the
// separation collapses and then inverts, so beyond roughly 24 blocks the winner
// at each pixel is decided by whatever rounding the rasterizer's interpolation
// happens to produce for two differently shaped coplanar quads, and it changes
// as the camera moves. That is the z-fight.
//
// A *world-space* inset cannot fix this, because the problem is the mapping from
// world distance to depth. A constant offset in window depth can: `2^-21` is
// exactly 8 ULPs at any depth in `[0.5, 1)` and more below it, so the separation
// is bounded from below across the entire depth range, independent of distance.
// Positive pushes away from the camera under our `[0,1]` convention (the crack
// and sign-text pipelines use negative biases to pull *toward* it), which is the
// direction the inset already meant.
//
// 8 rather than 4 because the inset does not merely fade, it inverts, so the
// nudge has to pay that back before it buys any margin: at 128 blocks the inset
// contributes -1 ULP, and the residual has to still clear the ULP or two a
// rasterizer can differ by when it interpolates two coplanar quads of different
// shapes.
//
// The cost is bounded the other way, and the same gate holds it: a constant
// window-depth offset costs world distance quadratically, so it is measured
// *relative to* the surface's distance from the camera, where it is linear --
// 0.05% of the distance at 2 blocks, 0.09% at 128, 0.5% at 512, which is as far
// as a 32-chunk render distance draws. In absolute terms that is 0.001 blocks at
// arm's length and 0.12 blocks at 128. The residual worth knowing: at the very
// edge of a 32-chunk render distance the push reaches ~2.7 blocks, so water
// within about three blocks of an opaque surface behind it could lose the depth
// test there. That is a deliberate trade against a z-fight at 30-130 blocks,
// where players actually are.
//
// Keep this a power of two: the ULP guarantee above is exact only because it is.
// `model_pipeline::FLUID_DEPTH_NUDGE` restates it for the gate, and
// `fluid_depth_nudge_matches_the_shader` fails if the two drift.
const FLUID_DEPTH_NUDGE: f32 = 4.76837158203125e-7;

const BRIGHTNESS_FACTOR: f32 = 0.5;

// This frame's dimension `AMBIENT_LIGHT_COLOR`. See the model shader's
// `ambient_light()` for the derivation; the two must move together or water
// and terrain disagree about what an unlit surface looks like.
fn ambient_light() -> vec3<f32> {
    return camera.fog_ambient_light.rgb;
}

// Byte-for-byte the model shader's `lightmap_color`/`sky_light_color`/
// `parabolic_mix_factor`/`lerp_byte` -- see that shader's comments and
// `crate::light::light_color_from_levels` for the full derivation.
const BLOCK_LIGHT_TINT: vec3<f32> = vec3<f32>(1.0, 216.0 / 255.0, 140.0 / 255.0);
const BLOCK_FACTOR: f32 = 1.4;

fn lerp_byte(t: f32, byte_from: f32, byte_to: f32) -> f32 {
    return (byte_from + floor(t * (byte_to - byte_from))) / 255.0;
}

fn sky_light_color() -> vec3<f32> {
    let t = clamp((1.0 - sky_darken()) / 0.76, 0.0, 1.0);
    return vec3<f32>(lerp_byte(t, 255.0, 122.0), lerp_byte(t, 255.0, 122.0), lerp_byte(t, 255.0, 255.0));
}

fn parabolic_mix_factor(level: f32) -> f32 {
    let x = 2.0 * level - 1.0;
    return x * x;
}

fn lightmap_color(sky_level: f32, block_level: f32) -> vec3<f32> {
    let sky_brightness = light_brightness(sky_level) * sky_darken();
    let block_brightness = light_brightness(block_level) * BLOCK_FACTOR;
    let block_mix = 0.9 * parabolic_mix_factor(block_level);
    let block_light_color = mix(BLOCK_LIGHT_TINT, vec3<f32>(1.0, 1.0, 1.0), block_mix);
    var color = ambient_light()
        + sky_light_color() * sky_brightness
        + block_light_color * block_brightness;
    color = clamp(color, vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(1.0, 1.0, 1.0));
    return mix(color, not_gamma_vec3(color), BRIGHTNESS_FACTOR);
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
    @location(1) shade: vec3<f32>,
    @location(2) tinted: f32,
    @location(3) @interpolate(flat) anim_idx: u32,
    @location(4) world: vec3<f32>,
    // Real, position-resolved water colour plus override flag (`.a`), exactly
    // like the model shader's own `tint_rgb_override` — see that shader's
    // comment. The fluid pipeline has no palette bind group at all, so before
    // this a water quad's *only* colour was the hardcoded `water` constant
    // below; this is additive to it, not a replacement, and a mesh that never
    // sets it (`.a == 0`) still gets that exact constant.
    @location(5) @interpolate(flat) tint_rgb_override: vec4<u32>,
    // This section's fade-in factor -- see the model shader's matching field.
    @location(6) @interpolate(flat) visibility: f32,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) ao: f32,
    @location(3) packed: vec4<u32>,
    @location(4) tint_rgb_override: vec4<u32>,
) -> VsOut {
    let light_byte = packed.x;
    let sky = f32((light_byte >> 4u) & 15u) / 15.0;
    let block = f32(light_byte & 15u) / 15.0;
    let tint_idx = packed.y;

    let world = position + origin.section_origin.xyz;

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    // Push the fragment away from the camera by a constant amount of *window*
    // depth. Scaling by `w` is what makes it constant after the perspective
    // divide: `(z + n*w) / w == z/w + n`. See `FLUID_DEPTH_NUDGE`.
    out.clip.z = out.clip.z + FLUID_DEPTH_NUDGE * out.clip.w;
    out.uv = uv;
    out.shade = vec3<f32>(ao, ao, ao) * lightmap_color(sky, block);
    out.tinted = select(0.0, 1.0, tint_idx != 255u);
    out.anim_idx = packed.z;
    out.world = world;
    out.tint_rgb_override = tint_rgb_override;
    out.visibility = section_visibility(camera.fog_ambient_light.w, origin.section_origin.w);
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
    // Default water colour (#3F76E4), a straight sRGB byte-space constant —
    // the fallback for an untinted quad (lava) or a tinted one with no live
    // biome data. When the mesher *did* resolve a real, position-blended
    // colour (`tint_rgb_override.a != 0` — see `ModelSectionView::
    // water_tint_at`'s Rust doc), use that instead. Tint and shade both go
    // through a single gamma round-trip together (see the model shader)
    // rather than multiplying them into the linear texel directly, which is
    // the same bug fixed there, on this shader's own multiply.
    let water = vec3<f32>(0.247, 0.463, 0.894);
    var tint_col = mix(vec3<f32>(1.0, 1.0, 1.0), water, in.tinted);
    if (in.tint_rgb_override.a != 0u) {
        tint_col = vec3<f32>(in.tint_rgb_override.rgb) / 255.0;
    }
    let lit_srgb = linear_to_srgb(tex.rgb) * tint_col * in.shade;
    // The section fade-in, same mix as the model shader's — see that shader's
    // comment. Not an alpha fade: `tex.a` reaches the return untouched, so this
    // has no effect on the fluid pipeline's own alpha blend/depth state.
    let materialised_srgb = mix(linear_to_srgb(camera.fog_color_start.rgb), lit_srgb, in.visibility);
    // Fog mixes in gamma space, inside the same round-trip — see the model
    // shader for the derivation from `fog.glsl` and for the measured size of the
    // linear-space error this replaced. All three fogged shaders must agree, or
    // water and the terrain it sits in dissolve at visibly different rates.
    let amount = fog_amount(in.world - camera.fog_eye.xyz);
    let fogged_srgb = mix(materialised_srgb, linear_to_srgb(camera.fog_color_start.rgb), amount);
    return vec4<f32>(srgb_to_linear(fogged_srgb), tex.a);
}
