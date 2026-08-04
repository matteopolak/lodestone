//! The two full-screen overlays vanilla draws in `ScreenEffectRenderer.submit`
//! (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/ScreenEffectRenderer.java`):
//! the underwater tint + scrolling texture, and the looping fire overlay.
//! Issues #108 and #112.
//!
//! # One pass, two textures
//!
//! Both are a textured, alpha-blended, depth-less quad drawn late in the
//! frame (after the world and the first-person hand, before the HUD) — see
//! `GameRenderer.java:568-577`, which calls `screenEffectRenderer.submit`
//! immediately after `renderItemInHand` and before `featureRenderDispatcher`.
//! Vanilla's two pipelines (`BLOCK_SCREEN_EFFECT`, `FIRE_SCREEN_EFFECT`) are
//! textually identical builds of the same `GUI_TEXTURED_SNIPPET` base
//! (`RenderPipelines.java:713-718`) — position+uv+colour, `TRANSLUCENT` blend,
//! no depth attachment — so one pipeline here draws both, parameterised only
//! by which texture bind group is active.
//!
//! # Bind groups: one, not four
//!
//! Same constraint as the sky pass (`sky_pipeline.rs`'s module doc): the model
//! shader is already at wgpu's 4-bind-group floor. This pipeline uses exactly
//! **one** bind group (a texture + sampler; there is no camera matrix at all,
//! see below), so it can never be the thing that pushes an adapter over the
//! floor.
//!
//! # Screen-space, not world-space
//!
//! Vanilla submits both quads through a small local `PoseStack` under a
//! perspective `hud3dProjection`, at a fixed depth (`z = -0.5`) with a size
//! chosen so it fills the frame regardless of FOV. Reproducing that exact
//! perspective would buy nothing here — the quads have no other 3-D content to
//! interact with — so this pass places them directly in NDC (`x, y` in
//! `-1.0..1.0`, no camera uniform, no projection). The underwater quad still
//! fills the screen either way.
//!
//! **The fire overlay is vanilla's real two-quad geometry, flattened to NDC —
//! not a tiled strip (issue #420, fixing a regression from #112's original
//! placement).** `ScreenEffectRenderer.submitFire`/`buildFireQuad`
//! (`ScreenEffectRenderer.java:168-184`) draws **exactly two** 1×1 unit quads
//! at local `z = -0.5`, each `translate(±0.24, -0.3, 0.0)` then
//! `rotateY(∓π/18)` (10°) — never a repeated/tiled sprite. A previous pass
//! here deliberately tiled four mirrored copies across a bottom strip instead
//! (both this doc and `docs/screen-overlays.md` said so outright, citing the
//! same constants and choosing to tile anyway to match #112's wording), which
//! is what the repo owner saw as "the fire texture repeated multiple times"
//! instead of vanilla's one large licking flame. [`fire_overlay_triangles`]
//! now reproduces the real transform — `rotateY` then `translate`, exactly
//! vanilla's pose-stack order — and only drops the `z` component afterwards
//! (an orthographic flatten, not vanilla's perspective projection, for the
//! same "no camera uniform" reason underwater/pumpkin/etc. stay in flat NDC),
//! then scales the pair uniformly so their combined horizontal extent exactly
//! fills NDC width — the one property the old tiled strip had
//! (`fire_quads_span_the_full_ndc_width_with_no_gaps`) that this fix
//! preserves, now met by two *overlapping* quads instead of four disjoint
//! tiles. UV is vanilla's own "mirrored corner mapping": `buildSpriteQuad`
//! passes `buildQuad` the sprite's `(u1, v1, u0, v0)` — swapped, not
//! identity — so local `x = -0.5` samples the frame's far U edge and
//! `x = +0.5` samples its near edge, identically for both quads (there is no
//! per-quad direction parameter in `buildFireQuad`). The **texture, its
//! 32-frame animation, the tint maths and the alpha blend** were already
//! real and are unchanged by this fix.
//!
//! # Underwater: a tint, not a second fog
//!
//! `submitWater` multiplies the `underwater.png` texel by a **grayscale**
//! colour (`ARGB.colorFromFloat(0.1F, brightness, brightness, brightness)`,
//! `ScreenEffectRenderer.java:159`) at alpha `0.1` — not blue; whatever blue
//! cast the overlay has comes entirely from the texture's own pixels. This is
//! wholly independent of the dimension fog this codebase already models
//! (`crate::fog`): fog fades *world geometry* into a colour as it recedes,
//! while this is a flat, non-fading screen-space layer with its own texture,
//! composited after the world and the hand are already drawn. Vanilla runs
//! both at once when submerged; nothing here changes `fog.rs`.
//!
//! # The shader is a file, not a string literal
//!
//! `src/shaders/overlay.wgsl`, pulled in with `include_str!`, like every other
//! shader in this crate. It used to be a Rust raw string, where one double
//! quote inside a WGSL comment ended the literal and rustc parsed the prose
//! after it as code. See `docs/shaders.md`.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use lodestone_assets::{
    ResourceManager, ScreenEffectAssetError, fire_frame_count, load_fire_texture,
    load_freeze_overlay_texture, load_nausea_overlay_texture, load_portal_overlay_texture,
    load_pumpkin_overlay_texture, load_spyglass_scope_texture, load_underwater_texture,
};

// ---------------------------------------------------------------------------
// Pure geometry (no GPU handles) — testable with no device.
// ---------------------------------------------------------------------------

/// One overlay vertex: NDC position, texture UV, and a baked RGBA tint
/// (multiplied onto the sampled texel — see the module doc's gamma note in
/// [`OVERLAY_WGSL`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct ScreenOverlayVertex {
    /// NDC position (`x, y` in `-1.0..1.0`).
    pub position: [f32; 2],
    /// Texture UV.
    pub uv: [f32; 2],
    /// Baked RGBA tint, straight (non-premultiplied) alpha.
    pub color: [f32; 4],
}

fn vertex(position: [f32; 2], uv: [f32; 2], color: [f32; 4]) -> ScreenOverlayVertex {
    ScreenOverlayVertex { position, uv, color }
}

/// The underwater overlay's per-fragment tint alpha — vanilla's constant
/// `0.1F` in `ScreenEffectRenderer.submitWater`.
pub const UNDERWATER_TINT_ALPHA: f32 = 0.1;

/// How many times the underwater texture tiles across the quad — vanilla's
/// constant `4.0F` (`uvSize` in `submitWater`).
pub const UNDERWATER_TILE_COUNT: f32 = 4.0;

/// The lightmap value the underwater overlay is tinted by — vanilla's own
/// curve, via [`crate::light::light_term`], so this pass and the terrain behind
/// it cannot disagree about how bright the water is.
///
/// This used to be a local `0.2 + 0.8 * max(sky, block)` approximation with the
/// comment "`Lightmap.getBrightness` is a per-dimension gamma-corrected curve
/// table this codebase has not ported". It is ported now (see
/// [`crate::light`]'s module docs), so the approximation is gone; note the
/// consequence that a fully dark cell now tints **black** rather than at the
/// retired `0.2` floor, which is what vanilla does.
///
/// `sky_darken` is passed as `1.0`: this pass has no clock, and the overlay is
/// drawn over an already-darkened scene. Threading the real factor in is the
/// remaining gap, and it is the same one `ScreenEffectRenderer` has for fog.
///
/// `packed` is `sky << 4 | block`, the same encoding
/// [`crate::entity`]'s light sampling already uses.
#[must_use]
pub fn underwater_brightness(packed_light: u8) -> f32 {
    crate::light::light_term(packed_light, 1.0)
}

/// Builds the underwater overlay's one NDC quad. `yaw_degrees`/`pitch_degrees`
/// are the camera's look direction (matching vanilla's `getYRot()`/`getXRot()`
/// convention: yaw about `+Y`, `0` facing `+Z`); the UV scroll formula and
/// vertex/UV pairing are transcribed unchanged from
/// `ScreenEffectRenderer.submitWater`/`buildQuad`.
#[must_use]
pub fn underwater_overlay_quad(
    yaw_degrees: f32,
    pitch_degrees: f32,
    packed_light: u8,
) -> [ScreenOverlayVertex; 4] {
    let brightness = underwater_brightness(packed_light);
    let color = [brightness, brightness, brightness, UNDERWATER_TINT_ALPHA];
    let u0 = -yaw_degrees / 64.0;
    let v0 = pitch_degrees / 64.0;
    let (u1, v1) = (u0 + UNDERWATER_TILE_COUNT, v0 + UNDERWATER_TILE_COUNT);
    // `buildQuad(x0,y0,x1,y1, u0=u1,v0=v1, u1=u0,v1=v0)` — vanilla passes the
    // *far* UV corner as its own `u0`/`v0` parameter; transcribed literally
    // rather than renamed, so this stays checkable against the source line.
    [
        vertex([-1.0, -1.0], [u1, v1], color),
        vertex([1.0, -1.0], [u0, v1], color),
        vertex([1.0, 1.0], [u0, v0], color),
        vertex([-1.0, 1.0], [u1, v0], color),
    ]
}

/// The underwater quad as two CCW triangles (`0,1,2` / `2,3,0`, matching
/// [`crate::sky::quad_indices`]), for a caller building a plain (non-indexed)
/// vertex buffer.
#[must_use]
pub fn underwater_overlay_triangles(
    yaw_degrees: f32,
    pitch_degrees: f32,
    packed_light: u8,
) -> [ScreenOverlayVertex; 6] {
    let q = underwater_overlay_quad(yaw_degrees, pitch_degrees, packed_light);
    [q[0], q[1], q[2], q[2], q[3], q[0]]
}

/// The fire overlay's translucency — vanilla's vertex colour constant
/// `-436207617` (`ARGB` `(229, 255, 255, 255)`, i.e. white at alpha
/// `229/255`) in `ScreenEffectRenderer.submitFire`/`buildFireQuad`.
pub const FIRE_TINT: [f32; 4] = [1.0, 1.0, 1.0, 229.0 / 255.0];

/// Vanilla's real fire-overlay transform constants
/// (`ScreenEffectRenderer.submitFire`, `ScreenEffectRenderer.java:168-180`):
/// `poseStack.translate(±FIRE_QUAD_OFFSET_X, FIRE_QUAD_OFFSET_Y, 0.0F);
/// poseStack.rotateY(∓FIRE_QUAD_TILT_RADIANS)` — one call of each sign, one
/// quad per call. Transcribed literally rather than simplified.
pub const FIRE_QUAD_OFFSET_X: f32 = 0.24;
/// See [`FIRE_QUAD_OFFSET_X`].
pub const FIRE_QUAD_OFFSET_Y: f32 = -0.3;
/// See [`FIRE_QUAD_OFFSET_X`] — vanilla's `Math.PI / 18`, i.e. 10 degrees.
pub const FIRE_QUAD_TILT_RADIANS: f32 = std::f32::consts::PI / 18.0;

/// `buildSpriteQuad`'s own local unit-square half-extent and fixed `z`
/// (`ScreenEffectRenderer.java:184`: `buildSpriteQuad(..., -0.5F, -0.5F,
/// 0.5F, 0.5F, -0.5F, ...)`).
const FIRE_QUAD_HALF_EXTENT: f32 = 0.5;
const FIRE_QUAD_LOCAL_Z: f32 = -0.5;

/// The uniform scale [`fire_quad`] applies after flattening `z`, chosen so
/// the two quads' combined horizontal extent exactly fills NDC width —
/// derived from [`FIRE_QUAD_OFFSET_X`]/[`FIRE_QUAD_TILT_RADIANS`], not
/// tuned. Each quad's outer edge (the one facing away from screen centre)
/// lands at local `x = ±(FIRE_QUAD_OFFSET_X + 0.5·sin(tilt) + 0.5·cos(tilt))`
/// before scaling (see [`fire_quad`]'s derivation in its own doc); scaling by
/// the reciprocal of that magnitude puts both outer edges exactly on
/// `x = ±1.0`. This is the one property the old four-tile strip had
/// (`fire_quads_span_the_full_ndc_width_with_no_gaps`) that this fix
/// preserves, now via two *overlapping* quads instead of four disjoint ones.
#[must_use]
pub fn fire_quad_scale() -> f32 {
    let (s, c) = FIRE_QUAD_TILT_RADIANS.sin_cos();
    1.0 / (FIRE_QUAD_OFFSET_X + 0.5 * s + 0.5 * c)
}

/// The fire overlay's exact NDC vertical extent (`y_min`, `y_max`), derived
/// from the same constants [`fire_overlay_triangles`] uses. `rotateY` never
/// touches `y`, so unlike the X extent (deliberately normalised to fill
/// `[-1, 1]`, see [`fire_quad_scale`]) this is whatever
/// [`FIRE_QUAD_OFFSET_Y`] ± [`FIRE_QUAD_HALF_EXTENT`] happens to scale to —
/// on the current constants, about `[-0.977, 0.244]`, i.e. the quads do
/// *not* quite reach the bottom NDC edge and extend well past the screen's
/// vertical centre. Exposed so a caller (a pixel gate, mostly) can predict
/// which rows the real geometry can touch instead of hardcoding a decimal
/// that would silently drift from the transform.
#[must_use]
pub fn fire_overlay_vertical_extent() -> (f32, f32) {
    let scale = fire_quad_scale();
    (
        (FIRE_QUAD_OFFSET_Y - FIRE_QUAD_HALF_EXTENT) * scale,
        (FIRE_QUAD_OFFSET_Y + FIRE_QUAD_HALF_EXTENT) * scale,
    )
}

/// One of the fire overlay's two real quads, flattened into NDC.
///
/// `tilt`/`offset_x` are the quad's own signed `rotateY`/X-translate
/// (`buildFireQuad` is called once with `(FIRE_QUAD_OFFSET_X,
/// -FIRE_QUAD_TILT_RADIANS)` and once with `(-FIRE_QUAD_OFFSET_X,
/// FIRE_QUAD_TILT_RADIANS)`). Vanilla's pose-stack applies `rotateY` to the
/// vertex first, then `translate` (`Matrix4f.translate` then `.rotateY`
/// accumulates as `pose · T · R`, so a vertex `v` maps to `T·(R·v)`) — this
/// reproduces exactly that, then drops the rotated `z` and scales by
/// [`fire_quad_scale`] (an orthographic flatten, not vanilla's perspective
/// projection — see the module doc's "Screen-space, not world-space" note
/// for why this whole pass has no camera/projection uniform to do better).
///
/// `mirror_u`/`v0`/`v1` are unchanged from before: vanilla's own "mirrored
/// corner mapping" (`buildSpriteQuad` passes `buildQuad` the sprite's
/// `(u1, v1, u0, v0)`, swapped) applies identically to both quads, so this
/// takes no direction flag — a caller always passes the same mirroring.
fn fire_quad(offset_x: f32, tilt: f32, v0: f32, v1: f32) -> [ScreenOverlayVertex; 4] {
    let scale = fire_quad_scale();
    let (s, c) = tilt.sin_cos();
    let corner = |local_x: f32, local_y: f32| -> [f32; 2] {
        // rotateY(tilt): x' = x·cosθ + z·sinθ (z is fixed at
        // FIRE_QUAD_LOCAL_Z, unaffected by y); y is untouched by a Y-axis
        // rotation. Then translate by offset_x, then drop z and scale.
        let x = local_x * c + FIRE_QUAD_LOCAL_Z * s + offset_x;
        let y = local_y + FIRE_QUAD_OFFSET_Y;
        [x * scale, y * scale]
    };
    let h = FIRE_QUAD_HALF_EXTENT;
    // Mirrored: local x=-h (this quad's own "left") samples v1/near-U as
    // u=1.0, local x=+h samples u=0.0 — see the function doc.
    [
        vertex(corner(-h, -h), [1.0, v1], FIRE_TINT),
        vertex(corner(h, -h), [0.0, v1], FIRE_TINT),
        vertex(corner(h, h), [0.0, v0], FIRE_TINT),
        vertex(corner(-h, h), [1.0, v0], FIRE_TINT),
    ]
}

/// Builds the fire overlay's real two-quad geometry for animation frame
/// `frame_index` (wrapped by `frame_count`, from
/// [`lodestone_assets::fire_frame_count`]) — see the module doc and
/// [`fire_quad`] for the transform this reproduces. Exactly two quads, one
/// per `submitFire` call, never a repeated/tiled copy.
#[must_use]
pub fn fire_overlay_triangles(frame_index: u32, frame_count: u32) -> [ScreenOverlayVertex; 12] {
    let frame_count = frame_count.max(1);
    let frame = frame_index % frame_count;
    let v0 = frame as f32 / frame_count as f32;
    let v1 = (frame + 1) as f32 / frame_count as f32;

    let a = fire_quad(FIRE_QUAD_OFFSET_X, -FIRE_QUAD_TILT_RADIANS, v0, v1);
    let b = fire_quad(-FIRE_QUAD_OFFSET_X, FIRE_QUAD_TILT_RADIANS, v0, v1);
    [
        a[0], a[1], a[2], a[2], a[3], a[0],
        b[0], b[1], b[2], b[2], b[3], b[0],
    ]
}

/// The pumpkin overlay's vertex tint — opaque white, i.e. no multiply at all
/// (`ARGB.white(1.0F)` in `Hud.extractTextureOverlay`, called with
/// `alpha = 1.0F` for every equippable camera overlay). The vignette shape
/// itself comes entirely from `pumpkinblur.png`'s own alpha channel; unlike
/// the underwater/fire overlays this pass contributes no colour multiply of
/// its own, gamma or otherwise.
pub const PUMPKIN_TINT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// Builds the pumpkin overlay's one static, full-screen NDC quad. Unlike the
/// underwater overlay this does not scroll with look direction and does not
/// tile — vanilla's `extractTextureOverlay` blits the texture once at
/// `(0, 0, guiWidth, guiHeight)` with UV `(0,0)-(1,1)`, no animation, no
/// per-frame recompute. Built once and never re-uploaded past construction.
#[must_use]
pub fn pumpkin_overlay_triangles() -> [ScreenOverlayVertex; 6] {
    let q = [
        vertex([-1.0, -1.0], [0.0, 1.0], PUMPKIN_TINT),
        vertex([1.0, -1.0], [1.0, 1.0], PUMPKIN_TINT),
        vertex([1.0, 1.0], [1.0, 0.0], PUMPKIN_TINT),
        vertex([-1.0, 1.0], [0.0, 0.0], PUMPKIN_TINT),
    ];
    [q[0], q[1], q[2], q[2], q[3], q[0]]
}

// ---------------------------------------------------------------------------
// Freeze overlay (issue #139): `Hud.extractCameraOverlays`'s
// `player.getTicksFrozen() > 0` branch, `Hud.java:293-295`:
// `extractTextureOverlay(POWDER_SNOW_OUTLINE_LOCATION, player.getPercentFrozen())`.
// The freeze *mechanic* (`frozen_ticks`/`percent_frozen`) is
// `lodestone_physics::player::PlayerState`'s (issue #212); this is only the
// overlay half.
// ---------------------------------------------------------------------------

/// Builds the freeze overlay's static full-screen NDC quad, same shape as
/// [`pumpkin_overlay_triangles`] — a static, untiled, unscrolled blit — but
/// with `percent` (vanilla's `Entity.getPercentFrozen()`, already `0.0..=1.0`
/// by construction, see `PlayerState::percent_frozen`) as the vertex alpha
/// instead of pumpkin's fixed `1.0`: `extractTextureOverlay`'s `alpha`
/// parameter is `ARGB.white(alpha)`, i.e. an opaque-white texel multiplied by
/// a *variable* alpha, not a fixed one. Clamped defensively even though
/// `percent_frozen` is already bounded, so a future caller passing a raw
/// ticks-based ratio cannot produce an out-of-range vertex alpha.
#[must_use]
pub fn freeze_overlay_triangles(percent: f32) -> [ScreenOverlayVertex; 6] {
    let percent = percent.clamp(0.0, 1.0);
    let color = [1.0, 1.0, 1.0, percent];
    let q = [
        vertex([-1.0, -1.0], [0.0, 1.0], color),
        vertex([1.0, -1.0], [1.0, 1.0], color),
        vertex([1.0, 1.0], [1.0, 0.0], color),
        vertex([-1.0, 1.0], [0.0, 0.0], color),
    ];
    [q[0], q[1], q[2], q[2], q[3], q[0]]
}

// ---------------------------------------------------------------------------
// Spyglass overlay (issue #154): `Hud.extractSpyglassOverlay`,
// `Hud.java:1033-1048`. Not the generic `camera_overlay` component path
// pumpkin uses — `player.isScoping()` gates a *dedicated* method with its own
// geometry (a centred lens + four solid-black letterbox bars), checked
// against the jar rather than assumed to be a two-line table addition.
// ---------------------------------------------------------------------------

/// Vanilla's settled spyglass zoom scale. `Hud.scopeScale` lerps toward this
/// constant every frame while scoping
/// (`Mth.lerp(0.5F * gameTimeDeltaTicks, this.scopeScale, 1.125F)`,
/// `Hud.java:276`) and snaps back to `0.5F` the instant scoping stops
/// (`Hud.java:281`, read on the *next* non-scoping frame — irrelevant here
/// since this pass only draws while scoping). This port uses the settled
/// value directly rather than modelling the few-frame ease-in ramp — a
/// deliberate simplification in the spirit of the fire overlay's placement
/// (see the module doc): the texture, the letterbox shape and this constant
/// are all real; only the animation into it is dropped.
pub const SPYGLASS_SCALE: f32 = 1.125;

/// The spyglass lens's half-extent in NDC on each axis, for a given screen
/// `aspect` (width/height). Derived algebraically from
/// `Hud.extractSpyglassOverlay`'s `srcWidth = srcHeight =
/// min(guiWidth, guiHeight)` and
/// `ratio = min(guiWidth/srcWidth, guiHeight/srcHeight) * scale`: one of the
/// two `Math.min` arms is always exactly `1.0` (whichever dimension *is* the
/// smaller one divides itself), so `ratio` reduces to `scale` and the smaller
/// screen dimension gets half-extent exactly `scale`, the larger one either
/// `scale / aspect` (landscape) or `scale * aspect` (portrait).
///
/// On a typical landscape screen (`aspect > 1`) this makes the vertical
/// half-extent `1.125 > 1.0`, i.e. the lens overflows past the top/bottom of
/// NDC — intentional, matching vanilla (no top/bottom bars in landscape); the
/// rasterizer clips it for free, so [`spyglass_lens_triangles`]/
/// [`spyglass_letterbox_triangles`] need no branch for it.
#[must_use]
pub fn spyglass_lens_half_extent(aspect: f32) -> (f32, f32) {
    if aspect >= 1.0 {
        (SPYGLASS_SCALE / aspect, SPYGLASS_SCALE)
    } else {
        (SPYGLASS_SCALE, SPYGLASS_SCALE * aspect)
    }
}

/// The spyglass lens's textured quad — `spyglass_scope.png`, centred, sized
/// by [`spyglass_lens_half_extent`]. Opaque white tint: vanilla's
/// `graphics.blit(RenderPipelines.GUI_TEXTURED, SPYGLASS_SCOPE_LOCATION, ...)`
/// call (`Hud.java:1043`) is the 9-argument overload with no colour
/// parameter, which defaults to full white at full alpha, the same as
/// [`PUMPKIN_TINT`].
#[must_use]
pub fn spyglass_lens_triangles(aspect: f32) -> [ScreenOverlayVertex; 6] {
    let (hw, hh) = spyglass_lens_half_extent(aspect);
    let q = [
        vertex([-hw, -hh], [0.0, 1.0], PUMPKIN_TINT),
        vertex([hw, -hh], [1.0, 1.0], PUMPKIN_TINT),
        vertex([hw, hh], [1.0, 0.0], PUMPKIN_TINT),
        vertex([-hw, hh], [0.0, 0.0], PUMPKIN_TINT),
    ];
    [q[0], q[1], q[2], q[2], q[3], q[0]]
}

/// Opaque black — vanilla's four `graphics.fill(RenderPipelines.GUI, ...,
/// -16777216)` calls around the lens (`Hud.java:1044-1047`; `-16777216` is
/// ARGB opaque black).
pub const LETTERBOX_TINT: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// The four letterbox bars filling the NDC screen outside the lens — top,
/// bottom, left, right, in that order, six vertices each. Together with
/// [`spyglass_lens_triangles`] these exactly tile `[-1,1]x[-1,1]` with no gap
/// and no overlap, by construction from the same [`spyglass_lens_half_extent`]
/// split. Drawn with the pipeline's spare 1x1 white texture (see
/// [`ScreenEffectRenderer`]'s doc) rather than a second pipeline or a loaded
/// asset: multiplying any texel by [`LETTERBOX_TINT`]'s zero RGB is black
/// regardless of what is sampled, so this needs only an opaque alpha channel
/// wherever it samples, not real texture content — the UV coordinates here
/// are therefore unused and left at `[0, 0]`.
#[must_use]
pub fn spyglass_letterbox_triangles(aspect: f32) -> [ScreenOverlayVertex; 24] {
    let (hw, hh) = spyglass_lens_half_extent(aspect);
    // Clamped to NDC for the bars only (never the lens itself, which is
    // allowed to overflow and rely on the rasterizer's free clip — see
    // `spyglass_lens_triangles`'s doc): a landscape screen's `hh` is always
    // `SPYGLASS_SCALE = 1.125 > 1.0`, so an unclamped top/bottom bar would be
    // an *inverted* (min > max) rect rather than a zero-area one. Clamping
    // degenerates it to a genuine zero-width quad instead, matching vanilla's
    // real behaviour (no bar drawn at all in that axis) without depending on
    // GPU clip semantics to make an inverted quad a no-op.
    let hw_c = hw.min(1.0);
    let hh_c = hh.min(1.0);
    let bar = |x0: f32, y0: f32, x1: f32, y1: f32| -> [ScreenOverlayVertex; 6] {
        let q = [
            vertex([x0, y0], [0.0, 0.0], LETTERBOX_TINT),
            vertex([x1, y0], [0.0, 0.0], LETTERBOX_TINT),
            vertex([x1, y1], [0.0, 0.0], LETTERBOX_TINT),
            vertex([x0, y1], [0.0, 0.0], LETTERBOX_TINT),
        ];
        [q[0], q[1], q[2], q[2], q[3], q[0]]
    };
    let top = bar(-1.0, hh_c, 1.0, 1.0);
    let bottom = bar(-1.0, -1.0, 1.0, -hh_c);
    let left = bar(-1.0, -hh_c, -hw_c, hh_c);
    let right = bar(hw_c, -hh_c, 1.0, hh_c);
    let mut out = [top[0]; 24];
    out[0..6].copy_from_slice(&top);
    out[6..12].copy_from_slice(&bottom);
    out[12..18].copy_from_slice(&left);
    out[18..24].copy_from_slice(&right);
    out
}

// ---------------------------------------------------------------------------
// Confusion overlay (issue #144, the nausea screen-space half):
// `Hud.extractConfusionOverlay`, `Hud.java:1109-1132`. The *other* half of
// #144 — the world-projection "spinning" warp vanilla applies alongside this
// — is `crate::camera::nausea_portal_warp`, not geometry, so it lives in
// `camera.rs` rather than here; see that function's doc for why.
// ---------------------------------------------------------------------------

/// Builds the confusion overlay's quad for a given `strength` (vanilla's
/// `overlayStrength = nauseaIntensity * (1 - screenEffectScale)`,
/// `Hud.java:305`, already clamped to `0.0..=1.0` by construction there —
/// clamped again here defensively). The quad is scaled about the screen
/// centre by `size = Mth.lerp(strength, 2.0F, 1.0F) = 2.0 - strength`
/// (`Hud.java:1113`, transcribed with vanilla's own literals rather than
/// simplified, since `2.0 - strength` reads as unrelated to the source line
/// it came from) — always `>= 1.0` for `strength` in its valid `(0, 1]`
/// range, so the quad always at least covers the full NDC screen; the
/// rasterizer clips whatever it overflows by, the same free clip
/// [`spyglass_lens_triangles`] relies on. UV stays anchored to the *unscaled*
/// corners (`0.0..1.0`), matching vanilla's pose-stack transform applying
/// only to position, never to the blit's own UV rectangle.
///
/// Tint is vanilla's `red = 0.2 * strength, green = 0.4 * strength, blue =
/// 0.2 * strength` (`Hud.java:1117-1119`) at alpha `1.0`
/// (`ARGB.colorFromFloat(1.0F, red, green, blue)`) — a green-biased tint,
/// unlike every other overlay in this pass, which is why it is not folded
/// into a shared "tint from strength" helper with anything else here.
#[must_use]
pub fn confusion_overlay_triangles(strength: f32) -> [ScreenOverlayVertex; 6] {
    let strength = strength.clamp(0.0, 1.0);
    let size = 2.0 - strength;
    let color = [0.2 * strength, 0.4 * strength, 0.2 * strength, 1.0];
    let q = [
        vertex([-size, -size], [0.0, 1.0], color),
        vertex([size, -size], [1.0, 1.0], color),
        vertex([size, size], [1.0, 0.0], color),
        vertex([-size, size], [0.0, 0.0], color),
    ];
    [q[0], q[1], q[2], q[2], q[3], q[0]]
}

// ---------------------------------------------------------------------------
// Portal overlay (issue #149, the screen-space half):
// `Hud.extractPortalOverlay`, `Hud.java:1097-1107`. The shared "spinning"
// world-projection warp is `crate::camera::nausea_portal_warp` — see the
// confusion overlay's module comment above; vanilla drives both this overlay
// and that warp from the same `Entity.portalEffectIntensity`, but only this
// half is geometry.
// ---------------------------------------------------------------------------

/// Vanilla's portal-overlay alpha curve (`Hud.java:1097-1102`):
/// `if (alpha < 1.0F) { alpha *= alpha; alpha *= alpha; alpha = alpha * 0.8F +
/// 0.2F; }` — i.e. `alpha^4 * 0.8 + 0.2` below `1.0`, identity at `1.0`.
/// `intensity` is vanilla's `portalEffectIntensity`, clamped defensively to
/// `0.0..=1.0` (vanilla's own value is time-integrated and typically stays in
/// range, but nothing here enforces that upstream).
#[must_use]
pub fn portal_overlay_alpha(intensity: f32) -> f32 {
    let a = intensity.clamp(0.0, 1.0);
    if a < 1.0 {
        let a2 = a * a;
        let a4 = a2 * a2;
        a4 * 0.8 + 0.2
    } else {
        a
    }
}

/// Builds the portal overlay's full-screen NDC quad for animation frame
/// `frame_index` (wrapped by `frame_count`, from
/// [`lodestone_assets::fire_frame_count`] applied to the loaded
/// `nether_portal.png` strip — see that texture's own loader doc for why the
/// exact same frame-count function applies). Unlike the fire strip's four
/// tiled quads, this is one full-screen quad, matching
/// `extractPortalOverlay`'s single `blitSprite` call — no tile mirroring is
/// needed since there is only one quad to begin with.
#[must_use]
pub fn portal_overlay_triangles(frame_index: u32, frame_count: u32, intensity: f32) -> [ScreenOverlayVertex; 6] {
    let alpha = portal_overlay_alpha(intensity);
    let frame_count = frame_count.max(1);
    let frame = frame_index % frame_count;
    let v0 = frame as f32 / frame_count as f32;
    let v1 = (frame + 1) as f32 / frame_count as f32;
    let color = [1.0, 1.0, 1.0, alpha];
    let q = [
        vertex([-1.0, -1.0], [0.0, v1], color),
        vertex([1.0, -1.0], [1.0, v1], color),
        vertex([1.0, 1.0], [1.0, v0], color),
        vertex([-1.0, 1.0], [0.0, v0], color),
    ];
    [q[0], q[1], q[2], q[2], q[3], q[0]]
}

// ---------------------------------------------------------------------------
// WGSL — one pipeline, shared by both textures.
// ---------------------------------------------------------------------------

const OVERLAY_WGSL: &str = include_str!("shaders/overlay.wgsl");

fn texture_bind_group_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn texture_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    label: &str,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

fn upload_plain_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    width: u32,
    height: u32,
    rgba: &[u8],
    address_mode: wgpu::AddressMode,
    filter: wgpu::FilterMode,
) -> (wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * width.max(1)),
            rows_per_image: Some(height.max(1)),
        },
        wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: address_mode,
        address_mode_v: address_mode,
        address_mode_w: address_mode,
        mag_filter: filter,
        min_filter: filter,
        ..Default::default()
    });
    (view, sampler)
}

fn build_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    color_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(OVERLAY_WGSL.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    const ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Float32x4,
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<ScreenOverlayVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &ATTRS,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            ..Default::default()
        },
        // No depth attachment — see the module doc; this draws after the
        // world and the hand, straight into the colour target.
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn vertex_buffer(device: &wgpu::Device, label: &str, verts: &[ScreenOverlayVertex]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(verts),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
    })
}

/// Owns the GPU resources for both overlays and drives them per frame.
#[derive(Debug)]
pub struct ScreenEffectRenderer {
    pipeline: wgpu::RenderPipeline,
    underwater_bind_group: wgpu::BindGroup,
    fire_bind_group: wgpu::BindGroup,
    pumpkin_bind_group: wgpu::BindGroup,
    /// The freezing vignette (issue #139) — `powder_snow_outline.png`.
    freeze_bind_group: wgpu::BindGroup,
    /// The spyglass lens (issue #154) — `spyglass_scope.png`.
    spyglass_bind_group: wgpu::BindGroup,
    /// A procedural 1x1 opaque-white texture with no backing asset, used by
    /// the spyglass letterbox bars ([`spyglass_letterbox_triangles`]'s doc)
    /// — a flat colour fill needs no real texture content, only opacity.
    white_bind_group: wgpu::BindGroup,
    /// The confusion overlay (issue #144) — `nausea.png`.
    nausea_bind_group: wgpu::BindGroup,
    /// The portal overlay (issue #149) — `nether_portal.png`.
    portal_bind_group: wgpu::BindGroup,
    fire_frame_count: u32,
    /// [`load_portal_overlay_texture`]'s strip is a different image from the
    /// fire strip (different frame content, same shape), so it gets its own
    /// count rather than reusing [`Self::fire_frame_count`].
    portal_frame_count: u32,
    underwater_vbuf: wgpu::Buffer,
    fire_vbuf: wgpu::Buffer,
    pumpkin_vbuf: wgpu::Buffer,
    freeze_vbuf: wgpu::Buffer,
    spyglass_lens_vbuf: wgpu::Buffer,
    spyglass_bars_vbuf: wgpu::Buffer,
    nausea_vbuf: wgpu::Buffer,
    portal_vbuf: wgpu::Buffer,
}

impl ScreenEffectRenderer {
    /// Loads every overlay texture from `manager` and builds the pass.
    ///
    /// # Errors
    ///
    /// Returns [`ScreenEffectAssetError`] if any texture is missing or fails
    /// to decode.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        manager: &ResourceManager,
    ) -> Result<Self, ScreenEffectAssetError> {
        let underwater_image = load_underwater_texture(manager)?;
        let fire_image = load_fire_texture(manager)?;
        let pumpkin_image = load_pumpkin_overlay_texture(manager)?;
        let freeze_image = load_freeze_overlay_texture(manager)?;
        let spyglass_image = load_spyglass_scope_texture(manager)?;
        let nausea_image = load_nausea_overlay_texture(manager)?;
        let portal_image = load_portal_overlay_texture(manager)?;
        let portal_frame_count = fire_frame_count(&portal_image);
        let fire_frame_count = fire_frame_count(&fire_image);

        let layout = texture_bind_group_layout(device, "lodestone-screen-effect-tex-bgl");
        let pipeline = build_pipeline(device, "lodestone-screen-effect-pipeline", &layout, color_format);

        let (uw_view, uw_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-underwater-texture",
            underwater_image.width,
            underwater_image.height,
            &underwater_image.rgba,
            wgpu::AddressMode::Repeat,
            wgpu::FilterMode::Linear,
        );
        let underwater_bind_group = texture_bind_group(
            device,
            &layout,
            "lodestone-underwater-texture-bg",
            &uw_view,
            &uw_sampler,
        );

        // Nearest, and clamp rather than repeat: this is a vertical strip of
        // independent animation frames, not a tileable texture — linear
        // filtering or wraparound at a frame's top/bottom edge would blend in
        // the neighbouring frame.
        let (fire_view, fire_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-fire-texture",
            fire_image.width,
            fire_image.height,
            &fire_image.rgba,
            wgpu::AddressMode::ClampToEdge,
            wgpu::FilterMode::Nearest,
        );
        let fire_bind_group = texture_bind_group(
            device,
            &layout,
            "lodestone-fire-texture-bg",
            &fire_view,
            &fire_sampler,
        );

        // Nearest, clamp: a single static image, no tiling or scroll — same
        // reasoning as the fire strip's sampler, minus the animation.
        let (pumpkin_view, pumpkin_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-pumpkin-overlay-texture",
            pumpkin_image.width,
            pumpkin_image.height,
            &pumpkin_image.rgba,
            wgpu::AddressMode::ClampToEdge,
            wgpu::FilterMode::Linear,
        );
        let pumpkin_bind_group = texture_bind_group(
            device,
            &layout,
            "lodestone-pumpkin-overlay-texture-bg",
            &pumpkin_view,
            &pumpkin_sampler,
        );

        // Nearest, clamp: a single static image, same reasoning as pumpkin's.
        let (freeze_view, freeze_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-freeze-overlay-texture",
            freeze_image.width,
            freeze_image.height,
            &freeze_image.rgba,
            wgpu::AddressMode::ClampToEdge,
            wgpu::FilterMode::Linear,
        );
        let freeze_bind_group = texture_bind_group(
            device,
            &layout,
            "lodestone-freeze-overlay-texture-bg",
            &freeze_view,
            &freeze_sampler,
        );

        let (spyglass_view, spyglass_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-spyglass-scope-texture",
            spyglass_image.width,
            spyglass_image.height,
            &spyglass_image.rgba,
            wgpu::AddressMode::ClampToEdge,
            wgpu::FilterMode::Linear,
        );
        let spyglass_bind_group = texture_bind_group(
            device,
            &layout,
            "lodestone-spyglass-scope-texture-bg",
            &spyglass_view,
            &spyglass_sampler,
        );

        // The letterbox bars' procedural stand-in — see the struct field doc.
        let (white_view, white_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-screen-effect-white-1x1",
            1,
            1,
            &[255, 255, 255, 255],
            wgpu::AddressMode::ClampToEdge,
            wgpu::FilterMode::Nearest,
        );
        let white_bind_group = texture_bind_group(
            device,
            &layout,
            "lodestone-screen-effect-white-1x1-bg",
            &white_view,
            &white_sampler,
        );

        let (nausea_view, nausea_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-nausea-overlay-texture",
            nausea_image.width,
            nausea_image.height,
            &nausea_image.rgba,
            wgpu::AddressMode::ClampToEdge,
            wgpu::FilterMode::Linear,
        );
        let nausea_bind_group = texture_bind_group(
            device,
            &layout,
            "lodestone-nausea-overlay-texture-bg",
            &nausea_view,
            &nausea_sampler,
        );

        // Nearest, clamp: an animation strip like fire's, same reasoning.
        let (portal_view, portal_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-portal-overlay-texture",
            portal_image.width,
            portal_image.height,
            &portal_image.rgba,
            wgpu::AddressMode::ClampToEdge,
            wgpu::FilterMode::Nearest,
        );
        let portal_bind_group = texture_bind_group(
            device,
            &layout,
            "lodestone-portal-overlay-texture-bg",
            &portal_view,
            &portal_sampler,
        );

        let underwater_vbuf = vertex_buffer(
            device,
            "lodestone-underwater-vbuf",
            &underwater_overlay_triangles(0.0, 0.0, 0xFF),
        );
        let fire_vbuf = vertex_buffer(device, "lodestone-fire-vbuf", &fire_overlay_triangles(0, fire_frame_count));
        let pumpkin_vbuf = vertex_buffer(device, "lodestone-pumpkin-vbuf", &pumpkin_overlay_triangles());
        let freeze_vbuf = vertex_buffer(device, "lodestone-freeze-vbuf", &freeze_overlay_triangles(0.0));
        let spyglass_lens_vbuf =
            vertex_buffer(device, "lodestone-spyglass-lens-vbuf", &spyglass_lens_triangles(1.0));
        let spyglass_bars_vbuf = vertex_buffer(
            device,
            "lodestone-spyglass-bars-vbuf",
            &spyglass_letterbox_triangles(1.0),
        );
        let nausea_vbuf = vertex_buffer(device, "lodestone-nausea-vbuf", &confusion_overlay_triangles(0.0));
        let portal_vbuf = vertex_buffer(
            device,
            "lodestone-portal-vbuf",
            &portal_overlay_triangles(0, portal_frame_count, 0.0),
        );

        Ok(Self {
            pipeline,
            underwater_bind_group,
            fire_bind_group,
            pumpkin_bind_group,
            freeze_bind_group,
            spyglass_bind_group,
            white_bind_group,
            nausea_bind_group,
            portal_bind_group,
            fire_frame_count,
            portal_frame_count,
            underwater_vbuf,
            fire_vbuf,
            pumpkin_vbuf,
            freeze_vbuf,
            spyglass_lens_vbuf,
            spyglass_bars_vbuf,
            nausea_vbuf,
            portal_vbuf,
        })
    }

    /// The fire strip's frame count, from the loaded texture — a caller
    /// ticking the animation forward derives its own frame index modulo this.
    #[must_use]
    pub fn fire_frame_count(&self) -> u32 {
        self.fire_frame_count
    }

    /// Draws the underwater overlay (screen tint + scrolling texture) as its
    /// own render pass, with `Load` (never `Clear`) — this runs after the
    /// world, entities and the first-person hand, and must not erase them.
    /// `yaw_degrees`/`pitch_degrees` are the live camera look direction;
    /// `packed_light` is `sky << 4 | block` at the player's eye, the same
    /// encoding [`crate::entity`]'s light sampling uses.
    pub fn draw_underwater(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        yaw_degrees: f32,
        pitch_degrees: f32,
        packed_light: u8,
    ) {
        let verts = underwater_overlay_triangles(yaw_degrees, pitch_degrees, packed_light);
        queue.write_buffer(&self.underwater_vbuf, 0, bytemuck::cast_slice(&verts));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lodestone-underwater-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.underwater_bind_group, &[]);
        pass.set_vertex_buffer(0, self.underwater_vbuf.slice(..));
        pass.draw(0..verts.len() as u32, 0..1);
    }

    /// Draws the fire overlay (looping flame strip) as its own `Load` render
    /// pass, for the reasons on [`Self::draw_underwater`]. `tick` selects the
    /// animation frame (`tick % `[`Self::fire_frame_count`]`, vanilla's
    /// default one-frame-per-tick `fire_1.png.mcmeta`).
    pub fn draw_fire(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        tick: u64,
    ) {
        let frame = (tick % u64::from(self.fire_frame_count)) as u32;
        let verts = fire_overlay_triangles(frame, self.fire_frame_count);
        queue.write_buffer(&self.fire_vbuf, 0, bytemuck::cast_slice(&verts));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lodestone-fire-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.fire_bind_group, &[]);
        pass.set_vertex_buffer(0, self.fire_vbuf.slice(..));
        pass.draw(0..verts.len() as u32, 0..1);
    }

    /// Draws the pumpkin overlay (issue #185) as its own `Load` render pass,
    /// for the reasons on [`Self::draw_underwater`]. Static geometry — the
    /// vertex buffer was written once at [`Self::new`] and never changes, so
    /// unlike the other two draws this has no per-frame `write_buffer`.
    pub fn draw_pumpkin(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lodestone-pumpkin-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.pumpkin_bind_group, &[]);
        pass.set_vertex_buffer(0, self.pumpkin_vbuf.slice(..));
        pass.draw(0..6, 0..1);
    }

    /// The `nether_portal.png` strip's frame count — same shape as
    /// [`Self::fire_frame_count`], separate storage since it is a different
    /// image.
    #[must_use]
    pub fn portal_frame_count(&self) -> u32 {
        self.portal_frame_count
    }

    /// Draws the freeze overlay (issue #139) as its own `Load` render pass,
    /// for the reasons on [`Self::draw_underwater`]. `percent` is vanilla's
    /// `Entity.getPercentFrozen()` (see [`freeze_overlay_triangles`]) — the
    /// caller is expected to have already checked `percent > 0.0`
    /// (`Hud.java`'s own `getTicksFrozen() > 0` guard), but this draws
    /// unconditionally like every other method here; gating is
    /// [`super::ScreenEffects`]'s job, one layer up.
    pub fn draw_freeze(&self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, percent: f32) {
        let verts = freeze_overlay_triangles(percent);
        queue.write_buffer(&self.freeze_vbuf, 0, bytemuck::cast_slice(&verts));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lodestone-freeze-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.freeze_bind_group, &[]);
        pass.set_vertex_buffer(0, self.freeze_vbuf.slice(..));
        pass.draw(0..verts.len() as u32, 0..1);
    }

    /// Draws the spyglass overlay (issue #154) as its own `Load` render pass:
    /// the lens quad, then the four letterbox bars, both re-derived from
    /// `aspect` every call since a window resize changes it (unlike
    /// [`Self::draw_pumpkin`]'s vertex buffer, these cannot be built once at
    /// [`Self::new`]). Two draw calls in one pass, one bind group active at a
    /// time — see [`spyglass_letterbox_triangles`]'s doc for why the bars
    /// need no second pipeline.
    pub fn draw_spyglass(&self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, aspect: f32) {
        let lens = spyglass_lens_triangles(aspect);
        let bars = spyglass_letterbox_triangles(aspect);
        queue.write_buffer(&self.spyglass_lens_vbuf, 0, bytemuck::cast_slice(&lens));
        queue.write_buffer(&self.spyglass_bars_vbuf, 0, bytemuck::cast_slice(&bars));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lodestone-spyglass-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        // Bars first, then the lens on top — vanilla's own call order in
        // `extractSpyglassOverlay` (`Hud.java:1043-1047`) draws the lens
        // first and the four fills after, but since the two regions are
        // disjoint by construction (see `spyglass_letterbox_triangles`'s
        // doc), draw order between them cannot change the result; bars
        // first here only so the lens's own bind group is the last one set.
        pass.set_bind_group(0, &self.white_bind_group, &[]);
        pass.set_vertex_buffer(0, self.spyglass_bars_vbuf.slice(..));
        pass.draw(0..bars.len() as u32, 0..1);
        pass.set_bind_group(0, &self.spyglass_bind_group, &[]);
        pass.set_vertex_buffer(0, self.spyglass_lens_vbuf.slice(..));
        pass.draw(0..lens.len() as u32, 0..1);
    }

    /// Draws the confusion overlay (issue #144, screen-space half) as its own
    /// `Load` render pass. `strength` is vanilla's `overlayStrength` (see
    /// [`confusion_overlay_triangles`]) — the caller is expected to have
    /// already applied the mutual-exclusion-with-portal and
    /// `screenEffectScale < 1.0` checks (`Hud.java:300-307`), matching every
    /// other `draw_*` method's "gating happens one layer up" convention.
    pub fn draw_confusion(&self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, strength: f32) {
        let verts = confusion_overlay_triangles(strength);
        queue.write_buffer(&self.nausea_vbuf, 0, bytemuck::cast_slice(&verts));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lodestone-confusion-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.nausea_bind_group, &[]);
        pass.set_vertex_buffer(0, self.nausea_vbuf.slice(..));
        pass.draw(0..verts.len() as u32, 0..1);
    }

    /// Draws the portal overlay (issue #149, screen-space half) as its own
    /// `Load` render pass. `frame` selects the animation frame
    /// (`frame % `[`Self::portal_frame_count`]`, same one-frame-per-tick
    /// cadence as [`Self::draw_fire`]); `intensity` is vanilla's
    /// `portalEffectIntensity` (see [`portal_overlay_triangles`]/
    /// [`portal_overlay_alpha`]).
    pub fn draw_portal(&self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, frame: u32, intensity: f32) {
        let verts = portal_overlay_triangles(frame, self.portal_frame_count, intensity);
        queue.write_buffer(&self.portal_vbuf, 0, bytemuck::cast_slice(&verts));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lodestone-portal-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.portal_bind_group, &[]);
        pass.set_vertex_buffer(0, self.portal_vbuf.slice(..));
        pass.draw(0..verts.len() as u32, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-derived from `lightmap.fsh` rather than from this function's output.
    ///
    /// `get_brightness(0) = 0`, but a fully dark cell is **not** pure black:
    /// vanilla seeds the accumulator with `AmbientColor`, which the overworld sets
    /// to `0x0A0A0A` (`DimensionTypes.java:36`), giving `0.0935` after the
    /// `notGamma` mix. This test previously asserted `0.0` on the strength of the
    /// claim that "vanilla has no floor" — the retired `0.2` floor was indeed
    /// invented, but the correct replacement was a *smaller* floor, not none.
    /// `get_brightness(1) = 1` and `notGamma(1) = 1` and the ambient term clamps
    /// away, so full light is still exactly `1.0` and every full-bright overlay
    /// gate is unmoved.
    #[test]
    fn underwater_brightness_reaches_the_ambient_floor_and_caps_at_1() {
        let ambient = 10.0_f32 / 255.0;
        let expected = ambient + ((1.0 - (1.0 - ambient).powi(4)) - ambient) * 0.5;
        assert!((expected - 0.093_545).abs() < 1e-5, "hypothesis drifted: {expected}");
        assert!(
            (underwater_brightness(0x00) - expected).abs() < 1e-6,
            "a fully dark cell must reach vanilla's ambient floor {expected}, not pure \
             black and not the retired ramp's 0.2; got {}",
            underwater_brightness(0x00)
        );
        assert!((underwater_brightness(0xFF) - 1.0).abs() < 1e-6, "fully lit reaches 1.0");
    }

    /// The interior of the curve, which is where the retired linear ramp was
    /// wrong. All three hypotheses are written out; the measurement must land on
    /// vanilla's. `level = 8/15`, `get_brightness = 0.2222`, plus the overworld
    /// ambient `10/255`, `notGamma` mixed at the default gamma of 0.5 gives
    /// `0.4819`. Dropping ambient gives `0.4281` and the retired ramp `0.6267`.
    #[test]
    fn underwater_brightness_follows_vanillas_curve_in_the_interior() {
        let level: f32 = 8.0 / 15.0;
        let curved = level / (4.0 - 3.0 * level);
        let mix = |c: f32| c + ((1.0 - (1.0 - c).powi(4)) - c) * 0.5;
        let vanilla = mix(curved + 10.0 / 255.0);
        let ambient_free = mix(curved);
        let retired_ramp = 0.2 + 0.8 * level;
        assert!((vanilla - 0.481_948).abs() < 1e-5, "hypothesis drifted: {vanilla}");
        assert!(
            (ambient_free - 0.428_136).abs() < 1e-5,
            "hypothesis drifted: {ambient_free}"
        );
        assert!((retired_ramp - 0.626_667).abs() < 1e-5, "hypothesis drifted: {retired_ramp}");
        assert!(
            (underwater_brightness(0x80) - vanilla).abs() < 1e-5,
            "sky 8 must tint at vanilla's {vanilla} -- not the retired ramp's \
             {retired_ramp} and not the ambient-free {ambient_free}; got {}",
            underwater_brightness(0x80)
        );
    }

    #[test]
    fn underwater_brightness_takes_the_brighter_channel() {
        // block=15, sky=0 should read identically to sky=15, block=0.
        let block_lit = underwater_brightness(0x0F);
        let sky_lit = underwater_brightness(0xF0);
        assert!((block_lit - sky_lit).abs() < 1e-6);
    }

    #[test]
    fn underwater_quad_alpha_is_vanillas_point_one() {
        let q = underwater_overlay_quad(0.0, 0.0, 0xFF);
        for v in q {
            assert!((v.color[3] - 0.1).abs() < 1e-6);
        }
    }

    #[test]
    fn underwater_quad_covers_the_full_ndc_screen() {
        let q = underwater_overlay_quad(0.0, 0.0, 0xFF);
        let xs: Vec<f32> = q.iter().map(|v| v.position[0]).collect();
        let ys: Vec<f32> = q.iter().map(|v| v.position[1]).collect();
        assert_eq!(xs.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
        assert_eq!(ys.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
    }

    #[test]
    fn underwater_uv_scrolls_with_yaw_and_pitch() {
        let still = underwater_overlay_quad(0.0, 0.0, 0xFF);
        let turned = underwater_overlay_quad(90.0, 0.0, 0xFF);
        assert_ne!(still[0].uv, turned[0].uv, "yaw must move the scroll");
        let tilted = underwater_overlay_quad(0.0, 45.0, 0xFF);
        assert_ne!(still[0].uv, tilted[0].uv, "pitch must move the scroll");
    }

    #[test]
    fn underwater_uv_tiles_four_times() {
        let q = underwater_overlay_quad(0.0, 0.0, 0xFF);
        // The quad's UV span (max - min on either axis) is the tile count.
        let us: Vec<f32> = q.iter().map(|v| v.uv[0]).collect();
        let span = us.iter().cloned().fold(f32::NEG_INFINITY, f32::max)
            - us.iter().cloned().fold(f32::INFINITY, f32::min);
        assert!((span - UNDERWATER_TILE_COUNT).abs() < 1e-6);
    }

    #[test]
    fn fire_tint_alpha_matches_vanillas_argb_constant() {
        assert!((FIRE_TINT[3] - 229.0 / 255.0).abs() < 1e-6);
        assert_eq!(&FIRE_TINT[0..3], &[1.0, 1.0, 1.0]);
    }

    /// `fire_overlay_triangles` is exactly two quads (12 vertices, two
    /// six-vertex triangle fans) — never a repeated/tiled sprite. This is
    /// the property issue #420 exists to restore: a previous pass here drew
    /// four mirrored copies of one tile instead, which is what the repo
    /// owner saw as the fire texture "repeated multiple times".
    #[test]
    fn fire_overlay_is_exactly_two_quads_not_a_tiled_strip() {
        let tris = fire_overlay_triangles(0, 32);
        assert_eq!(tris.len(), 12, "two quads, six vertices each");
    }

    /// Preserves the one invariant the old four-tile strip had — see the
    /// module doc's "The fire overlay is vanilla's real two-quad geometry"
    /// section — now met by two *overlapping* quads instead of four
    /// disjoint ones (see `the_two_fire_quads_overlap_rather_than_tile`,
    /// below, for the overlap itself and its numeric rejection of the old
    /// four-tile hypothesis).
    #[test]
    fn fire_quads_span_the_full_ndc_width_with_no_gaps() {
        let tris = fire_overlay_triangles(0, 32);
        let xs: Vec<f32> = tris.iter().map(|v| v.position[0]).collect();
        let min = xs.iter().cloned().fold(f32::INFINITY, f32::min);
        let max = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        // `fire_quad_scale` goes through `sin_cos`, unlike the old design's
        // exact literal arithmetic, so this is an epsilon check rather than
        // `assert_eq!` -- the whole point of `fire_quad_scale`'s own
        // derivation is that these land within float rounding of exactly
        // `[-1, 1]`, not bit-identically on it.
        assert!((min - -1.0).abs() < 1e-5, "min {min} should be within rounding of -1.0");
        assert!((max - 1.0).abs() < 1e-5, "max {max} should be within rounding of 1.0");
    }

    /// Predicts the *exact* NDC vertical extent from
    /// [`FIRE_QUAD_OFFSET_X`]/[`FIRE_QUAD_OFFSET_Y`]/[`FIRE_QUAD_TILT_RADIANS`]
    /// via [`fire_overlay_vertical_extent`] (the same function a pixel gate
    /// uses to know which rows to check) and requires the real geometry to
    /// land on it exactly — not merely "somewhere in the bottom area". The
    /// old strip's `[-1.0, FIRE_STRIP_TOP=-0.3]` is the rejected hypothesis:
    /// the real extent's top edge lands at `+0.244`, `0.544` NDC units past
    /// where the old strip was capped, and its bottom edge does not even
    /// reach `-1.0` — both measurable distances a bounding-box check alone
    /// (min/max only) cannot mistake for rounding noise.
    #[test]
    fn fire_quads_vertical_extent_matches_the_predicted_transform() {
        let tris = fire_overlay_triangles(0, 32);
        let ys: Vec<f32> = tris.iter().map(|v| v.position[1]).collect();
        let measured_min = ys.iter().cloned().fold(f32::INFINITY, f32::min);
        let measured_max = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let (predicted_min, predicted_max) = fire_overlay_vertical_extent();

        assert!(
            (measured_min - predicted_min).abs() < 1e-5,
            "measured y_min {measured_min} vs predicted {predicted_min}"
        );
        assert!(
            (measured_max - predicted_max).abs() < 1e-5,
            "measured y_max {measured_max} vs predicted {predicted_max}"
        );

        // The rejected hypothesis, made concrete: the old four-tile strip's
        // top edge was a hardcoded -0.3. The real transform's top edge is
        // measurably far from that -- not a rounding-distance disagreement.
        const OLD_STRIP_TOP: f32 = -0.3;
        assert!(
            (predicted_max - OLD_STRIP_TOP).abs() > 0.3,
            "predicted top {predicted_max} is suspiciously close to the retired \
             OLD_STRIP_TOP={OLD_STRIP_TOP} -- the fix may not have changed anything"
        );
        // Nor does the real geometry reach the exact bottom edge the old
        // strip always touched (`-1.0`) -- a further, independent way the
        // two shapes measurably differ.
        assert!(
            (predicted_min - (-1.0)).abs() > 0.01,
            "predicted bottom {predicted_min} should not land on the old strip's -1.0 \
             bottom edge exactly"
        );
    }

    /// The concrete, numeric form of "two quads, not four tiles": the two
    /// real quads' own X-spans genuinely overlap near screen centre (vanilla
    /// draws both licks converging), which four disjoint tiles — by
    /// construction, since adjacent tiles only ever touch at a shared edge —
    /// cannot produce. Verified failing: the four-tile hypothesis predicts
    /// exactly zero overlap; the measured overlap here is `> 0.3` NDC units
    /// away from that, not a rounding-distance call.
    #[test]
    fn the_two_fire_quads_overlap_rather_than_tile() {
        let tris = fire_overlay_triangles(0, 32);
        let quad_a_xs: Vec<f32> = tris[0..6].iter().map(|v| v.position[0]).collect();
        let quad_b_xs: Vec<f32> = tris[6..12].iter().map(|v| v.position[0]).collect();
        let (a_min, a_max) = (
            quad_a_xs.iter().cloned().fold(f32::INFINITY, f32::min),
            quad_a_xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        );
        let (b_min, b_max) = (
            quad_b_xs.iter().cloned().fold(f32::INFINITY, f32::min),
            quad_b_xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        );
        let overlap = a_max.min(b_max) - a_min.max(b_min);
        assert!(
            overlap > 0.3,
            "expected the two fire quads to overlap by a wide margin (quad A [{a_min}, \
             {a_max}], quad B [{b_min}, {b_max}]), got overlap={overlap} -- the rejected \
             four-disjoint-tile hypothesis predicts exactly 0 overlap, so this must clear \
             it by far more than rounding noise"
        );
    }

    #[test]
    fn fire_frame_selects_the_right_v_slice() {
        let frame5 = fire_overlay_triangles(5, 32);
        let v0 = 5.0 / 32.0;
        let v1 = 6.0 / 32.0;
        for vert in frame5 {
            assert!(vert.uv[1] >= v0 - 1e-6 && vert.uv[1] <= v1 + 1e-6);
        }
    }

    #[test]
    fn fire_frame_wraps_past_the_last_frame() {
        let wrapped = fire_overlay_triangles(32, 32);
        let first = fire_overlay_triangles(0, 32);
        assert_eq!(wrapped, first, "frame 32 of a 32-frame strip is frame 0 again");
    }

    #[test]
    fn pumpkin_overlay_covers_the_full_ndc_screen() {
        let tris = pumpkin_overlay_triangles();
        let xs: Vec<f32> = tris.iter().map(|v| v.position[0]).collect();
        let ys: Vec<f32> = tris.iter().map(|v| v.position[1]).collect();
        assert_eq!(xs.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
        assert_eq!(ys.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
    }

    #[test]
    fn pumpkin_overlay_is_untinted_and_opaque() {
        // `Hud.extractTextureOverlay` is called with `alpha = 1.0F` and no
        // colour multiply for an equippable camera overlay -- the vignette
        // shape comes entirely from the texture's own alpha channel.
        for v in pumpkin_overlay_triangles() {
            assert_eq!(v.color, [1.0, 1.0, 1.0, 1.0]);
        }
    }

    #[test]
    fn pumpkin_overlay_uv_spans_the_whole_texture_once() {
        // No tiling, no scroll: UV must be exactly the unit square, unlike
        // underwater's 4x scroll-shifted tiling.
        let tris = pumpkin_overlay_triangles();
        let us: Vec<f32> = tris.iter().map(|v| v.uv[0]).collect();
        let vs: Vec<f32> = tris.iter().map(|v| v.uv[1]).collect();
        assert_eq!(us.iter().cloned().fold(f32::INFINITY, f32::min), 0.0);
        assert_eq!(us.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
        assert_eq!(vs.iter().cloned().fold(f32::INFINITY, f32::min), 0.0);
        assert_eq!(vs.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
    }

    /// Vanilla's own "mirrored corner mapping" (`buildSpriteQuad` passes
    /// `buildQuad` the sprite's `(u1, v1, u0, v0)`, swapped —
    /// `ScreenEffectRenderer.java:184,199`) applies identically to *both*
    /// quads — `buildFireQuad` takes no direction parameter and is called
    /// the same way for each sign of `translate`/`rotateY`. This is the
    /// opposite of the old design, where mirroring alternated *between*
    /// tiles purely so a repeating strip did not look copy-pasted; there is
    /// no such artifact to avoid with only two quads.
    #[test]
    fn fire_both_quads_use_the_same_mirrored_uv_mapping() {
        let tris = fire_overlay_triangles(0, 32);
        // Each quad's own first two verts: local-left corner, local-right
        // corner (see `fire_quad`'s corner order).
        let quad_a_u = [tris[0].uv[0], tris[1].uv[0]];
        let quad_b_u = [tris[6].uv[0], tris[7].uv[0]];
        assert_eq!(quad_a_u, [1.0, 0.0], "local-left samples u=1.0, local-right samples u=0.0");
        assert_eq!(quad_a_u, quad_b_u, "both quads use the identical mirrored mapping");
    }

    // -- freeze overlay (#139) -----------------------------------------

    #[test]
    fn freeze_overlay_covers_the_full_ndc_screen() {
        let tris = freeze_overlay_triangles(0.5);
        let xs: Vec<f32> = tris.iter().map(|v| v.position[0]).collect();
        let ys: Vec<f32> = tris.iter().map(|v| v.position[1]).collect();
        assert_eq!(xs.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
        assert_eq!(ys.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
    }

    #[test]
    fn freeze_overlay_alpha_tracks_percent_frozen() {
        for v in freeze_overlay_triangles(0.25) {
            assert!((v.color[3] - 0.25).abs() < 1e-6);
            assert_eq!(&v.color[0..3], &[1.0, 1.0, 1.0]);
        }
    }

    #[test]
    fn freeze_overlay_alpha_clamps_to_valid_range() {
        for v in freeze_overlay_triangles(-1.0) {
            assert_eq!(v.color[3], 0.0);
        }
        for v in freeze_overlay_triangles(2.0) {
            assert_eq!(v.color[3], 1.0);
        }
    }

    // -- spyglass overlay (#154) ----------------------------------------

    #[test]
    fn spyglass_lens_half_extent_matches_vanillas_min_dimension_rule() {
        // Landscape (aspect > 1): the smaller dimension (vertical) gets the
        // full scale; the larger (horizontal) is compressed by aspect.
        let (hw, hh) = spyglass_lens_half_extent(16.0 / 9.0);
        assert!((hh - SPYGLASS_SCALE).abs() < 1e-6);
        assert!((hw - SPYGLASS_SCALE / (16.0 / 9.0)).abs() < 1e-6);

        // Portrait (aspect < 1): mirrored.
        let (hw, hh) = spyglass_lens_half_extent(9.0 / 16.0);
        assert!((hw - SPYGLASS_SCALE).abs() < 1e-6);
        assert!((hh - SPYGLASS_SCALE * (9.0 / 16.0)).abs() < 1e-6);

        // Square: both axes equal, both exactly the scale.
        let (hw, hh) = spyglass_lens_half_extent(1.0);
        assert!((hw - SPYGLASS_SCALE).abs() < 1e-6);
        assert!((hh - SPYGLASS_SCALE).abs() < 1e-6);
    }

    #[test]
    fn spyglass_lens_is_centred_and_untinted() {
        let (hw, hh) = spyglass_lens_half_extent(16.0 / 9.0);
        let tris = spyglass_lens_triangles(16.0 / 9.0);
        let xs: Vec<f32> = tris.iter().map(|v| v.position[0]).collect();
        let ys: Vec<f32> = tris.iter().map(|v| v.position[1]).collect();
        assert!((xs.iter().cloned().fold(f32::INFINITY, f32::min) - -hw).abs() < 1e-6);
        assert!((xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max) - hw).abs() < 1e-6);
        assert!((ys.iter().cloned().fold(f32::INFINITY, f32::min) - -hh).abs() < 1e-6);
        assert!((ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max) - hh).abs() < 1e-6);
        for v in tris {
            assert_eq!(v.color, [1.0, 1.0, 1.0, 1.0]);
        }
    }

    #[test]
    fn spyglass_letterbox_is_opaque_black_and_tiles_the_screen_with_the_lens() {
        let aspect = 16.0 / 9.0;
        let (hw, hh) = spyglass_lens_half_extent(aspect);
        let bars = spyglass_letterbox_triangles(aspect);
        for v in bars {
            assert_eq!(v.color, [0.0, 0.0, 0.0, 1.0]);
        }
        // Every bar vertex must lie on the outer NDC edge or on the lens
        // boundary — nothing should stray inside the lens or outside NDC.
        for v in bars {
            let (x, y) = (v.position[0], v.position[1]);
            assert!(x >= -1.0 - 1e-6 && x <= 1.0 + 1e-6, "x out of NDC: {x}");
            assert!(y >= -1.0 - 1e-6 && y <= 1.0 + 1e-6, "y out of NDC: {y}");
            let inside_lens = x > -hw + 1e-4 && x < hw - 1e-4 && y > -hh + 1e-4 && y < hh - 1e-4;
            assert!(!inside_lens, "bar vertex ({x}, {y}) falls inside the lens rect");
        }
    }

    // -- confusion overlay (#144) ----------------------------------------

    #[test]
    fn confusion_overlay_always_covers_at_least_the_full_screen() {
        for strength in [0.01, 0.3, 0.7, 1.0] {
            let tris = confusion_overlay_triangles(strength);
            let xs: Vec<f32> = tris.iter().map(|v| v.position[0]).collect();
            let max_x = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            assert!(max_x >= 1.0 - 1e-6, "strength {strength}: max_x {max_x} must be >= 1.0");
        }
    }

    #[test]
    fn confusion_overlay_shrinks_toward_unscaled_as_strength_rises() {
        let weak = confusion_overlay_triangles(0.1);
        let strong = confusion_overlay_triangles(0.9);
        let weak_extent = weak.iter().map(|v| v.position[0].abs()).fold(0.0_f32, f32::max);
        let strong_extent = strong.iter().map(|v| v.position[0].abs()).fold(0.0_f32, f32::max);
        assert!(
            strong_extent < weak_extent,
            "higher nausea strength must shrink `size` toward 1.0 (Hud.java:1113): \
             weak={weak_extent}, strong={strong_extent}"
        );
    }

    #[test]
    fn confusion_overlay_tint_is_green_biased_and_scales_with_strength() {
        let strength = 0.5_f32;
        let tris = confusion_overlay_triangles(strength);
        for v in tris {
            assert!((v.color[0] - 0.2 * strength).abs() < 1e-6);
            assert!((v.color[1] - 0.4 * strength).abs() < 1e-6);
            assert!((v.color[2] - 0.2 * strength).abs() < 1e-6);
            assert_eq!(v.color[3], 1.0);
            assert!(v.color[1] > v.color[0] && v.color[1] > v.color[2], "green must dominate");
        }
    }

    // -- portal overlay (#149) --------------------------------------------

    #[test]
    fn portal_overlay_alpha_is_identity_at_full_intensity() {
        assert!((portal_overlay_alpha(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn portal_overlay_alpha_follows_vanillas_quartic_floor_curve() {
        // Hud.java:1097-1102: alpha = alpha^4 * 0.8 + 0.2, below 1.0.
        let intensity = 0.5_f32;
        let expected = intensity.powi(4) * 0.8 + 0.2;
        assert!((expected - 0.25).abs() < 1e-6, "hypothesis drifted: {expected}");
        assert!((portal_overlay_alpha(intensity) - expected).abs() < 1e-6);
        // The floor: even as intensity -> 0, alpha approaches 0.2, never 0 —
        // distinguishing this from a naive linear or unclamped curve.
        assert!((portal_overlay_alpha(0.0) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn portal_overlay_covers_the_full_ndc_screen_and_selects_the_right_frame() {
        let tris = portal_overlay_triangles(5, 32, 1.0);
        let xs: Vec<f32> = tris.iter().map(|v| v.position[0]).collect();
        let ys: Vec<f32> = tris.iter().map(|v| v.position[1]).collect();
        assert_eq!(xs.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
        assert_eq!(ys.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
        let v0 = 5.0 / 32.0;
        let v1 = 6.0 / 32.0;
        for v in tris {
            assert!(v.uv[1] >= v0 - 1e-6 && v.uv[1] <= v1 + 1e-6);
        }
    }

    #[test]
    fn portal_overlay_frame_wraps_past_the_last_frame() {
        let wrapped = portal_overlay_triangles(32, 32, 1.0);
        let first = portal_overlay_triangles(0, 32, 1.0);
        assert_eq!(wrapped, first, "frame 32 of a 32-frame strip is frame 0 again");
    }
}
