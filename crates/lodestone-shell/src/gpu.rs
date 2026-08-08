//! GPU render state for the shell: owns the block pipeline, the atlas, a depth
//! buffer, and a per-section table of uploaded meshes + camera uniforms, and
//! draws them all in one pass.
//!
//! The **packed** (demo-world) path still gives every section its own
//! camera-uniform buffer, rewritten with the current `view_proj` each frame
//! before the pass opens (see [`upload_packed_section`](RenderState::upload_packed_section)
//! and the top of [`render_inner`](RenderState::render_inner)) — the demo
//! world is capped at a few thousand sections and never runs live, so this was
//! never the measured cost.
//!
//! The **model** (live-vanilla) path does not: issue #75 profiled a live
//! session and found this same per-section-buffer shape responsible for 52.9%
//! of main-thread CPU, rewriting *every* resident section's whole camera
//! uniform every frame — thousands of `queue.write_buffer` calls for data that
//! is almost entirely constant (`view_proj` is identical for every section;
//! only `section_origin` differs, and it never changes for a section's
//! lifetime). [`ModelRenderer`] instead keeps one shared camera+fog buffer
//! (written once per frame) and one [`SectionOriginArena`] of per-section
//! origins addressed by a dynamic offset at draw time (written once, at
//! upload). See `docs/section-camera-uniform.md`.
use std::collections::HashMap;

use lodestone_assets::equipment::{ArmourLayerType, ArmourSlot};
use lodestone_render::{BlockPipeline, DepthBuffer, GpuAtlas, InstanceTint, fog::FogSettings};

use lodestone_model::event::EquipmentSlot;

use crate::mesher::SectionKey;
use crate::particles::ParticleRenderer;

mod block_entities;
mod debug_lines;
mod entities;
mod entity_passes;
mod first_person;
mod frame;
mod glint;
mod nametag;
mod occlusion;
mod outline;
mod screen_effects;
mod sections;
mod sign_text;
mod sources;
mod state;
mod stats;
mod terrain;
mod world_items;
#[cfg(test)]
mod pixel_gates;
#[cfg(test)]
mod tests;

pub use debug_lines::{
    DebugLineVertex, chunk_border_vertices, debug_line_vertices, entity_hitbox_vertices,
};
pub use occlusion::TerrainOcclusion;
pub use outline::{CrackTarget, gather_crack_targets};
pub use screen_effects::ScreenEffects;
pub use sources::{
    BellSource, BlockEntitySource, EntityLightSource, HandSwingSource, MainHandSource,
    OutlineShapeSource, SignSource, SkullSource, SkyDarkenSource, ThirdPersonBodySource,
    ThirdPersonBodyState,
};
pub use stats::RenderStats;

use block_entities::BlockEntityRenderer;
use debug_lines::{DebugLineRenderer, DebugLinesSource};
use entities::EntityRenderer;
use nametag::NameTagRenderer;
use outline::OutlineRenderer;
use sign_text::SignTextRenderer;
use sources::TimeOfDaySource;
use terrain::{ModelRenderer, SectionGpu, SectionOriginArena};
#[cfg(test)]
use entities::{load_humanoid_armour_textures, model_tint};
#[cfg(test)]
use lodestone_render::{AnimInput, ArmourModelSet, EntityModelSet};
#[cfg(test)]
use sources::LOCAL_PLAYER_DRAW_ID;

/// The sky colour, in linear RGB.
///
/// Shared deliberately: this is both what the frame clears to *and* what
/// distance fog fades terrain into. If the two drifted apart the horizon would
/// show a band of haze in a colour the sky never is, so they read one constant.
///
/// This is `srgb_to_linear([0.53, 0.71, 0.92])` — `#87B5EB`, the intended
/// sky-blue hex, divided by 255 and then actually linearised. The constant
/// used to hold that `#87B5EB / 255` triple directly, labelled linear when it
/// was really sRGB; every consumer (this clear colour, and the fog colour in
/// `sim::fog_for_render_distance`) treats it as linear and gets gamma-encoded
/// again on the way to the screen, so the mislabelled value washed the sky out
/// (it displayed as `(192, 219, 246)`, saturation 0.22, instead of the intended
/// `(135, 181, 235)`).
///
/// This is the **bring-up default** only — `RenderState::new` seeds both the
/// clear colour and the fog colour from it, but `app.rs`'s `redraw()` then
/// drives both away from it together every frame a dimension-conditioned fog
/// (`Sim::fog_settings`, e.g. `FogSettings::nether`/`the_end`) or a submersion
/// override is active, via `RenderState::set_fog`/`set_clear_color` — always
/// called as a pair with the same colour, per `docs/dimension-visuals.md`.
pub const SKY_COLOR: [f32; 3] = [0.242_867, 0.462_361, 0.827_571];

/// Fraction of the view distance at which fog begins.
///
/// **This is vanilla's span expressed as a fraction, not a taste knob** (issue
/// #388). Vanilla does not use a fraction at all:
/// `FogRenderer.setupFog` (`FogRenderer.java:198-200`) fades over an absolute
/// band of `clamp(renderDistanceInBlocks / 10, 4, 64)` blocks ending at the view
/// distance — see [`lodestone_render::fog::render_distance_fade_span`], which is
/// the authoritative form and the one
/// [`FogSettings::for_render_distance`](lodestone_render::fog::FogSettings::for_render_distance)
/// uses. In the unclamped middle of that formula the band is exactly a tenth of
/// the view distance, so the onset is exactly `0.9` — algebraically identical
/// for every view distance from 40 to 640 blocks, i.e. render distances 3
/// through 40, which is the whole range anyone plays at.
///
/// It was `0.75`, and that is the bug this issue was filed over: at render
/// distance 16 the fade began 38 blocks early and a fragment 240 blocks out read
/// **0.75** fogged where vanilla reads **0.375** — twice the haze, well inside
/// terrain that is loaded and drawn. The fraction form also has no cap, so the
/// error grew with the render distance the player chose: 32 blocks of fade at
/// RD 8, 128 at RD 32, against vanilla's 12.8 and 51.2.
///
/// Outside `3..=40` chunks the fraction and the real formula part company (RD 2:
/// `28.8` here against vanilla's `28.0`; RD 48: `691.2` against `704.0`) because
/// only the formula has the 4-block floor and the 64-block cap.
/// `sim::fog_for_render_distance` is the last caller still going through the
/// fraction rather than `for_render_distance`; migrating it is a one-line change
/// this one deliberately did not make, since that file is held elsewhere.
/// `fog_start_fraction_matches_vanillas_span` below pins both the agreement and
/// the divergence so the gap cannot widen unnoticed.
pub const FOG_START_FRACTION: f32 = 0.9;

/// The render distance [`RenderState::new`] seeds its bring-up fog with, in
/// chunks — the same default `Config` carries, so a `RenderState` built before
/// the shell has read a config is fogged for the distance it will most likely
/// be asked for. Not authoritative for anything: every real path calls
/// `set_fog` with the configured distance before it draws.
const DEFAULT_RENDER_DISTANCE_CHUNKS: u32 = 8;

/// Owns all GPU resources needed to render the world.
#[derive(Debug)]
pub struct RenderState {
    pipeline: BlockPipeline,
    #[allow(dead_code)]
    atlas: GpuAtlas,
    #[allow(dead_code)]
    uv_buffer: wgpu::Buffer,
    atlas_bind_group: wgpu::BindGroup,
    depth: DepthBuffer,
    sections: HashMap<SectionKey, SectionGpu>,
    /// The packed path's group-0 binding 0: this frame's view-projection (and,
    /// from issue #400 on, its fog), shared by every packed section and written
    /// **once** per frame. See `docs/section-camera-uniform.md`.
    packed_shared_cam_buffer: wgpu::Buffer,
    /// The one group-0 bind group every packed section draws through, over
    /// [`Self::packed_shared_cam_buffer`] and [`Self::packed_origin_arena`].
    /// Built once; a draw picks its section by dynamic offset, never by
    /// rebuilding this.
    packed_cam_bind_group: wgpu::BindGroup,
    /// Per-section world origins for the packed table, one arena slot each,
    /// written at upload rather than per frame (issue #76). Allocated on every
    /// run — the packed table is empty in live play, and 2 MiB is cheaper than a
    /// second construction path.
    packed_origin_arena: SectionOriginArena,
    model: Option<ModelRenderer>,
    outline: OutlineRenderer,
    /// The render half of `ExtractSet::Debug` (`docs/plugin-api.md`); see
    /// [`DebugLinesSource`] for why it starts empty until something installs
    /// a source.
    debug_lines: DebugLineRenderer,
    debug_lines_source: DebugLinesSource,
    entities: EntityRenderer,
    /// Render-frame counter driving the mob-fire billboard's texture
    /// animation (issue #434) — see `prepare_flame`'s doc for why this counts
    /// render frames rather than the real 20 Hz game tick.
    flame_frame_counter: std::cell::Cell<u64>,
    /// Block-break debris **and** sheet particles (flame, smoke, crits,
    /// splashes). Bound to *both* stitches: whichever atlas the terrain draws
    /// from, so a debris fragment is textured from the same pixels as the block
    /// it came off, and the separate particle sheet, so a flame is textured
    /// from `textures/particle/flame.png` — see [`Self::particle_sheet_atlas`].
    particles: ParticleRenderer,
    particle_atlas_bind_group: wgpu::BindGroup,
    /// The stitched particle sheet uploaded to the GPU, or `None` on a jar-less
    /// run (headless tests, no `client.jar`), in which case
    /// [`Self::particle_atlas_bind_group`] holds a 1×1 transparent stand-in in
    /// the sheet slots.
    ///
    /// Kept as a field, not dropped after building the bind group: `has_*`
    /// needs it, and issue #45's whole lesson is that "the sheet texture is
    /// installed" must be answerable from outside this module rather than
    /// inferred from pixels.
    particle_sheet_atlas: Option<GpuAtlas>,
    /// One-shot latch for the "sheet instances submitted, no sheet texture"
    /// warning in [`Self::prepare_particles`]. A per-frame log would be 60
    /// lines a second of the same sentence.
    warned_missing_particle_sheet: bool,
    /// What a pixel nothing else drew this frame clears to. Seeded from
    /// [`SKY_COLOR`] at construction; kept in step with [`fog`](Self::fog)'s
    /// colour thereafter via [`RenderState::set_clear_color`] — see that
    /// method's doc for why the two must never disagree.
    clear: wgpu::Color,
    /// Linear distance fog fading the outermost loaded chunks into the sky (or,
    /// later, a biome water colour when submerged). Defaults to a sky-coloured
    /// fog sized for the default render distance; drive it from the real render
    /// distance / eye-in-fluid state via [`RenderState::set_fog`].
    fog: FogSettings,
    /// The player's render distance in chunks, set with [`RenderState::set_fog`]
    /// because the sky disc's gradient end is clamped to it (#399). Defaults to
    /// [`DEFAULT_RENDER_DISTANCE_CHUNKS`] alongside `fog`.
    render_distance_chunks: u32,
    /// Whether the per-frame terrain cull runs at all — vanilla's `smartCull`
    /// switch, and this client's false-cull diagnostic. `true` by default; set it
    /// with [`RenderState::set_terrain_culling`]. With it off, every resident
    /// section submits a draw at every heading, which is the pre-#543 behaviour
    /// and the A/B arm the instruction harness measures against.
    terrain_culling: bool,
    /// The section occlusion graph (U3): one [`SectionVisibility`] per section
    /// the mesher has produced, maintained by `upload_section`/`remove_section`.
    ///
    /// Sections with **no** geometry are in here too — a fully-enclosed
    /// underground section meshes to nothing and is exactly the blocker that
    /// makes the underground free. All-*air* sections are not, and must not be:
    /// they never reach a mesh worker at all, which is why the walk defaults an
    /// absent in-bounds coord to open (see `walk_visible_bounded`).
    vis_graph: lodestone_render::VisibilityGraph,
    /// This frame's reachable set and the key it was walked for. `RefCell`
    /// because `render_inner` takes `&self` (as `flame_frame_counter` already
    /// does), and cached because vanilla's cadence is to re-walk only on an
    /// 8-block camera-cell crossing or a graph change — never on rotation, which
    /// is what keeps mouse movement off the walk.
    occlusion: std::cell::RefCell<occlusion::OcclusionCache>,
    /// Whether the reachable set culls, only counts, or is not walked at all.
    /// See [`RenderState::set_terrain_occlusion`].
    occlusion_mode: TerrainOcclusion,
    /// How each mob's world light is sampled. Full-bright until the shell wires
    /// a real world in via [`RenderState::set_entity_light_source`].
    entity_light: EntityLightSource,
    /// How bright the sky is *right now*. Permanent noon until the shell wires a
    /// world clock in via [`RenderState::set_sky_darken_source`].
    sky_darken: SkyDarkenSource,
    /// Where the local player's own third-person body comes from, if a
    /// caller has wired one in. Unset until the shell has both a
    /// third-person camera and a way to describe the local player's pose —
    /// see [`RenderState::set_third_person_body_source`].
    third_person_body: ThirdPersonBodySource,
    /// How far through an arm swing the local player is *right now*. A rested
    /// arm until the shell wires its swing clock in via
    /// [`RenderState::set_hand_swing_source`].
    hand_swing: HandSwingSource,
    /// What the local player is holding in their main hand, for the first-person
    /// pass. Empty (bare arm) until the shell wires it in via
    /// [`RenderState::set_main_hand_source`].
    main_hand: MainHandSource,
    /// Vanilla's `ItemInHandRenderer` swap state (issue #366): which held item is
    /// *drawn* — which lags [`Self::main_hand`] across a hotbar change — and how far
    /// the hand is lowered.
    ///
    /// Stepped by [`RenderState::set_main_hand_source`], which is the shell's one
    /// per-frame `&mut self` hop; read by `prepare_first_person_hand` for both the
    /// item and the bare-arm branch. Its default is "fully equipped, empty hand", so
    /// a caller that never installs a main-hand source sees exactly the pre-#366
    /// behaviour.
    equip: first_person::HeldItemEquip,
    /// Vanilla bobs the hand with a *second, independent* application of
    /// `bobView` — `GameRenderer.renderItemInHand` seeds a fresh `PoseStack`
    /// with the **unbobbed** inverted model-view and re-applies the bob to that
    /// (`GameRenderer.java:333-362`), rather than inheriting the world's already
    /// bobbed matrix. Unset reads as `BobFrame::default`, i.e. the pre-existing
    /// unbobbed hand, so headless tests are unaffected.
    hand_bob: first_person::HandBobSource,
    outline_shape: OutlineShapeSource,
    /// The sky pass (disc/sun/moon/stars/clouds), built once the vanilla
    /// celestial atlas and cloud texture are available. `None` — no
    /// `client.jar`, a headless test, or simply before [`RenderState::install_sky`]
    /// runs — reproduces this struct's behaviour before the sky existed
    /// exactly: [`render_inner`](Self::render_inner) clears straight to
    /// [`Self::clear`] and draws no sky pass at all.
    sky: Option<lodestone_render::SkyRenderer>,
    /// The world clock the sky pass reads (see [`TimeOfDaySource`]). Permanent
    /// noon until the shell wires a world clock in via
    /// [`RenderState::set_time_of_day_source`] — the same "unset means noon"
    /// convention [`SkyDarkenSource`] already uses.
    time_of_day: TimeOfDaySource,
    /// The underwater/fire screen-overlay pass (issues #108, #112), built once
    /// the vanilla `underwater.png`/`fire_1.png` textures are available. `None`
    /// — no `client.jar`, a headless test, or simply before
    /// [`RenderState::install_screen_effects`] runs — draws neither overlay,
    /// the same "no pass installed, nothing extra drawn" convention
    /// [`Self::sky`] uses.
    screen_effects: Option<lodestone_render::ScreenEffectRenderer>,
    /// Billboarded entity/player nametags (issue #100). Always constructed —
    /// unlike [`Self::sky`]/[`Self::screen_effects`], there is no "install"
    /// step: [`NameTagRenderer::new`] loads its own jar-sourced font
    /// (fail-open to drawing nothing, same contract as
    /// [`crate::hud::vanilla_font::VanillaFont::shared`]), so nothing
    /// downstream needs to know whether it succeeded.
    nametag: NameTagRenderer,
    /// Block-entity rigs — chests today (issue #23). Always constructed, like
    /// [`Self::nametag`] and unlike [`Self::sky`]: it loads its own sheets from
    /// the jar and fail-opens to drawing nothing, so there is no install step for
    /// a caller to forget.
    ///
    /// A chest has no block model whatsoever in 26.2, so without this the block
    /// pass leaves a **hole** where every chest is; that is why it is
    /// unconditional rather than opt-in.
    block_entities: BlockEntityRenderer,
    /// Where this frame's chests come from. Empty until the shell wires the
    /// world in via [`RenderState::set_block_entity_source`] — the same
    /// "unset means draw nothing" convention every other source here uses.
    block_entity_source: BlockEntitySource,
    /// Where this frame's skulls and heads come from. Same "unset means draw
    /// nothing" convention as [`Self::block_entity_source`], and a separate
    /// field for the reason [`SkullSource`] documents.
    skull_source: SkullSource,
    /// Where this frame's bells come from. Same "unset means draw nothing"
    /// convention as [`Self::skull_source`], and a separate field for the
    /// reason [`BellSource`] documents. Unset by every current caller — see
    /// that source's own doc for why a bell still draws (always at rest)
    /// rather than not existing at all.
    bell_source: BellSource,
    /// World-space sign text (issue #23). Always constructed, like
    /// [`Self::nametag`]: it loads its own jar-sourced font and fail-opens to
    /// drawing nothing. A sign's *board* is a real block model (unlike chest
    /// or skull) and already draws through the ordinary terrain pass with no
    /// help from this field — this only ever draws the text painted on it.
    sign_text: SignTextRenderer,
    /// Where this frame's signs come from. Same "unset means draw nothing"
    /// convention as [`Self::skull_source`].
    sign_source: SignSource,
    /// The rain/snow pass, built once `textures/environment/{rain,snow}.png` are
    /// available. `None` — no `client.jar`, a headless test, or simply before
    /// [`RenderState::install_weather`] runs — draws no precipitation, the same
    /// "no pass installed, nothing extra drawn" convention [`Self::sky`] uses.
    ///
    /// Note the *darkening* half of weather does not live here at all: rain and
    /// thunder reach pixels through the sky/fog colours and the `sky_darken`
    /// source, all of which `crate::app` composes before it calls
    /// [`Self::set_fog`] / [`Self::set_sky_darken_source`]. So a build with no
    /// jar still gets a correctly darkened storm — it just gets no visible
    /// droplets, which is the honest degradation.
    weather: Option<lodestone_render::WeatherRenderer>,
    /// The enchantment-glint pass (issue #452): the `GlintPipeline` plus an
    /// uploaded non-sRGB `enchanted_glint_item.png` sheet and one shared group-0
    /// uniform buffer. `None` — no `client.jar`, a headless test, or simply
    /// before [`RenderState::install_glint`] runs — draws no second pass, so an
    /// enchanted held item renders without its shimmer: the same "no pass
    /// installed, nothing extra drawn" convention [`Self::sky`] uses.
    glint: Option<glint::GlintPass>,
}

/// Per-part instance accumulation for the sheep wool layer, before upload.
/// Mirrors [`ArmourPartAccum`], minus the texture grouping: wool has one
/// sheet, so there is nothing to group by beyond the part itself.
struct WoolPartAccum {
    range: lodestone_render::PartRange,
    transforms: Vec<glam::Mat4>,
    lights: Vec<u32>,
    tints: Vec<InstanceTint>,
}

/// One model type's uploaded flame-instance buffer for a frame (issue #434)
/// — the mob-fire counterpart to [`WoolPartAccum`]/`ArmourDrawBatch`, simpler
/// than either because the flame mesh has no per-part skeleton attachment:
/// one buffer, one draw.
struct FlameBatch {
    /// The `EntityDraw::type_path` this batch's mesh
    /// (`EntityRenderer::flame_gpu_models`) is keyed by.
    model: String,
    buffer: wgpu::Buffer,
    count: u32,
}

/// One model type's uploaded per-part instance buffers for a frame. `parts[p]`
/// holds one matrix per visible instance of part `p`; a `None` slot is a part
/// with no geometry (nothing to draw).
#[derive(Debug)]
struct EntityDrawBatch {
    model: &'static str,
    count: u32,
    parts: Vec<Option<wgpu::Buffer>>,
}

/// One `(armour slot, texture)` group's uploaded instance buffers for a frame.
///
/// The **order of these in the returned `Vec` is load bearing**: leather's
/// `humanoid` layer list is a dyeable base sheet and an untinted
/// `leather_overlay` at the *same* inflation, so the two are coplanar and the
/// overlay only wins the (`LessEqual`) depth test if it is drawn second. Batches
/// are accumulated in insertion order — slot in `ArmourSlot::ALL` order, then
/// layer in declaration order — never through a `HashMap`.
#[derive(Debug)]
struct ArmourDrawBatch {
    slot: ArmourSlot,
    texture: (&'static str, ArmourLayerType),
    /// `(index range, instance buffer, instance count)` per armour part that
    /// anything in this group used.
    parts: Vec<(lodestone_render::PartRange, wgpu::Buffer, u32)>,
}

/// Per-part instance accumulation for one `(slot, texture)` group, before upload.
struct ArmourAccum {
    slot: ArmourSlot,
    texture: (&'static str, ArmourLayerType),
    parts: Vec<ArmourPartAccum>,
}

struct ArmourPartAccum {
    range: lodestone_render::PartRange,
    transforms: Vec<glam::Mat4>,
    lights: Vec<u32>,
    tints: Vec<InstanceTint>,
}

/// The [`ArmourSlot`] an [`EquipmentSlot`] maps onto, or `None`.
///
/// This is vanilla's `EquipmentSlot.Type.HUMANOID_ARMOR` predicate
/// (`EquipmentSlot.java:15-19`) and nothing looser. In particular:
///
/// * **`Body` is not `Chest`.** `BODY` is `ANIMAL_ARMOR` — wolf armour and horse
///   barding live there — and `SADDLE` is its own type. A fold of `"body"` into
///   the chest slot was removed from the item census for exactly this reason
///   (`docs/item-prototypes.md`), and reintroducing it here would put a horse's
///   diamond barding on a player's torso.
/// * **`EquipmentSlot::isArmor` is the wrong predicate** even though it sounds
///   right: it is the *union* of humanoid and animal armour
///   (`EquipmentSlot.java:73-75`).
/// * `MainHand`/`OffHand` are held items and go through `merge_held_items`.
fn humanoid_armour_slot(slot: EquipmentSlot) -> Option<ArmourSlot> {
    match slot {
        EquipmentSlot::Head => Some(ArmourSlot::Head),
        EquipmentSlot::Chest => Some(ArmourSlot::Chest),
        EquipmentSlot::Legs => Some(ArmourSlot::Legs),
        EquipmentSlot::Feet => Some(ArmourSlot::Feet),
        EquipmentSlot::MainHand
        | EquipmentSlot::OffHand
        | EquipmentSlot::Body
        | EquipmentSlot::Saddle => None,
    }
}

/// A 1×1 fully transparent [`GpuAtlas`], for a texture slot whose real contents
/// are not available yet.
///
/// Used for the particle pass's sheet slot before
/// [`RenderState::install_particle_sheet_atlas`] runs. Transparent rather than
/// magenta-or-similar on purpose: the particle shader discards below `a < 0.02`,
/// so an unbacked sheet particle disappears instead of painting a debug colour
/// over the world. The *loud* half of that pairing is the one-shot warning in
/// [`RenderState::prepare_particles`] plus
/// [`RenderState::has_particle_sheet_atlas`] — a silent placeholder with no way
/// to observe it would be the island pattern again.
fn transparent_placeholder_atlas(device: &wgpu::Device, queue: &wgpu::Queue) -> GpuAtlas {
    GpuAtlas::from_rgba(device, queue, 1, 1, &[0, 0, 0, 0], &[])
}

/// The bytes the sky clear actually lands on in these tests' readbacks.
///
/// Every headless test here uses an **`Rgba8Unorm`** target, so no gamma
/// encode happens on write and the readback is [`SKY_COLOR`] (which is
/// linear) scaled straight to bytes — *not* the `#87B5EB` the player sees on
/// the sRGB swapchain.
///
/// Derived rather than hardcoded because it was hardcoded twice and both
/// copies went stale: when `SKY_COLOR` was corrected from a mislabelled sRGB
/// triple to its true linear value, one of the three copies was updated and
/// two were not. Those two tests then classified *every* pixel in the frame
/// as "mob" — including the corners, which contain no mob — so their
/// silhouette assertions were measuring the whole frame.
#[cfg(test)]
#[must_use]
fn sky_clear_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}
