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
//! The **model** (live-vanilla) path does not: That fix profiled a live
//! session and found this same per-section-buffer shape responsible for 52.9%
//! of main-thread CPU, rewriting *every* resident section's whole camera
//! uniform every frame — thousands of `queue.write_buffer` calls for data that
//! is almost entirely constant (`view_proj` is identical for every section;
//! only `section_origin` differs, and it never changes for a section's
//! lifetime). [`ModelRenderer`] instead keeps one shared camera+fog buffer
//! (written once per frame) and one [`SectionOriginArena`] of per-section
//! origins addressed by a dynamic offset at draw time (written once, at
//! upload). See `docs/section-camera-uniform.md`.
use std::{collections::HashMap, ops::Range};

use lodestone_assets::equipment::{ArmourLayerType, ArmourSlot};
use lodestone_render::{
    BlockPipeline, DepthBuffer, GpuAtlas, InstanceBufferArena, InstanceTint, fog::FogSettings,
};

use lodestone_model::event::EquipmentSlot;

use crate::mesher::SectionKey;
use crate::particles::ParticleRenderer;

mod block_entities;
mod debug_lines;
// `pub(crate)` for one item only: `entity_texture_from_image`, which the
// container screen's inventory avatar shares rather than copying — see that
// function's own doc for why a second copy of the `Rgba8UnormSrgb` choice is a
// +48%-brightness bug waiting to happen. Everything else in here stays
// `pub(super)`, i.e. `pub(in crate::gpu)`.
pub(crate) mod entities;
mod beacon_beam;
mod lightning_bolt;
mod display_text;
mod end_portal;
mod entity_passes;
mod first_person;
mod frame;
mod glint;
// `pub(crate)`, not `pub(super)`: `hud::HudRenderer` owns a second, independent
// `GpuQueryTimer` for its own encoder/passes (a separate `wgpu::QuerySet` —
// HUD draws through its own `queue.submit`, not `RenderState::render`'s), so
// this type needs to be visible outside `crate::gpu` too. See
// `gpu_timing`'s module doc.
pub(crate) mod gpu_timing;
mod maps;
// The moving-block-model seam: block geometry drawn somewhere other than its own
// cell. Falling blocks today; piston heads are the second intended producer.
mod moving_blocks;
mod nametag;
mod occlusion;
mod outline;
mod pack_trace;
mod plugin_billboards;
mod screen_effects;
mod sections;
mod sign_text;
mod sources;
mod spawner_mobs;
mod state;
mod stats;
mod terrain;
mod terrain_cull_diagnostics;
mod world_items;
#[cfg(test)]
mod pixel_gates;
#[cfg(test)]
mod tests;

pub use debug_lines::{
    DebugLineVertex, chunk_border_vertices, debug_line_vertices, entity_hitbox_vertices,
    f3_overlay_vertices,
};
pub(crate) use debug_lines::{EntityHitboxState, f3_overlay_vertices_with_states};
pub(crate) use debug_lines::push_box;
pub use occlusion::TerrainOcclusion;
pub use plugin_billboards::{PluginBillboardInstance, plugin_billboard_vertices};
pub use outline::{CrackTarget, gather_crack_targets};
pub use screen_effects::ScreenEffects;
pub use maps::MapCacheCounters;
pub use sources::{
    AmbientLightSource, BannerSource, BeaconSource, BellSource, BlockEntitySource, BrushableSource,
    CampfireSource,
    ConduitSource, CopperGolemStatueSource, DecoratedPotSource, EnchantingTableSource,
    EndGatewayBeamSource, EndGatewaySource, EndPortalSource,
    EntityLightSource, HandSwingSource, ItemUseSource, ItemUseState, LecternSource, MainHandItem,
    MainHandSource, MapPicture, MapSource, MovingPistonSource, OutlineShapeSource, ShadowGroundSource,
    ShelfSource, ShulkerSource,
    SignSource, SkullSource, SkyDarkenSource, SpawnerSource, ThirdPersonBodySource,
    ThirdPersonBodyState, VaultSource,
};
pub use stats::RenderStats;

use beacon_beam::BeaconBeamRenderer;
use lightning_bolt::LightningBoltRenderer;
use block_entities::BlockEntityRenderer;
use display_text::DisplayTextRenderer;
use end_portal::EndPortalRenderer;
use debug_lines::{DebugLineRenderer, DebugLinesSource};
use entities::EntityRenderer;
use nametag::NameTagRenderer;
use outline::OutlineRenderer;
use plugin_billboards::{PluginBillboardRenderer, PluginBillboardsSource};
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
/// That fix). Vanilla does not use a fraction at all:
/// `FogRenderer.setupFog` fades over an absolute
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
    /// This frame's `bobHurt` transform, in **eye space** — the damage tilt and
    /// the death roll (`crate::camera_rig::BobFrame::hurt_transform`), installed
    /// once per frame by `app::redraw` and multiplied into every world-space
    /// view-projection this state writes.
    ///
    /// # Why a matrix here rather than fields on `Camera`
    ///
    /// `bobHurt` is almost entirely a **roll**, and `Camera` is parameterised by
    /// `position`/`yaw`/`pitch` — two angles — so the fold in
    /// `crate::camera_rig::bobbed_camera` structurally cannot carry it. That is
    /// why this half of the bob was held at zero for so long: not because the
    /// maths was unverified (it was ported and tested), but because there was no
    /// seam it could reach the matrix through. `Camera::view_projection_eye_space`
    /// is that seam, and it is vanilla's own
    /// `projectionMatrix.mul(bobStack.last().pose())`.
    ///
    /// Identity when the player has not been hit recently, which is almost every
    /// frame — and identity is *exactly* identity, so an unhurt frame's matrices
    /// are bit-identical to what they were before this existed.
    eye_bob: glam::Mat4,
    /// Vanilla's Damage Tilt accessibility option, for the **first-person hand**
    /// pass, which applies `bobHurt` a second time independently of the world's
    /// copy (`GameRenderer.renderItemInHand`).
    ///
    /// A strength rather than a matrix, unlike [`Self::eye_bob`], because the hand
    /// composes `bobHurt` and `bobView` together in one
    /// `crate::camera_rig::BobFrame::eye_transform` call — and it can, since
    /// nothing folds the hand's bob into a `Camera`.
    ///
    /// Defaults to vanilla's `1.0`, not `0.0`: an unhurt `BobFrame` produces a zero
    /// tilt at *any* strength, so the default cannot affect a caller that never
    /// installs a bob source, and a `0.0` default would be indistinguishable from
    /// the accessibility option being switched on.
    damage_tilt_strength: f32,
    /// Vanilla's **Glint Speed**/**Glint Strength** accessibility options
    /// ([`crate::config::Options::glint_speed`]/`glint_strength`), pushed down per
    /// frame by `app/redraw.rs` the same way [`Self::damage_tilt_strength`] is and
    /// read by `gpu::glint::glint_uniform` for the world and hand glint draws.
    ///
    /// Seeded to `lodestone_render::glint::DEFAULT_SPEED`/`DEFAULT_STRENGTH`,
    /// which *are* vanilla's shipped option values — so an install that never
    /// calls the setter, and every existing gate written against the defaults,
    /// sees byte-identical output.
    ///
    /// The speed is `f64` because `glint_clock` is: the scroll is a wall-clock
    /// millisecond count multiplied by the speed, and narrowing it to `f32` would
    /// lose resolution in the clock, not in the option.
    glint_speed: f64,
    /// See [`Self::glint_speed`].
    glint_strength: f32,
    /// Vanilla's **Clouds** option ([`crate::config::Options::cloud_status`]),
    /// pushed down per frame by `app/redraw.rs` and stamped onto the `SkyFrame`
    /// this renderer builds.
    ///
    /// `lodestone_render::SkyFrame::with_cloud_status` had **zero** production
    /// callers before this field: the pass always built
    /// `CloudStatus::default()`, so FAST cloud geometry existed and was pixel-gated
    /// and no player could ever select it.
    ///
    /// Defaults to `CloudStatus::default()` — `Fancy`, vanilla's own — so a
    /// renderer nobody pushes the option into draws exactly what it drew before.
    cloud_status: lodestone_render::CloudStatus,
    /// The connected **dimension's** own `Skybox` — vanilla's
    /// `DimensionType.skybox()`, not a graphics option. Pushed down per frame by
    /// `app/redraw.rs` from [`crate::Sim::sky_mode`] and stamped onto the
    /// `SkyFrame` this renderer builds.
    ///
    /// Sits beside [`Self::cloud_status`] rather than inside the fog settings on
    /// purpose: [`Self::set_fog`]/[`Self::set_clear_color`] carry *colours*, and a
    /// colour cannot express "draw no sun". The Nether's fog and clear were
    /// already correct while the overworld sun, moon, star field and cloud deck
    /// all still drew over it — this is the field that stops them.
    ///
    /// Defaults to `SkyMode::default()` (`Overworld`), so a renderer nobody
    /// pushes this into draws exactly what it drew before.
    sky_mode: lodestone_render::SkyMode,
    /// This level's void-fog geometry — the dimension's `min_y` and the
    /// flat-world onset fork. Pushed down per frame by `app/redraw.rs` from
    /// [`crate::Sim::void_fog`] and stamped onto the `SkyFrame`.
    ///
    /// A field rather than the `VoidFog::OVERWORLD` constant the frame builder
    /// used to name inline, because that constant is only right for a non-flat
    /// overworld: it hardcoded `min_y = -64` for the Nether and the End (whose
    /// floor is `0`, so their fade never applied at all) and a 32-block onset
    /// for superflat levels (whose own is `1.0`, so their ground rendered under
    /// a near-black sky).
    ///
    /// Defaults to [`lodestone_render::fog::VoidFog::OVERWORLD`], so a renderer
    /// nobody pushes this into draws exactly what it drew before.
    void_fog: lodestone_render::fog::VoidFog,
    pipeline: BlockPipeline,
    #[allow(dead_code)]
    atlas: GpuAtlas,
    #[allow(dead_code)]
    uv_buffer: wgpu::Buffer,
    atlas_bind_group: wgpu::BindGroup,
    depth: DepthBuffer,
    sections: HashMap<SectionKey, SectionGpu>,
    /// The packed path's group-0 binding 0: this frame's view-projection (and,
    /// from that fix on, its fog), shared by every packed section and written
    /// **once** per frame. See `docs/section-camera-uniform.md`.
    packed_shared_cam_buffer: wgpu::Buffer,
    /// The one group-0 bind group every packed section draws through, over
    /// [`Self::packed_shared_cam_buffer`] and [`Self::packed_origin_arena`].
    /// Built once; a draw picks its section by dynamic offset, never by
    /// rebuilding this.
    packed_cam_bind_group: wgpu::BindGroup,
    /// Per-section world origins for the packed table, one arena slot each,
    /// written at upload rather than per frame. Allocated on every
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
    /// The fishing line — a **second** [`DebugLineRenderer`], not a second
    /// implementation of one.
    ///
    /// It is separate from `debug_lines` above rather than sharing its buffer
    /// for two reasons, and only the first is obvious: the F3 overlay is toggled
    /// off almost always and this is gameplay geometry that must draw
    /// regardless, and the two want different on-screen widths (see
    /// `debug_lines::VANILLA_LINE_WIDTH_PX`). Everything expensive — the
    /// pipeline, the shader, the ribbon expansion — is shared, which is the
    /// point: that expansion already exists twice in this module tree and a
    /// third copy is how they stop agreeing.
    ///
    /// Fed by `RenderState::fishing_line_vertices`, drawn immediately after the
    /// entity sprite pass so the bobber and its line reach the frame together.
    fishing_line: DebugLineRenderer,
    /// The render half of `ExtractSet::Debug`'s billboard channel (issue
    /// #161, `docs/plugin-api.md`) — [`DebugLinesSource`]'s sibling for
    /// textured/billboard-style plugin draws rather than plain lines. See
    /// [`PluginBillboardsSource`] for why it starts empty.
    plugin_billboards: PluginBillboardRenderer,
    plugin_billboards_source: PluginBillboardsSource,
    /// The block atlas's own sprite table (`ResourceLocation` string → UV
    /// rect), snapshotted once at construction — what
    /// [`plugin_billboard_vertices`] resolves
    /// [`lodestone_ecs::PluginTexture::Named`] against. `Arc`-shared rather
    /// than rebuilt per install, since `install_plugin_billboards_source`
    /// (`app/session.rs`) needs its own clone to capture in a closure that
    /// outlives this borrow. Empty on the demo (no vanilla atlas) path, which
    /// is the honest answer: every `Named` billboard falls back to a flat
    /// tint there, exactly as an unresolved id does.
    plugin_atlas_sprites: std::sync::Arc<HashMap<String, [f32; 4]>>,
    entities: EntityRenderer,
    /// Retained CPU bytes and one GPU buffer for every tinted entity/block-entity
    /// instance in a frame. `render_inner` clears, appends, then uploads it once.
    instance_arena: InstanceBufferArena,
    /// Render-frame counter driving the mob-fire billboard's texture
    /// animation — see `prepare_flame`'s doc for why this counts
    /// render frames rather than the real 20 Hz game tick.
    flame_frame_counter: std::cell::Cell<u64>,
    /// The live game tick, as of the last [`Self::update_animation`] call —
    /// the section fade-in's clock (`lodestone_render::section_visibility`).
    /// `update_animation` is documented to run once per frame *before*
    /// `render`, so a section uploaded mid-frame (`upload_section`) reads a
    /// same-frame-or-one-frame-stale tick, never a future one — close enough
    /// for a 0.75 s fade. A `Cell`, not a new per-frame argument threaded
    /// through `upload_section`'s several call sites, for the same reason
    /// `flame_frame_counter` is one: `render_inner` and `upload_section` both
    /// take `&self`/`&mut self` without a fresh tick in hand.
    section_fade_tick: std::cell::Cell<u64>,
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
    /// needs it, and that fix's whole lesson is that "the sheet texture is
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
    /// because the sky disc's gradient end is clamped to it. Defaults to
    /// [`DEFAULT_RENDER_DISTANCE_CHUNKS`] alongside `fog`.
    render_distance_chunks: u32,
    /// Whether the per-frame terrain cull runs at all — vanilla's own smart-cull
    /// switch, and this client's false-cull diagnostic. `true` by default; set it
    /// with [`RenderState::set_terrain_culling`]. With it off, every resident
    /// section submits a draw at every heading, which is the pre-fix behaviour
    /// and the A/B arm the instruction harness measures against.
    terrain_culling: bool,
    /// Opt-in edge latch for live terrain/sign cull evidence. It is kept on
    /// the renderer because `render_inner` only has `&self`.
    terrain_cull_diagnostics: std::cell::RefCell<terrain_cull_diagnostics::Probe>,
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
    /// The camera's block position as of the most recent [`RenderState::render`]
    /// call — vanilla's `LevelRenderer.compileSections`'s `BlockPos
    /// cameraPosition = camera.blockPos`, cached here so `upload_section` can
    /// read it without a signature change threading a camera argument through
    /// every one of its several call sites (issue tracked in `docs/`: the
    /// near-distance fade skip).
    ///
    /// `Cell`, not a plain field, for the same reason [`Self::occlusion`] is a
    /// `RefCell`: `render`/`render_inner` take `&self`. One frame stale by
    /// construction — a section upload that lands between two `render` calls
    /// reads the *previous* frame's camera position, which is harmless here:
    /// the 768-squared-block threshold this feeds is coarse (≈27.7 blocks),
    /// and the camera cannot cross it in one frame. `None` only before the
    /// first frame has ever rendered, in which case `upload_section` falls
    /// back to "unknown, so always fade" — the pre-fix behaviour, for a
    /// vanishingly small startup window rather than a wrong guess.
    last_camera_block_pos: std::cell::Cell<Option<[i32; 3]>>,
    /// Whether the reachable set culls, only counts, or is not walked at all.
    /// See [`RenderState::set_terrain_occlusion`].
    occlusion_mode: TerrainOcclusion,
    /// How each mob's world light is sampled. Full-bright until the shell wires
    /// a real world in via [`RenderState::set_entity_light_source`].
    entity_light: EntityLightSource,
    /// The raw block-state id at a world block position, for the entity-shadow
    /// pass's ground scan (`entity_passes::prepare_shadows`). `None`/unset
    /// samples as "no ground here", the same "never invent geometry" default
    /// every other polled source in this struct takes — see
    /// [`RenderState::set_shadow_ground_source`].
    shadow_ground: ShadowGroundSource,
    /// Vanilla's `entityShadows` video option (`Options.entityShadows`).
    /// `true` — vanilla's own default — until the menu tree drives it; see
    /// [`RenderState::set_entity_shadows_enabled`].
    entity_shadows_enabled: bool,
    /// How bright the sky is *right now*. Permanent noon until the shell wires a
    /// world clock in via [`RenderState::set_sky_darken_source`].
    sky_darken: SkyDarkenSource,
    /// This frame's dimension `AMBIENT_LIGHT_COLOR`. The overworld's own grey
    /// until the shell wires the current dimension in via
    /// [`RenderState::set_ambient_light_source`].
    ambient_light: AmbientLightSource,
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
    /// This frame's in-progress eat or drink, which replaces the held item's
    /// ordinary pose with `ItemInHandRenderer.applyEatTransform`'s. `None` — the
    /// default — is the plain held-item pose, so the eating bob is invisible until
    /// the shell wires it in via [`RenderState::set_item_use_source`].
    item_use: ItemUseSource,
    /// Vanilla's `ItemInHandRenderer` swap state: which held item is
    /// *drawn* — which lags [`Self::main_hand`] across a hotbar change — and how far
    /// the hand is lowered.
    ///
    /// Stepped by [`RenderState::set_main_hand_source`], which is the shell's one
    /// per-frame `&mut self` hop; read by `prepare_first_person_hand` for both the
    /// item and the bare-arm branch. Its default is "fully equipped, empty hand", so
    /// a caller that never installs a main-hand source sees exactly the pre-fix
    /// behaviour.
    equip: first_person::HeldItemEquip,
    /// Vanilla bobs the hand with a *second, independent* application of
    /// `bobView` — `GameRenderer.renderItemInHand` seeds a fresh `PoseStack`
    /// with the **unbobbed** inverted model-view and re-applies the bob to that
    ///, rather than inheriting the world's already
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
    /// The underwater/fire screen-overlay pass, built once
    /// the vanilla `underwater.png`/`fire_1.png` textures are available. `None`
    /// — no `client.jar`, a headless test, or simply before
    /// [`RenderState::install_screen_effects`] runs — draws neither overlay,
    /// the same "no pass installed, nothing extra drawn" convention
    /// [`Self::sky`] uses.
    screen_effects: Option<lodestone_render::ScreenEffectRenderer>,
    /// Billboarded entity/player nametags. Always constructed —
    /// unlike [`Self::sky`]/[`Self::screen_effects`], there is no "install"
    /// step: [`NameTagRenderer::new`] loads its own jar-sourced font
    /// (fail-open to drawing nothing, same contract as
    /// [`crate::hud::vanilla_font::VanillaFont::shared`]), so nothing
    /// downstream needs to know whether it succeeded.
    nametag: NameTagRenderer,
    /// The colour format [`RenderState::new`] built every pipeline for — the
    /// *target's* format, i.e. the sRGB one on a native swapchain. Kept so
    /// [`RenderState::set_world_text_view`] can derive its non-sRGB
    /// counterpart itself rather than being told, which is what stops the
    /// format and the view being decided in two files that can disagree.
    color_format: wgpu::TextureFormat,
    /// The colour format the three flat-colour world-text passes
    /// ([`Self::nametag`], [`Self::sign_text`], [`Self::display_text`]) are
    /// currently built for. Starts equal to [`Self::color_format`] and becomes
    /// its raw (non-sRGB) counterpart the first time
    /// [`RenderState::set_world_text_view`] is called — see that method for
    /// why vanilla's text has to composite on raw gamma bytes.
    world_text_format: wgpu::TextureFormat,
    /// This frame's raw (non-sRGB) view of the same colour texture the world
    /// is drawing into, installed by [`RenderState::set_world_text_view`] and
    /// **taken** by `render_inner`, so a view from a swapchain image that has
    /// already been presented can never be attached a second time.
    world_text_view: std::cell::RefCell<Option<wgpu::TextureView>>,
    /// Block-entity rigs — chests today. Always constructed, like
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
    /// Where this frame's copper golem statues come from. Same "unset means
    /// draw nothing" convention as [`Self::skull_source`], and a real cuboid
    /// rig through this same batcher — see [`CopperGolemStatueSource`]'s doc
    /// for why it does *not* join the campfire/vault/brushable/shelf
    /// item-model family below.
    copper_golem_statue_source: CopperGolemStatueSource,
    /// Where this frame's bells come from. Same "unset means draw nothing"
    /// convention as [`Self::skull_source`], and a separate field for the
    /// reason [`BellSource`] documents. Installed per frame from
    /// `Sim::bell_source`, which also carries the `BLOCK_EVENT`-driven shake
    /// trigger (`BellShakes`) — a bell at rest and an untracked bell are
    /// indistinguishable, not "the trigger has no producer": that used to be
    /// true and stopped being true once `BellShakes` landed.
    bell_source: BellSource,
    /// Where this frame's spawner/trial-spawner display mobs come from. Same
    /// "unset means draw nothing" convention as [`Self::skull_source`], and a
    /// separate field for [`SpawnerSource`]'s reason. Installed per frame
    /// from `Sim::spawner_source`; unlike the sources above, leaving this
    /// unset draws nothing *extra* rather than leaving a hole, because both
    /// spawner block types have real block-model geometry for the cage.
    spawner_source: SpawnerSource,
    /// Where this frame's shulker boxes come from. Same "unset means
    /// draw nothing" convention as [`Self::skull_source`] — and here that
    /// degradation is a **hole**, not a missing decoration: a 26.2 shulker box has
    /// no block model of its own, so an unset source leaves an empty cell where
    /// every box is, exactly as chest does.
    shulker_source: ShulkerSource,
    /// Where this frame's decorated pots come from. Same "unset means
    /// draw nothing" convention as [`Self::skull_source`] — and here that
    /// degradation is a **hole**, not a missing decoration: a 26.2 decorated
    /// pot has real block-model geometry declared but every visible triangle
    /// (base and sides alike) comes from this pass, exactly as chest and
    /// shulker.
    decorated_pot_source: DecoratedPotSource,
    /// Where this frame's conduits come from. Same "unset means draw nothing"
    /// convention as [`Self::skull_source`] — and here that degradation is a
    /// **hole**, not a missing decoration: a conduit has real block-model
    /// geometry declared but every visible triangle comes from this pass. See
    /// [`ConduitSource`] for why this closure, unlike
    /// [`Self::shulker_source`]'s, needs a per-position tick tracker behind it.
    conduit_source: ConduitSource,
    /// Where this frame's banners come from. Same "unset means draw
    /// nothing" convention as [`Self::skull_source`], and here that degradation is
    /// merely a missing decoration: a banner's pole and cloth **are** drawn by this
    /// pass, but the block itself has no terrain model, so an unset source leaves a
    /// hole exactly as chest and shulker do.
    banner_source: BannerSource,
    /// Where this frame's lectern books come from. Same "unset means draw
    /// nothing" convention as [`Self::skull_source`], and here the degradation is
    /// the mildest in the family: a lectern's shelf, base and posts are all real
    /// block models, so an unset source leaves a complete but empty lectern
    /// rather than a hole.
    lectern_source: LecternSource,
    /// Where this frame's campfire cooking items come from. Unlike every other
    /// block-entity source here this is consumed by the **item** geometry pass
    /// and not by `prepare_block_entities`, because a campfire's renderer owns no
    /// mesh: the fire and the logs are block models the terrain pass already
    /// draws, so an unset source leaves a complete campfire cooking nothing.
    campfire_source: CampfireSource,
    /// Where this frame's vault display-item clusters come from. Same
    /// odd-one-out shape as [`Self::campfire_source`]: consumed by the item
    /// geometry pass, not `prepare_block_entities`, because a vault's cage is
    /// real block-model geometry and `VaultRenderer` draws only the floating
    /// reward on top of it. An unset source leaves a complete vault showing no
    /// reward, never a hole.
    vault_source: VaultSource,
    /// Where this frame's brushable-block revealed items come from. Same
    /// odd-one-out shape as [`Self::campfire_source`]: consumed by the item
    /// geometry pass, not `prepare_block_entities`, because a suspicious
    /// sand/gravel block's appearance is entirely its own real block model —
    /// `BrushableBlockRenderer` draws only the revealed item on top of it. An
    /// unset source leaves a complete, correctly-dusted block with no item
    /// floating above it, never a hole.
    brushable_source: BrushableSource,
    /// Where this frame's shelved items come from. Same odd-one-out shape as
    /// [`Self::campfire_source`]: consumed by the item geometry pass, not
    /// `prepare_block_entities`, because a shelf's board/back/sides are all
    /// real block-model geometry the terrain mesher already draws. An unset
    /// source leaves a complete, empty shelf, never a hole.
    shelf_source: ShelfSource,
    /// Where this frame's moving pistons come from. Consumed by
    /// [`Self::prepare_moving_blocks`] and the **model** pipeline, not by
    /// `prepare_block_entities` and not by the item path — a third destination, and
    /// the only source in this struct that shares a vertex buffer with falling
    /// blocks. An unset source is a **hole** for the two ticks a push lasts, like
    /// [`Self::shulker_source`] and unlike [`Self::campfire_source`]:
    /// `moving_piston` has no block model at all.
    moving_piston_source: MovingPistonSource,
    /// Where this frame's enchanting-table books come from. Same "unset means
    /// draw nothing" convention as [`Self::lectern_source`], whose mesh it shares,
    /// and the same mild degradation — an enchanting table's own block model is
    /// complete, so an unset source leaves a table with no book floating over it.
    enchanting_table_source: EnchantingTableSource,
    /// Where this frame's filled-map pictures come from. Same "unset
    /// means draw nothing" convention as [`Self::skull_source`], and here the
    /// degradation is that a held `filled_map` falls back to its ordinary flat item
    /// model rather than showing a blank map, because the map branch declines
    /// before it builds any geometry.
    map_source: MapSource,
    /// Retained per-device map textures and the most recent held/framed map
    /// meshes. [`maps::MapRenderCache`] owns the exact revision and pose
    /// invalidation rules.
    map_cache: std::cell::RefCell<maps::MapRenderCache>,
    /// World-space sign text. Always constructed, like
    /// [`Self::nametag`]: it loads its own jar-sourced font and fail-opens to
    /// drawing nothing. A sign's *board* is a real block model (unlike chest
    /// or skull) and already draws through the ordinary terrain pass with no
    /// help from this field — this only ever draws the text painted on it.
    sign_text: SignTextRenderer,
    /// Where this frame's signs come from. Same "unset means draw nothing"
    /// convention as [`Self::skull_source`].
    sign_source: SignSource,
    /// World-space `text_display` glyphs and background panels. Always
    /// constructed, like [`Self::sign_text`]: it loads the same jar-sourced
    /// font and fail-opens to drawing nothing. See `gpu/display_text.rs`'s
    /// module doc for why this needs a per-draw orientation rather than
    /// sign text's fixed one or a nametag's always-camera-facing one.
    display_text: DisplayTextRenderer,
    /// This frame's extracted `Display`-family entities (`text_display`/
    /// `item_display`/`block_display`), installed by
    /// [`set_display_draws`](Self::set_display_draws). Unlike
    /// [`Self::sign_source`]'s poll-by-position closure, this is a plain
    /// `Vec` — the extract already happened in
    /// `crate::display_entities::extract_display_draws`, so there is
    /// nothing left here to poll.
    /// Read by three consumers, one per subtype: `gpu/display_text.rs` for
    /// `text_display`, `gpu/moving_blocks.rs`'s `merge_block_displays` for
    /// `block_display`, and `gpu/world_items.rs`'s `merge_item_displays` for
    /// `item_display`.
    display_draws: Vec<crate::display_entities::DisplayDraw>,
    /// The beacon light beam. Always constructed, like [`Self::sign_text`]:
    /// it loads its own jar-sourced texture and fail-opens to drawing
    /// nothing. Not a hole in the world the way chest/skull are — a 26.2
    /// beacon's block model has real pyramid-frame geometry — but before
    /// this landed a beacon had **no visual sign it was active at all**.
    beacon_beam: BeaconBeamRenderer,
    /// Lightning bolts. Always constructed and never `Option`: unlike every
    /// sibling here it needs **no jar asset at all** — a bolt is untextured
    /// procedural geometry — so there is no pack-presence gate and no
    /// fail-open case. See `gpu/lightning_bolt.rs`'s module doc for why it
    /// cannot be an `EntityPipeline` variant.
    lightning_bolt: LightningBoltRenderer,
    /// Where this frame's beacon beams come from. Same "unset means draw
    /// nothing" convention as [`Self::skull_source`]. See [`BeaconSource`]
    /// for why this needs no per-position tracker the way
    /// [`Self::bell_source`] does.
    beacon_source: BeaconSource,
    /// The end portal / end gateway star-field pass. Always constructed,
    /// like [`Self::beacon_beam`]: it loads its own two jar-sourced textures
    /// and fail-opens to drawing nothing. A hole in the world the way
    /// chest/skull are — `end_portal.json`/`end_gateway.json` have zero
    /// model elements — so before this landed a stronghold's portal and an
    /// End island's gateway were both fully invisible.
    end_portal: EndPortalRenderer,
    /// Where this frame's end portals come from. Same "unset means draw
    /// nothing" convention as [`Self::skull_source`]; like [`BeaconSource`],
    /// needs no per-position tracker — see [`EndPortalSource`]'s doc.
    end_portal_source: EndPortalSource,
    /// Where this frame's end gateways come from — same shape as
    /// [`Self::end_portal_source`]. Only the always-visible star-field face
    /// list; the teleport beam is [`Self::end_gateway_beam_source`], a
    /// separate source with its own clock and tracker.
    end_gateway_source: EndGatewaySource,
    /// Where this frame's end gateway teleport beams come from. See
    /// [`EndGatewayBeamSource`]'s doc.
    end_gateway_beam_source: EndGatewayBeamSource,
    /// The end-portal star-field shader's `GameTime` term — an
    /// ever-increasing tick clock, unlike [`Self::beacon_source`]'s own
    /// `floorMod(40)`-wrapped `animation_time`. A plain scalar rather than a
    /// closure captured alongside a `*Source`, because the star-field swirl
    /// needs it every frame regardless of which (if any) portals are in
    /// view. See [`set_end_portal_game_time`](Self::set_end_portal_game_time).
    end_portal_game_time: f32,
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
    /// The enchantment-glint pass: the `GlintPipeline` plus an
    /// uploaded non-sRGB `enchanted_glint_item.png` sheet and one shared group-0
    /// uniform buffer. `None` — no `client.jar`, a headless test, or simply
    /// before [`RenderState::install_glint`] runs — draws no second pass, so an
    /// enchanted held item renders without its shimmer: the same "no pass
    /// installed, nothing extra drawn" convention [`Self::sky`] uses.
    glint: Option<glint::GlintPass>,
    /// Coarse per-pass GPU timing (sky / world / first-person), or `None` when
    /// the device was not granted `Features::TIMESTAMP_QUERY` — see
    /// `gpu_timing`'s module doc. `RefCell` for the same reason
    /// [`Self::occlusion`] is: `render`/`render_inner` take `&self`, but a
    /// query timer's ring-buffer bookkeeping is real per-frame mutation.
    /// Read through [`RenderState::gpu_timing_report`].
    gpu_timer: std::cell::RefCell<Option<gpu_timing::GpuQueryTimer>>,
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

/// One player's uploaded cape-instance buffer for a frame, grouped by cape
/// **URL** rather than by part — unlike wool/armour the cape's texture is
/// per-player, not fixed, so the grouping key `prepare_entities`' own skin
/// batching uses is the one that applies here too. There is exactly one part
/// (`"cape"`) in `EntityRenderer::cape_model`, so unlike [`WoolPartAccum`]
/// there is nothing left to key by once the URL is fixed.
struct CapeDrawBatch {
    /// The cape texture URL this batch's instances share — looked up in
    /// `EntityRenderer::player_skins` at draw time, the same cache (and the
    /// same "not installed yet" fallback behaviour) a body's own skin uses.
    url: String,
    instances: Range<u64>,
    count: u32,
}

/// One wing's uploaded elytra-instance buffer for a frame, keyed by
/// `(texture, wing)`.
///
/// Two keys rather than the cape's one, because the elytra differs from the
/// cape on both axes at once: it has **two** parts (`"left_wing"` and
/// `"right_wing"`, which carry different geometry and so cannot share one
/// instanced draw), and its texture is *usually* the fixed jar sheet but is
/// the wearer's own cape sheet when they have one — see
/// `RenderState::prepare_elytra` for the `WingsLayer.getPlayerElytraTexture`
/// preference order this implements.
struct ElytraDrawBatch {
    /// The cape texture URL this batch's instances share, looked up in
    /// `EntityRenderer::player_skins` at draw time — or `None` for the jar
    /// sheet in `EntityRenderer::elytra_texture`, which is what a wearer with
    /// no cape (i.e. almost every mob, and most players) draws with.
    texture: Option<String>,
    /// Which wing, as a range into `EntityRenderer::elytra_gpu`.
    range: lodestone_render::PartRange,
    instances: Range<u64>,
    count: u32,
}

/// One `(painting shape, face)` pair's uploaded instance buffer for a frame.
///
/// Two keys, both forced by the same constraint — this engine binds one texture
/// per draw:
///
/// * **shape**, because a painting's geometry is a function of `(width,
///   height)` and nine shapes cover all 51 vanilla variants;
/// * **face**, because the front samples the variant's own sprite and the back
///   and edges sample one shared `back.png`. Vanilla emits both in a single
///   stream, which it can only do because both live in its paintings atlas.
///
/// So a frame containing one 2x1 `sea` and one 2x1 `pool` is three batches: a
/// `sea` front, a `pool` front, and one shared 2x1 frame carrying both
/// instances.
struct PaintingDrawBatch {
    /// Index into `EntityRenderer::painting_models` — the shape.
    model: usize,
    /// Which of that model's two parts: `parts[0]` front, `parts[1]` frame.
    part: usize,
    /// The variant whose sprite this batch binds, or `None` for the shared
    /// back/edge tile.
    variant: Option<&'static str>,
    instances: Range<u64>,
    count: u32,
}

/// One model type's uploaded flame-instance buffer for a frame
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

/// This frame's uploaded entity-shadow vertex buffer — simpler than every
/// sibling batch above, because every shadow piece shares the one
/// `textures/misc/shadow.png` sprite and none of them are instanced (see
/// `lodestone_render::entity_pipeline::ShadowVertex`'s doc), so a frame's
/// worth of shadows is exactly one buffer and one draw call.
struct ShadowBatch {
    buffer: wgpu::Buffer,
    /// Vertex count, not instance count — six [`lodestone_render::entity_pipeline::ShadowVertex`]
    /// per piece (two triangles), drawn with `draw(0..count, 0..1)`.
    count: u32,
}

/// One sprite cell's uploaded orb-instance buffer for a frame — the
/// experience-orb counterpart to [`FlameBatch`].
///
/// The batch key is the **sprite cell**, not the entity type: every orb in the
/// world is the same entity type, and what differs between two of them is which
/// of the eleven 16×16 cells of `experience_orb.png` their value buckets into (see
/// `lodestone_render::experience_orb_icon`). The cell is baked into the quad's
/// UVs, so it is a `PartRange` of `EntityRenderer::orb_gpu_model` — and `icon` is
/// that part's index.
struct OrbBatch {
    /// Which sprite cell, i.e. which part of the shared orb mesh.
    icon: u32,
    instances: Range<u64>,
    count: u32,
}

/// One camera-facing entity sprite's uploaded instance buffer for a frame — the
/// [`lodestone_render::entity_sprite`] counterpart to [`OrbBatch`].
///
/// The batch key is the **sprite**, i.e. the row of
/// [`lodestone_render::entity_sprite::ENTITY_SPRITES`], because unlike the orb's
/// eleven cells the two sprites are two separate sheets and therefore two
/// separate group-1 bind groups. `sprite` indexes both
/// `EntityRenderer::sprite_gpu_model`'s part list and
/// `EntityRenderer::sprite_textures`, which is the invariant that keeps the
/// geometry and the sheet from being chosen independently.
struct EntitySpriteBatch {
    /// Which row of `ENTITY_SPRITES`, i.e. which part of the shared mesh and
    /// which texture bind group.
    sprite: usize,
    instances: Range<u64>,
    count: u32,
}

/// One model type's uploaded per-part instance buffers for a frame. `parts[p]`
/// holds one matrix per visible instance of part `p`; a `None` slot is a part
/// with no geometry (nothing to draw).
#[derive(Debug)]
struct EntityDrawBatch {
    model: &'static str,
    count: u32,
    parts: Vec<Option<Range<u64>>>,
    /// The fetched-skin texture URL every instance in this batch shares, or
    /// `None` for the model's own default sheet.
    ///
    /// Texture identity used to *be* `model`, which collapsed every player in the
    /// world into one `player_wide` batch sharing one bind group — the blocker
    /// that stopped remote skins reaching a pixel. This is the same shape
    /// [`ArmourTextureKey`] already gives armour: the sheet is chosen at the draw,
    /// and the batch key is what guarantees one texture per batch.
    ///
    /// `Some(url)` with no bind group installed yet resolves to the default sheet
    /// too, so the fallback covers "in flight" and "no skin declared" alike.
    skin: Option<String>,
    /// The variant sheet **reference** every instance in this batch shares
    /// (`entity/wolf/wolf_ashen`), or `None` for the model's own default sheet.
    ///
    /// [`Self::skin`]'s shape and reasoning, one axis over: texture identity is not
    /// `model`, so a wolf's breed has to join the batch key or nine breeds collapse
    /// into one bind group and all of them draw pale. `Some(reference)` with no bind
    /// group installed (no vanilla pack, or a sheet the pack omits) falls back to the
    /// model sheet, which is exactly the pre-variant behaviour.
    ///
    /// Ranked *below* `skin` at the draw: a player has no variant axis and a wolf has
    /// no skin URL, so the two can never both be `Some` for one entity today — the
    /// order is stated so it does not have to be rediscovered if one ever does.
    variant_sheet: Option<&'static str>,
}

/// Entity uploads split by render phase.
///
/// Boat water patches are invisible depth writers, not visible entity models.
/// Keeping them in a separate list makes it structurally impossible for a
/// material/skin batch boundary to place one between a boat and its rider.
#[derive(Debug, Default)]
struct PreparedEntityBatches {
    /// Ordinary visible entity bodies.
    visible: Vec<EntityDrawBatch>,
    /// Invisible boat-interior depth masks, submitted after all visible opaque
    /// geometry and immediately before translucent water.
    water_masks: Vec<EntityDrawBatch>,
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
    texture: ArmourTextureKey,
    /// `(index range, shared instance-buffer range, instance count)` per armour part that
    /// anything in this group used.
    parts: Vec<(lodestone_render::PartRange, Range<u64>, u32)>,
}

/// Which sheet an [`ArmourDrawBatch`] samples: a material's own armour sheet, or a
/// smithing-table trim sprite drawn over it.
///
/// One enum rather than two parallel batch lists, because the two share
/// everything else — the same four meshes, the same parts, the same wearer
/// matrices, the same pipeline — and, critically, the same **order**: a slot's
/// trim has to be drawn after that slot's own layers or the `LessEqual` depth
/// test rejects it as coplanar. Keeping them in one insertion-ordered `Vec` is
/// what makes that free.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ArmourTextureKey {
    /// A material layer's sheet, keyed as [`EntityRenderer::armour_textures`] is.
    Sheet((&'static str, ArmourLayerType)),
    /// A trim sprite, keyed as [`EntityRenderer::trim_textures`] is.
    Trim(lodestone_assets::ResourceLocation),
}

/// Per-part instance accumulation for one `(slot, texture)` group, before upload.
struct ArmourAccum {
    slot: ArmourSlot,
    texture: ArmourTextureKey,
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
/// and nothing looser. In particular:
///
/// * **`Body` is not `Chest`.** `BODY` is `ANIMAL_ARMOR` — wolf armour and horse
///   barding live there — and `SADDLE` is its own type. A fold of `"body"` into
///   the chest slot was removed from the item census for exactly this reason
///   (`docs/item-prototypes.md`), and reintroducing it here would put a horse's
///   diamond barding on a player's torso.
/// * **`EquipmentSlot::isArmor` is the wrong predicate** even though it sounds
///   right: it is the *union* of humanoid and animal armour
///  .
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
