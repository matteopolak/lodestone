
// Camera plus this frame's distance fog, folded into one group-0 uniform. Fog
// lives here (rather than in its own bind group) so the model shader stays
// within the portable `max_bind_groups` floor of 4. `fog_eye.xyz` is the camera
// world position; `fog_color_start.rgb` is the fog colour and `.w` the
// **render-distance** term's start distance (measured cylindrically, see
// `fog_amount` below); `fog_end_enabled.x` is that term's end distance and
// `.y` is 0/1 enabled. `fog_eye.w` / `fog_end_enabled.w` are vanilla's second,
// independent **environmental** term's start/end (measured spherically) —
// two lanes that were unused before issue #401 (F2/F3), so this struct did
// not grow to add those. `fog_ambient_light.rgb` is a later, genuine growth —
// see its own comment below — and does not cost a bind group: growing one
// group's uniform buffer is unrelated to the 4-bind-group floor, which limits
// how many *groups* this shader binds (still four: camera, atlas, palette,
// anim).
//
// Shared by every section drawn this frame — written once per frame, not once
// per section (see `ModelSharedCameraUniform`'s doc for the profile that made
// this a separate binding from `Origin`, below).
struct Camera {
    view_proj: mat4x4<f32>,
    fog_eye: vec4<f32>,
    fog_color_start: vec4<f32>,
    fog_end_enabled: vec4<f32>,
    // `rgb` = this frame's dimension `AMBIENT_LIGHT_COLOR` (`crate::light::
    // OVERWORLD_AMBIENT_LIGHT` when unset) — the floor `lightmap_color` seeds
    // its accumulator with before either light half is added. `w` = this
    // frame's clock, in the same seconds `Origin.section_origin.w` (below)
    // measures a section's fade `build_time` in -- see `section_visibility`
    // and `SECTION_FADE_DURATION_SECS`. Mirrors
    // `lodestone_render::fog::FogUniform::ambient_light` exactly; see that
    // field's doc for why grey-everywhere was wrong (the Nether's and End's
    // real floors are markedly *brighter* than the overworld's).
    fog_ambient_light: vec4<f32>,
};

// A section's world-space origin, bound at group 0 binding 1 with a dynamic
// offset: one physically resident buffer of these serves every section, so
// re-aiming the camera (binding 0, above) never needs to touch this one.
//
// `section_origin.w` is this section's fade `build_time`, in
// `camera.fog_ambient_light.w`'s clock -- see `section_visibility`. Written
// once, at the section's first upload, never touched again for its lifetime
// (a block-edit remesh of an already-resident section reuses the same slot
// and so the same `build_time`, which is what stops an ordinary block break
// from re-triggering the fade -- see `lodestone_render::SECTION_FADE_ALREADY_VISIBLE`
// for the sentinel that opts a section out entirely).
struct Origin {
    section_origin: vec4<f32>,
};

// Vanilla's `Options.chunkSectionFadeInTime` shipped default (`Options.java`,
// range 0.0..=2.0 seconds). This client has no video-settings UI to expose
// the option yet, so it is hardcoded exactly like `BRIGHTNESS_FACTOR` below --
// and must move with `lodestone_render::SECTION_FADE_DURATION_SECS`, the same
// constant on the Rust side (`section_fade_duration_matches_the_shader`
// checks the two do not drift).
const SECTION_FADE_DURATION_SECS: f32 = 0.75;

// Vanilla's `SectionRenderDispatcher.RenderSection.getVisibility`: 0 the
// instant a section is built, ramping linearly to 1 over
// `SECTION_FADE_DURATION_SECS`. `build_time` in the past by more than the
// duration (the `SECTION_FADE_ALREADY_VISIBLE` sentinel, or simply an old
// section) saturates to 1 immediately -- `clamp` subsumes vanilla's
// `elapsed >= fadeDuration` branch.
fn section_visibility(now: f32, build_time: f32) -> f32 {
    return clamp((now - build_time) / SECTION_FADE_DURATION_SECS, 0.0, 1.0);
}

// The factor the *sky* half of the lightmap is scaled by, so terrain darkens at
// night. Rides `fog_end_enabled.z`, the same spare lane the entity pass uses, so
// terrain and mobs cannot disagree about what time it is.
//
// `0.0` is the `not wired yet` sentinel and reads as full daylight: every caller
// builds this uniform from a `FogUniform` that zeroes the lane, and taking 0.0
// literally would render all sky-lit terrain pure black. Vanilla's real range is
// [0.24, 1.0], so 0.0 is never legitimate.
//
// Only the sky half is scaled -- see `lightmap_color` below.
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

// Vanilla's real `notGamma` (`lightmap.fsh:24-29`): scale the whole triple by
// `maxScaled / maxComponent` where `maxScaled = 1 - (1 - maxComponent)^4`.
// Guards the `0.0 / 0.0` at pure black. Mirrors `crate::light::not_gamma_vec3`
// exactly -- see that function's doc for why a grey specialisation (the
// pre-N1 `not_gamma_grey`) is only exact while every input already happens to
// be grey.
fn not_gamma_vec3(c: vec3<f32>) -> vec3<f32> {
    let max_component = max(c.r, max(c.g, c.b));
    if (max_component <= 0.0) {
        return vec3<f32>(0.0, 0.0, 0.0);
    }
    let inv = 1.0 - max_component;
    let max_scaled = 1.0 - inv * inv * inv * inv;
    return c * (max_scaled / max_component);
}

// `Options.gamma`'s default (0.5, `Options.java:900`), which `lightmap.fsh`
// consumes as `BrightnessFactor`. Hardcoded because this client has no
// brightness setting yet; 0.0 is vanilla's `Moody`, 1.0 its `Bright`.
const BRIGHTNESS_FACTOR: f32 = 0.5;

// `EnvironmentAttributes.AMBIENT_LIGHT_COLOR` for the *current* dimension --
// grey `0x0A0A0A` in the overworld, warm brown `0x302821` in the Nether, sage
// `0x3F473F` in the End. `lightmap.fsh` seeds its accumulator with it (`color =
// max(AmbientColor, nightVisionColor)`) before adding either light half, so an
// unlit surface is not pure black in vanilla. Rides `camera.fog_ambient_light`
// (see that field's comment) rather than a scalar constant now, because the
// Nether's and End's floors are not grey and are markedly *brighter* than the
// overworld's -- hardcoding the overworld's value here under-lit both of them.
fn ambient_light() -> vec3<f32> {
    return camera.fog_ambient_light.rgb;
}

// Vanilla's warm torch tint, `EnvironmentAttributes.BLOCK_LIGHT_TINT`
// (`0xFFFFD88C`), and its `BlockFactor` (`blockLightFlicker + 1.4`, flicker
// not modelled -- see `crate::light`'s doc). Mirrors `crate::light::
// BLOCK_LIGHT_TINT`/`BLOCK_FACTOR`.
const BLOCK_LIGHT_TINT: vec3<f32> = vec3<f32>(1.0, 216.0 / 255.0, 140.0 / 255.0);
const BLOCK_FACTOR: f32 = 1.4;

// Vanilla's `SKY_LIGHT_COLOR` timeline track, recovered from `sky_darken()`
// instead of the raw tick -- see `crate::light::sky_light_color_from_darken`'s
// doc for the full derivation and the JVM-oracle verification. `SKY_LIGHT_COLOR`
// and `SKY_LIGHT_FACTOR` share identical keyframe ticks with no easing, so
// `t = clamp((1 - sky_darken) / 0.76, 0, 1)` recovers the same interpolation
// parameter, and `lerp_byte` is `Mth.lerpInt`'s floor -- a `round` here is off
// by one byte on roughly half of all ticks.
fn lerp_byte(t: f32, byte_from: f32, byte_to: f32) -> f32 {
    return (byte_from + floor(t * (byte_to - byte_from))) / 255.0;
}

fn sky_light_color() -> vec3<f32> {
    let t = clamp((1.0 - sky_darken()) / 0.76, 0.0, 1.0);
    return vec3<f32>(lerp_byte(t, 255.0, 122.0), lerp_byte(t, 255.0, 122.0), lerp_byte(t, 255.0, 255.0));
}

// `lightmap.fsh:31-33`'s parabolic block-tint mix factor: `0.0` at
// `level = 0.5`, `1.0` at both ends.
fn parabolic_mix_factor(level: f32) -> f32 {
    let x = 2.0 * level - 1.0;
    return x * x;
}

// One lightmap texel, as the real three-channel value: `lightmap.fsh`'s whole
// main(). Mirrors `crate::light::light_color_from_levels` exactly -- see that
// function's doc for the full derivation, in order. The sky/block combine is
// **additive**, not `max` (the pre-N2 model's approximation): vanilla adds
// both halves, with block light amplified by `BLOCK_FACTOR` and tinted warm.
//
// Only the sky half is scaled by `sky_darken()`. Block light is a torch: it
// does not dim at dusk.
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

// Vanilla's per-pipeline alpha test threshold, as a pipeline-overridable
// constant so one shader can serve both terrain passes exactly as
// `terrain.fsh` does with `#ifdef ALPHA_CUTOUT` and
// `RenderPipeline.Builder.withShaderDefine("ALPHA_CUTOUT", ...)`.
//
// Vanilla ships three terrain pipelines and **three different answers**: the
// solid one defines no threshold at all and runs no test, the cutout one uses
// `0.5`, and the translucent one uses `0.1`. This shader used to hardcode
// `0.5` for every pass, which is right for cutout and wrong for translucent by
// a factor of five -- and the difference is not academic: 191 of the 256
// texels in the real 26.2 `white_stained_glass.png` have alpha `102/255 =
// 0.400`, so three quarters of every stained-glass face was discarded and the
// background (the sky, for a pane against it) painted instead.
//
// The default is the cutout value, because the opaque pass carries both solid
// and cutout geometry in one mesh and so must keep the stricter test. That is
// harmless for a solid sprite: `RenderLayer::from_sprite_alpha` calls a sprite
// `Solid` only when every texel's alpha is exactly 255, and `AtlasBuilder`
// re-extrudes each sprite's gutter from its own edge pixels at every mip
// level, so a solid sprite's filtered alpha cannot reach this threshold from
// any direction.
override alpha_cutout: f32 = 0.5;

@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<uniform> origin: Origin;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_smp: sampler;
@group(2) @binding(0) var<uniform> palette: Palette;
@group(3) @binding(0) var<uniform> anim: AnimSlots;

// Linear fog factor for a `dist` world units from the eye: 0 nearer than
// start, 1 beyond end, linear between, and always 0 for a degenerate range.
// Mirrors `crate::fog::fog_factor`.
fn linear_fog(dist: f32, start: f32, end: f32) -> f32 {
    if (end <= start) {
        return 0.0;
    }
    return clamp((dist - start) / (end - start), 0.0, 1.0);
}

// Vanilla's `total_fog_value` (`fog.glsl:49-53`): the `max` of two
// independent linear ramps over two different distance metrics from the
// fragment-relative vector `rel = world - eye`. Mirrors
// `crate::fog::total_fog_factor` so the headless gates describe this function
// rather than a separate model of it.
fn fog_amount(rel: vec3<f32>) -> f32 {
    let sph = length(rel);
    let cyl = max(length(rel.xz), abs(rel.y));
    let env = linear_fog(sph, camera.fog_eye.w, camera.fog_end_enabled.w);
    let rd = linear_fog(cyl, camera.fog_color_start.w, camera.fog_end_enabled.x);
    return max(env, rd) * camera.fog_end_enabled.y;
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

// Vanilla `terrain.fsh`'s `sampleNearest`, the *default* terrain sample in
// 26.2 (`TextureFilteringMethod.NONE`, `Options.java`'s shipped value, is the
// `UseRgss == 0` branch of that shader's `main`). It is not a plain
// `textureSample`, and the difference is the whole point:
//
// `uv` is converted to texel coordinates, split into the bilinear tap below
// it (`texel_center`) and the interpolation weight (`texel_offset`), and that
// weight is then rescaled about `0.5` by `pixel_size / texel_screen_size` —
// screen pixels per texel. Magnified (a texel spans many screen pixels) the
// weight is stretched and clamped, giving point sampling with a one-pixel
// ramp at each texel edge: sharp, but anti-aliased. Minified (many texels per
// screen pixel) the weight is compressed toward `0.5`, so the sampled point
// stops sliding with the sub-texel position of the fragment and locks to the
// texel lattice.
//
// That second regime is why this matters for a **cutout**. `fs_main` turns
// alpha into a visibility decision at `0.5`, so a filtered alpha that slides
// continuously with the camera makes texels cross the threshold and wink in
// and out — most visibly on a ground plate at a grazing angle, the most
// minified geometry in a scene. Locking the sample to the lattice removes the
// sliding term.
//
// The LOD is untouched: `textureSampleGrad` is handed the *original*
// derivatives, so the snap moves where we sample, never which mip level.
//
// `texel_screen_size` is a length and can be exactly zero on a degenerate
// fragment; the divide is floored so this cannot produce `inf`/`NaN` (which
// would survive the `clamp` and reach the atlas as a garbage coordinate).
// The rotated-grid tap pattern, in texels, from vanilla `terrain.fsh`'s
// `sampleRGSS`.
const RGSS_OFFSETS: array<vec2<f32>, 4> = array<vec2<f32>, 4>(
    vec2<f32>(0.125, 0.375),
    vec2<f32>(-0.125, -0.375),
    vec2<f32>(0.375, -0.125),
    vec2<f32>(-0.375, 0.125),
);

fn snap_uv(uv: vec2<f32>, pixel_size: vec2<f32>, texel_screen_size: vec2<f32>) -> vec2<f32> {
    let uv_texel_coords = uv / pixel_size;
    let texel_center = round(uv_texel_coords) - vec2<f32>(0.5, 0.5);
    var texel_offset = uv_texel_coords - texel_center;
    let scale = pixel_size / max(texel_screen_size, vec2<f32>(1.0e-12, 1.0e-12));
    texel_offset = (texel_offset - vec2<f32>(0.5, 0.5)) * scale + vec2<f32>(0.5, 0.5);
    texel_offset = clamp(texel_offset, vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0));
    return (texel_center + texel_offset) * pixel_size;
}

// Vanilla `terrain.fsh`'s `sampleRGSS` -- rotated-grid supersampling, the
// `TextureFilteringMethod.RGSS` branch, and **this shader's only sampling
// path**. Vanilla ships `NONE` (plain `sample_nearest`) as its default and
// offers this one as a video setting; we take it unconditionally because it is
// what actually fixes the reported artefact and there is no live setting to
// hang it off yet. Measured on a leaf-litter ground plate at a grazing angle,
// as the fraction of the area a 4x-supersampled render of the same camera
// says the plate should paint in the most minified band (see
// `lodestone-shell`'s `cutout_minification_flicker_pixels`):
//
//     plain textureSample  0.401     sample_nearest  0.399     sample_rgss  0.779
//
// So the vanilla-parity default would have been no improvement at all. If a
// `textureFiltering` option is ever wired, this is the function it selects and
// `sample_nearest` is the `NONE` arm; the two already compose exactly as
// vanilla composes them.
//
// Four sub-texel taps on a rotated grid,
// taken at two adjacent mip levels and blended, then cross-faded with
// `sample_nearest` over the one-to-two-texels-per-pixel transition so a
// magnified surface stays crisp.
//
// Two things do the anti-aliasing work. The taps are a 4x supersample of the
// alpha the cutout test below is about to threshold, so a texel that is only
// marginally over or under the reference contributes a fraction rather than
// flipping the whole fragment. And the level is chosen from the *geometric
// mean* of the two derivative lengths rather than the larger of them, which is
// an anisotropy-aware LOD: a surface seen at a grazing angle lands on a
// sharper level than a hardware isotropic choice gives it, so there is less
// mip-to-mip travel as the camera moves.
fn sample_rgss(
    uv: vec2<f32>,
    pixel_size: vec2<f32>,
    du: vec2<f32>,
    dv: vec2<f32>,
    texel_screen_size: vec2<f32>,
) -> vec4<f32> {
    let max_texel_size = max(texel_screen_size.x, texel_screen_size.y);
    let min_pixel_size = min(pixel_size.x, pixel_size.y);
    let blend_factor = smoothstep(min_pixel_size, min_pixel_size * 2.0, max_texel_size);
    // Magnified: the cross-fade below would weight the eight taps at zero, so
    // skip them. Legal inside a branch because every sample here states its own
    // level or gradient and so needs no implicit derivative -- `du`/`dv` were
    // taken in uniform control flow by the caller.
    if (blend_factor <= 0.0) {
        return sample_nearest(uv, pixel_size, du, dv, texel_screen_size);
    }

    let du_length = length(du);
    let dv_length = length(dv);
    let effective = sqrt(min(du_length, dv_length) * max(du_length, dv_length));
    let mip_exact = max(0.0, log2(effective / max(min_pixel_size, 1.0e-12)));
    let mip_low = floor(mip_exact);
    let mip_blend = fract(mip_exact);

    var low = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    var high = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    for (var i = 0; i < 4; i = i + 1) {
        let sample_uv = uv + RGSS_OFFSETS[i] * pixel_size;
        low = low + textureSampleLevel(atlas_tex, atlas_smp, sample_uv, mip_low);
        high = high + textureSampleLevel(atlas_tex, atlas_smp, sample_uv, mip_low + 1.0);
    }
    let rgss = mix(low * 0.25, high * 0.25, mip_blend);
    return mix(
        sample_nearest(uv, pixel_size, du, dv, texel_screen_size),
        rgss,
        blend_factor,
    );
}

fn sample_nearest(
    uv: vec2<f32>,
    pixel_size: vec2<f32>,
    du: vec2<f32>,
    dv: vec2<f32>,
    texel_screen_size: vec2<f32>,
) -> vec4<f32> {
    return textureSampleGrad(
        atlas_tex,
        atlas_smp,
        snap_uv(uv, pixel_size, texel_screen_size),
        du,
        dv,
    );
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) shade: vec3<f32>,
    @location(2) @interpolate(flat) tint_idx: u32,
    @location(3) @interpolate(flat) anim_idx: u32,
    @location(4) world: vec3<f32>,
    // A real, position-resolved biome colour (rgb) plus an override flag
    // (.a: 0 = "no override, use `palette.colors[tint_idx]`", 255 = "use
    // .rgb directly"). Additive to the four attributes above — see
    // `ModelVertex::tint_rgb_override`'s Rust doc for why a per-vertex colour
    // exists alongside the palette rather than replacing it: the palette is
    // one buffer shared by every section drawn this frame, so it cannot hold
    // a different grass green per section, but a constant/redstone tint
    // never needs to vary and is cheaper to leave in the palette.
    @location(5) @interpolate(flat) tint_rgb_override: vec4<u32>,
    // `ModelVertex::cutout_bypass`, `packed.w` -- nonzero skips the cutout
    // discard below entirely, painting the fully sampled texel (including
    // whatever colour sits under an alpha hole) solid. Vanilla's
    // `options.cutoutLeaves == false` (FAST): leaves draw through the solid
    // pass, which never runs the alpha test at all, rather than through any
    // per-material "opaque leaves" state. A new vertex attribute costs
    // nothing against the four-bind-group floor -- it is not a bind group.
    @location(6) @interpolate(flat) cutout_bypass: u32,
    // This section's fade-in factor (`section_visibility`), resolved once per
    // vertex from a per-section constant and a per-frame clock -- both
    // uniform across the draw, so `flat` costs nothing and avoids any
    // per-fragment drift.
    @location(7) @interpolate(flat) visibility: f32,
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

    let world = position + origin.section_origin.xyz;

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.uv = uv;
    out.shade = vec3<f32>(ao, ao, ao) * lightmap_color(sky, block);
    out.tint_idx = packed.y;
    out.anim_idx = packed.z;
    out.world = world;
    out.tint_rgb_override = tint_rgb_override;
    out.cutout_bypass = packed.w;
    out.visibility = section_visibility(camera.fog_ambient_light.w, origin.section_origin.w);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Derivatives and the atlas' texel size, resolved once in uniform control
    // flow so both the static and the animated path below can use them. The
    // dimensions come from the bound texture rather than a uniform, so nothing
    // has to be re-uploaded when a resource-pack reload re-stitches the atlas
    // at a different size -- this is `terrain.fsh`'s `TextureSize`.
    let pixel_size = vec2<f32>(1.0, 1.0) / vec2<f32>(textureDimensions(atlas_tex, 0));
    let du = dpdx(in.uv);
    let dv = dpdy(in.uv);
    let texel_screen_size = sqrt(du * du + dv * dv);
    // Unconditional sample keeps the mipmapped path in uniform control flow;
    // static quads (anim_idx == 0) stop here with no extra sampling. Only
    // animated quads pay for the two frame samples. Those two use an explicit
    // LOD, so they are legal inside the branch -- and they are taken at the
    // *snapped* coordinate, because the frame offsets are whole numbers of
    // texels and so preserve the lattice `sample_nearest` locked onto.
    var tex = sample_rgss(in.uv, pixel_size, du, dv, texel_screen_size);
    if (in.anim_idx != 0u) {
        let slot = anim.slots[in.anim_idx];
        let snapped = snap_uv(in.uv, pixel_size, texel_screen_size);
        let a = textureSampleLevel(atlas_tex, atlas_smp, snapped + vec2<f32>(0.0, slot.v_off_a), 0.0);
        let b = textureSampleLevel(atlas_tex, atlas_smp, snapped + vec2<f32>(0.0, slot.v_off_b), 0.0);
        tex = mix(a, b, slot.blend);
    }
    // Cutout: drop near-transparent texels (cross-plants, leaves) so they render
    // correctly on the opaque pass -- unless this quad opted out
    // (`cutout_bypass != 0`, vanilla's FAST leaves: the solid pass never runs
    // this test at all, so the sampled texel simply paints, holes included).
    //
    // The threshold is `alpha_cutout`, set per pipeline -- see its declaration
    // above. It is **not** one constant for all terrain.
    if (in.cutout_bypass == 0u && tex.a < alpha_cutout) {
        discard;
    }
    // Per-quad tint. `tint_rgb_override.a != 0` means the mesher already
    // resolved this quad's *real*, position-blended biome colour (grass,
    // foliage, dry-foliage, water) at mesh time — see `ModelVertex::
    // tint_rgb_override`'s Rust doc — so read that straight from the vertex
    // rather than the palette, which cannot vary per section. Otherwise fall
    // back to the palette slot exactly as before: it resolves a constant/
    // redstone tint to its real colour, and the untinted slot (255) leaves
    // the texel untouched. A biome-dependent quad with no live override (the
    // reserved slot's own plains default) also lands here, at its reserved
    // palette slot — the two paths agree by construction.
    var tint_col = vec3<f32>(1.0, 1.0, 1.0);
    if (in.tint_rgb_override.a != 0u) {
        tint_col = vec3<f32>(in.tint_rgb_override.rgb) / 255.0;
    } else if (in.tint_idx != 255u) {
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
    // The section fade-in: mix the lit fragment toward the fog colour by
    // `in.visibility`, so a freshly built section materialises out of the fog
    // instead of popping in solid. Byte-for-byte `terrain.fsh`'s own
    // `color = mix(FogColor * vec4(1,1,1,color.a), color, ChunkVisibility)` --
    // note it is **not** an alpha fade: only `rgb` moves, alpha (and every
    // pipeline's blend/depth state) is untouched, so this costs nothing beyond
    // the mix itself and cannot interact with translucent draw order. This
    // happens *before* the distance fog below, exactly like vanilla layers the
    // two: a section can be both mid-materialising and distance-fogged at once.
    let materialised_srgb = mix(linear_to_srgb(camera.fog_color_start.rgb), lit_srgb, in.visibility);
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
    let amount = fog_amount(in.world - camera.fog_eye.xyz);
    let fogged_srgb = mix(materialised_srgb, linear_to_srgb(camera.fog_color_start.rgb), amount);
    return vec4<f32>(srgb_to_linear(fogged_srgb), tex.a);
}
