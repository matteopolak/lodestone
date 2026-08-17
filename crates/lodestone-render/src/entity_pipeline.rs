//! The entity render pass: an **instanced**, depth-tested pipeline that draws a
//! baked [`EntityMesh`] once per model type and reads
//! each visible entity's world transform from a per-instance matrix.
//!
//! This is the entity counterpart to [`ModelPipeline`](crate::model_pipeline).
//! It reuses the wide [`ModelVertex`] layout for the
//! mesh — so a baked mob shares vertex plumbing with baked blocks — but differs
//! in the one way entities require: the vertex position is transformed by a
//! per-instance `mat4x4` supplied through a second, `Instance`-step vertex
//! buffer. That is what makes a mob farm of hundreds of the same model a single
//! instanced draw with one small matrix per mob, rather than hundreds of
//! meshes.
//!
//! # Bindings and buffers
//!
//! * **Group 0**: [`EntityCameraUniform`] — the camera ([`CameraUniform`];
//!   only `view_proj` is read, `section_origin` is left zero because an entity's
//!   world position lives in its instance matrix) **followed by this frame's
//!   [`FogUniform`]**. Fog is folded in here rather than given its own bind
//!   group, matching [`ModelCameraUniform`](crate::model_pipeline::ModelCameraUniform):
//!   the fog block must travel with the camera anyway, and one uniform means
//!   the entity pass can never drift out of step with the terrain pass's fog.
//! * **Group 1**: the entity's texture sheet + sampler.
//! * **Vertex buffer 0**: [`ModelVertex`] (locations 0–3; the shader reads
//!   position and UV).
//! * **Vertex buffer 1**: [`EntityInstanceRaw`] (locations 4–7 = the four columns
//!   of the model matrix, location 8 = the packed light byte, location 9 = the
//!   packed dye tint + hurt-overlay word, location 10 = the creeper white-flash
//!   overlay byte), stepped per instance.
//!
//! # Shading: world light per instance, direction per fragment
//!
//! A mob's brightness has two independent factors and they are applied in
//! different spaces for a reason:
//!
//! 1. **World light**, one packed sky/block byte per *instance*. Vanilla samples
//!    the lightmap once per entity at its block position, so a mob is uniformly
//!    lit by the block it stands in; this shader reproduces terrain's light term
//!    (vanilla's `lightmap.fsh` curve — see [`crate::light`]) from that byte.
//!    Without it a mob renders full-bright and out-shines the terrain around it
//!    by up to an order of magnitude at night — the reported "mobs are super
//!    bright, blocks are dark" defect, in which nothing was wrong with the
//!    blocks.
//! 2. **Direction.** [`ModelVertex`] carries no normal, so the fragment shader
//!    reconstructs a face normal from screen-space derivatives of the
//!    interpolated world position (`-cross(dpdx, dpdy)`, negated to point at the
//!    eye) and applies **vanilla's two-light diffuse**: `min(1, (max(0, n·L0) +
//!    max(0, n·L1)) * 0.6 + 0.4)` with `L0/L1` from
//!    `blaze3d.platform.Lighting.DIFFUSE_LIGHT_0/1`. The negation is what makes
//!    the double-sided raster state below safe — entity meshes are drawn without
//!    back-face culling (robust visibility while per-model winding parity is
//!    still being pixel-verified), and taking the eye-facing side is exactly what
//!    vanilla's front/back pair in `entity.vsh` resolves to.
//!
//!    This was **one** light and an `abs()` until issue #383. That formula lights
//!    a face pointing away from the light as brightly as one pointing into it
//!    (up and down both `0.9085`, vanilla `1.0` and `0.4`) and bottoms out at
//!    `0.4` on every normal *perpendicular* to its single direction. Box faces
//!    never land on that band, so standing mobs looked passable; the rotated
//!    first-person arm sat at `0.497` across 97% of its pixels, which is the dark
//!    side a player reported. See `entity_diffuse_two_lights_pixels.rs` and
//!    `docs/entity-rendering.md`.
//!
//! Their product is multiplied into the texel in **gamma space**, through the
//! same `srgb_to_linear(linear_to_srgb(rgb) * shade)` round-trip the model
//! shader uses. Vanilla is not colour-managed and multiplies shade into gamma
//! byte values; doing it in linear light and re-encoding pulls every factor
//! toward 1.0 (a shade of 0.6 reads as 0.79), which is the washed-out look
//! `4e8f058` removed from terrain. Entities carried the same bug afterwards.
//!
//! # Texture format is part of the brightness
//!
//! The sheet bound to group 1 must be an **`_srgb`** format, like the block
//! atlas. A vanilla PNG holds gamma-encoded bytes; binding it as plain `Unorm`
//! hands the shader `0.50` where the linear value is `0.21`, and the sRGB render
//! target then encodes it a *second* time — a measured **+48%** on every mob
//! pixel, enough on its own to make a mob brighter than the brightest sunlit
//! block face.

use wgpu::util::DeviceExt;

use crate::block::{CameraUniform, DEPTH_FORMAT};
use crate::entity::EntityMesh;
use crate::models::ModelVertex;

/// A per-instance entity record for the instance vertex buffer: a column-major
/// `mat4x4<f32>` laid out as four `vec4` attributes, the entity's packed
/// sky/block light byte, and a per-instance tint.
///
/// Light rides the *instance* buffer, not the vertex buffer, because the vertex
/// buffer is shared by every instance of a model type — a per-vertex light byte
/// could only ever say one thing for all mobs of that kind. Vanilla's own
/// lightmap sample is per entity, so this is also the faithful granularity. The
/// tint rides here for the same reason and at the same granularity: vanilla's
/// `submitModel(model, state, pose, renderType, light, overlay, color, …)` takes
/// one `color` per submitted model, and dyed leather armour is the case that
/// needs it.
///
/// # Why the instance buffer and not a fifth bind group
///
/// Because a bind group is the one resource this pass cannot afford. The model
/// shader is at wgpu's default `max_bind_groups` of 4 and a fifth group compiles
/// on an M5 (which reports 8) while crashing at startup on any 4-group adapter —
/// see `CLAUDE.md`. A vertex attribute has no such ceiling: this adds location 9
/// to a buffer that already exists.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EntityInstanceRaw {
    /// The model→world matrix, column-major (four columns of four floats).
    pub model: [[f32; 4]; 4],
    /// Packed sky/block light, `sky << 4 | block` (`0..=15` each), widened to
    /// `u32` for the `Uint32` vertex attribute. Same encoding as
    /// [`ModelVertex::light`](crate::models::ModelVertex::light), so the entity
    /// and model shaders unpack it with identical code.
    pub light: u32,
    /// Packed `AARRGGBB`: bits 0–23 are the **gamma-space** tint, multiplied
    /// into the texel exactly as before; bits 24–31 are the hurt/death overlay
    /// alpha, added on top ([`HURT_OVERLAY_ALPHA_BYTE`] when set, `0` when not).
    /// [`NO_TINT`] (white, no overlay) is what every mob passes by default.
    ///
    /// The overlay byte rides in the tint word's previously-unused top byte
    /// rather than a new vertex attribute, for the same reason fog rides in the
    /// group-0 camera uniform: this shader is at wgpu's 4-bind-group floor (see
    /// `CLAUDE.md`), and this instance buffer already has a spare byte sitting
    /// idle in every existing tint value, so widening the *meaning* of one
    /// `Uint32` costs nothing a new attribute would.
    ///
    /// Gamma space, not linear: vanilla is not colour-managed and its vertex
    /// colour multiplies the gamma-encoded texel byte. The shader therefore
    /// folds the tint multiply into the *same* `srgb_to_linear(linear_to_srgb(rgb)
    /// * …)` round-trip the directional and world-light shades already use, and
    /// blends the overlay in that same gamma-space stage. Doing either in linear
    /// light pulls the factor toward 1.0 and washes the result out.
    pub tint: u32,
    /// A creeper's white-flash overlay alpha byte (low 8 bits; the rest are
    /// unused today), `0` when absent. This is a **separate** attribute from
    /// [`tint`](Self::tint)'s hurt-overlay byte rather than a third meaning
    /// packed into the same word, because the two are genuinely independent
    /// channels on vanilla's own `OverlayTexture`: the red (hurt) row and the
    /// white (flash) row are different `v` coordinates into the same lookup
    /// texture, selected by `hasRedOverlay`, and `tint`'s spare byte was
    /// already fully claimed by the boolean red gate. See
    /// [`crate::entity_anim::creeper_white_overlay_progress`] for the value
    /// this is derived from and
    /// [`creeper_overlay_alpha_from_progress`] for the derivation.
    ///
    /// **Mutually exclusive with the red overlay, and the shader enforces
    /// it**: vanilla's `OverlayTexture` puts red and white on different rows
    /// of one lookup (`y < 8` is always flat red regardless of `u`), so a
    /// creeper that is somehow both hurt and swelling in the same frame shows
    /// red, never a blend of the two — exactly `entity.wgsl`'s priority order.
    pub white_overlay: u32,
}

/// The `tint` value meaning "leave the texel alone": opaque white, no overlay.
pub const NO_TINT: u32 = 0x00FF_FFFF;

/// The hurt/death overlay's alpha byte, packed into `tint`'s bits 24–31.
///
/// Vanilla's `OverlayTexture` (`net/minecraft/client/renderer/texture/
/// OverlayTexture.java`) bakes a 16×16 lookup texture whose `y < 8` (red) row is
/// a flat `ARGB.color(...)` of `-1291911168` for every `x` — i.e. `(178, 255, 0,
/// 0)` — sampled whenever `LivingEntityRenderer.java:281` sets
/// `state.hasRedOverlay = entity.hurtTime > 0 || entity.deathTime > 0`. `178` is
/// that overlay's alpha byte; `255, 0, 0` is pure red, which is why the blend
/// below mixes against a literal `vec3(1.0, 0.0, 0.0)` rather than reading a
/// colour out of this word.
///
/// **`178` is how much of the entity's own colour survives, not how much red is
/// added.** Vanilla's `entity.fsh:57` is
/// `mix(overlayColor.rgb, color.rgb, overlayColor.a)`, so the alpha is the weight
/// on `color`, giving roughly 30% red. This comment previously described the
/// constant correctly and its *role* not at all, and the shader below was written
/// with the arguments the other way round — 70% red (issue #371). If you are
/// tempted to tune this number because the flash looks wrong, check the argument
/// order first.
pub const HURT_OVERLAY_ALPHA_BYTE: u32 = 178;

impl EntityInstanceRaw {
    /// Pack a [`glam::Mat4`] into the instance format (column-major), lit
    /// full-bright and untinted. Kept for callers with no world to sample.
    #[must_use]
    pub fn from_mat4(m: glam::Mat4) -> Self {
        Self::new(m, u32::from(crate::entity::ENTITY_FULLBRIGHT))
    }

    /// Pack a transform and a packed sky/block light byte into the instance
    /// format (column-major), untinted.
    #[must_use]
    pub fn new(m: glam::Mat4, light: u32) -> Self {
        Self {
            model: m.to_cols_array_2d(),
            light,
            tint: NO_TINT,
            white_overlay: 0,
        }
    }

    /// Set this instance's packed `0x00RRGGBB` gamma-space tint.
    ///
    /// Builder-style for the same reason [`EntityInstance::with_light`] is: only
    /// dyed armour has anything to pass, and every other caller wants
    /// [`NO_TINT`].
    ///
    /// [`EntityInstance::with_light`]: crate::entity::EntityInstance::with_light
    #[must_use]
    pub fn with_tint(mut self, rgb: [u8; 3]) -> Self {
        self.tint = (self.tint & 0xFF00_0000)
            | (u32::from(rgb[0]) << 16)
            | (u32::from(rgb[1]) << 8)
            | u32::from(rgb[2]);
        self
    }

    /// Set or clear the hurt/death red overlay (bits 24–31 of `tint`).
    ///
    /// Vanilla's gate is boolean, not a fade: `hasRedOverlay = entity.hurtTime
    /// > 0 || entity.deathTime > 0` (`LivingEntityRenderer.java:281`) — no
    /// interpolation by how much of `hurtTime` remains, so this takes a `bool`
    /// rather than a `0.0..=1.0` strength. Builder-style, like [`with_tint`],
    /// so a caller that also dyes leather can chain both without either
    /// clobbering the other's bits.
    ///
    /// [`with_tint`]: Self::with_tint
    #[must_use]
    pub fn with_hurt_overlay(mut self, active: bool) -> Self {
        let alpha = if active { HURT_OVERLAY_ALPHA_BYTE } else { 0 };
        self.tint = (self.tint & 0x00FF_FFFF) | (alpha << 24);
        self
    }

    /// Set or clear the creeper white-flash overlay (see [`Self::white_overlay`]).
    /// `alpha_byte` is vanilla's `OverlayTexture` alpha — `0` clears it; a
    /// non-zero byte is what [`creeper_overlay_alpha_from_progress`] returns for
    /// an active blink. Builder-style, like [`with_tint`]/[`with_hurt_overlay`].
    ///
    /// [`with_tint`]: Self::with_tint
    #[must_use]
    pub fn with_creeper_white_overlay(mut self, alpha_byte: u8) -> Self {
        self.white_overlay = u32::from(alpha_byte);
        self
    }

    /// The instance-stepped vertex-buffer layout: four `Float32x4` columns at
    /// shader locations 4–7, the packed light `Uint32` at location 8, the
    /// packed tint/hurt-overlay `Uint32` at location 9, and the creeper
    /// white-overlay `Uint32` at location 10.
    #[must_use]
    pub const fn instance_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 7] = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 0,
                shader_location: 4,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 16,
                shader_location: 5,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 32,
                shader_location: 6,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 48,
                shader_location: 7,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 64,
                shader_location: 8,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 68,
                shader_location: 9,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 72,
                shader_location: 10,
            },
        ];
        wgpu::VertexBufferLayout {
            array_stride: core::mem::size_of::<EntityInstanceRaw>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }
}

// ---------------------------------------------------------------------------
// Mob fire (issue #434 — player report: "mobs dont show flames yet")
// ---------------------------------------------------------------------------

/// The mob-fire billboard's geometry, derived from vanilla's
/// `FlameFeatureRenderer.prepare`
/// (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/feature/
/// FlameFeatureRenderer.java:29-66`) — **modelled**, not transliterated: this
/// is a clean re-derivation of the same rule in idiomatic Rust, with every
/// constant cited back to the line it comes from.
///
/// # The rule
///
/// One camera-yaw-billboarded column of quads, stacked from the entity's feet
/// upward, shrinking and receding as it rises:
///
/// * `s = width * 1.4` (`:32`) is the whole billboard's scale. **It is not
///   folded into the geometry here.** Every position [`flame_quads`] emits is
///   in vanilla's *pre-scale* local units, exactly as they are written in
///   `prepare` before `pose.scale(s, s, s)` runs, and the caller applies `s` as
///   a uniform scale in the instance matrix — [`flame_instance_matrix`] is the
///   one place that happens.
///
///   That split is deliberate and it is what makes one baked mesh per entity
///   *type* correct: the mesh's shape depends only on the ratio
///   `h = height / s`, which is invariant under a uniform hitbox scale, so a
///   baby and an adult of the same type share this geometry and differ only in
///   `s`. An earlier version of this doc claimed the scale was multiplied
///   through "at the end" and it never was, which left every flame `1/s` times
///   too large — worst on a wide mob, where `s` is furthest from `1`.
/// * `h = height / s` (`:36`) is how many scaled units tall the stack has to
///   fill; the loop below runs once per `0.45`-unit slice of it (`:60`),
///   which is [`FLAME_QUAD_HEIGHT`]'s value.
/// * Quad `ss` (0-indexed, `:41`, `:64`) has half-width `r`, starting at `0.5`
///   (`:34`) and shrinking by `×0.9` per quad (`:62`) — [`FLAME_WIDTH_DECAY`].
/// * Quad `ss`'s vertical span is `[-yo, 1.4-yo]` (`:56-59`), with `yo`
///   decreasing by `0.45` per quad (`:61`) — the same `0.45` as the height
///   step, so consecutive quads overlap by `1.4 - 0.45 = 0.95` scaled units
///   rather than tiling edge-to-edge.
/// * Quad `ss`'s local Z is a constant pose-level push-back,
///   `0.3 - i32(h_initial) * 0.02` (`:39`), plus a further `-0.03` per quad
///   (`:63`) — so later, smaller quads sit measurably behind earlier ones,
///   which is what keeps the stack from z-fighting itself.
/// * Quad `ss` alternates texture (`ss % 2 == 0` → `fire_0`, else `fire_1`,
///   `:45`) and flips its U mapping every *other pair* of quads
///   (`ss / 2 % 2 == 0`, `:50-54`).
///
/// # What this does not model
///
/// The **animation frame** (which of the 32 stacked rows in `fire_0`/`fire_1`
/// is current) is deliberately absent from this struct — it is the one
/// per-tick-varying quantity, carried instead by [`FlameInstanceRaw::frame`]
/// so a static, once-baked mesh can still animate. See [`FlameVertex::uv`]'s
/// doc for the exact contract between what is baked here and what the shader
/// adds at draw time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlameVertex {
    /// Position in the flame's own local space: `+Y` up from the entity's feet,
    /// `+Z` the direction the stack leans *toward the camera*.
    ///
    /// **Not** scaled by `s` — see [`flame_quads`]'s doc for why the scale
    /// belongs to [`flame_instance_matrix`] instead. `+Z` being toward the
    /// camera is what makes the sign of the billboard rotation load-bearing:
    /// the whole stack steps forward in `z` and insets laterally as it rises, so
    /// it is *not* symmetric under a sign flip.
    pub position: [f32; 3],
    /// `[u, v_local]`, **not** a complete UV pair on its own:
    ///
    /// * `u` is the complete, final U into the combined 32-wide flame texture
    ///   ([`lodestone_assets::entity_flame::combine_flame_halves`]) —
    ///   `0.0..0.5` for a `fire_0` vertex, `0.5..1.0` for `fire_1`, already
    ///   carrying this quad's horizontal flip. Fully baked; the shader reads
    ///   it unchanged.
    /// * `v_local` is only `0.0` (top of whichever frame cell is current) or
    ///   `1.0` (bottom) — it does **not** know which of the 32 frames is
    ///   current. The shader combines it with [`FlameInstanceRaw::frame`] as
    ///   `v = (frame + v_local) / 32.0`. Baking the animation into the mesh
    ///   itself would mean re-uploading every flame mesh on every frame
    ///   advance; carrying only the two endpoints here and letting the
    ///   *instance* (which already changes every frame for every moving mob)
    ///   supply the frame index costs nothing extra against the pipeline's
    ///   existing per-instance-attribute budget.
    pub uv: [f32; 2],
}

/// One quad of the mob-fire billboard, in [`FlameVertex`]'s local space,
/// wound bottom-left → bottom-right → top-right → top-left (matching every
/// other baked quad in this crate — see `push_part_quads`'s doc in
/// `entity.rs`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlameQuad {
    /// The four corners, in winding order.
    pub vertices: [FlameVertex; 4],
    /// `true` selects the right half of the combined flame texture
    /// (`fire_1`), `false` the left half (`fire_0`) —
    /// `FlameFeatureRenderer.java:45`'s `ss % 2 == 0 ? fire1 : fire2` (vanilla
    /// names its *first* alternate `fire1` for the sprite `ModelBakery.FIRE_0`
    /// resolves to — the naming looks swapped at a glance and is not; `fire1`
    /// the local variable and `FIRE_0` the sprite are the same texture).
    pub fire_1: bool,
}

/// Vertical size of one flame quad in scaled local units
/// (`FlameFeatureRenderer.java:58-59`'s `1.4`), and simultaneously the step
/// `yo`/`h` advance by per quad (`:60-61`) — vanilla reuses one literal for
/// both, so consecutive quads overlap by `1.4 - 0.45 = 0.95` rather than
/// tiling edge to edge.
const FLAME_QUAD_HEIGHT: f32 = 1.4;
const FLAME_STEP: f32 = 0.45;
const FLAME_INITIAL_HALF_WIDTH: f32 = 0.5;
const FLAME_WIDTH_DECAY: f32 = 0.9;
const FLAME_Z_STEP: f32 = 0.03;
const FLAME_SCALE_FACTOR: f32 = 1.4;
/// A safety cap absent from vanilla's own unguarded `while` loop
/// (`FlameFeatureRenderer.java:44`): a pathological `(width, height)` pair —
/// zero width, or a height orders of magnitude beyond any real vanilla entity
/// — must produce a bounded mesh rather than an unbounded allocation. No real
/// `lodestone_data::entity_dimensions` entry comes close to needing this many
/// quads (the tallest, the ender dragon at height 8, needs single digits).
const MAX_FLAME_QUADS: usize = 64;

/// Vanilla's `FlameFeatureRenderer.prepare` (see [`FlameQuad`]'s doc for the
/// full derivation), for an entity whose **base** hitbox is `width × height`
/// blocks (`lodestone_data::entity_dimensions::base_dimensions` — vanilla's
/// `state.boundingBoxWidth`/`boundingBoxHeight`, `EntityRenderer.java:168-169`,
/// i.e. `Entity.getBbWidth()`/`getBbHeight()`, not this crate's own baked mesh
/// AABB).
///
/// Empty for a non-positive `width` (nothing to scale by) or an already
/// non-positive `height` — both degenerate inputs vanilla's own loop would
/// simply not enter (`while (h > 0.0F)`) — and capped at
/// [`MAX_FLAME_QUADS`] for anything else.
#[must_use]
pub fn flame_quads(width: f32, height: f32) -> Vec<FlameQuad> {
    if !(width > 0.0) || !(height > 0.0) {
        return Vec::new();
    }
    let s = width * FLAME_SCALE_FACTOR; // `:32`
    let h_initial = height / s; // `:36`
    // The pose-level push-back applied once, before any quad — `:39`. Vanilla
    // truncates toward zero (an `int` cast on a positive float), matching
    // `as i32` here.
    let base_z = 0.3 - (h_initial as i32) as f32 * 0.02;

    let mut quads = Vec::with_capacity(8);
    let mut h = h_initial;
    let mut r = FLAME_INITIAL_HALF_WIDTH; // `:34`
    let mut yo = 0.0f32; // `:37`
    let mut zo = 0.0f32; // `:40`, the loop-local component added to `base_z`
    let mut ss: u32 = 0;
    while h > 0.0 && quads.len() < MAX_FLAME_QUADS {
        let fire_1 = ss % 2 != 0; // `:45`
        let flip = (ss / 2) % 2 == 0; // `:50`
        // At `+r` (right edge): `u0` unflipped, `u1` flipped. At `-r` (left
        // edge): the opposite. See `FlameQuad::fire_1`'s doc for why vanilla's
        // baseline (unflipped) orientation is itself already mirrored.
        let (u_right, u_left) = if flip { (1.0, 0.0) } else { (0.0, 1.0) };
        let half_base = if fire_1 { 0.5 } else { 0.0 };
        let u_right = half_base + u_right * 0.5;
        let u_left = half_base + u_left * 0.5;
        let z = base_z + zo;
        let y0 = -yo; // bottom, `:56-57`
        let y1 = FLAME_QUAD_HEIGHT - yo; // top, `:58-59`
        // Bottom vertices sample `v1` (bottom of the frame cell, local 1.0),
        // top vertices sample `v0` (top of the frame cell, local 0.0) —
        // `:56-59`'s vertex argument order, carried into `FlameVertex::uv`'s
        // `v_local` convention.
        quads.push(FlameQuad {
            vertices: [
                FlameVertex {
                    position: [-r, y0, z],
                    uv: [u_left, 1.0],
                },
                FlameVertex {
                    position: [r, y0, z],
                    uv: [u_right, 1.0],
                },
                FlameVertex {
                    position: [r, y1, z],
                    uv: [u_right, 0.0],
                },
                FlameVertex {
                    position: [-r, y1, z],
                    uv: [u_left, 0.0],
                },
            ],
            fire_1,
        });
        h -= FLAME_STEP; // `:60`
        yo -= FLAME_STEP; // `:61`
        r *= FLAME_WIDTH_DECAY; // `:62`
        zo -= FLAME_Z_STEP; // `:63`
        ss += 1;
    }
    quads
}

/// The camera-yaw billboard a flame is rotated by — vanilla's
/// `Mth.rotationAroundAxis(Mth.Y_AXIS, camera.orientation, …)`
/// (`EntityRenderDispatcher.java:164`), reduced to a matrix.
///
/// # The sign is not free, and reasoning it away is how it shipped wrong
///
/// `rotationAroundAxis` is a swing/twist decomposition: it keeps `(0, q.y, 0,
/// q.w)` and normalises, which for `Camera.setRotation`'s
/// `rotationYXZ(PI - yaw, -pitch, 0)` is exactly `Ry(PI - yaw)` — the pitch term
/// contributes nothing after the projection, so this takes a yaw and not a whole
/// camera. Hence the `PI -` here: **not** `Ry(yaw)`, and **not** `Ry(-yaw)`
/// either. The two differ by a further half turn.
///
/// An earlier version of this pass used `Ry(yaw)` and argued the sign could not
/// matter because entity draws are double-sided (`cull_mode: None`), so a flat
/// billboard reads the same face-on either way. **The flame is not a flat
/// billboard** — [`flame_quads`] emits a stack that both steps forward in `z`
/// and insets laterally as it rises, and a stack with depth and lateral
/// asymmetry is not sign-symmetric. With the sign wrong the flame counter-rotates
/// as you orbit instead of following, so it looks right from one side and
/// displaced from the other. That is a depth-order symptom, which is exactly what
/// `cull_mode: None` turns a bad transform into.
///
/// The derivable invariant, and what [`flame_instance_matrix`]'s gate asserts:
/// the local `+Z` this rotation maps must point **toward** the camera, i.e.
/// against [`crate::Camera::forward`]. Derive it from a real camera; do not
/// assert a polarity.
#[must_use]
pub fn flame_billboard_rotation(camera_yaw_deg: f32) -> glam::Mat4 {
    glam::Mat4::from_rotation_y(core::f32::consts::PI - camera_yaw_deg.to_radians())
}

/// The uniform scale a flame is drawn at — vanilla's
/// `s = state.boundingBoxWidth * 1.4` (`FlameFeatureRenderer.java:32`).
///
/// `bb_width` is the entity's **own** hitbox width, not its type's: vanilla reads
/// `EntityRenderState.boundingBoxWidth`, which is `Entity.getBbWidth()` after
/// `getDimensions().scale(getAgeScale())`, so a baby's is half its type's. That
/// is the whole of the baby-flame fix — see [`flame_instance_matrix`].
#[must_use]
pub fn flame_scale(bb_width: f32) -> f32 {
    bb_width * FLAME_SCALE_FACTOR
}

/// The model→world matrix for one flame instance:
/// `translate(feet) · scale(s) · billboard`, matching
/// `FlameFeatureRenderer.prepare`'s own `pose.scale(s, s, s)` then
/// `pose.rotate(rotation)` order.
///
/// `bb_width` is the entity's own hitbox width (see [`flame_scale`]). The
/// per-quad push-back `0.3 - (int)h * 0.02` is already inside the baked
/// geometry, so vanilla's third step (`pose.translate(0, 0, …)`) has no
/// counterpart here.
///
/// # Why the size is per **instance** and the mesh is per **type**
///
/// Two things vary with an entity's hitbox in vanilla: the uniform scale `s`, and
/// the number of stacked quads, which comes from `h = height / s`. Under a
/// *uniform* box scale — which is what an age scale is — `h` is invariant:
/// halving both width and height leaves `height / (width · 1.4)` unchanged. So a
/// baby and an adult of one type share a layer count and differ only in `s`.
///
/// That is why this is a per-instance scale rather than a per-entity mesh, and
/// why one mesh per entity type is still correct. The layer count genuinely does
/// vary — but across **aspect ratios**, not across ages: a spider (wide and low)
/// gets far fewer quads than a zombie, and both keep their count when they are
/// babies. A "babies get fewer layers" rule would be a second, wrong change on
/// top of this one.
#[must_use]
pub fn flame_instance_matrix(feet: glam::Vec3, camera_yaw_deg: f32, bb_width: f32) -> glam::Mat4 {
    glam::Mat4::from_translation(feet)
        * glam::Mat4::from_scale(glam::Vec3::splat(flame_scale(bb_width)))
        * flame_billboard_rotation(camera_yaw_deg)
}

/// [`flame_quads`], baked into a [`ModelVertex`]/index pair a
/// [`GpuEntityModel::upload_parts`] call can upload directly — the same
/// vertex type every other baked entity mesh in this crate uses, so the flame
/// pass shares `ModelVertex::vertex_layout()` (locations 0..=3) with the mob
/// body pass; only the *instance* format
/// ([`FlameInstanceRaw`] vs. [`EntityInstanceRaw`]) differs between the two
/// pipelines. The fields [`ModelVertex`] carries that flame has no use for
/// (`ao`, `light`, `tint`, `anim`, `tint_rgb_override`) are set to their
/// inert/untinted defaults — `vs_main_flame`/`fs_main_flame` never read them,
/// exactly as `vs_main`/`fs_main` already never read `ModelVertex::light`
/// meaningfully (see `push_part_quads`'s doc).
///
/// Wound the same way every other baked quad here is: two triangles,
/// `(0, 1, 2)` and `(0, 2, 3)`, over the bottom-left/bottom-right/top-right/
/// top-left order [`flame_quads`] already emits.
#[must_use]
pub fn flame_mesh(width: f32, height: f32) -> (Vec<ModelVertex>, Vec<u32>) {
    let quads = flame_quads(width, height);
    let mut vertices = Vec::with_capacity(quads.len() * 4);
    let mut indices = Vec::with_capacity(quads.len() * 6);
    for quad in &quads {
        let base = vertices.len() as u32;
        for v in &quad.vertices {
            vertices.push(ModelVertex {
                position: v.position,
                uv: v.uv,
                ao: 1.0,
                light: 0,
                tint: 255,
                anim: 0,
                cutout_bypass: 0,
                tint_rgb_override: [0, 0, 0, 0],
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (vertices, indices)
}

/// One per-instance record for the flame pass's instance buffer: a model
/// matrix and the current animation frame — nothing else.
///
/// Deliberately **not** [`EntityInstanceRaw`]: flame carries no per-instance
/// light (vanilla forces full-bright block light, `LightCoordsUtil.withBlock(
/// state.lightCoords, 15)`, `FlameFeatureRenderer.java:42`), no tint (a flat
/// vertex colour `-1`, `:71`) and no hurt/creeper overlay (fire is not a mob
/// layer vanilla's `OverlayTexture` ever touches) — carrying three unused
/// attributes per instance for a value that is *always* the same constant
/// would be pure waste, and would invite a future caller to wire a tint that
/// vanilla's own flame never has.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FlameInstanceRaw {
    /// The model→world matrix, column-major — feet position composed with the
    /// camera-yaw-only billboard rotation
    /// (`Mth.rotationAroundAxis(Mth.Y_AXIS, camera.orientation, …)`,
    /// `EntityRenderDispatcher.java:163`). Building that rotation is the
    /// caller's job (it needs the camera, which this crate's pure geometry
    /// functions deliberately do not take) — see `docs/entity-rendering.md`'s
    /// "Mob fire" section.
    pub model: [[f32; 4]; 4],
    /// Which of the 32 stacked rows in the combined flame texture is current
    /// this frame — the same value for every flame instance drawn in one
    /// frame, duplicated per instance because a per-instance attribute has no
    /// bind-group cost (see [`EntityInstanceRaw::light`]'s doc for the same
    /// argument made about a different field). Combined with
    /// [`FlameVertex::uv`]'s `v_local` in `vs_main_flame`.
    pub frame: u32,
}

impl FlameInstanceRaw {
    /// Pack a model matrix and animation frame into the instance format.
    #[must_use]
    pub fn new(model: glam::Mat4, frame: u32) -> Self {
        Self {
            model: model.to_cols_array_2d(),
            frame,
        }
    }

    /// The instance-stepped vertex-buffer layout: four `Float32x4` matrix
    /// columns at locations 4-7 (matching [`EntityInstanceRaw::instance_layout`]'s
    /// own first four attributes byte-for-byte, since both are "a `glam::Mat4`
    /// column-major"), and the frame index as a `Uint32` at location 8.
    #[must_use]
    pub const fn instance_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 5] = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 0,
                shader_location: 4,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 16,
                shader_location: 5,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 32,
                shader_location: 6,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 48,
                shader_location: 7,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 64,
                shader_location: 8,
            },
        ];
        wgpu::VertexBufferLayout {
            array_stride: core::mem::size_of::<FlameInstanceRaw>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }
}

/// Derives vanilla's `OverlayTexture` alpha byte from a creeper's white-flash
/// progress (`0.0..=1.0`, [`crate::entity_anim::creeper_white_overlay_progress`]).
///
/// Transcribed from the decompiled 26.2 client's `OverlayTexture` constructor:
/// the white row (`y >= 8`) at column `x` holds alpha
/// `(1.0 - x / 15.0 * 0.75) * 255.0`, and `OverlayTexture.u(progress)` selects
/// `x = (int)(progress * 15.0)` — so this is that same two-step quantise then
/// derive, not a continuous formula in `progress`. The quantisation matters:
/// vanilla's overlay is a **16-column lookup texture**, not a shader-computed
/// gradient, so a real client's alpha visibly steps between 15 discrete levels
/// rather than fading continuously, and reproducing that stepping (rather than
/// deriving alpha straight from `progress` with no floor) is what makes this
/// byte-match a real frame instead of merely "close".
///
/// `progress == 0.0` returns `0`, which this crate's convention (matching
/// [`EntityInstanceRaw::with_hurt_overlay`]) reads as "absent" rather than the
/// literal vanilla alpha at `x = 0` (`255`, fully transparent white / no
/// visible tint) — the two are visually identical (`mix(white, colour, 1.0) ==
/// colour` either way), so collapsing them costs nothing and gives the shader
/// one sentinel instead of two representations of "no effect".
/// [`crate::entity_anim::creeper_white_overlay_progress`] never returns a value
/// in `(0.0, 0.5)` (the "off" bucket of the blink is exactly `0.0`), so this
/// collapse never discards a real non-zero-but-near-zero progress.
#[must_use]
pub fn creeper_overlay_alpha_from_progress(progress: f32) -> u8 {
    if progress <= 0.0 {
        return 0;
    }
    let u = (progress.clamp(0.0, 1.0) * 15.0) as i32;
    let alpha = (1.0 - f32::from(u as i16) / 15.0 * 0.75) * 255.0;
    alpha.round().clamp(1.0, 255.0) as u8
}

/// GPU-resident geometry for one entity model type: a vertex buffer, an index
/// buffer, and the index count. Uploaded once; every instance of the model
/// reuses it.
#[derive(Debug)]
pub struct GpuEntityModel {
    /// Vertex buffer of [`ModelVertex`].
    pub vertices: wgpu::Buffer,
    /// `u32` index buffer.
    pub indices: wgpu::Buffer,
    /// Number of indices to draw (all parts).
    pub index_count: u32,
    /// One index sub-range per skeleton part, in mesh part order. Drawing part
    /// `p` instanced over that part's matrices is what animates a limb.
    pub parts: Vec<crate::entity::PartRange>,
}

impl GpuEntityModel {
    /// Upload an [`EntityMesh`], or `None` if it is empty (nothing to draw).
    #[must_use]
    pub fn upload(device: &wgpu::Device, mesh: &EntityMesh) -> Option<Self> {
        if mesh.indices.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuEntityModel {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
            parts: mesh.parts.clone(),
        })
    }

    /// Upload a [`BlockEntityMesh`](crate::block_entity::BlockEntityMesh), or
    /// `None` if empty.
    ///
    /// Takes the three pieces rather than the mesh type because a block-entity
    /// mesh differs from an [`EntityMesh`] only in what lives *beside* the
    /// buffers on the CPU (a part hierarchy with pose overrides instead of a
    /// slot-based [`Skeleton`](crate::entity_anim::Skeleton)). The GPU-resident
    /// shape is identical, so the alternative would be either a second copy of
    /// this buffer-creation code or a `BlockEntityMesh → EntityMesh` conversion
    /// that fabricates a skeleton nothing reads.
    #[must_use]
    pub fn upload_parts(
        device: &wgpu::Device,
        vertices: &[crate::models::ModelVertex],
        indices: &[u32],
        parts: Vec<crate::entity::PartRange>,
    ) -> Option<Self> {
        if indices.is_empty() {
            return None;
        }
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-block-entity-vertices"),
            contents: bytemuck::cast_slice(vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-block-entity-indices"),
            contents: bytemuck::cast_slice(indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuEntityModel {
            vertices: vertex_buffer,
            indices: index_buffer,
            index_count: indices.len() as u32,
            parts,
        })
    }

    /// Upload an [`ArmourMesh`](crate::entity::ArmourMesh), or `None` if empty.
    ///
    /// `parts` carries the ranges in the mesh's own order, with the part *names*
    /// left behind on the CPU side: an armour draw gets its ranges from
    /// [`ArmourMesh::attach`](crate::entity::ArmourMesh::attach), which pairs
    /// each range with the wearer's part index, so the GPU struct never needs to
    /// be indexed by name.
    #[must_use]
    pub fn upload_armour(device: &wgpu::Device, mesh: &crate::entity::ArmourMesh) -> Option<Self> {
        if mesh.indices.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-armour-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-armour-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuEntityModel {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
            parts: mesh.parts.iter().map(|(_, r)| *r).collect(),
        })
    }

    /// Upload a [`WoolMesh`](crate::entity::WoolMesh), or `None` if empty.
    ///
    /// Mirrors [`upload_armour`](Self::upload_armour) exactly — the same
    /// `parts`-carries-ranges-only, names-left-on-the-CPU shape, since a wool
    /// draw likewise gets its ranges from
    /// [`WoolMesh::attach`](crate::entity::WoolMesh::attach), which pairs each
    /// range with the wearer's part index.
    #[must_use]
    pub fn upload_wool(device: &wgpu::Device, mesh: &crate::entity::WoolMesh) -> Option<Self> {
        if mesh.indices.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-wool-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-wool-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuEntityModel {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
            parts: mesh.parts.iter().map(|(_, r)| *r).collect(),
        })
    }

    /// Upload a [`CapeMesh`](crate::entity::CapeMesh), or `None` if empty.
    ///
    /// Mirrors [`upload_wool`](Self::upload_wool) exactly — the cape mesh is
    /// static geometry (no per-material variant, unlike armour), uploaded
    /// once regardless of whether any player currently in view has a cape.
    #[must_use]
    pub fn upload_cape(device: &wgpu::Device, mesh: &crate::entity::CapeMesh) -> Option<Self> {
        if mesh.indices.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-cape-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-cape-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuEntityModel {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
            parts: mesh.parts.iter().map(|(_, r)| *r).collect(),
        })
    }
}

/// Build an instance buffer from a slice of model matrices and the matching
/// per-instance packed light bytes, or `None` if empty.
///
/// `lights` is indexed in lockstep with `transforms`
/// ([`EntityBatch::lights`](crate::entity::EntityBatch::lights) alongside any of
/// that batch's per-part matrix vectors). A short or missing `lights` entry
/// falls back to [`ENTITY_FULLBRIGHT`](crate::entity::ENTITY_FULLBRIGHT) rather
/// than panicking or rendering black: a light plumbing mistake should look like
/// the old behaviour, not like a crash mid-frame.
#[must_use]
pub fn upload_instances(
    device: &wgpu::Device,
    transforms: &[glam::Mat4],
    lights: &[u32],
) -> Option<wgpu::Buffer> {
    if transforms.is_empty() {
        return None;
    }
    let fallback = u32::from(crate::entity::ENTITY_FULLBRIGHT);
    let raw: Vec<EntityInstanceRaw> = transforms
        .iter()
        .enumerate()
        .map(|(i, m)| EntityInstanceRaw::new(*m, lights.get(i).copied().unwrap_or(fallback)))
        .collect();
    Some(
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-instances"),
            contents: bytemuck::cast_slice(&raw),
            usage: wgpu::BufferUsages::VERTEX,
        }),
    )
}

/// One instance's full colour state: the gamma-space dye tint **and** whether
/// the hurt/death red overlay applies to it.
///
/// # Why one type instead of two parallel slices
///
/// [`upload_instances_tinted`] used to take a bare `&[[u8; 3]]`, and the
/// obvious way to add the overlay was a second `&[bool]` beside it. That is the
/// shape this repo keeps getting bitten by: a lockstep invariant across two
/// arguments that nothing enforces, so a later edit that filters or reorders one
/// of them silently paints the wrong mob red. Bundling the two means a tint
/// physically cannot travel without its overlay flag — the same move that made
/// `sprite_rect` return its atlas alongside its rect rather than leaving the
/// pairing to the caller.
///
/// [`NONE`](Self::NONE) is what every undyed, unhurt instance passes, and it
/// packs to exactly [`NO_TINT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceTint {
    /// The gamma-space `[r, g, b]` multiplied into the texel — `[255, 255, 255]`
    /// for "leave the texel alone".
    pub rgb: [u8; 3],
    /// Whether this instance draws with the hurt/death red overlay
    /// ([`HURT_OVERLAY_ALPHA_BYTE`]). Boolean, not a fade, per
    /// [`EntityInstanceRaw::with_hurt_overlay`].
    pub hurt: bool,
    /// A creeper's white-flash overlay alpha byte, `0` when absent — see
    /// [`EntityInstanceRaw::white_overlay`] and
    /// [`creeper_overlay_alpha_from_progress`]. A separate field from
    /// [`hurt`](Self::hurt) rather than a third `enum` state, because the
    /// caller ([`lodestone_shell::entities`]'s draw-extraction) computes both
    /// independently off different source data (`HurtTime` vs. a creeper's
    /// integrated swell) and the two are mutually exclusive only by
    /// *vanilla's* rule, enforced in the shader — not by construction here.
    pub creeper_white_overlay: u8,
}

impl InstanceTint {
    /// Untinted and unhurt: packs to [`NO_TINT`].
    pub const NONE: Self = Self {
        rgb: [255, 255, 255],
        hurt: false,
        creeper_white_overlay: 0,
    };

    /// A dye tint with no overlay.
    #[must_use]
    pub const fn rgb(rgb: [u8; 3]) -> Self {
        Self {
            rgb,
            hurt: false,
            creeper_white_overlay: 0,
        }
    }

    /// The same tint with the hurt/death overlay set or cleared.
    #[must_use]
    pub const fn with_hurt(mut self, hurt: bool) -> Self {
        self.hurt = hurt;
        self
    }

    /// The same tint with the creeper white-flash overlay alpha set (`0`
    /// clears it).
    #[must_use]
    pub const fn with_creeper_white_overlay(mut self, alpha_byte: u8) -> Self {
        self.creeper_white_overlay = alpha_byte;
        self
    }

    /// Fold all three channels into one instance's packed words.
    #[must_use]
    fn apply(self, inst: EntityInstanceRaw) -> EntityInstanceRaw {
        inst.with_tint(self.rgb)
            .with_hurt_overlay(self.hurt)
            .with_creeper_white_overlay(self.creeper_white_overlay)
    }
}

impl Default for InstanceTint {
    fn default() -> Self {
        Self::NONE
    }
}

/// [`upload_instances`] with a per-instance gamma-space tint and hurt overlay.
///
/// `tints` is indexed in lockstep with `transforms`; a short or missing entry
/// falls back to [`InstanceTint::NONE`], for the same reason `lights` falls back
/// to full-bright — a plumbing mistake should render the *untinted* thing, not a
/// black one, because "grey leather" is a legible bug and "black leather" looks
/// like a lighting failure somewhere else entirely. A missing entry likewise
/// draws *unhurt*: a mob that should have flashed and did not is a missed frame,
/// where a mob reddened by an indexing slip looks like a damage event that never
/// happened.
#[must_use]
pub fn upload_instances_tinted(
    device: &wgpu::Device,
    transforms: &[glam::Mat4],
    lights: &[u32],
    tints: &[InstanceTint],
) -> Option<wgpu::Buffer> {
    if transforms.is_empty() {
        return None;
    }
    let fallback = u32::from(crate::entity::ENTITY_FULLBRIGHT);
    let raw: Vec<EntityInstanceRaw> = transforms
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let inst = EntityInstanceRaw::new(*m, lights.get(i).copied().unwrap_or(fallback));
            match tints.get(i) {
                Some(tint) => tint.apply(inst),
                None => inst,
            }
        })
        .collect();
    Some(
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-tinted-instances"),
            contents: bytemuck::cast_slice(&raw),
            usage: wgpu::BufferUsages::VERTEX,
        }),
    )
}

/// Build a flame-instance buffer from a slice of billboard transforms and this
/// frame's current animation `frame` (`0..32`) — the mob-fire counterpart to
/// [`upload_instances`]/[`upload_instances_tinted`], `None` if empty.
///
/// Every transform gets the **same** `frame`: the animation advances once per
/// render frame for every flame drawn that frame, not per instance — see
/// `FlameInstanceRaw::frame`'s doc.
#[must_use]
pub fn upload_flame_instances(
    device: &wgpu::Device,
    transforms: &[glam::Mat4],
    frame: u32,
) -> Option<wgpu::Buffer> {
    if transforms.is_empty() {
        return None;
    }
    let raw: Vec<FlameInstanceRaw> = transforms
        .iter()
        .map(|m| FlameInstanceRaw::new(*m, frame))
        .collect();
    Some(
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-flame-instances"),
            contents: bytemuck::cast_slice(&raw),
            usage: wgpu::BufferUsages::VERTEX,
        }),
    )
}

/// The one place the entity pipeline's raster/depth/vertex state is spelled out,
/// parameterised by the things that vary across callers: the label, the depth
/// comparison, the colour-target blend state and whether the pass writes depth.
/// Four pipelines share it — the mob pass, the armour pass, the banner
/// mask-layer pass and the mob-fire pass — so a change to the vertex layout or
/// the colour target cannot land on one and miss the others.
///
/// `blend`/`depth_write` were added for [`EntityPipeline::banner_layer_pipeline`]
/// (issue #174 step B) without touching the mob/armour pipelines' own
/// behaviour: both existing callers pass `blend: None, depth_write: true`,
/// exactly what this function hardcoded before — zero behaviour change for
/// either.
///
/// `fragment_entry` was added in step C, for the same reason: every existing
/// caller (`Self::new`, `armour_pipeline`) passes `"fs_main"`, the literal
/// entry point this function hardcoded before, so this is zero behaviour
/// change for mobs and armour. [`EntityPipeline::banner_layer_pipeline`]
/// passes `"fs_main_no_cutout"` — see that function's doc for why a mask
/// layer needs the fragment shader itself to change, not just pipeline state.
///
/// `vertex_entry`/`instance_layout` were added for
/// [`EntityPipeline::flame_pipeline`] (mob fire, issue #434): the flame quads
/// carry no light/tint/overlay at all (see [`FlameInstanceRaw`]'s doc for why
/// that instance format is smaller than [`EntityInstanceRaw`]'s), so the flame
/// pass needs its own vertex entry point (`"vs_main_flame"`) reading its own,
/// narrower instance attributes — a shader entry point can only declare one
/// fixed set of `@location` inputs. Every existing caller keeps passing
/// `"vs_main"` and [`EntityInstanceRaw::instance_layout()`], so this is zero
/// behaviour change for mobs, armour and banners.
///
/// `write_mask` was added for [`EntityPipeline::water_mask_pipeline`] (owner
/// report: "placing down a boat still shows water through the bottom").
/// Every existing caller passes `wgpu::ColorWrites::ALL`, the literal value
/// this function hardcoded before, so this is zero behaviour change for
/// every other pipeline. `water_mask_pipeline` passes
/// `wgpu::ColorWrites::empty()` — the whole trick behind vanilla's
/// `RenderPipelines.WATER_MASK` (`ColorTargetState(…, writeMask = 0)`, read
/// directly from `.cache/mc/26.2/client-src`): the same depth-tested,
/// depth-writing geometry as every other entity, with the fragment shader's
/// colour output silently discarded rather than composited.
fn build_entity_pipeline(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
    camera_layout: &wgpu::BindGroupLayout,
    texture_layout: &wgpu::BindGroupLayout,
    label: &str,
    depth_compare: wgpu::CompareFunction,
    blend: Option<wgpu::BlendState>,
    depth_write: bool,
    vertex_entry: &str,
    fragment_entry: &str,
    instance_layout: wgpu::VertexBufferLayout<'_>,
    write_mask: wgpu::ColorWrites,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&format!("{label}-shader")),
        source: wgpu::ShaderSource::Wgsl(ENTITY_WGSL.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(&format!("{label}-layout")),
        bind_group_layouts: &[Some(camera_layout), Some(texture_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&format!("{label}-pipeline")),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some(vertex_entry),
            buffers: &[Some(ModelVertex::vertex_layout()), Some(instance_layout)],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend,
                write_mask,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            // Double-sided for now: robust visibility while per-model winding
            // parity is pixel-verified. See the module docs. Vanilla's armour
            // render type is `armorCutoutNoCull`, i.e. also double-sided.
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(depth_write),
            depth_compare: Some(depth_compare),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// A depth-tested, instanced pipeline for baked entity geometry.
#[derive(Debug)]
pub struct EntityPipeline {
    /// The render pipeline.
    pub pipeline: wgpu::RenderPipeline,
    /// Bind-group layout for the camera uniform (group 0).
    pub camera_layout: wgpu::BindGroupLayout,
    /// Bind-group layout for the entity texture + sampler (group 1).
    pub texture_layout: wgpu::BindGroupLayout,
}

impl EntityPipeline {
    /// Build the entity pipeline targeting `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-entity-camera-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // Vertex reads the view-projection; fragment reads the folded
                // fog block (eye, colour, range), so both stages bind it.
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-entity-texture-bgl"),
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
        });

        let pipeline = build_entity_pipeline(
            device,
            color_format,
            &camera_layout,
            &texture_layout,
            "lodestone-entity",
            // Vanilla's own value, translated (issue #21). Every 26.2 entity
            // render type is built from `ENTITY_SNIPPET`, which pins
            // `DepthStencilState.DEFAULT` (`RenderPipelines.java:49-56`), and that
            // is `(GREATER_THAN_OR_EQUAL, writeDepth = true)`
            // (`DepthStencilState.java:5-6`) — `LessEqual` under this engine's
            // `[0,1]` DirectX-style depth. `ENTITY_SOLID` (`:232`),
            // `ENTITY_CUTOUT` (`:245`), `ENTITY_CUTOUT_CULL` (`:238`) and
            // `ENTITY_TRANSLUCENT` (`:274`) all inherit it; none overrides
            // `withDepthStencilState`.
            //
            // This was `Less` until `entity_depth_coincident_pixels.rs` existed to
            // prove the change safe — see that gate's module docs for the measured
            // before/after (coincident red-then-blue read `[189, 0, 0]`, i.e. the
            // *first* draw winning, with blue covering 0 of 16384 pixels).
            wgpu::CompareFunction::LessEqual,
            None,
            true,
            "vs_main",
            "fs_main",
            EntityInstanceRaw::instance_layout(),
            wgpu::ColorWrites::ALL,
        );

        EntityPipeline {
            pipeline,
            camera_layout,
            texture_layout,
        }
    }

    /// A second render pipeline over **this** pipeline's own bind-group layouts,
    /// for the humanoid-armour layers.
    ///
    /// Sharing `self`'s layout objects rather than creating equivalent ones is
    /// deliberate: every camera and texture bind group already built through
    /// [`camera_bind_group`](Self::camera_bind_group) /
    /// [`texture_bind_group`](Self::texture_bind_group) is then valid here with
    /// no second set of uploads, and there is no reliance on wgpu deduplicating
    /// two structurally identical layout descriptors.
    ///
    /// # Why `LessEqual`, and why it is no longer only here
    ///
    /// Vanilla's own entity depth state is
    /// `DepthStencilState.DEFAULT = (GREATER_THAN_OR_EQUAL, writeDepth = true)`
    /// (`DepthStencilState.java:6`), which under this engine's `[0,1]`
    /// DirectX-style depth — vanilla is reversed-Z — is `LessEqual`.
    ///
    /// This doc used to say the base pipeline "uses `Less`, so it is the one that
    /// departs from vanilla; that is left alone here rather than 'fixed', because
    /// changing it would alter how *every* mob's coplanar geometry resolves and
    /// this change has no pixel gate to prove that safe." Both halves were true
    /// when written. Issue #21 built the missing gate
    /// (`entity_depth_coincident_pixels.rs`) and then made the change, so
    /// [`Self::new`] is now `LessEqual` too and this pipeline is depth-identical
    /// to it. It survives as a separate pipeline for its label and its blend
    /// state, not for its depth compare — if the two ever need to diverge again,
    /// this is where it happens.
    ///
    /// Armour needed the faithful value first, for a concrete reason worth
    /// keeping: leather's `humanoid` layer list is **two coplanar layers** at one
    /// inflation — a greyscale dyeable base and an untinted `leather_overlay`
    /// detail pass drawn straight over it (`equipment/leather.json`). Under
    /// `Less` the second draw fails the depth test against the first at every
    /// texel and the overlay is silently invisible; under `LessEqual` it wins,
    /// which is what vanilla does. That is the same mechanism the base pipeline
    /// was getting wrong for every mob, one layer up.
    #[must_use]
    pub fn armour_pipeline(
        &self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        build_entity_pipeline(
            device,
            color_format,
            &self.camera_layout,
            &self.texture_layout,
            "lodestone-entity-armour",
            wgpu::CompareFunction::LessEqual,
            None,
            true,
            "vs_main",
            "fs_main",
            EntityInstanceRaw::instance_layout(),
            wgpu::ColorWrites::ALL,
        )
    }

    /// A third render pipeline over this pipeline's own bind-group layouts,
    /// for banner mask layers (issue #174 step B).
    ///
    /// Vanilla's `RenderPipelines.BANNER_PATTERN` (`RenderPipelines.java:311-318`)
    /// draws the flag opaque, then each pattern mask layer **translucent, at
    /// equal depth, with depth write off and no alpha cutout** — layers stack
    /// visually rather than z-fighting or punching holes in each other.
    /// Concretely, against this pipeline's siblings:
    ///
    /// | | mob (`Self::new`) | armour ([`Self::armour_pipeline`]) | banner layer (here) |
    /// |---|---|---|---|
    /// | `depth_compare` | `LessEqual` | `LessEqual` | `LessEqual` |
    /// | `blend` | `None` (cutout) | `None` (cutout) | `ALPHA_BLENDING` |
    /// | `depth_write` | `true` | `true` | `false` |
    ///
    /// The first column read `Less` until issue #21; all three now agree on the
    /// depth compare, and the blend/depth-write pair is what makes this pipeline
    /// distinct.
    ///
    /// `LessEqual`, not `Less`: the flag base and every mask layer share the
    /// same depth per vanilla's coincident-geometry draw, the same reasoning
    /// [`armour_pipeline`](Self::armour_pipeline)'s own doc gives for leather's
    /// coplanar base/overlay pair — under `Less` every mask layer after the
    /// first would fail the depth test against the base it is coincident
    /// with and never draw at all.
    ///
    /// Per `CLAUDE.md`: depth here is `[0,1]` DirectX-style, not vanilla's
    /// reversed-Z, so vanilla's `GREATER_THAN_OR_EQUAL` is this engine's
    /// `LessEqual` — already accounted for above, not a separate flip to
    /// apply on top.
    ///
    /// **Step C, landed**: this now binds `entity.wgsl`'s `fs_main_no_cutout`
    /// entry point rather than `fs_main`. The alpha-cutout `discard` in
    /// `fs_main` is unconditional and vanilla's banner draw has none, so a
    /// mask layer's antialiased/partial-alpha edge texels would have been
    /// lost here rather than blended — this is the shader-side half of
    /// matching `RenderPipelines.BANNER_PATTERN`; the pipeline-state half
    /// (blend/depth-write, below) is step B. See `entity.wgsl`'s own comment
    /// on `fs_main_no_cutout` and `docs/banner-shield-patterns.md`.
    #[must_use]
    pub fn banner_layer_pipeline(
        &self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        build_entity_pipeline(
            device,
            color_format,
            &self.camera_layout,
            &self.texture_layout,
            "lodestone-entity-banner-layer",
            wgpu::CompareFunction::LessEqual,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            false,
            "vs_main",
            "fs_main_no_cutout",
            EntityInstanceRaw::instance_layout(),
            wgpu::ColorWrites::ALL,
        )
    }

    /// A fourth render pipeline over this pipeline's own bind-group layouts,
    /// for the mob-fire billboard (issue #434 — player report: "mobs dont show
    /// flames yet"). Reuses [`Self::camera_layout`]/[`Self::texture_layout`]
    /// exactly like [`armour_pipeline`](Self::armour_pipeline)/
    /// [`banner_layer_pipeline`](Self::banner_layer_pipeline): the flame pass
    /// needs its own *texture* (the combined `fire_0`/`fire_1` strip built by
    /// `lodestone_assets::entity_flame::load_combined_flame_texture`), bound as
    /// a fresh [`wgpu::BindGroup`] over this same layout, not a new bind group
    /// *slot* — the entity shader still spends exactly two groups (camera,
    /// texture), never a fifth, per `CLAUDE.md`'s 4-bind-group-floor note.
    ///
    /// Depth state matches [`Self::new`]/[`armour_pipeline`](Self::armour_pipeline):
    /// `LessEqual` (vanilla's `DepthStencilState.DEFAULT`,
    /// `GREATER_THAN_OR_EQUAL` under this engine's `[0,1]` depth), depth write
    /// on. Vanilla's own flame render type
    /// (`RenderTypes.entityCutoutCull`, `.cache/mc/26.2/client-src/net/
    /// minecraft/client/renderer/rendertype/RenderTypes.java:429`) inherits
    /// `ENTITY_SNIPPET`'s `DepthStencilState.DEFAULT` like every other entity
    /// render type — there is no separate depth state to translate here, only
    /// the fixed cutout/blend spelled out below.
    ///
    /// `blend: None` (cutout, not translucent) and a dedicated
    /// `"fs_main_flame"` entry point are the load-bearing divergence from
    /// [`Self::new`]: vanilla's `ENTITY_CUTOUT_CULL` pipeline
    /// (`RenderPipelines.java:238-243`) is `ALPHA_CUTOUT` at `0.1`, not the
    /// `ALPHA_BLENDING` translucency this doc's own brief initially assumed —
    /// see `fs_main_flame`'s doc in `entity.wgsl` for the corrected threshold
    /// and for why the flame fragment skips `shade_entity`'s two-light
    /// diffuse. `vs_main_flame` reads [`FlameInstanceRaw`]'s attributes rather
    /// than [`EntityInstanceRaw`]'s, hence the dedicated `instance_layout`
    /// argument below.
    #[must_use]
    pub fn flame_pipeline(
        &self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        build_entity_pipeline(
            device,
            color_format,
            &self.camera_layout,
            &self.texture_layout,
            "lodestone-entity-flame",
            wgpu::CompareFunction::LessEqual,
            None,
            true,
            "vs_main_flame",
            "fs_main_flame",
            FlameInstanceRaw::instance_layout(),
            wgpu::ColorWrites::ALL,
        )
    }

    /// A render pipeline over this pipeline's own bind-group layouts, for the
    /// experience-orb billboard.
    ///
    /// Reuses [`Self::camera_layout`]/[`Self::texture_layout`] like every sibling
    /// here: the orb needs its own *texture* (the standalone 64×64
    /// `entity/experience/experience_orb.png`, not a slice of any atlas), bound as a
    /// fresh [`wgpu::BindGroup`] over this same layout — never a third bind-group
    /// *slot*. The entity shader still spends exactly two, well under `CLAUDE.md`'s
    /// portable 4-group floor.
    ///
    /// # State, and where each field comes from
    ///
    /// Vanilla's render type is `RenderTypes.entityTranslucentCullItemTarget`, i.e.
    /// `RenderPipelines.ENTITY_TRANSLUCENT`:
    /// `ColorTargetState(BlendFunction.TRANSLUCENT)`, `ALPHA_CUTOUT 0.1F`,
    /// `PER_FACE_LIGHTING`, `withCull(false)`, and `ENTITY_SNIPPET`'s inherited
    /// `DepthStencilState.DEFAULT`. Against the siblings:
    ///
    /// | | mob ([`Self::new`]) | banner layer ([`Self::banner_layer_pipeline`]) | orb (here) |
    /// |---|---|---|---|
    /// | `blend` | `None` (cutout) | `ALPHA_BLENDING` | `ALPHA_BLENDING` |
    /// | `depth_write` | `true` | `false` | **`true`** |
    /// | cutout | `0.5` (`fs_main`) | none | `0.1` (`fs_main_orb`) |
    ///
    /// **`depth_write: true` is the one that is easy to get wrong**, because the
    /// nearest translucent sibling turns it off. A banner mask layer is coincident
    /// geometry stacked over a flag that already wrote depth, so writing again would
    /// z-fight; an orb is a free-standing entity, and with depth write off a pile of
    /// orbs on the ground draws in submission order rather than depth order and the
    /// far ones paint over the near ones. `ENTITY_TRANSLUCENT` overrides the blend
    /// function and nothing about depth, so `DepthStencilState.DEFAULT`'s
    /// `writeDepth = true` carries through — the same `LessEqual` translation of
    /// `GREATER_THAN_OR_EQUAL` this file applies everywhere else, per `CLAUDE.md`'s
    /// `[0,1]`-depth note.
    ///
    /// `vs_main` and [`EntityInstanceRaw`], not a narrower flame-style format: an orb
    /// genuinely carries both a per-instance light (its own `+7`-boosted block
    /// sample) and a per-instance tint (the pulsing green, which changes every tick
    /// *and* differs between two orbs of different ages), so the wide instance row
    /// is used rather than wasted.
    #[must_use]
    pub fn orb_pipeline(
        &self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        build_entity_pipeline(
            device,
            color_format,
            &self.camera_layout,
            &self.texture_layout,
            "lodestone-entity-orb",
            wgpu::CompareFunction::LessEqual,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            true,
            "vs_main",
            "fs_main_orb",
            EntityInstanceRaw::instance_layout(),
            wgpu::ColorWrites::ALL,
        )
    }

    /// A render pipeline over this pipeline's own bind-group layouts, for the
    /// boat water-clip mask (owner report: "placing down a boat still shows
    /// water through the bottom").
    ///
    /// Vanilla's `RenderPipelines.WATER_MASK`
    /// (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/
    /// RenderPipelines.java`): `DepthStencilState.DEFAULT` (this engine's
    /// `LessEqual`, same translation every sibling here applies) with
    /// `ColorTargetState(…, writeMask = 0)` — a normal depth-tested,
    /// depth-writing draw whose fragment output never reaches the
    /// framebuffer. `blend` is therefore irrelevant (there is nothing to
    /// blend) and left `None` to match the base mob pipeline's state, the
    /// nearest sibling with the same `depth_write: true`.
    ///
    /// Reuses `vs_main`/`fs_main` and [`EntityInstanceRaw`] — the ordinary mob
    /// shader and instance format, unlike [`Self::flame_pipeline`]'s
    /// dedicated entry point — because the water mask needs nothing the base
    /// shader does not already compute; only the pipeline's own
    /// `write_mask` differs. The texture bind group this pipeline's layout
    /// still requires is real (see [`lodestone_assets::entity_models`]'s
    /// `boat_water_patch` corpus entry) but its sampled texels never leave
    /// the fragment stage.
    ///
    /// Reuses [`Self::camera_layout`]/[`Self::texture_layout`] like every
    /// sibling here — the entity shader still spends exactly two bind groups,
    /// nowhere near `CLAUDE.md`'s 4-bind-group floor.
    #[must_use]
    pub fn water_mask_pipeline(
        &self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        build_entity_pipeline(
            device,
            color_format,
            &self.camera_layout,
            &self.texture_layout,
            "lodestone-entity-water-mask",
            wgpu::CompareFunction::LessEqual,
            None,
            true,
            "vs_main",
            "fs_main",
            EntityInstanceRaw::instance_layout(),
            wgpu::ColorWrites::empty(),
        )
    }

    /// A fifth render pipeline over this pipeline's own bind-group layouts,
    /// for a `decal: true` armour-trim pattern (issue #17's trims — see
    /// `docs/armour-rendering.md`'s "Trims" section for the full design).
    ///
    /// Reuses [`Self::camera_layout`]/[`Self::texture_layout`] exactly like
    /// [`armour_pipeline`](Self::armour_pipeline)/
    /// [`banner_layer_pipeline`](Self::banner_layer_pipeline)/
    /// [`flame_pipeline`](Self::flame_pipeline): a trim decal needs its own
    /// *texture* (the baked `armor_trims` sprite for the wearer's
    /// `(pattern, material)` pair — `lodestone_assets::trim::TrimAtlas`),
    /// bound as a fresh [`wgpu::BindGroup`] over this same layout, never a
    /// third bind-group *slot* — the entity shader still spends exactly two
    /// groups, nowhere near `CLAUDE.md`'s 4-bind-group floor.
    ///
    /// # Why a *third* depth mode, and why it is almost never selected
    ///
    /// `TrimPattern.decal()` (`ArmorTrim.java` → `Sheets.armorTrimsSheet`)
    /// forks the render type vanilla submits a trim through:
    ///
    /// | `decal` | vanilla pipeline | this engine |
    /// |---|---|---|
    /// | `false` | `ARMOR_CUTOUT_NO_CULL` — `ENTITY_SNIPPET`'s own default | [`armour_pipeline`](Self::armour_pipeline) (identical: no override) |
    /// | `true` | `ARMOR_DECAL_CUTOUT_NO_CULL` — `CompareOp.EQUAL, writeDepth=false` | **here** |
    ///
    /// (`RenderPipelines.java:203-219`, read directly from
    /// `.cache/mc/26.2/client-src`). `CompareOp.EQUAL` has no "direction" to
    /// flip under this engine's `[0,1]` DirectX-style depth — unlike
    /// `GREATER_THAN_OR_EQUAL`/`LessEqual` elsewhere in this file, equality
    /// is its own mirror image, so `CompareFunction::Equal` is not a
    /// translation, it is the same predicate. `depth_write_enabled: false`
    /// carries over unchanged for the same reason.
    ///
    /// **Every one of 26.2's 18 trim patterns has `"decal": false`**
    /// (`lodestone_assets::trim::TRIM_PATTERNS`, checked directly against
    /// every `data/minecraft/trim_pattern/*.json` in `client.jar` — see that
    /// module's doc comment). So in practice every real trim today draws
    /// through [`armour_pipeline`](Self::armour_pipeline), and this pipeline
    /// is exercised by no vanilla content — it still has to exist, because
    /// `decal` is genuine per-pattern registry data (a resource pack, or a
    /// future vanilla release, can set it), and `Sheets.armorTrimsSheet`'s
    /// fork is a real one, not a simplification this engine is free to
    /// collapse to "always the cutout pipeline".
    ///
    /// `blend: None` (cutout, not translucent) matches vanilla's
    /// `ALPHA_CUTOUT 0.1F` shader define on both trim pipelines — the trim
    /// atlas sprites are not measured to be strictly binary alpha the way the
    /// nine humanoid armour sheets are (`docs/armour-rendering.md`'s
    /// `hat`-shell note), so this keeps the shared `fs_main`'s existing `0.5`
    /// cutout rather than asserting a threshold this crate has not verified
    /// against the real trim sprites; a future pass that needs the faithful
    /// `0.1` should add a dedicated fragment entry the way
    /// [`flame_pipeline`](Self::flame_pipeline) did for its own threshold,
    /// not change `fs_main` for every existing caller.
    #[must_use]
    pub fn trim_decal_pipeline(
        &self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        build_entity_pipeline(
            device,
            color_format,
            &self.camera_layout,
            &self.texture_layout,
            "lodestone-entity-trim-decal",
            wgpu::CompareFunction::Equal,
            None,
            false,
            "vs_main",
            "fs_main",
            EntityInstanceRaw::instance_layout(),
            wgpu::ColorWrites::ALL,
        )
    }

    /// Build the group-0 uniform buffer for the entity pass with fog
    /// **disabled**. `view_proj` is taken from the camera; `section_origin` is
    /// unused (zero) because an entity's world position lives in its instance
    /// matrix.
    ///
    /// The buffer is sized for the whole [`EntityCameraUniform`], so a caller
    /// that later wants fog can overwrite it in place with
    /// [`queue.write_buffer`](wgpu::Queue::write_buffer) — see
    /// [`camera_buffer_with_fog`](Self::camera_buffer_with_fog).
    #[must_use]
    pub fn camera_buffer(
        &self,
        device: &wgpu::Device,
        camera: &crate::camera::Camera,
    ) -> wgpu::Buffer {
        self.camera_buffer_with_fog(device, camera, crate::fog::FogUniform::disabled())
    }

    /// Build the group-0 uniform buffer for the entity pass with an explicit fog
    /// block, so mobs fade into the distance (or into water fog) on exactly the
    /// same curve as the terrain behind them.
    #[must_use]
    pub fn camera_buffer_with_fog(
        &self,
        device: &wgpu::Device,
        camera: &crate::camera::Camera,
        fog: crate::fog::FogUniform,
    ) -> wgpu::Buffer {
        entity_camera_buffer(
            device,
            EntityCameraUniform {
                camera: CameraUniform::new(camera, [0.0, 0.0, 0.0]),
                fog,
            },
        )
    }

    /// Create the camera bind group from a uniform buffer.
    #[must_use]
    pub fn camera_bind_group(
        &self,
        device: &wgpu::Device,
        camera_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-entity-camera-bg"),
            layout: &self.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        })
    }

    /// Create the texture bind group from a texture view and sampler (one
    /// entity sheet).
    #[must_use]
    pub fn texture_bind_group(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-entity-texture-bg"),
            layout: &self.texture_layout,
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
}

/// The group-0 uniform for the entity pipeline: the [`CameraUniform`] followed
/// by this frame's [`FogUniform`](crate::fog::FogUniform).
///
/// Byte-compatible with
/// [`ModelCameraUniform`](crate::model_pipeline::ModelCameraUniform) on purpose
/// — same layout, same shader-side `Camera` struct, same `fog_amount` — so a
/// mob and the block behind it can never be fogged by different math. Rewrite
/// the whole struct each frame via [`queue.write_buffer`](wgpu::Queue::write_buffer).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EntityCameraUniform {
    /// View-projection (and an unused zero section origin).
    pub camera: CameraUniform,
    /// Distance fog for this frame (eye position, colour, start/end) — plus,
    /// in its one spare lane, this frame's sky darkening. See
    /// [`with_sky_darken`](Self::with_sky_darken).
    pub fog: crate::fog::FogUniform,
}

/// Which lane of `FogUniform::end_enabled` carries the sky-darken factor.
/// `end_enabled` is documented as `x = end`, `y = enabled`, `zw` unused; this is
/// the `z`.
const SKY_DARKEN_LANE: usize = 2;

impl EntityCameraUniform {
    /// Set this frame's **sky darkening**: the factor vanilla's `LightTexture`
    /// scales the *sky* half of the lightmap by, `1.0` at noon down to `0.24` at
    /// midnight. See [`sky_darken`](Self::sky_darken) for the read side.
    ///
    /// # Why this term has to exist at all
    ///
    /// A server's sky-light array is time-**invariant**: it encodes how much sky
    /// reaches a block, not how bright the sky currently is. Measured live
    /// against a vanilla 26.2 oracle at one position, with the server clock as
    /// the control:
    ///
    /// ```text
    /// noon     clock= 6000  packed=0xF0  sky=15 block=0  light_term=1.000
    /// midnight clock=18000  packed=0xF0  sky=15 block=0  light_term=1.000
    /// ```
    ///
    /// So sampling world light correctly — which `52f109f` did — cannot darken a
    /// mob at night, because the sampled byte is the same byte. Vanilla darkens
    /// purely client-side, in `LightTexture.updateLightTexture`, by scaling the
    /// sky contribution by `Level.getSkyDarken(partialTick) * 0.95 + 0.05`.
    /// `crate::entity::sky_darken_for_time_of_day` is that curve.
    ///
    /// # Why a spare fog lane and not a new field
    ///
    /// [`EntityCameraUniform`] is byte-identical to
    /// [`ModelCameraUniform`](crate::model_pipeline::ModelCameraUniform) on
    /// purpose, and the model shader is at wgpu's 4-bind-group floor, so neither
    /// growing the struct nor adding a bind group is free. `end_enabled.zw` were
    /// already unused and the model shader does not read them, so terrain is
    /// unaffected until it opts in.
    ///
    /// # Why `0.0` reads as full daylight
    ///
    /// Every path that builds this uniform derives its fog from
    /// [`FogUniform::new`](crate::fog::FogUniform::new) or
    /// [`FogUniform::disabled`](crate::fog::FogUniform::disabled), both of which
    /// zero the lane. Taking `0.0` literally would render every mob in every
    /// existing caller at the `0.2` floor — a silent, global regression of
    /// exactly the shape [`ENTITY_FULLBRIGHT`](crate::entity::ENTITY_FULLBRIGHT)
    /// exists to prevent. Vanilla's factor is floored at `0.24`, so `0.0` is
    /// never a legitimate value and is safe as the "not wired yet" sentinel: the
    /// shader reads it as `1.0`, i.e. today's behaviour.
    #[must_use]
    pub const fn with_sky_darken(mut self, sky_darken: f32) -> Self {
        self.fog.end_enabled[SKY_DARKEN_LANE] = sky_darken;
        self
    }

    /// This frame's sky-darken factor as the shader will interpret it: the raw
    /// lane, or `1.0` when the lane is the unset `0.0` sentinel.
    #[must_use]
    pub fn sky_darken(&self) -> f32 {
        let raw = self.fog.end_enabled[SKY_DARKEN_LANE];
        if raw <= 0.0 { 1.0 } else { raw }
    }
}

/// Create the entity pass's group-0 uniform buffer from a full
/// [`EntityCameraUniform`]. For callers holding a [`Camera`](crate::camera::Camera),
/// [`EntityPipeline::camera_buffer_with_fog`] is the convenient wrapper.
#[must_use]
pub fn entity_camera_buffer(
    device: &wgpu::Device,
    uniform: EntityCameraUniform,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lodestone-entity-camera-uniform"),
        contents: bytemuck::bytes_of(&uniform),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

const ENTITY_WGSL: &str = include_str!("shaders/entity.wgsl");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_raw_is_four_columns_plus_a_light_and_a_tint_word() {
        assert_eq!(core::mem::size_of::<EntityInstanceRaw>(), 76);
        let layout = EntityInstanceRaw::instance_layout();
        assert_eq!(layout.array_stride, 76);
        assert_eq!(layout.step_mode, wgpu::VertexStepMode::Instance);
        assert_eq!(layout.attributes.len(), 7);
        // Instance attributes start at location 4, past ModelVertex's 0..=3.
        assert_eq!(layout.attributes[0].shader_location, 4);
        assert_eq!(layout.attributes[3].shader_location, 7);
        assert_eq!(layout.attributes[3].offset, 48);
        // The light word sits immediately after the matrix, the tint after it,
        // and the creeper white-overlay word after that.
        assert_eq!(layout.attributes[4].shader_location, 8);
        assert_eq!(layout.attributes[4].offset, 64);
        assert_eq!(layout.attributes[4].format, wgpu::VertexFormat::Uint32);
        assert_eq!(layout.attributes[5].shader_location, 9);
        assert_eq!(layout.attributes[5].offset, 68);
        assert_eq!(layout.attributes[5].format, wgpu::VertexFormat::Uint32);
        assert_eq!(layout.attributes[6].shader_location, 10);
        assert_eq!(layout.attributes[6].offset, 72);
        assert_eq!(layout.attributes[6].format, wgpu::VertexFormat::Uint32);
    }

    /// A tint must round-trip its bytes in `0x00RRGGBB` order, and an instance
    /// built without one must be **white**, not zero. Zero would be black, and
    /// every mob in the game goes through [`EntityInstanceRaw::new`].
    #[test]
    fn tint_defaults_to_white_and_packs_rgb_in_order() {
        let m = glam::Mat4::IDENTITY;
        assert_eq!(EntityInstanceRaw::new(m, 0).tint, NO_TINT);
        assert_eq!(EntityInstanceRaw::from_mat4(m).tint, NO_TINT);
        assert_eq!(NO_TINT, 0x00FF_FFFF);
        let leather = EntityInstanceRaw::new(m, 0)
            .with_tint(lodestone_assets::equipment::UNDYED_LEATHER_RGB);
        assert_eq!(leather.tint, 0x00A0_6540);
        // R in the high byte: a byte-order slip would make leather blue.
        assert_eq!((leather.tint >> 16) & 0xFF, 0xA0);
        assert_eq!(leather.tint & 0xFF, 0x40);
    }

    /// The overlay byte lives in bits 24-31, and setting or clearing it must
    /// never disturb the tint's own bits 0-23 (or vice versa) — dyed leather
    /// worn by a hurt mob needs both at once.
    #[test]
    fn hurt_overlay_shares_the_tint_word_without_colliding() {
        let m = glam::Mat4::IDENTITY;

        // Off by default, same as tint.
        let plain = EntityInstanceRaw::new(m, 0);
        assert_eq!(plain.tint, NO_TINT);

        // Overlay alone: RGB bits untouched (still opaque white), alpha byte set
        // to vanilla's 178 (`-1291911168`'s alpha channel, `OverlayTexture`'s red
        // row, `LivingEntityRenderer.java:281`).
        let hurt = EntityInstanceRaw::new(m, 0).with_hurt_overlay(true);
        assert_eq!(hurt.tint & 0x00FF_FFFF, NO_TINT);
        assert_eq!((hurt.tint >> 24) & 0xFF, HURT_OVERLAY_ALPHA_BYTE);
        assert_eq!(HURT_OVERLAY_ALPHA_BYTE, 178);

        // Setting then clearing must return to exactly the untouched value, not
        // merely a "no visible effect" value — a stray bit here would silently
        // change the packed word's meaning for any future consumer.
        let cleared = hurt.with_hurt_overlay(false);
        assert_eq!(cleared.tint, NO_TINT);

        // Tint and overlay compose: dyed leather (bits 0-23) plus hurt (bits
        // 24-31) must both read back correctly regardless of call order.
        let leather_hurt = EntityInstanceRaw::new(m, 0)
            .with_tint(lodestone_assets::equipment::UNDYED_LEATHER_RGB)
            .with_hurt_overlay(true);
        let hurt_leather = EntityInstanceRaw::new(m, 0)
            .with_hurt_overlay(true)
            .with_tint(lodestone_assets::equipment::UNDYED_LEATHER_RGB);
        assert_eq!(leather_hurt.tint, hurt_leather.tint);
        assert_eq!(leather_hurt.tint & 0x00FF_FFFF, 0x00A0_6540);
        assert_eq!((leather_hurt.tint >> 24) & 0xFF, HURT_OVERLAY_ALPHA_BYTE);
    }

    /// [`InstanceTint`] is the thing that stops the overlay flag from being a
    /// second, parallel slice nothing keeps in step with the tints. Both halves
    /// must survive the fold into one packed word, in both orders, and
    /// [`InstanceTint::NONE`] must be indistinguishable from the pre-overlay
    /// `NO_TINT` — otherwise every undyed mob in the world changes colour the
    /// day this type lands.
    #[test]
    fn instance_tint_carries_both_halves_into_one_packed_word() {
        let m = glam::Mat4::IDENTITY;
        let raw = |t: InstanceTint| t.apply(EntityInstanceRaw::new(m, 0)).tint;

        assert_eq!(raw(InstanceTint::NONE), NO_TINT);
        assert_eq!(InstanceTint::default(), InstanceTint::NONE);

        let leather = lodestone_assets::equipment::UNDYED_LEATHER_RGB;
        assert_eq!(raw(InstanceTint::rgb(leather)) & 0x00FF_FFFF, 0x00A0_6540);
        assert_eq!(raw(InstanceTint::rgb(leather)) >> 24, 0);

        // The case a parallel `&[bool]` gets wrong: a dyed *and* hurt instance.
        let both = InstanceTint::rgb(leather).with_hurt(true);
        assert_eq!(raw(both) & 0x00FF_FFFF, 0x00A0_6540);
        assert_eq!(raw(both) >> 24, HURT_OVERLAY_ALPHA_BYTE);

        // Hurt with no dye still leaves the texel's own colour alone.
        let hurt_only = InstanceTint::NONE.with_hurt(true);
        assert_eq!(raw(hurt_only) & 0x00FF_FFFF, NO_TINT);
        assert_eq!(raw(hurt_only) >> 24, HURT_OVERLAY_ALPHA_BYTE);
        assert_eq!(raw(hurt_only.with_hurt(false)), NO_TINT);
    }

    /// The creeper white-overlay word is a genuinely separate attribute from
    /// `tint`, so setting it must not perturb the tint/hurt word at all, and
    /// vice versa — the two channels have to compose freely because
    /// [`InstanceTint`] carries both.
    #[test]
    fn creeper_white_overlay_lives_in_its_own_word() {
        let m = glam::Mat4::IDENTITY;

        let plain = EntityInstanceRaw::new(m, 0);
        assert_eq!(plain.white_overlay, 0);

        let flashing = EntityInstanceRaw::new(m, 0).with_creeper_white_overlay(200);
        assert_eq!(flashing.white_overlay, 200);
        assert_eq!(flashing.tint, NO_TINT, "the tint word must be untouched");

        // Compose with a hurt overlay and a dye tint — three independent bits
        // of state, none of which may bleed into another.
        let leather = lodestone_assets::equipment::UNDYED_LEATHER_RGB;
        let combined = EntityInstanceRaw::new(m, 0)
            .with_tint(leather)
            .with_hurt_overlay(true)
            .with_creeper_white_overlay(100);
        assert_eq!(combined.tint & 0x00FF_FFFF, 0x00A0_6540);
        assert_eq!((combined.tint >> 24) & 0xFF, HURT_OVERLAY_ALPHA_BYTE);
        assert_eq!(combined.white_overlay, 100);

        // Clearing (0) must return exactly to the untouched value.
        assert_eq!(flashing.with_creeper_white_overlay(0).white_overlay, 0);
    }

    /// [`InstanceTint`] threads the white-overlay alpha the same way it threads
    /// `hurt` — through `apply`, into the dedicated word.
    #[test]
    fn instance_tint_carries_the_white_overlay_alpha() {
        let m = glam::Mat4::IDENTITY;
        let raw = |t: InstanceTint| t.apply(EntityInstanceRaw::new(m, 0));

        assert_eq!(raw(InstanceTint::NONE).white_overlay, 0);
        let flashing = InstanceTint::NONE.with_creeper_white_overlay(150);
        assert_eq!(raw(flashing).white_overlay, 150);
        // And it does not disturb the packed tint/hurt word.
        assert_eq!(raw(flashing).tint, NO_TINT);
    }

    /// [`creeper_overlay_alpha_from_progress`]: hand-evaluated from
    /// `OverlayTexture`'s constructor (`(1 - x/15*0.75) * 255`, `x =
    /// (int)(progress * 15)`), not read back off this implementation.
    #[test]
    fn creeper_overlay_alpha_transcribes_the_vanilla_lookup_texture() {
        assert_eq!(
            creeper_overlay_alpha_from_progress(0.0),
            0,
            "0.0 is the absent sentinel"
        );
        for progress in [0.5f32, 0.6, 0.75, 0.9, 1.0] {
            let x = (progress * 15.0) as i32;
            let want = ((1.0 - x as f32 / 15.0 * 0.75) * 255.0).round() as u8;
            assert_eq!(
                creeper_overlay_alpha_from_progress(progress),
                want,
                "progress {progress}"
            );
        }
    }

    /// The alpha must strictly decrease as progress increases (more of the
    /// entity's own colour survives at low progress, less at high progress —
    /// i.e. *more* white shows through as the fuse burns down), and never hits
    /// the `0` sentinel for any progress this crate's blink actually produces
    /// (`0.5..=1.0`).
    #[test]
    fn creeper_overlay_alpha_decreases_and_never_hits_the_sentinel_in_the_blinks_active_range() {
        let mut prev = 256u16;
        for step in 0..=20 {
            let progress = 0.5 + 0.5 * (step as f32 / 20.0);
            let alpha = creeper_overlay_alpha_from_progress(progress);
            assert_ne!(alpha, 0, "progress {progress} hit the absent sentinel");
            assert!(
                u16::from(alpha) <= prev,
                "alpha rose from {prev} to {alpha} between steps, at progress {progress}"
            );
            prev = u16::from(alpha);
        }
    }

    /// The uniform the entity shader's `Camera` struct maps onto: 80 bytes of
    /// camera (a `mat4x4` plus a `vec4`) then 64 of fog (four `vec4`s, since
    /// `FogUniform` grew a fourth for per-dimension `ambient_light`). If this
    /// ever stops matching the model pipeline's uniform, the two passes would fog
    /// differently and a mob would visibly detach from its background.
    #[test]
    fn camera_uniform_matches_the_model_pipelines_layout() {
        assert_eq!(core::mem::size_of::<EntityCameraUniform>(), 144);
        assert_eq!(
            core::mem::size_of::<EntityCameraUniform>(),
            core::mem::size_of::<crate::model_pipeline::ModelCameraUniform>()
        );
        assert_eq!(core::mem::size_of::<CameraUniform>(), 80);
    }

    /// A light byte supplied per instance must survive packing unchanged, and a
    /// caller that supplies none must get the full-bright fallback rather than
    /// black.
    #[test]
    fn instance_light_packs_and_defaults_full_bright() {
        let m = glam::Mat4::IDENTITY;
        assert_eq!(EntityInstanceRaw::new(m, 0).light, 0);
        assert_eq!(EntityInstanceRaw::new(m, 0xF0).light, 0xF0);
        assert_eq!(
            EntityInstanceRaw::from_mat4(m).light,
            u32::from(crate::entity::ENTITY_FULLBRIGHT)
        );
    }

    /// The sky-darken lane must round-trip, must read as full daylight when
    /// unset, and must not disturb any fog field — it rides a *spare* lane
    /// precisely so entities and terrain keep fogging on identical numbers.
    #[test]
    fn sky_darken_rides_a_spare_fog_lane_without_touching_fog() {
        let fog = crate::fog::FogUniform::new(
            &crate::fog::FogSettings::for_view_distance([0.1, 0.2, 0.3], 128.0, 0.5),
            [1.0, 2.0, 3.0],
        );
        let base = EntityCameraUniform {
            camera: CameraUniform {
                view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
                section_origin: [0.0; 4],
            },
            fog,
        };
        // Unset is the 0.0 sentinel, which reads as full daylight — not as the
        // 0.2 floor, which would black out every existing caller's mobs.
        assert_eq!(base.fog.end_enabled[SKY_DARKEN_LANE], 0.0);
        assert_eq!(base.sky_darken(), 1.0);

        let dark = base.with_sky_darken(0.24);
        assert!((dark.sky_darken() - 0.24).abs() < 1e-6);
        // Everything else is byte-identical: same eye, same colour+start, same
        // end and enabled flag.
        assert_eq!(dark.fog.eye, base.fog.eye);
        assert_eq!(dark.fog.color_start, base.fog.color_start);
        assert_eq!(dark.fog.end_enabled[0], base.fog.end_enabled[0]);
        assert_eq!(dark.fog.end_enabled[1], base.fog.end_enabled[1]);
        assert_eq!(dark.camera.view_proj, base.camera.view_proj);
        // `with_sky_darken` itself did not grow the struct further — it still
        // matches the model pipeline at `FogUniform`'s current (four-`vec4`)
        // size.
        assert_eq!(core::mem::size_of::<EntityCameraUniform>(), 144);
    }

    #[test]
    fn from_mat4_is_column_major() {
        // A translation matrix: glam stores translation in the 4th column, so
        // the packed [3] row must carry it (column-major → model[3] is col 3).
        let m = glam::Mat4::from_translation(glam::Vec3::new(1.0, 2.0, 3.0));
        let raw = EntityInstanceRaw::from_mat4(m);
        assert_eq!(raw.model[3][0], 1.0);
        assert_eq!(raw.model[3][1], 2.0);
        assert_eq!(raw.model[3][2], 3.0);
        assert_eq!(raw.model[3][3], 1.0);
    }

    // -----------------------------------------------------------------------
    // Mob fire geometry (issue #434)
    // -----------------------------------------------------------------------

    /// A zombie's real `(width, height)` from
    /// `lodestone_data::entity_dimensions::base_dimensions` (verified against
    /// the generated table directly: index 151, `(0.6, 1.95)`). Predicted by
    /// hand-simulating `FlameFeatureRenderer.prepare` in Python before writing
    /// this Rust: **6** quads, `s = 0.84`, first quad half-width `0.42` world
    /// blocks, last (6th) quad half-width `0.2952 * 0.84 ≈ 0.248` world
    /// blocks, top edge of the stack at `3.65 * 0.84 ≈ 3.066` world blocks
    /// above the feet.
    ///
    /// The **rejected hypothesis**, stated before measuring: a version of this
    /// function that forgot the `/s` scale on `h` (i.e. used raw `height`
    /// directly as the loop bound, an easy mistake since `height` alone *is*
    /// vanilla's other bounding-box field) predicts **5** quads for a zombie,
    /// not 6 — the two hypotheses are only 1 quad apart here, which is exactly
    /// why this needs an exact count assertion and not a "some quads" check.
    #[test]
    fn zombie_flame_geometry_matches_the_hand_derived_prediction() {
        let quads = flame_quads(0.6, 1.95);
        assert_eq!(
            quads.len(),
            6,
            "zombie (0.6x1.95): predicted 6 quads (s=0.84, h=2.3214, step 0.45); \
             the rejected /s-forgetting hypothesis predicts 5"
        );

        let s = 0.6 * 1.4;
        // First quad: half-width r=0.5 (local) -> 0.42 world; y spans 0..1.4
        // local -> 0..1.176 world.
        let first = &quads[0];
        let first_half_width = (first.vertices[1].position[0] - first.vertices[0].position[0]) / 2.0;
        assert!(
            (first_half_width - 0.5).abs() < 1e-5,
            "first quad's local half-width must be the initial r=0.5, got {first_half_width}"
        );
        assert!(
            (first_half_width * s - 0.42).abs() < 1e-4,
            "first quad's world half-width must be 0.42 blocks, got {}",
            first_half_width * s
        );
        assert!(
            (first.vertices[0].position[1] - 0.0).abs() < 1e-5,
            "first quad's bottom edge must sit at the feet (local y=0)"
        );
        assert!(
            (first.vertices[2].position[1] - 1.4).abs() < 1e-5,
            "first quad's top edge must be at local y=1.4"
        );

        // Last (6th) quad: r has shrunk by 0.9^5, top edge at local y = 3.65.
        let last = quads.last().expect("6 quads");
        let last_half_width = (last.vertices[1].position[0] - last.vertices[0].position[0]) / 2.0;
        let expected_r = 0.5 * 0.9f32.powi(5);
        assert!(
            (last_half_width - expected_r).abs() < 1e-4,
            "6th quad's local half-width must be 0.5 * 0.9^5 = {expected_r}, got {last_half_width}"
        );
        assert!(
            (last.vertices[2].position[1] - 3.65).abs() < 1e-4,
            "6th quad's top edge must be at local y=3.65, got {}",
            last.vertices[2].position[1]
        );
        // World-space top of the whole stack.
        let world_top = last.vertices[2].position[1] * s;
        assert!(
            (world_top - 3.066).abs() < 1e-3,
            "top of the zombie's flame stack must be ~3.066 world blocks above \
             its feet, got {world_top}"
        );
    }

    /// A player's `(0.6, 1.8)` — one `0.45` step short of a zombie's `1.95`,
    /// so the predicted count drops by exactly one quad (5, not 6): a control
    /// that the quad count actually tracks height rather than being a fixed
    /// constant for "any biped-shaped hitbox".
    #[test]
    fn player_flame_geometry_has_one_fewer_quad_than_a_zombie() {
        assert_eq!(flame_quads(0.6, 1.8).len(), 5);
        assert_eq!(flame_quads(0.6, 1.95).len(), 6);
    }

    /// A spider's `(1.4, 0.9)` — wide and short rather than tall and narrow.
    /// `s = 1.96` is more than double a zombie's, so despite a *smaller* real
    /// height, `h = height / s = 0.459` is much smaller still: predicted 2
    /// quads, not the "shorter hitbox, fewer quads by a small margin" a reader
    /// might guess from height alone. This is the case that actually exercises
    /// the `/s` division mattering in the *other* direction from the zombie
    /// test above.
    #[test]
    fn spider_flame_geometry_reflects_the_width_scaled_height() {
        let quads = flame_quads(1.4, 0.9);
        assert_eq!(quads.len(), 2, "spider (1.4x0.9): predicted 2 quads (s=1.96, h=0.459)");
        let s = 1.4 * 1.4;
        let first_half_width = (quads[0].vertices[1].position[0] - quads[0].vertices[0].position[0]) / 2.0;
        assert!(
            (first_half_width * s - 0.98).abs() < 1e-3,
            "spider's first quad world half-width must be 0.98 blocks, got {}",
            first_half_width * s
        );
    }

    /// **The billboard sign, derived from a real [`Camera`] rather than
    /// asserted as a polarity, at eight azimuths.**
    ///
    /// The invariant: the flame's stack leans along its local `+Z`, so after the
    /// billboard that direction must point **toward** the camera — every quad
    /// must land strictly nearer the eye than the mob's own centre line, from
    /// every direction you can orbit to.
    ///
    /// Both wrong hypotheses are computed in the same run and required to fail
    /// somewhere, which is the part that matters: a **sign flip alone**
    /// (`Ry(-yaw)`) is invisible at yaw `0` and `180`, so a gate that only looked
    /// from one side — or from the two obvious opposed sides — passes with it. It
    /// takes an off-axis azimuth to separate them.
    #[test]
    fn the_flames_lean_is_toward_the_camera_from_every_azimuth() {
        use crate::Camera;
        use glam::{Mat4, Vec3};

        // The most-forward local point of the stack: the first quad's own z,
        // read off the geometry rather than restated from `0.3`.
        let quads = flame_quads(0.6, 1.95);
        let lean_z = quads[0].vertices[0].position[2];
        assert!(
            lean_z > 0.0,
            "the premise of this gate is that the stack leans along +Z; it is {lean_z}"
        );

        // How much nearer the eye a leaning point should be, in world blocks.
        let expect_nearer = lean_z * flame_scale(0.6);

        let feet = Vec3::new(4.0, 65.0, -7.0);
        let mut sign_flip_failed = false;
        let mut half_turn_failed = false;
        for step in 0..8 {
            let yaw = 45.0 * step as f32;
            // A real camera looking at the mob from `yaw`, placed by its own
            // `forward` so nothing about the placement restates the rotation
            // under test.
            let mut camera = Camera {
                yaw,
                pitch: 0.0,
                ..Camera::default()
            };
            camera.position = feet - camera.forward() * 6.0;

            let apply = |m: Mat4| {
                let world = m.transform_point3(Vec3::new(0.0, 0.0, lean_z));
                let centre = m.transform_point3(Vec3::ZERO);
                // Positive means the leaning point is nearer the eye.
                camera.position.distance(centre) - camera.position.distance(world)
            };

            let correct = apply(flame_instance_matrix(feet, yaw, 0.6));
            assert!(
                (correct - expect_nearer).abs() < 1e-3,
                "yaw {yaw}: the lean must put the stack {expect_nearer} blocks \
                 nearer the eye, measured {correct}"
            );

            // Hypothesis A: the sign flipped. Invisible at yaw 0 and 180.
            let flipped = apply(
                Mat4::from_translation(feet)
                    * Mat4::from_scale(Vec3::splat(flame_scale(0.6)))
                    * Mat4::from_rotation_y(-yaw.to_radians()),
            );
            if flipped < 0.0 {
                sign_flip_failed = true;
            }
            // Hypothesis B: the half turn dropped — what shipped.
            let no_half_turn = apply(
                Mat4::from_translation(feet)
                    * Mat4::from_scale(Vec3::splat(flame_scale(0.6)))
                    * Mat4::from_rotation_y(yaw.to_radians()),
            );
            if no_half_turn < 0.0 {
                half_turn_failed = true;
            }
        }
        assert!(
            sign_flip_failed,
            "the sign-flip hypothesis must lean away from the eye at some \
             azimuth, or this gate cannot see a sign error"
        );
        assert!(
            half_turn_failed,
            "the dropped-half-turn hypothesis must lean away from the eye at \
             some azimuth"
        );
    }

    /// **The flame's size is per entity, and an age scale changes the scale but
    /// not the layer count.**
    ///
    /// Both numbers are predicted from vanilla's constants rather than asserted
    /// as "smaller": an adult zombie's `s` is `0.6 × 1.4 = 0.84`, a baby's box is
    /// `getDimensions().scale(0.5)` so its `s` is exactly `0.42`.
    ///
    /// The layer-count half is the counter-intuitive one and it is the reason
    /// this is not a "babies get fewer quads" change. The count comes from
    /// `h = height / s = height / (width × 1.4)`, which is **invariant** under a
    /// uniform box scale — so vanilla itself gives a baby zombie the same six
    /// quads at half the size. The spider arm is the control proving the count is
    /// not simply constant: it varies with **aspect ratio**, which an age scale
    /// does not touch.
    #[test]
    fn an_age_scale_halves_the_flames_scale_and_leaves_its_layer_count_alone() {
        // Zombie: base 0.6 x 1.95.
        let adult_w = 0.6f32;
        let baby_w = adult_w * 0.5;
        assert!((flame_scale(adult_w) - 0.84).abs() < 1e-6);
        assert!((flame_scale(baby_w) - 0.42).abs() < 1e-6);
        assert!(
            flame_scale(baby_w) < flame_scale(adult_w),
            "a baby's flame must be strictly smaller"
        );

        let adult = flame_quads(adult_w, 1.95);
        let baby = flame_quads(baby_w, 1.95 * 0.5);
        assert_eq!(adult.len(), 6);
        assert_eq!(
            baby.len(),
            adult.len(),
            "a uniform box scale leaves h = height / (width * 1.4) unchanged, so \
             vanilla gives a baby the same layer count at half the scale"
        );

        // The control: the count really does vary, just not with age. A spider is
        // wide and low, so its h is far smaller than a zombie's.
        assert_eq!(flame_quads(1.4, 0.9).len(), 2);
        assert_eq!(flame_quads(1.4 * 0.5, 0.9 * 0.5).len(), 2);

        // And the whole point of the per-instance scale: the same mesh at two
        // different scales really does produce two different world sizes.
        let adult_top = adult.last().unwrap().vertices[2].position[1] * flame_scale(adult_w);
        let baby_top = baby.last().unwrap().vertices[2].position[1] * flame_scale(baby_w);
        assert!(
            (adult_top - 2.0 * baby_top).abs() < 1e-3,
            "the baby's flame must be exactly half as tall in world blocks: \
             {adult_top} vs {baby_top}"
        );
    }

    /// The **negative control**: a non-positive width or height must yield an
    /// empty mesh rather than dividing by zero, looping forever, or producing
    /// NaN geometry. Vanilla's own `while (h > 0.0F)` simply never enters for
    /// these inputs; this must not enter either.
    #[test]
    fn degenerate_dimensions_yield_no_flame_quads() {
        assert!(flame_quads(0.0, 1.95).is_empty(), "zero width must not divide by s=0");
        assert!(flame_quads(0.6, 0.0).is_empty(), "zero height must yield h=0, no iterations");
        assert!(flame_quads(-0.6, 1.95).is_empty(), "negative width must not be entered");
        assert!(flame_quads(0.6, -1.0).is_empty(), "negative height must not be entered");
    }

    /// `flame_mesh` must bake exactly 4 vertices and 6 indices per quad
    /// [`flame_quads`] produces, wound the same way every other baked quad in
    /// this crate is (two triangles sharing the bottom-left/top-right
    /// diagonal), and every index must stay in bounds.
    #[test]
    fn flame_mesh_bakes_one_quad_per_four_vertices_and_six_indices() {
        let (vertices, indices) = flame_mesh(0.6, 1.95);
        assert_eq!(vertices.len(), 6 * 4);
        assert_eq!(indices.len(), 6 * 6);
        for &i in &indices {
            assert!((i as usize) < vertices.len(), "index {i} out of bounds");
        }
        // First quad's two triangles: (0,1,2) and (0,2,3).
        assert_eq!(&indices[0..6], &[0, 1, 2, 0, 2, 3]);
    }

    /// [`FlameQuad::fire_1`] must alternate starting from `fire_0`
    /// (`FlameFeatureRenderer.java:45`'s `ss % 2 == 0 ? fire1 : fire2`, and
    /// `fire1` names `ModelBakery.FIRE_0` — see that field's own doc for why
    /// the naming looks swapped and is not).
    #[test]
    fn flame_quads_alternate_textures_starting_from_fire_0() {
        let quads = flame_quads(0.6, 1.95);
        assert_eq!(
            quads.iter().map(|q| q.fire_1).collect::<Vec<_>>(),
            vec![false, true, false, true, false, true]
        );
    }

    /// [`FlameInstanceRaw`] must occupy exactly locations 4-8 (past
    /// [`ModelVertex`]'s 0..=3, matching [`EntityInstanceRaw`]'s own promise),
    /// with the frame index immediately after the matrix.
    #[test]
    fn flame_instance_raw_is_a_matrix_plus_one_frame_word() {
        assert_eq!(core::mem::size_of::<FlameInstanceRaw>(), 68);
        let layout = FlameInstanceRaw::instance_layout();
        assert_eq!(layout.array_stride, 68);
        assert_eq!(layout.attributes.len(), 5);
        assert_eq!(layout.attributes[0].shader_location, 4);
        assert_eq!(layout.attributes[3].shader_location, 7);
        assert_eq!(layout.attributes[4].shader_location, 8);
        assert_eq!(layout.attributes[4].offset, 64);
        assert_eq!(layout.attributes[4].format, wgpu::VertexFormat::Uint32);
    }
}
