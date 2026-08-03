
// Camera plus this frame's distance fog, folded into one group-0 uniform. Fog
// lives here (rather than in its own bind group) so the model shader stays
// within the portable `max_bind_groups` floor of 4. `fog_eye.xyz` is the camera
// world position; `fog_color_start.rgb` is the fog colour and `.w` the distance
// where fog begins; `fog_end_enabled.x` is where fog is full and `.y` is 0/1.
//
// Shared by every section drawn this frame — written once per frame, not once
// per section (see `ModelSharedCameraUniform`'s doc for the profile that made
// this a separate binding from `Origin`, below).
struct Camera {
    view_proj: mat4x4<f32>,
    fog_eye: vec4<f32>,
    fog_color_start: vec4<f32>,
    fog_end_enabled: vec4<f32>,
};

// A section's world-space origin, bound at group 0 binding 1 with a dynamic
// offset: one physically resident buffer of these serves every section, so
// re-aiming the camera (binding 0, above) never needs to touch this one.
struct Origin {
    section_origin: vec4<f32>,
};

// The factor the *sky* half of the lightmap is scaled by, so terrain darkens at
// night. Rides `fog_end_enabled.z`, the same spare lane the entity pass uses, so
// terrain and mobs cannot disagree about what time it is.
//
// `0.0` is the `not wired yet` sentinel and reads as full daylight: every caller
// builds this uniform from a `FogUniform` that zeroes the lane, and taking 0.0
// literally would render all sky-lit terrain pure black. Vanilla's real range is
// [0.24, 1.0], so 0.0 is never legitimate.
//
// Only the sky half is scaled -- see `lightmap_term` below.
fn sky_darken() -> f32 {
    let raw = camera.fog_end_enabled.z;
    return select(raw, 1.0, raw <= 0.0);
}

// Vanilla's lightmap, one axis at a time. Verbatim from `lightmap.fsh` in the
// real 26.2 client.jar; mirrored in Rust by `crate::light`, whose module docs
// carry the derivation, the two vanilla terms deliberately left out, and the
// measured divergence from issue #386's table. `entity.wgsl` and `fluid.wgsl`
// hold the same three functions -- WGSL has no include, so change all four
// together.
//
//     float get_brightness(float level) { return level / (4.0 - 3.0 * level); }
//
// `level` is the raw nibble over 15. Strongly concave: half light is a fifth of
// the brightness, which is what the retired `0.2 + 0.8 * l` ramp got wrong --
// not at either endpoint, where the two agree exactly, but everywhere between.
fn light_brightness(level: f32) -> f32 {
    return level / (4.0 - 3.0 * level);
}

// Vanilla's `notGamma`, specialised to a grey value. `lightmap.fsh` scales an
// RGB triple by `maxScaled / maxComponent`; with all three components equal that
// collapses to `1 - (1 - c)^4`, with no division (so `c == 0` is 0 rather than
// vanilla's 0/0). Grey is right while the overworld's `SKY_LIGHT_COLOR` is white
// and its `AMBIENT_LIGHT_COLOR` black.
fn not_gamma_grey(c: f32) -> f32 {
    let inverted = 1.0 - c;
    return 1.0 - inverted * inverted * inverted * inverted;
}

// `Options.gamma`'s default (0.5, `Options.java:900`), which `lightmap.fsh`
// consumes as `BrightnessFactor`. Hardcoded because this client has no
// brightness setting yet; 0.0 is vanilla's `Moody`, 1.0 its `Bright`.
const BRIGHTNESS_FACTOR: f32 = 0.5;

// `EnvironmentAttributes.AMBIENT_LIGHT_COLOR` for the overworld, which
// `DimensionTypes.java:36` sets to -16119286 == 0x0A0A0A -- grey 10/255, *not*
// black. `lightmap.fsh` seeds its accumulator with it (`color =
// max(AmbientColor, nightVisionColor)`) before adding either light half, so an
// unlit surface is not pure black in vanilla: it reads 0.0935 after the
// `not_gamma` mix. Grey, so it stays a scalar here; the Nether's 0x302821 and
// the End's 0x3F473F are not, and are part of the per-dimension colour pass.
const AMBIENT_LIGHT: f32 = 0.039215688;

// One lightmap texel, as a scalar: `lightmap.fsh`'s whole main(), minus the
// ambient colour (black in the overworld) and with `max` still standing in for
// vanilla's additive sky+block combine (#383's third divergence -- doing it
// faithfully needs a vec3, because `BLOCK_LIGHT_TINT` is not white).
//
// The curve is applied to the raw *level* and `sky_darken` multiplies the
// result, which is the order `lightmap.fsh` uses:
//
//     float sky_brightness = get_brightness(sky_level) * lightmapInfo.SkyFactor;
//
// Only the sky half is scaled. Block light is a torch: it does not dim at dusk.
fn lightmap_term(sky_level: f32, block_level: f32) -> f32 {
    let sky = light_brightness(sky_level) * sky_darken();
    let block = light_brightness(block_level);
    let c = clamp(AMBIENT_LIGHT + max(sky, block), 0.0, 1.0);
    return mix(c, not_gamma_grey(c), BRIGHTNESS_FACTOR);
}

// The default (plains) tint palette. A quad's tint byte indexes this; slot 255
// is white (untinted). Replaces the single hardcoded green so grass, foliage and
// every other tinted source render their own colour.
struct Palette {
    colors: array<vec4<f32>, 256>,
};

// Per-slot animation offsets for the current tick. A quad's `anim` byte indexes
// this; slot 0 is the static sentinel (all zero). `v_off_a`/`v_off_b` are the V
// offsets (in normalised atlas units) of the two frames straddling the tick, and
// `blend` is the interpolation weight between them.
struct AnimSlot {
    v_off_a: f32,
    v_off_b: f32,
    blend: f32,
    pad: f32,
};
struct AnimSlots {
    slots: array<AnimSlot, 256>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<uniform> origin: Origin;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_smp: sampler;
@group(2) @binding(0) var<uniform> palette: Palette;
@group(3) @binding(0) var<uniform> anim: AnimSlots;

// Linear fog factor for a fragment `dist` world units from the eye: 0 nearer than
// start, 1 beyond end, linear between, and always 0 when disabled. Mirrors
// `crate::fog::fog_factor` so the headless tests describe the shader's behaviour.
fn fog_amount(dist: f32) -> f32 {
    let start = camera.fog_color_start.w;
    let end = camera.fog_end_enabled.x;
    let enabled = camera.fog_end_enabled.y;
    if (end <= start) {
        return 0.0;
    }
    return clamp((dist - start) / (end - start), 0.0, 1.0) * enabled;
}

// sRGB transfer functions (component-wise). The atlas is an _srgb texture, so
// `textureSample` returns linear-light texels; the tint palette holds straight
// sRGB bytes. Multiplying a linear texel by an sRGB tint and then re-encoding on
// the sRGB surface gamma-compresses the tint's green/red ratio (grass 1.30 ->
// ~1.13, measurably greyer than vanilla). Vanilla applies the biome tint in
// gamma space, so we convert the texel to sRGB, tint there, then convert back.
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
    @location(2) @interpolate(flat) tint_idx: u32,
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

    let world = position + origin.section_origin.xyz;

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.uv = uv;
    out.shade = ao * lightmap_term(sky, block);
    out.tint_idx = packed.y;
    out.anim_idx = packed.z;
    out.world = world;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Unconditional sample keeps the plain (mipmapped) path in uniform control
    // flow; static quads (anim_idx == 0) stop here with no extra sampling. Only
    // animated quads pay for the two frame samples, and they use an explicit LOD
    // (no derivatives) so the branch is legal.
    var tex = textureSample(atlas_tex, atlas_smp, in.uv);
    if (in.anim_idx != 0u) {
        let slot = anim.slots[in.anim_idx];
        let a = textureSampleLevel(atlas_tex, atlas_smp, in.uv + vec2<f32>(0.0, slot.v_off_a), 0.0);
        let b = textureSampleLevel(atlas_tex, atlas_smp, in.uv + vec2<f32>(0.0, slot.v_off_b), 0.0);
        tex = mix(a, b, slot.blend);
    }
    // Cutout: drop near-transparent texels (cross-plants, leaves) so they render
    // correctly on the opaque pass.
    if (tex.a < 0.5) {
        discard;
    }
    // Per-quad tint: the palette slot resolves grass/foliage/etc. to their real
    // default colour; the untinted slot (255) leaves the texel untouched.
    var tint_col = vec3<f32>(1.0, 1.0, 1.0);
    if (in.tint_idx != 255u) {
        tint_col = palette.colors[in.tint_idx].rgb;
    }
    // Both the tint and the shade (AO * light) are vanilla, non-colour-managed
    // multiplies: vanilla applies them to gamma byte values, not linear light.
    // Doing them in linear space and re-encoding pulls every factor toward
    // 1.0 (a shade of 0.6 reads as 0.79 once re-encoded) — the washed-out
    // look. So both go through one gamma round-trip together: convert the
    // linear texel to sRGB, multiply tint and shade there, convert back. A
    // single round-trip (rather than one per multiply) means fewer transfer
    // applications and less rounding.
    let lit_srgb = linear_to_srgb(tex.rgb) * tint_col * in.shade;
    // Fade the lit fragment toward the fog colour by its view distance, so the
    // outermost loaded chunks dissolve into the sky rather than ending in a wall.
    //
    // The mix is in **gamma** space, inside the same round-trip as the tint and
    // shade above, because vanilla's is: `fog.glsl`'s `apply_fog` does
    // `mix(inColor.rgb, fogColor.rgb, fogValue)` on `terrain.fsh`'s
    // `texture * vertexColor`, which are raw non-colour-managed bytes, and
    // `FogColor` is `ARGB.vector4fFromARGB32(...)`, i.e. bytes over 255.
    //
    // This used to mix in linear light, and it is exactly the failure the
    // comment above warns about — one line later, on the same value. It is a
    // *magnitude* bug, so nothing that asserted "distant things are foggier"
    // could see it: linear mixing pulls the result toward the brighter colour,
    // and the error is largest where the factor is *smallest*. For a grey-0.3
    // fragment against a 0.75 fog, a true factor of 0.25 rendered as an apparent
    // 0.373 and 0.5 as 0.627 — roughly 50% and 25% too much haze, worst right at
    // the ramp's onset, which is the reported "too foggy too early".
    //
    // `linear_to_srgb(camera.fog_color_start.rgb)` is three `pow`s per fragment
    // on a value that is uniform across the whole draw. Storing the fog colour
    // gamma-encoded in `FogUniform` would remove them; it also changes what
    // `FogUniform::color_start` *means* for every reader, so it is deliberately
    // not done here. See `docs/fog.md`.
    let amount = fog_amount(length(in.world - camera.fog_eye.xyz));
    let fogged_srgb = mix(lit_srgb, linear_to_srgb(camera.fog_color_start.rgb), amount);
    return vec4<f32>(srgb_to_linear(fogged_srgb), tex.a);
}
