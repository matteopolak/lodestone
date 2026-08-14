
// Camera plus this frame's distance fog, folded into one group-0 uniform — the
// same layout the model/fluid shaders use, so entities and terrain fog
// identically. `fog_eye.xyz` is the camera world position; `fog_color_start.rgb`
// is the fog colour and `.w` the **render-distance** term's start distance
// (measured cylindrically, see `fog_amount` below); `fog_end_enabled.x` is
// that term's end distance and `.y` is 0/1 enabled. `fog_end_enabled.z` is
// this frame's sky darkening — see `sky_darken()` below and
// `EntityCameraUniform::with_sky_darken`. `fog_eye.w` / `fog_end_enabled.w`
// are vanilla's second, independent **environmental** term's start/end
// (measured spherically) — two lanes unused before issue #401 (F2/F3).
// `fog_ambient_light.rgb` is this frame's dimension `AMBIENT_LIGHT_COLOR` —
// see `ambient_light()` below and the model shader's matching field comment.
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

// Identical to the model shader's `linear_fog`/`fog_amount` and to
// `crate::fog::fog_factor`/`total_fog_factor`.
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

// This frame's sky darkening: the factor vanilla's `LightTexture` scales the
// *sky* half of the lightmap by, 1.0 at noon down to 0.24 at midnight.
//
// This term is why `mobs sample world light` was not enough to darken them at
// night. The server's sky-light array does not change with the clock — measured
// live at one position, with the server clock as the control, the packed byte is
// 0xF0 and `light_term` is 1.000 at both noon *and* midnight. Vanilla darkens
// client-side only, in `LightTexture.updateLightTexture`.
//
// `0.0` is the `not wired yet` sentinel and reads as full daylight: every caller
// builds this uniform from a `FogUniform` that zeroes the lane, and taking 0.0
// literally would render every sky-lit mob pure black. Vanilla's real range is
// [0.24, 1.0], so 0.0 is never a legitimate value.
fn sky_darken() -> f32 {
    let raw = camera.fog_end_enabled.z;
    return select(raw, 1.0, raw <= 0.0);
}

// Vanilla's lightmap, byte-for-byte the model shader's copy -- see `model.wgsl`'s
// comments and `crate::light`'s module docs for the derivation from
// `lightmap.fsh`. Duplicated because WGSL has no include. Any drift between the
// two shows up as mobs that do not belong to the scene they stand in, so these
// four functions and the Rust mirror change together or not at all.
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

const BRIGHTNESS_FACTOR: f32 = 0.5;

// This frame's dimension `AMBIENT_LIGHT_COLOR`. See the model shader's
// `ambient_light()` for the derivation; the two must move together or
// entities and terrain disagree about what an unlit surface looks like.
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

// Only the *sky* half is darkened. A torch-lit mob is as bright at midnight as
// at noon, which is vanilla's behaviour: `lightmap.fsh` scales the sky
// contribution by `SkyFactor` and leaves the block contribution alone. Get this
// wrong and every lit interior goes dark at sunset. The sky/block combine is
// additive with a warm block tint, not `max` -- see the model shader's
// `lightmap_color` and `crate::light::light_color_from_levels`.
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

// sRGB transfer functions, as in the model shader: vanilla is not colour
// managed and multiplies shade into gamma byte values, so the shade multiply
// happens between these two, not in linear light.
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
    @location(1) world: vec3<f32>,
    // Flat: world light is one lightmap sample for the whole entity (vanilla's
    // granularity), so interpolating it across a mob would be meaningless.
    @location(2) @interpolate(flat) light_term: vec3<f32>,
    // Flat for the same reason: vanilla's `submitModel` colour is one value per
    // submitted model. `vec3(1)` is `NO_TINT` and is what every mob carries;
    // dyed leather armour is the only thing that sets it today.
    @location(3) @interpolate(flat) tint: vec3<f32>,
    // Flat, and boolean-shaped (0.0 or HURT_OVERLAY_ALPHA_BYTE/255): vanilla's
    // hurt/death overlay is a hard per-tick gate, not a fade — see
    // `HURT_OVERLAY_ALPHA_BYTE`'s doc.
    @location(4) @interpolate(flat) overlay: f32,
    // Flat: a creeper's white-flash overlay alpha (`OverlayTexture`'s white
    // row), 0.0 when absent. Independent of `overlay` above — vanilla's red
    // and white overlays are different rows of one lookup texture, selected
    // by `hasRedOverlay`, never blended together. See
    // `EntityInstanceRaw::white_overlay`'s doc.
    @location(5) @interpolate(flat) white_overlay: f32,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(4) m0: vec4<f32>,
    @location(5) m1: vec4<f32>,
    @location(6) m2: vec4<f32>,
    @location(7) m3: vec4<f32>,
    @location(8) light: u32,
    @location(9) tint: u32,
    @location(10) white_overlay: u32,
) -> VsOut {
    let model = mat4x4<f32>(m0, m1, m2, m3);
    let world = model * vec4<f32>(position, 1.0);
    // Byte-for-byte the model shader's light term. `sky_darken` is applied inside
    // `lightmap_color`, to the *curved* sky brightness rather than to the raw
    // level, because that is the order `lightmap.fsh` uses.
    let sky = f32((light >> 4u) & 15u) / 15.0;
    let block = f32(light & 15u) / 15.0;
    var out: VsOut;
    out.clip = camera.view_proj * world;
    out.uv = uv;
    out.world = world.xyz;
    out.light_term = lightmap_color(sky, block);
    // Unpack bits 0-23 as 0x00RRGGBB. These bytes are *gamma-space* sRGB,
    // exactly as vanilla's vertex colour is, and are multiplied inside the
    // transfer round-trip below rather than in linear light.
    out.tint = vec3<f32>(
        f32((tint >> 16u) & 255u),
        f32((tint >> 8u) & 255u),
        f32(tint & 255u),
    ) / 255.0;
    // Bits 24-31: the hurt/death overlay alpha (0 or HURT_OVERLAY_ALPHA_BYTE).
    out.overlay = f32((tint >> 24u) & 255u) / 255.0;
    // Bits 0-7 of the separate `white_overlay` attribute: a creeper's
    // white-flash alpha, 0 when absent.
    out.white_overlay = f32(white_overlay & 255u) / 255.0;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let tex_col = textureSample(tex, smp, in.uv);
    // Cutout transparent texels (e.g. between legs on a sheet with padding).
    if (tex_col.a < 0.5) {
        discard;
    }
    return shade_entity(in, tex_col);
}

// `EntityPipeline::banner_layer_pipeline`'s fragment entry point (issue #174
// step C). Vanilla's `RenderPipelines.BANNER_PATTERN` draws its mask layers
// translucent, depth-write-off, with **no alpha cutout at all** — a banner
// pattern's antialiased mask edge is meant to blend, not vanish, and `fs_main`'s
// unconditional `discard` below 0.5 would lose exactly those edge texels. This
// entry point is otherwise identical to `fs_main`: same shading, same tint, same
// fog — the pipeline (blend state, depth write) is what changes the *result*,
// this is only the one difference the pipeline cannot express by itself, since
// `build_entity_pipeline` configures pipeline state, not the shader program.
@fragment
fn fs_main_no_cutout(in: VsOut) -> @location(0) vec4<f32> {
    let tex_col = textureSample(tex, smp, in.uv);
    return shade_entity(in, tex_col);
}

// `EntityPipeline::orb_pipeline`'s fragment entry point — the experience-orb
// billboard. Two differences from `fs_main`, both read off vanilla's own
// `ExperienceOrbRenderer` rather than chosen:
//
//  * the cutout is `0.1`, not `0.5`. `RenderPipelines.ENTITY_TRANSLUCENT` (which
//    `RenderTypes.entityTranslucentCullItemTarget` builds on) declares
//    `ALPHA_CUTOUT 0.1F` — the same threshold `fs_main_flame` uses and for the
//    same reason: the orb sprite's glow has a soft low-alpha fringe that a `0.5`
//    cutout would clip into a hard-edged disc;
//  * the output alpha is halved. Vanilla's four `vertex` calls are
//    `setColor(rc, 255, bc, 128)` — a **vertex alpha of 128**, which multiplies
//    the texel's. Without it the orb draws fully opaque through the alpha-blended
//    pipeline, which is the plausible-looking wrong version: it draws, it is the
//    right colour, and it is twice as solid as vanilla's.
//
// The `rgb` half is `shade_entity`'s unchanged: the pulsing green comes in as the
// per-instance `InstanceTint`, multiplied into the gamma-encoded texel exactly
// where a dyed-leather tint is.
@fragment
fn fs_main_orb(in: VsOut) -> @location(0) vec4<f32> {
    let tex_col = textureSample(tex, smp, in.uv);
    if (tex_col.a < 0.1) {
        discard;
    }
    // `128.0 / 255.0`, vanilla's vertex alpha, written as the division so the
    // provenance survives.
    const ORB_VERTEX_ALPHA: f32 = 128.0 / 255.0;
    let shaded = shade_entity(in, tex_col);
    return vec4<f32>(shaded.rgb, shaded.a * ORB_VERTEX_ALPHA);
}

// Shared by both fragment entry points above; see `fs_main`'s own comments
// (unchanged) for the derivation of every step here.
fn shade_entity(in: VsOut, tex_col: vec4<f32>) -> vec4<f32> {
    // Reconstruct a face normal from world-position derivatives, so the mob reads
    // as 3D without a per-vertex normal.
    //
    // The **negation** is the outward (camera-facing) normal, and it is derived
    // rather than asserted. With a right-handed view matrix, NDC y points up while
    // framebuffer y points down, so for a plane facing the camera `dpdx` runs along
    // view +x and `dpdy` along view -y: `cross(+x, -y) = -z`, i.e. the raw cross
    // product points *away* from the eye. Negating it gives the normal of the side
    // being looked at, which for a closed mesh is the outward one — the same result
    // vanilla gets by computing both signs and letting `gl_FrontFacing` choose
    // (`entity.vsh`'s PER_FACE_LIGHTING pair). The pipeline is `cull_mode: None`,
    // so a lone quad is lit from whichever side is visible, again as vanilla does.
    //
    // Getting this sign backwards is invisible on `+X`/`-X` and `+Z`/`-Z` box faces
    // (the two lights are mirror images, so those pairs are equal) and shows up
    // only as an inverted up/down: a mob lit from below. It is also invisible to
    // any gate that checks a frame's *set* of shades rather than which surface got
    // which — a flip permutes the set without changing it, and a sign-flip control
    // was measured passing exactly such an assertion. What pins it is
    // `entity_diffuse_two_lights_pixels.rs`'s `modal_byte_at_edge_row`: the topmost
    // band of a box seen from above must read vanilla's `1.0`, and the bottommost
    // band seen from below must read `0.4`.
    let n = -normalize(cross(dpdx(in.world), dpdy(in.world)));
    // Vanilla's **two** diffuse lights, not one. Read from the 26.2 client jar:
    // `com.mojang.blaze3d.platform.Lighting.DIFFUSE_LIGHT_0/1` are
    // `(0.2, 1.0, -0.7)` and `(-0.2, 1.0, 0.7)` normalised, and `updateLevel`
    // installs exactly those for the world — the entry the first-person hand also
    // renders under, since `renderItemInHand` runs inside `renderLevel` and the
    // only `setupFor(ITEMS_3D)` in `GameRenderer` is afterwards, for the GUI.
    let light_0 = normalize(vec3<f32>(0.2, 1.0, -0.7));
    let light_1 = normalize(vec3<f32>(-0.2, 1.0, 0.7));
    // `assets/minecraft/shaders/include/light.glsl`:
    //
    //     lightValue = max(vec2(0.0), light);
    //     min(1.0, (lightValue.x + lightValue.y) * 0.6 + 0.4)
    //
    // with MINECRAFT_LIGHT_POWER 0.6 and MINECRAFT_AMBIENT_LIGHT 0.4. The two
    // `max`es are the whole difference from what this shader used to do, which was
    // one light and an `abs()`: that lights a face pointing *away* from the light
    // exactly as brightly as one pointing into it (up and down both 0.9085, where
    // vanilla is 1.0 and 0.4), and drops to the 0.4 floor on every normal
    // *perpendicular* to the single direction. Axis-aligned box faces never land
    // on that band, which is why standing mobs looked passable; the first-person
    // arm is rotated and sat at 0.497 over 97% of its pixels, reported as issue
    // #383's dark side. Two near-opposing lights have no perpendicular band at
    // all — their dark region is the underside, which is what a shaded model
    // should have.
    let d0 = max(dot(n, light_0), 0.0);
    let d1 = max(dot(n, light_1), 0.0);
    let diffuse = min(1.0, (d0 + d1) * 0.6 + 0.4);
    // Direction, world light and the per-instance tint are one shade, multiplied
    // in gamma space through a single transfer round-trip (one round-trip, not
    // one per factor, so there is less rounding) — exactly the model shader's
    // treatment of `ao * light`.
    //
    // The tint belongs in here and not outside: vanilla's dye colour is a vertex
    // colour multiplied into the gamma-encoded texel byte, and doing it in
    // linear light would pull it toward white. Leather's base sheet is
    // near-greyscale, so it is the whole visible colour of the piece.
    let shaded = linear_to_srgb(tex_col.rgb) * in.tint * diffuse * in.light_term;
    // Vanilla's hurt/death overlay (`OverlayTexture`, sampled per
    // `LivingEntityRenderer.java:281`'s `hasRedOverlay`) is a flat-red **blend**
    // at a fixed alpha, not a multiply — multiplying by red would crush the mob
    // toward black instead of washing it red. Blended in the same gamma-space
    // stage as the tint/shade multiply above, per this shader's convention that
    // colour math happens in gamma bytes, not linear light.
    // The overlay strength was inverted (issue #371). Vanilla's `entity.fsh:57`:
    //
    //     color.rgb = mix(overlayColor.rgb, color.rgb, overlayColor.a);
    //
    // The alpha weights **the entity's own colour**, not the red. At vanilla's
    // hurt alpha of `178/255 = 0.698` that is `0.698*colour + 0.302*red` — about
    // 30% red. We had `mix(shaded, red, 0.698)`, i.e. 70% red, with the mob's own
    // colour the minority contributor. That is what shipped, and what a player
    // reported as far too red.
    //
    // Simply swapping the two arguments is wrong, and fails in the loudest
    // possible way: `mix(a, b, t)` is `a` at `t = 0`, and our `overlay` is **0**
    // for an unharmed entity, so it would paint every mob — and the first-person
    // arm — solid red. Vanilla has no such case because its no-overlay state is
    // alpha *near 1*: `OverlayTexture`'s `y >= 8` rows are white at high alpha,
    // and `mix(white, colour, ~1)` is a no-op. Our sentinel is the opposite
    // polarity, so the blend has to be written against ours rather than
    // transliterated from vanilla's.
    //
    // Hence: `overlay` stays vanilla's alpha with 0 meaning absent, and the red
    // weight is its complement, taken only when the overlay is actually present.
    let red_weight = select(0.0, 1.0 - in.overlay, in.overlay > 0.0);
    var overlaid = mix(shaded, vec3<f32>(1.0, 0.0, 0.0), red_weight);
    // A creeper's white-flash overlay (`OverlayTexture`'s white row,
    // `CreeperRenderer.getWhiteOverlayProgress`). Unlike the red overlay this
    // is a genuine `mix(white, colour, alpha)` with no polarity inversion to
    // account for: our sentinel (`white_overlay == 0` means "absent") already
    // agrees with vanilla's own no-overlay edge (`alpha == 1.0` at `progress
    // == 0`, where `mix(white, colour, 1.0)` is a no-op) on what "no effect"
    // looks like, so the blend is written exactly as vanilla's `entity.fsh`
    // has it, only gated on the sentinel rather than always applied.
    //
    // **Only applied when the red overlay is absent** — vanilla's
    // `OverlayTexture` puts red and white on different rows of one lookup
    // (`v == 3` for hurt is a flat red regardless of `u`), so a creeper that
    // is somehow both hurt and swelling in the same frame shows red, never a
    // blend of the two. `red_weight > 0.0` is exactly "the red overlay is
    // active", the same condition `in.overlay > 0.0` gates above.
    if (red_weight <= 0.0 && in.white_overlay > 0.0) {
        overlaid = mix(vec3<f32>(1.0, 1.0, 1.0), overlaid, in.white_overlay);
    }
    // Fade toward the fog colour by view distance, on the same curve as terrain,
    // so a mob at the render-distance edge or under water dissolves with the
    // blocks around it instead of hanging in front of them.
    //
    // In **gamma** space, folded into this shader's existing round-trip, per
    // vanilla's `apply_fog` — see the model shader for the derivation and the
    // measured size of the linear-space error this replaced. Terrain, water and
    // entities must all mix in the same space or a mob fogs at a different rate
    // from the block it is standing on.
    let amount = fog_amount(in.world - camera.fog_eye.xyz);
    let fogged_srgb = mix(overlaid, linear_to_srgb(camera.fog_color_start.rgb), amount);
    return vec4<f32>(srgb_to_linear(fogged_srgb), tex_col.a);
}

// ---------------------------------------------------------------------------
// Mob fire (issue #434 — player report: "mobs dont show flames yet")
//
// `EntityPipeline::flame_pipeline`'s vertex/fragment entry points. Distinct
// from `vs_main`/`fs_main` because the flame instance format
// (`FlameInstanceRaw`, entity_pipeline.rs) has no light/tint/overlay word at
// all — just a matrix and the current animation frame — so this pass needs
// its own, narrower `@location` set rather than reusing `VsOut`'s.
// ---------------------------------------------------------------------------

struct FlameVsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) world: vec3<f32>,
};

@vertex
fn vs_main_flame(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(4) m0: vec4<f32>,
    @location(5) m1: vec4<f32>,
    @location(6) m2: vec4<f32>,
    @location(7) m3: vec4<f32>,
    @location(8) frame: u32,
) -> FlameVsOut {
    let model = mat4x4<f32>(m0, m1, m2, m3);
    let world = model * vec4<f32>(position, 1.0);
    var out: FlameVsOut;
    out.clip = camera.view_proj * world;
    out.world = world.xyz;
    // `uv.x` is already the complete, final U into the combined 32-wide flame
    // texture (see `FlameVertex::uv`'s doc in entity_pipeline.rs) — carried
    // through unchanged. `uv.y` is only the *local* top/bottom of whichever
    // frame cell is current (0.0 or 1.0); this is where it is combined with
    // the per-instance `frame` into the real V, exactly the contract that
    // doc promises.
    const FLAME_FRAME_COUNT: f32 = 32.0;
    out.uv = vec2<f32>(uv.x, (f32(frame) + uv.y) / FLAME_FRAME_COUNT);
    return out;
}

@fragment
fn fs_main_flame(in: FlameVsOut) -> @location(0) vec4<f32> {
    let tex_col = textureSample(tex, smp, in.uv);
    // Vanilla's `ENTITY_CUTOUT_CULL` pipeline is `ALPHA_CUTOUT` at `0.1`
    // (`RenderPipelines.java:238-243`), not the `0.5` `fs_main` uses — a
    // lower threshold keeps more of a flame sprite's soft, low-alpha fringe
    // than the mob-body cutout would.
    if (tex_col.a < 0.1) {
        discard;
    }
    // Vanilla forces the flame's light coords to full block-light
    // (`LightCoordsUtil.withBlock(state.lightCoords, 15)`,
    // `FlameFeatureRenderer.java:42`) and submits a flat white vertex colour
    // (`fireVertex`'s `setColor(-1)`, `:71`) with no per-face lighting define
    // on `ENTITY_CUTOUT_CULL` — fire reads as self-lit, not shaded by the
    // scene the way a mob's body is. This entry point therefore skips
    // `shade_entity`'s two-light diffuse and world-light dimming entirely
    // (there is no per-instance light byte to look one up from — see
    // `FlameInstanceRaw`'s doc) and only applies distance fog, so a burning
    // mob at the render-distance edge still dissolves with the terrain around
    // it instead of hanging in front of it as a flat-lit cutout.
    let amount = fog_amount(in.world - camera.fog_eye.xyz);
    let fogged_srgb = mix(linear_to_srgb(tex_col.rgb), linear_to_srgb(camera.fog_color_start.rgb), amount);
    return vec4<f32>(srgb_to_linear(fogged_srgb), tex_col.a);
}
