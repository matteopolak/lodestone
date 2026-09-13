//! Filled-map drawing: the per-map 128×128 texture and the quads that sample it.
//!
//! # One depth variant and no map shader
//!
//! A map's picture is one textured quad, and the *model* pipeline already draws
//! textured quads with absolute baked UVs from a texture at group 1. So a map
//! draw uses the model pipeline's map-surface depth variant with **group 1 swapped**
//! from the stitched block atlas to the map's own texture. That is not a separate
//! shader — the model shader is at
//! wgpu's 4-bind-group floor (camera / atlas / palette / anim), so a fifth group
//! for a map texture would validate on an 8-group adapter and crash at startup on
//! a 4-group one.
//!
//! # Retained map resources
//!
//! [`MapPicture`](super::MapPicture) carries the stable saved-map id and the
//! `MapState` colour revision alongside its shared pixels. [`MapRenderCache`]
//! retains each texture/upload/bind group under that exact pair and retains the
//! latest held/framed mesh signatures, all behind a `RefCell` because the render
//! path itself takes `&self`. A new render state owns a new device/session cache.
//!
//! # What is not drawn
//!
//! Vanilla's `map_background` frame sprite and the `MapDecoration` icons (the
//! player arrow, banner markers) — both want the map-decorations atlas, which the
//! asset layer does not stitch. `SessionMaps` carries the decorations already, so
//! this is an asset job rather than a wiring one.

use std::{collections::HashMap, hash::Hash, sync::Arc};

use glam::{Mat4, Vec3};
use lodestone_game::maps::MapId;
use lodestone_render::map_item::{MAP_SIZE, map_quad_mesh, map_texture_rgba};
use lodestone_render::texture::GpuAtlas;
use lodestone_render::model_pipeline::MapDepthDiagnostic;
use lodestone_render::{ENTITY_FULLBRIGHT, GpuModelMesh, ModelMesh, ModelPipeline};

use crate::entities::EntityDraw;

use super::pack_trace::{should_trace_candidate, unit_quad_normal, unit_quad_plane};
use super::{MapPicture, RenderState};

/// GPU resources that stay valid for this [`RenderState`] and are shared by a
/// held or framed map draw.
pub(super) type PreparedMap = (Arc<GpuModelMesh>, Arc<wgpu::BindGroup>);

/// A map texture's exact content identity. The id scopes the revision: two
/// unrelated maps naturally both start at revision zero.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct MapTextureKey {
    map_id: MapId,
    color_revision: u64,
}

impl MapTextureKey {
    const fn new(map_id: MapId, color_revision: u64) -> Self {
        Self {
            map_id,
            color_revision,
        }
    }

    const fn from_picture(picture: &MapPicture) -> Self {
        Self::new(picture.map_id, picture.color_revision)
    }
}

/// Every value that can change a framed map's vertex data. Floats are compared
/// by representation: interpolated entity poses that differ by even one bit
/// need a fresh mesh, while an unchanged second frame is exactly reusable.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FramedMapInput {
    entity_id: i32,
    map_id: MapId,
    feet: [u32; 3],
    yaw: u32,
    pitch: u32,
    rotation: u8,
    invisible: bool,
    light: u8,
}

impl FramedMapInput {
    fn new(
        entity_id: i32,
        map_id: MapId,
        feet: [f32; 3],
        yaw: f32,
        pitch: f32,
        rotation: u8,
        invisible: bool,
        light: u8,
    ) -> Self {
        Self {
            entity_id,
            map_id,
            feet: feet.map(f32::to_bits),
            yaw: yaw.to_bits(),
            pitch: pitch.to_bits(),
            rotation,
            invisible,
            light,
        }
    }

    fn pose(&self) -> Mat4 {
        framed_map_pose(
            Vec3::from_array(self.feet.map(f32::from_bits)),
            f32::from_bits(self.yaw),
            f32::from_bits(self.pitch),
            self.rotation,
            self.invisible,
        )
    }
}

/// The exact visible framed-map input sequence for one prepared world frame.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FramedMapsKey(Vec<FramedMapInput>);

impl FramedMapsKey {
    const fn new(inputs: Vec<FramedMapInput>) -> Self {
        Self(inputs)
    }
}

/// A retained value for a key that may have many simultaneously visible maps.
struct RetainedMapEntries<K, V> {
    entries: HashMap<K, Arc<V>>,
}

impl<K, V> Default for RetainedMapEntries<K, V> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash + Clone, V> RetainedMapEntries<K, V> {
    fn contains_key(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    fn get_or_insert_with(&mut self, key: K, build: impl FnOnce() -> V) -> Arc<V> {
        if let Some(value) = self.entries.get(&key) {
            return Arc::clone(value);
        }
        let value = Arc::new(build());
        self.entries.insert(key, Arc::clone(&value));
        value
    }

    fn retain(&mut self, keep: impl FnMut(&K, &mut Arc<V>) -> bool) {
        self.entries.retain(keep);
    }
}

/// A retained value for the complete visible framed-map batch. Keeping only the
/// latest signature bounds mesh VRAM while covering the stationary case that
/// dominated the live profile.
struct RetainedLast<K, V> {
    entry: Option<(K, Arc<V>)>,
}

impl<K, V> Default for RetainedLast<K, V> {
    fn default() -> Self {
        Self { entry: None }
    }
}

impl<K: PartialEq, V> RetainedLast<K, V> {
    fn get_or_insert_with(&mut self, key: K, build: impl FnOnce() -> V) -> Arc<V> {
        if let Some((previous, value)) = &self.entry
            && previous == &key
        {
            return Arc::clone(value);
        }
        let value = Arc::new(build());
        self.entry = Some((key, Arc::clone(&value)));
        value
    }
}

struct CachedFramedBatch {
    map_id: MapId,
    mesh: Arc<GpuModelMesh>,
}

/// Per-device retained map resources. This is intentionally owned by
/// [`RenderState`]: a device or colour-format/session rebuild creates a new
/// state and therefore cannot reuse stale wgpu handles.
pub(super) struct MapRenderCache {
    textures: RetainedMapEntries<MapTextureKey, wgpu::BindGroup>,
    held_mesh: RetainedLast<u32, GpuModelMesh>,
    framed_batches: RetainedLast<FramedMapsKey, Vec<CachedFramedBatch>>,
}

impl Default for MapRenderCache {
    fn default() -> Self {
        Self {
            textures: RetainedMapEntries::default(),
            held_mesh: RetainedLast::default(),
            framed_batches: RetainedLast::default(),
        }
    }
}

impl std::fmt::Debug for MapRenderCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapRenderCache")
            .field("textures", &self.textures.entries.len())
            .field("held_mesh", &self.held_mesh.entry.is_some())
            .field("framed_batches", &self.framed_batches.entry.is_some())
            .finish()
    }
}

impl MapRenderCache {
    fn texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &ModelPipeline,
        picture: &MapPicture,
    ) -> Arc<wgpu::BindGroup> {
        let key = MapTextureKey::from_picture(picture);
        // One retained revision per map id. A patch invalidates only its own
        // texture while releasing the superseded GPU allocation promptly.
        if !self.textures.contains_key(&key) {
            // The steady state takes the `contains_key` fast path. Only a new
            // revision walks the cache to drop this map's superseded texture.
            self.textures
                .retain(|cached, _| cached.map_id != key.map_id || cached == &key);
        }
        self.textures.get_or_insert_with(key, || {
            map_texture_bind_group(device, queue, pipeline, picture.colors.as_slice())
        })
    }
}

/// The item id whose picture this module draws.
pub const FILLED_MAP_ITEM: &str = "filled_map";

/// The two entity types that hang an item on a wall. Neither has a renderer of
/// its own (both are `HangingEntity`, out of that fix's block-entity scope), so
/// a framed map draws its picture with no surrounding frame border — the picture
/// is the part a player is looking at.
pub const ITEM_FRAME_TYPES: [&str; 2] = ["item_frame", "glow_item_frame"];

/// `EntityTypes.GLOW_ITEM_FRAME`'s registry path, as [`EntityDraw::type_path`]
/// carries it. The wire distinguishes the two frame types and three things
/// downstream depend on which one this is: the `#back` sprite
/// (`block/glow_item_frame`), the block-light floor of 5 on the frame's own body,
/// and the full-bright light its contents draw at.
pub const GLOW_ITEM_FRAME_TYPE_PATH: &str = "glow_item_frame";

/// Opt-in live switches that remove one map-rendering decision at a time.
///
/// These are deliberately diagnostics, not graphics settings: every false
/// default preserves the production path, while a live report can identify the
/// first boundary whose removal restores the picture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MapDiagnosticSwitches {
    pub(super) disable_frustum_cull: bool,
    pub(super) disable_backface_cull: bool,
    /// Which of the three depth decisions the map pipeline still makes.
    ///
    /// `LODESTONE_MAP_DISABLE_DEPTH` turns off all three at once, which is what
    /// it has always meant — but a run under it cannot say *which* one mattered,
    /// because the comparison, the write and the polygon offset all vanish
    /// together. The three narrower switches each remove exactly one, so three
    /// runs separate what one run conflates.
    pub(super) depth: MapDepthDiagnostic,
}

impl Default for MapDiagnosticSwitches {
    fn default() -> Self {
        Self {
            disable_frustum_cull: false,
            disable_backface_cull: false,
            depth: MapDepthDiagnostic::PRODUCTION,
        }
    }
}

impl MapDiagnosticSwitches {
    /// Whether the map pipeline departs from the production depth state at all.
    /// Retained under the old name because the draw-site pipeline selection asks
    /// exactly this question and does not care which axis moved.
    pub(super) const fn disable_depth(self) -> bool {
        self.depth.is_diagnostic()
    }
}

fn map_diagnostic_switches_from(enabled: impl Fn(&str) -> bool) -> MapDiagnosticSwitches {
    let all_off = enabled("LODESTONE_MAP_DISABLE_DEPTH");
    MapDiagnosticSwitches {
        disable_frustum_cull: enabled("LODESTONE_MAP_DISABLE_FRUSTUM_CULL"),
        disable_backface_cull: enabled("LODESTONE_MAP_DISABLE_BACKFACE_CULL"),
        depth: MapDepthDiagnostic {
            compare: !all_off && !enabled("LODESTONE_MAP_DISABLE_DEPTH_TEST"),
            write: !all_off && !enabled("LODESTONE_MAP_DISABLE_DEPTH_WRITE"),
            bias: !all_off && !enabled("LODESTONE_MAP_DISABLE_DEPTH_BIAS"),
        },
    }
}

/// Read the process-wide live diagnostic configuration once, before the first
/// map pipeline is selected. Web builds have no process environment, so they
/// always retain the ordinary renderer.
pub(super) fn map_diagnostic_switches() -> MapDiagnosticSwitches {
    static SWITCHES: std::sync::OnceLock<MapDiagnosticSwitches> = std::sync::OnceLock::new();
    *SWITCHES.get_or_init(|| {
        #[cfg(not(target_arch = "wasm32"))]
        {
            map_diagnostic_switches_from(|name| {
                std::env::var(name).is_ok_and(|entry| !entry.is_empty() && entry != "0")
            })
        }
        #[cfg(target_arch = "wasm32")]
        {
            MapDiagnosticSwitches::default()
        }
    })
}

/// The map's gather broad phase, using the same wall-offset entity bounds and
/// half-block renderer inflation as vanilla rather than centring an invented
/// envelope on the packet attachment anchor.
fn framed_map_in_frustum(
    frustum: &lodestone_render::Frustum,
    feet: Vec3,
    yaw: f32,
    pitch: f32,
) -> bool {
    let (min, max) = lodestone_render::entity::item_frame_culling_aabb(
        feet, yaw, pitch, true,
    );
    frustum.intersects_aabb(min, max)
}

/// `ItemFrameRenderer` scales map coordinates by `1 / 128`; its final
/// `translate(0, 0, -1)` and `MapRenderer`'s `MAP_Z_OFFSET (-.01)` therefore
/// put the image plane `1.01 / 128` in front of the content origin.
const MAP_RENDERER_DEPTH: f32 = 1.01 / 128.0;

/// Extra outward clearance for a framed map's picture, in blocks, read once
/// from `LODESTONE_MAP_LIFT_PROBE` at process start. `0.0` — the default and
/// the only value any non-diagnostic run ever uses — leaves the pose exactly
/// where `ItemFrameRenderer`/`MapRenderer` put it.
///
/// # This is a ruler, not a setting
///
/// An invisible frame's picture stands `MAP_RENDERER_DEPTH` — 7.9 mm — outside
/// its attachment block's face, and that is all the world-space separation it
/// has from the wall. A live report of the picture *losing* to that wall is a
/// claim that the separation has the wrong sign or the wrong magnitude, and the
/// only way to tell which is to measure it: raise the clearance until the
/// picture comes back, and the value that works **is** the deficit.
///
/// The magnitudes are pre-committed, because each one names a different
/// mechanism and a probe without them is a tuning knob:
///
/// | smallest value that restores the picture | what it means |
/// | --- | --- |
/// | it never comes back | not a world-space deficit at all — look at the pipeline or at what else writes depth there |
/// | under `0.008` (i.e. under `MAP_RENDERER_DEPTH` itself) | the surfaces are effectively tied; the separation is reaching the buffer but is too small to survive rounding |
/// | around `0.0625` (`1/16`) | the *content lift* is wrong — that is exactly the gap between vanilla's visible `0.4375` and invisible `0.5` |
/// | around `0.5` | the picture is on the wrong side of its attachment block |
///
/// Native-only and read once, like every other `LODESTONE_MAP_*` switch: the
/// browser build has no process environment and always takes `0.0`.
fn map_lift_probe() -> f32 {
    static PROBE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *PROBE.get_or_init(|| {
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::env::var("LODESTONE_MAP_LIFT_PROBE")
                .ok()
                .and_then(|value| value.trim().parse::<f32>().ok())
                .filter(|value| value.is_finite())
                .unwrap_or(0.0)
        }
        #[cfg(target_arch = "wasm32")]
        {
            0.0
        }
    })
}

/// The source-resolution result for one visible framed-map candidate. This is
/// deliberately narrower than [`MapSkip`]: the live trace needs to distinguish
/// a frame that never reached a source lookup from one whose individual map
/// data has not arrived yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FramedMapSource {
    /// No lookup occurred because an earlier gather prerequisite was absent.
    NotQueried,
    /// This frame installed no map source at all.
    Unavailable,
    /// The source was installed but had no picture for this entity yet.
    Unresolved,
    /// The source supplied the picture, including its stable map id.
    Resolved,
}

/// One framed map in the edge-triggered live diagnostic. The float fields use
/// bits so an actual pose change is observable, while camera motion alone is
/// not allowed to turn this into a per-frame log.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FramedMapDiagnosticFrame {
    entity_id: i32,
    entity_type: String,
    feet: [u32; 3],
    yaw: u32,
    pitch: u32,
    rotation: u8,
    invisible: bool,
    item: Option<String>,
    map_id: Option<i32>,
    source: FramedMapSource,
    quad_normal: [u32; 3],
}

impl FramedMapDiagnosticFrame {
    fn from_draw(draw: &EntityDraw) -> Self {
        let pose = framed_map_pose(
            draw.feet,
            draw.yaw,
            draw.pitch,
            draw.item_frame_rotation,
            draw.invisible,
        );
        let normal = unit_quad_normal(pose);
        Self {
            entity_id: draw.id,
            entity_type: draw.type_path.to_string(),
            feet: draw.feet.to_array().map(f32::to_bits),
            yaw: draw.yaw.to_bits(),
            pitch: draw.pitch.to_bits(),
            rotation: draw.item_frame_rotation,
            invisible: draw.invisible,
            item: draw.item.as_ref().map(ToString::to_string),
            map_id: None,
            source: FramedMapSource::NotQueried,
            quad_normal: normal.map(f32::to_bits),
        }
    }
}

/// Gather state compared between frames. Camera values stay outside this key:
/// they are logged with an edge, but walking must never emit a line every
/// frame. The stable world-space normal is enough to diagnose a cull-facing
/// mismatch from that camera snapshot without making every hemisphere crossing
/// a new diagnostic event.
#[derive(Clone, Debug)]
struct FramedMapGatherDiagnostic {
    model_ready: bool,
    map_source_installed: bool,
    /// Filled-map item-frame entities observed before any renderer culling.
    candidate_count: usize,
    /// The configured or once-latched entity remains named even while absent.
    tracked_entity_id: Option<i32>,
    /// The single tracked frame, when it is still in the entity snapshot.
    selected: Option<FramedMapDiagnosticFrame>,
    selected_in_frustum: bool,
    selected_submitted: bool,
    projection_state: Option<FramedMapProjectionState>,
    projection: Option<FramedMapProjectionSnapshot>,
    submitted_instances: usize,
    submitted_batches: usize,
}

#[derive(Clone, Copy, Debug)]
struct FramedMapTrackCandidate {
    entity_id: i32,
    score: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FramedMapProjectionState {
    centre_in_clip: bool,
    corner_clip_mask: u8,
    map_in_front_mask: u8,
    projected_winding_positive: bool,
}

#[derive(Clone, Copy, Debug)]
struct FramedMapProjectionPoint {
    map_clip: [f32; 4],
    comparison_clip: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
struct FramedMapProjectionSnapshot {
    comparison_surface: &'static str,
    /// How many representable `Depth32Float` values separate the picture's
    /// **centre** from the surface behind it, positive when the picture is
    /// ahead. This is the number the live report turns on, and it is the one
    /// quantity `map_in_front_mask` cannot supply: that mask says only which
    /// side of the comparison each corner fell on, and its corner *pairing* is
    /// unreliable because the pose's `Ry(180)` reverses the picture's x order
    /// relative to the comparison quad. Index `0` is the centre of both, so it
    /// is paired correctly whatever the turn is.
    ///
    /// Read it against `docs/coplanar-overlay-depth.md`: the picture's own
    /// polygon offset is worth `20` values plus an unbounded slope term, so a
    /// margin of `-20` or worse at the centre means the picture is losing on
    /// geometry rather than on rounding. A margin comfortably above zero while
    /// the picture is visibly missing means the world pose is fine and
    /// something else owns those pixels.
    ///
    /// Under the reversed-Z projection the shipped `1.01 / 128` clearance is
    /// worth thousands of these at every ordinary viewing distance, so a
    /// *small* margin here is now itself the finding.
    centre_depth_ulp_margin: f32,
    points: [FramedMapProjectionPoint; 5],
}

/// The gap from `value` to the next representable `f32` above it — the unit
/// [`FramedMapProjectionSnapshot::centre_depth_ulp_margin`] counts in.
fn depth_ulp(value: f32) -> f32 {
    let next = f32::from_bits(value.to_bits().wrapping_add(1));
    let step = next - value;
    if step.is_finite() && step > 0.0 { step } else { f32::EPSILON }
}

/// Signed separation of two window-space depths in representable values,
/// positive when `map_clip` is nearer the eye than `comparison_clip`.
///
/// Both are reversed-Z `[0, 1]` depths, so "nearer" is "**greater**" — the same
/// convention vanilla uses — which is why this returns `map - comparison`. This
/// is a diagnostic the owner reads a *sign* off, so getting the direction
/// backwards does not make it noisy, it makes it lie: a healthy picture would
/// report as buried in its wall.
fn depth_ulp_margin(map_clip: [f32; 4], comparison_clip: [f32; 4]) -> f32 {
    if map_clip[3] <= 0.0 || comparison_clip[3] <= 0.0 {
        return f32::NAN;
    }
    let map_depth = map_clip[2] / map_clip[3];
    let comparison_depth = comparison_clip[2] / comparison_clip[3];
    (map_depth - comparison_depth) / depth_ulp(comparison_depth)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FramedMapDrawDiagnostic {
    tracked_entity_id: Option<i32>,
    tracked_submitted: bool,
    tracked_drawn: bool,
    submitted_instances: usize,
    submitted_batches: usize,
    drawn_instances: usize,
}

/// Per-process diagnostic latch. It deliberately has no renderer or GPU state:
/// this is an opt-in observer of existing gather/draw boundaries, not another
/// rendering path. The two independent keys make a gather-with-data/draw-zero
/// transition observable even when source resolution did not change.
#[derive(Default)]
struct MapLiveDiagnostics {
    tracked_entity_id: Option<i32>,
    gather: Option<FramedMapGatherDiagnostic>,
    draw: Option<FramedMapDrawDiagnostic>,
}

impl MapLiveDiagnostics {
    fn track_entity(
        &mut self,
        configured: Option<i32>,
        candidates: &[FramedMapTrackCandidate],
    ) -> Option<i32> {
        if let Some(configured) = configured {
            self.tracked_entity_id = Some(configured);
            return self.tracked_entity_id;
        }
        let best = candidates
            .iter()
            .min_by(|left, right| left.score.total_cmp(&right.score));
        let retained = self.tracked_entity_id.and_then(|id| {
            candidates.iter().find(|candidate| candidate.entity_id == id)
        });
        // NDC-square score. Keep the current subject through tiny crossings,
        // but switch when another map is materially closer to the crosshair.
        self.tracked_entity_id = match (retained, best) {
            (Some(current), Some(best)) if current.score <= best.score + 0.04 => {
                Some(current.entity_id)
            }
            (_, Some(best)) => Some(best.entity_id),
            _ => None,
        };
        self.tracked_entity_id
    }

    fn gather_changed(&mut self, next: &FramedMapGatherDiagnostic) -> bool {
        if self.gather.as_ref().is_some_and(|previous| {
            previous.model_ready == next.model_ready
                && previous.map_source_installed == next.map_source_installed
                && previous.candidate_count == next.candidate_count
                && previous.tracked_entity_id == next.tracked_entity_id
                && previous.selected == next.selected
                && previous.selected_in_frustum == next.selected_in_frustum
                && previous.selected_submitted == next.selected_submitted
                && previous.projection_state == next.projection_state
        }) {
            return false;
        }
        self.gather = Some(next.clone());
        true
    }

    fn draw_changed(&mut self, next: FramedMapDrawDiagnostic) -> bool {
        if self.draw.is_some_and(|previous| {
            previous.tracked_entity_id == next.tracked_entity_id
                && previous.tracked_submitted == next.tracked_submitted
                && previous.tracked_drawn == next.tracked_drawn
        }) {
            return false;
        }
        self.draw = Some(next);
        true
    }
}

fn configured_map_trace_entity() -> Option<i32> {
    static ENTITY: std::sync::OnceLock<Option<i32>> = std::sync::OnceLock::new();
    *ENTITY.get_or_init(|| {
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::env::var("LODESTONE_MAP_TRACE_ENTITY")
                .ok()
                .and_then(|value| value.parse().ok())
        }
        #[cfg(target_arch = "wasm32")]
        {
            None
        }
    })
}

fn map_live_diagnostics() -> &'static std::sync::Mutex<MapLiveDiagnostics> {
    static DIAGNOSTICS: std::sync::OnceLock<std::sync::Mutex<MapLiveDiagnostics>> =
        std::sync::OnceLock::new();
    DIAGNOSTICS.get_or_init(|| std::sync::Mutex::new(MapLiveDiagnostics::default()))
}

#[cfg(not(target_arch = "wasm32"))]
fn map_diagnostic_file() -> Option<&'static std::sync::Mutex<std::fs::File>> {
    static FILE: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        std::sync::OnceLock::new();
    FILE.get_or_init(|| {
        std::env::var_os("LODESTONE_MAP_DIAG_FILE").and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(path)
                .ok()
                .map(std::sync::Mutex::new)
        })
    })
    .as_ref()
}

fn map_diagnostics_enabled() -> bool {
    tracing::enabled!(target: "maps", tracing::Level::DEBUG)
        || {
            #[cfg(not(target_arch = "wasm32"))]
            {
                map_diagnostic_file().is_some()
            }
            #[cfg(target_arch = "wasm32")]
            {
                false
            }
        }
}

#[cfg(not(target_arch = "wasm32"))]
fn persist_map_diagnostic(line: &str) {
    use std::io::Write as _;

    let Some(file) = map_diagnostic_file() else {
        return;
    };
    let Ok(mut file) = file.lock() else {
        return;
    };
    let _ = writeln!(file, "{line}");
    let _ = file.flush();
}

#[cfg(target_arch = "wasm32")]
fn persist_map_diagnostic(_line: &str) {}

fn tracked_map_entity(candidates: &[FramedMapTrackCandidate]) -> Option<i32> {
    let Ok(mut state) = map_live_diagnostics().lock() else {
        return configured_map_trace_entity().or_else(|| candidates.first().map(|entry| entry.entity_id));
    };
    state.track_entity(configured_map_trace_entity(), candidates)
}

fn clip_point(matrix: Mat4, point: Vec3) -> [f32; 4] {
    (matrix * point.extend(1.0)).to_array()
}

fn clip_contains(clip: [f32; 4]) -> bool {
    let [x, y, z, w] = clip;
    w > 0.0 && x.abs() <= w && y.abs() <= w && z >= 0.0 && z <= w
}

fn framed_map_track_candidate(
    draw: &EntityDraw,
    camera_position: Vec3,
    map_view_projection: Mat4,
) -> Option<FramedMapTrackCandidate> {
    if draw.feet.distance_squared(camera_position) > 64.0 * 64.0 {
        return None;
    }
    let centre = framed_map_pose(
        draw.feet,
        draw.yaw,
        draw.pitch,
        draw.item_frame_rotation,
        draw.invisible,
    )
    .transform_point3(Vec3::ZERO);
    let [x, y, _, w] = clip_point(map_view_projection, centre);
    if w <= 0.0 {
        return None;
    }
    let ndc = Vec3::new(x / w, y / w, 0.0);
    if ndc.x.abs() > 1.25 || ndc.y.abs() > 1.25 {
        return None;
    }
    Some(FramedMapTrackCandidate {
        entity_id: draw.id,
        score: ndc.x * ndc.x + ndc.y * ndc.y,
    })
}

fn framed_map_projection(
    draw: &EntityDraw,
    map_view_projection: Mat4,
    frame_view_projection: Mat4,
) -> (FramedMapProjectionState, FramedMapProjectionSnapshot) {
    let pose = framed_map_pose(
        draw.feet,
        draw.yaw,
        draw.pitch,
        draw.item_frame_rotation,
        draw.invisible,
    );
    let body = lodestone_render::entity::item_frame_body_matrix(draw.feet, draw.yaw, draw.pitch);
    let locals = [
        Vec3::ZERO,
        Vec3::new(-0.5, -0.5, 0.0),
        Vec3::new(0.5, -0.5, 0.0),
        Vec3::new(0.5, 0.5, 0.0),
        Vec3::new(-0.5, 0.5, 0.0),
    ];
    let mut corner_clip_mask = 0u8;
    let mut map_in_front_mask = 0u8;
    let points = locals.map(|local| {
        let map_world = pose.transform_point3(local);
        let comparison_world = body.transform_point3(Vec3::new(
            local.x + 0.5,
            local.y + 0.5,
            if draw.invisible { 1.0 } else { 15.001 / 16.0 },
        ));
        let map_clip = clip_point(map_view_projection, map_world);
        let comparison_clip = if draw.invisible {
            clip_point(map_view_projection, comparison_world)
        } else {
            clip_point(frame_view_projection, comparison_world)
        };
        FramedMapProjectionPoint {
            map_clip,
            comparison_clip,
        }
    });
    for (index, point) in points.iter().enumerate() {
        if index != 0 && clip_contains(point.map_clip) {
            corner_clip_mask |= 1 << (index - 1);
        }
        let map_depth = point.map_clip[2] / point.map_clip[3];
        let comparison_depth = point.comparison_clip[2] / point.comparison_clip[3];
        // Reversed-Z: nearer the eye is the *greater* depth. See
        // `depth_ulp_margin`.
        if map_depth > comparison_depth {
            map_in_front_mask |= 1 << index;
        }
    }
    let ndc_xy = |point: &FramedMapProjectionPoint| {
        Vec3::new(
            point.map_clip[0] / point.map_clip[3],
            point.map_clip[1] / point.map_clip[3],
            0.0,
        )
    };
    let a = ndc_xy(&points[1]);
    let b = ndc_xy(&points[2]);
    let c = ndc_xy(&points[3]);
    let projected_winding_positive = (b.x - a.x) * (c.y - a.y)
        - (b.y - a.y) * (c.x - a.x)
        > 0.0;
    (
        FramedMapProjectionState {
            centre_in_clip: clip_contains(points[0].map_clip),
            corner_clip_mask,
            map_in_front_mask,
            projected_winding_positive,
        },
        FramedMapProjectionSnapshot {
            comparison_surface: if draw.invisible {
                "attachment_wall"
            } else {
                "frame_plate"
            },
            centre_depth_ulp_margin: depth_ulp_margin(
                points[0].map_clip,
                points[0].comparison_clip,
            ),
            points,
        },
    )
}

fn note_framed_map_gather(
    camera: &lodestone_render::Camera,
    diagnostic: FramedMapGatherDiagnostic,
) {
    if !map_diagnostics_enabled() {
        return;
    }
    let Ok(mut state) = map_live_diagnostics().lock() else {
        return;
    };
    if !state.gather_changed(&diagnostic) {
        return;
    }
    let switches = map_diagnostic_switches();
    persist_map_diagnostic(&format!(
        "gather camera={:?} yaw={} pitch={} fov={} aspect={} model={} source_installed={} candidates={} tracked={:?} \
         switches={switches:?} in_frustum={} submitted={} instances={} batches={} projection_state={:?} \
         projection={:?} selected={:?}",
        camera.position.to_array(),
        camera.yaw,
        camera.pitch,
        camera.fov_y_degrees,
        camera.aspect,
        diagnostic.model_ready,
        diagnostic.map_source_installed,
        diagnostic.candidate_count,
        diagnostic.tracked_entity_id,
        diagnostic.selected_in_frustum,
        diagnostic.selected_submitted,
        diagnostic.submitted_instances,
        diagnostic.submitted_batches,
        diagnostic.projection_state,
        diagnostic.projection,
        diagnostic.selected,
    ));
    tracing::debug!(
        target: "maps",
        camera_position = ?camera.position.to_array(),
        camera_yaw = camera.yaw,
        camera_pitch = camera.pitch,
        camera_fov = camera.fov_y_degrees,
        camera_aspect = camera.aspect,
        model_ready = diagnostic.model_ready,
        map_source_installed = diagnostic.map_source_installed,
        candidate_count = diagnostic.candidate_count,
        switches = ?switches,
        tracked_entity_id = diagnostic.tracked_entity_id,
        selected_in_frustum = diagnostic.selected_in_frustum,
        selected_submitted = diagnostic.selected_submitted,
        projection_state = ?diagnostic.projection_state,
        projection = ?diagnostic.projection,
        submitted_instances = diagnostic.submitted_instances,
        submitted_batches = diagnostic.submitted_batches,
        selected = ?diagnostic.selected,
        "framed-map gather changed"
    );
}

pub(super) fn note_framed_map_draw(
    camera: &lodestone_render::Camera,
    submitted_instances: usize,
    submitted_batches: usize,
    drawn_instances: usize,
) {
    if !map_diagnostics_enabled() {
        return;
    }
    let Ok(mut state) = map_live_diagnostics().lock() else {
        return;
    };
    let (tracked_entity_id, tracked_submitted) = state
        .gather
        .as_ref()
        .map_or((state.tracked_entity_id, false), |gather| {
            (gather.tracked_entity_id, gather.selected_submitted)
        });
    // The opaque pass issues every prepared map draw without a conditional in
    // between. If the complete submitted count was drawn, the tracked member
    // was drawn too; otherwise this edge records the boundary failure.
    let tracked_drawn = tracked_submitted
        && submitted_instances != 0
        && drawn_instances == submitted_instances;
    let diagnostic = FramedMapDrawDiagnostic {
        tracked_entity_id,
        tracked_submitted,
        tracked_drawn,
        submitted_instances,
        submitted_batches,
        drawn_instances,
    };
    if !state.draw_changed(diagnostic) {
        return;
    }
    persist_map_diagnostic(&format!(
        "draw camera={:?} yaw={} pitch={} fov={} aspect={} tracked={tracked_entity_id:?} \
         submitted={tracked_submitted} drawn={tracked_drawn} instances={submitted_instances} \
         batches={submitted_batches} drawn_instances={drawn_instances}",
        camera.position.to_array(),
        camera.yaw,
        camera.pitch,
        camera.fov_y_degrees,
        camera.aspect,
    ));
    tracing::debug!(
        target: "maps",
        camera_position = ?camera.position.to_array(),
        camera_yaw = camera.yaw,
        camera_pitch = camera.pitch,
        camera_fov = camera.fov_y_degrees,
        camera_aspect = camera.aspect,
        tracked_entity_id,
        tracked_submitted,
        tracked_drawn,
        submitted_instances,
        submitted_batches,
        drawn_instances,
        "framed-map draw changed"
    );
}

/// Vanilla's `renderMap` scale (`ItemInHandRenderer.renderMap`: `scale(0.38f)`
/// around a `[-0.5, -0.5]`-centred unit quad).
const HELD_MAP_SCALE: f32 = 0.38;

/// `renderTwoHandedMap`'s resting translation, `translate(0, 0.04, -0.72)`.
/// Camera space here is the same right-handed space `first_person_item_mesh`
/// meshes into, so `-z` is into the screen.
const HELD_MAP_OFFSET: Vec3 = Vec3::new(0.0, -0.1, -0.72);

/// Build a bind group over one map's colour grid, ready for group 1 of the model
/// pipeline.
///
/// `mip_level_count` is 1 and the sampler is `Nearest`: a map is a 128-pixel image
/// blown up to hand-size, and vanilla's `DynamicTexture` is nearest-filtered too,
/// so a linear filter would smear terrain edges that are one pixel wide by design.
#[must_use]
pub(super) fn map_texture_bind_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &ModelPipeline,
    colors: &[u8],
) -> wgpu::BindGroup {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lodestone-filled-map"),
        size: wgpu::Extent3d {
            width: MAP_SIZE,
            height: MAP_SIZE,
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
        &map_texture_rgba(colors),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(MAP_SIZE * 4),
            rows_per_image: Some(MAP_SIZE),
        },
        wgpu::Extent3d {
            width: MAP_SIZE,
            height: MAP_SIZE,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("lodestone-filled-map-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    pipeline.atlas_bind_group(
        device,
        &GpuAtlas {
            texture,
            view,
            sampler,
            width: MAP_SIZE,
            height: MAP_SIZE,
        },
    )
}

/// The camera-space pose for a map held in the first-person hand.
///
/// `inverse_arm_height` is the equip/swap dip every held item takes, and it is
/// read here for the same reason `prepare_first_person_hand` reads it for both of
/// its other branches: swapping a map in must lower and raise as one motion rather
/// than have the map pop into place.
#[must_use]
fn held_map_pose(inverse_arm_height: f32) -> Mat4 {
    Mat4::from_translation(
        HELD_MAP_OFFSET + Vec3::new(0.0, inverse_arm_height * -0.6, 0.0),
    ) * Mat4::from_scale(Vec3::splat(HELD_MAP_SCALE))
}

/// The block-light level a **glow** frame lights its map picture at.
///
/// A glow frame lights what it holds itself instead of reading the world, and
/// the map branch is deliberately a shade below the ordinary item branch: the
/// 26.2 renderer subtracts a fixed 30 from the packed full-bright value for a
/// map, and the packed block channel counts a level as 16, so 30 is just under
/// two levels. Sky stays full. See `docs/filled-map-rendering.md`.
const GLOW_FRAME_MAP_BLOCK_LIGHT: u8 = 13;

/// The packed sky/block light a framed map picture is drawn at.
///
/// `frame_light` is the frame's **own** light — for a glow frame that is the
/// sampled world value raised to its block-light floor, which is a different
/// number again, so this cannot be folded into one function with it.
///
/// This existed as a hole rather than as a wrong number: the two helpers this
/// mirrors were written for the framed-*item* path and the map path sampled the
/// world directly, so a glow-framed map in an unlit room drew black while the
/// item beside it drew bright. The type doc for the glow frame's registry path
/// already described the wiring, which is exactly why nobody counted its
/// callers.
#[must_use]
const fn framed_map_light(frame_light: u8, glow: bool) -> u8 {
    if glow {
        lodestone_render::ENTITY_FULLBRIGHT | GLOW_FRAME_MAP_BLOCK_LIGHT
    } else {
        frame_light
    }
}

/// The world pose for a map hanging in an item frame at `feet`, facing along the
/// frame's real `Direction`.
///
/// ```text
/// item_frame_space(feet, yaw, pitch) · T(0, 0, content_lift) · Rz(rot · 90°) · Ry(180°) · T(0, 0, MAP_RENDERER_DEPTH)
/// ```
///
/// # The orientation is the frame's own, not `Ry(yaw)`
///
/// `Ry(yaw)` applied to the quad's local `+z` gives `(sin yaw, 0, cos yaw)`; the
/// frame's real facing is `(-sin yaw, 0, cos yaw)`
/// ([`lodestone_render::entity::item_frame_facing_step`], derived from
/// `ItemFrame.setDirection`). The two **agree at yaw 0 and 180 and are opposite
/// at 90 and 270**, so a pose built from `Ry(yaw)` puts the picture's front face
/// *into* the wall on every east- and west-facing frame — and the model
/// pipeline culls back faces, so the picture drew **zero pixels** there while
/// the wider `item_frame_map` body still drew around the hole. Measured, at
/// yaw 0/180 11,236 px against yaw 90/270's 0
/// (`tests/framed_map_pixels.rs`). The same coincidence hid the earlier
/// translation bug, and fixing the translation left the rotation behind: a gate
/// that asserts only where the picture's *centre* lands passes under either
/// reading of the orientation.
///
/// So the rotation is [`lodestone_render::entity::item_frame_facing`] — one
/// owner for "which way does this frame look", shared with the frame body's own
/// [`lodestone_render::entity::item_frame_body_matrix`] — and the lift is a
/// world-space step applied on the left, leaving that orientation alone.
///
/// # The `Ry(180°)`, and why it is not `Rz(180°)`
///
/// `item_frame_facing` maps frame-local `-z` to the direction the frame looks,
/// so the picture's own `+z` front must be turned to meet it. Vanilla spells the
/// same turn about `z` rather than about `y`, and lays its quad out with `v`
/// growing **up** where [`map_quad_mesh`] grows it down — the two differences
/// cancel, and only the `y` turn also puts the winding the right way round for
/// a culled pipeline. (Vanilla's own text render state culls back faces too, so
/// the difference is which vertex order each of us emits, not whether either
/// side culls; an earlier version of this comment said vanilla did not.) Those
/// two differences compose: on the `z == 0` plane every vertex of this quad
/// lands, `Rz(180) · diag(1, -1, 1)` and `Ry(180)` are the same map, and only
/// `Ry(180)` also turns the face outward. Substituting `Rz(180)` here draws the
/// picture upside-down *and* back-to-front.
///
/// # The in-plane rotation is quartered, not eighthed
///
/// Vanilla's own item-frame rotation accessor counts eighths and an ordinary framed item is drawn
/// at `rotation · 45°`, but the map branch is `rotation % 4 * 2` eighths — i.e.
/// `(rotation % 4) · 90°`. A map only ever hangs at a right angle, and the odd
/// half-steps fold onto the even ones rather than tilting the picture.
#[must_use]
fn framed_map_pose(feet: Vec3, yaw: f32, pitch: f32, rotation: u8, invisible: bool) -> Mat4 {
    framed_map_pose_with_extra_lift(feet, yaw, pitch, rotation, invisible, map_lift_probe())
}

/// [`framed_map_pose`] with the live [`map_lift_probe`] supplied explicitly, so
/// the probe's **sign** is assertable without a process-wide environment read.
///
/// The sign is the whole risk here: this file has already shipped one pose whose
/// lift went into the wall instead of out of it, and a probe that measures the
/// deficit by making it worse would read as "the deficit is larger than any value
/// I tried".
#[must_use]
fn framed_map_pose_with_extra_lift(
    feet: Vec3,
    yaw: f32,
    pitch: f32,
    rotation: u8,
    invisible: bool,
    extra_lift: f32,
) -> Mat4 {
    let quarter_turns = f32::from(rotation % 4) * 90.0;
    // This is ItemFrameRenderer's content branch. `map_quad_mesh` has already
    // absorbed Java's 1/128 XY scale and (-64, -64) centring translation. The
    // final positive local depth is intentional: our Ry(180) compatibility
    // turn flips it to Java's negative map depth while preserving the front
    // face that this pipeline culls against.
    lodestone_render::entity::item_frame_space(feet, yaw, pitch)
        * Mat4::from_translation(Vec3::new(
            0.0,
            0.0,
            lodestone_render::entity::item_frame_content_lift(invisible),
        ))
        * Mat4::from_rotation_z(quarter_turns.to_radians())
        * Mat4::from_rotation_y(std::f32::consts::PI)
        // `Ry(180)` has already turned local `+z` outward, so a larger value
        // here moves the picture further from the wall. `map_lift_probe()` is
        // `0.0` in every run that is not a live measurement.
        * Mat4::from_translation(Vec3::new(0.0, 0.0, MAP_RENDERER_DEPTH + extra_lift))
}

/// Why a framed or held map declined to draw, for the one-shot diagnostic below.
///
/// An empty item frame and a frame holding a map whose contents have not arrived
/// look **identical** on screen, so a silent skip here is indistinguishable from
/// a rendering bug — which is exactly how this subsystem was reported. Every
/// early return in the two `prepare_*` functions names one of these.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MapSkip {
    /// The baked model set is absent, so there is no pipeline to draw through.
    NoModels,
    /// No [`MapSource`](super::sources::MapSource) has been installed this
    /// frame. `Sim::map_source` returns `None` off a live server, so this is the
    /// ordinary state on the demo world and a real defect on a join.
    NoSource,
    /// A source is installed and answered nothing: no `MAP_ITEM_DATA` has been
    /// folded for this map yet. On a vanilla server `ServerEntity.sendChanges`
    /// pushes a framed map's contents every ten ticks to every player in the
    /// level, so this should clear within half a second of the frame coming into
    /// view — and never clearing means the packet is not arriving or not
    /// folding.
    NoContents,
}

impl MapSkip {
    const fn reason(self) -> &'static str {
        match self {
            Self::NoModels => "no baked model set",
            Self::NoSource => "no map source installed for this frame",
            Self::NoContents => "no MAP_ITEM_DATA has arrived for this map yet",
        }
    }
}

/// One bit per `(site, reason)` already reported, so a per-frame decline logs
/// once rather than sixty times a second.
///
/// Cleared by [`note_map_drawn`] whenever a map actually draws, so a decline
/// that comes back after a working period is reported again instead of being
/// swallowed by a latch that never resets.
static REPORTED_MAP_SKIPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Log, at most once per `(site, reason)` between successful draws, why a map
/// was not drawn. Always returns `None` so a call site can `return`
/// `note_map_skip(...)` in place of a bare `?`.
fn note_map_skip<T>(site: &'static str, skip: MapSkip, id: Option<i32>) -> Option<T> {
    use std::sync::atomic::Ordering;
    let bit = 1u32 << (skip as u32 + if site == "item frame" { 0 } else { 16 });
    let previous = REPORTED_MAP_SKIPS.fetch_or(bit, Ordering::Relaxed);
    if previous & bit == 0 {
        tracing::warn!(
            target: "render",
            site,
            map_id = ?id,
            "not drawing a filled map: {}",
            skip.reason()
        );
    }
    None
}

/// Clear the one-shot latch, so the next decline is reported afresh.
fn note_map_drawn() {
    REPORTED_MAP_SKIPS.store(0, std::sync::atomic::Ordering::Relaxed);
}

impl RenderState {
    /// This frame's held filled map, or `None` when the hand holds anything else.
    ///
    /// Returns the quad *and* its texture bind group together: the two are
    /// meaningless apart, and pairing them is what stops a later frame drawing one
    /// map's geometry with another map's pixels.
    pub(super) fn prepare_held_map(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        inverse_arm_height: f32,
    ) -> Option<PreparedMap> {
        let (item, _) = self.equip.visible()?;
        if item.path() != FILLED_MAP_ITEM {
            // Not a decline: the hand is holding something else, which is not a
            // map that failed to draw.
            return None;
        }
        const SITE: &str = "hand";
        let Some(model) = self.model.as_ref() else {
            return note_map_skip(SITE, MapSkip::NoModels, None);
        };
        if !self.map_source.is_installed() {
            return note_map_skip(SITE, MapSkip::NoSource, None);
        }
        let Some(picture) = self.map_source.picture(None, None) else {
            return note_map_skip(SITE, MapSkip::NoContents, None);
        };
        // Full bright, as the GUI item path nails every vertex: the map is drawn
        // in its own camera-space pass with no world position to sample.
        let mut cache = self.map_cache.borrow_mut();
        let gpu = cache.held_mesh.get_or_insert_with(inverse_arm_height.to_bits(), || {
            let mesh = map_quad_mesh(held_map_pose(inverse_arm_height), ENTITY_FULLBRIGHT);
            // A map quad is structurally non-empty. Keeping this assertion beside
            // the retained build avoids an `Option` cache entry whose only valid
            // state would permanently re-run a failed upload every frame.
            GpuModelMesh::upload(device, &mesh).expect("a map quad has six indices")
        });
        let texture = cache.texture(device, queue, &model.pipeline, &picture);
        note_map_drawn();
        Some((gpu, texture))
    }

    /// Every filled map hanging in an item frame this frame, grouped into
    /// world-space meshes by the texture each map samples.
    ///
    /// The live source resolves a frame's retained `minecraft:map_id` from its
    /// entity id. Frames with the same map share one texture and draw, while
    /// distinct maps remain separate batches so one map's transparent pixels
    /// cannot hide another's actual contents.
    ///
    /// # Every decline is logged
    ///
    /// A frame holding a map whose contents have not arrived is pixel-identical
    /// to a frame holding nothing, so each decline names its reason through
    /// [`note_map_skip`]. The commonest one on this client is
    /// [`MapSkip::NoContents`]: the integrated `lodestone-server` has no map
    /// saved data and no `MAP_ITEM_DATA` producer at all, so a singleplayer
    /// framed map can never fill in. A missing picture does not abort batches
    /// for other frames that do have their data.
    pub(super) fn prepare_framed_maps(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &lodestone_render::Camera,
        entities: &[EntityDraw],
        map_view_projection: Mat4,
        frame_view_projection: Mat4,
    ) -> Vec<PreparedMap> {
        const SITE: &str = "item frame";
        let frustum = camera.frustum();
        let diagnostic_switches = map_diagnostic_switches();
        let candidates: Vec<_> = entities
            .iter()
            .filter(|draw| {
                ITEM_FRAME_TYPES.contains(&draw.type_path.as_ref())
                    && draw.item.as_ref().is_some_and(|id| id.path() == FILLED_MAP_ITEM)
            })
            .collect();
        // This is vanilla `EntityRenderer.shouldRender`'s broad phase. Keep it
        // separate from the diagnostic selection below so a true frustum-edge
        // transition names the candidate that was culled.
        let in_frustum = |draw: &EntityDraw| {
            diagnostic_switches.disable_frustum_cull
                || framed_map_in_frustum(&frustum, draw.feet, draw.yaw, draw.pitch)
        };
        let track_candidates = candidates
            .iter()
            .filter_map(|draw| {
                framed_map_track_candidate(draw, camera.position, map_view_projection)
            })
            .collect::<Vec<_>>();
        let tracked_entity_id = tracked_map_entity(&track_candidates);
        let selected = candidates
            .iter()
            .copied()
            .find(|draw| Some(draw.id) == tracked_entity_id);
        let projection = selected.map(|draw| {
            framed_map_projection(
                draw,
                map_view_projection,
                frame_view_projection,
            )
        });
        let mut diagnostic = FramedMapGatherDiagnostic {
            model_ready: self.model.is_some(),
            map_source_installed: self.map_source.is_installed(),
            candidate_count: candidates.len(),
            tracked_entity_id,
            selected: selected.map(|draw| FramedMapDiagnosticFrame::from_draw(draw)),
            selected_in_frustum: selected.is_some_and(in_frustum),
            selected_submitted: false,
            projection_state: projection.map(|value| value.0),
            projection: projection.map(|value| value.1),
            submitted_instances: 0,
            submitted_batches: 0,
        };
        let wanted: Vec<_> = candidates
            .into_iter()
            .filter(|draw| in_frustum(draw))
            .collect();
        if wanted.is_empty() {
            // Ordinary empty frames remain silent after their first edge. The
            // diagnostic does record a transition from a visible candidate, so
            // a live repro can distinguish gather disappearance from culling.
            note_framed_map_gather(camera, diagnostic);
            return Vec::new();
        }
        let Some(model) = self.model.as_ref() else {
            note_framed_map_gather(camera, diagnostic);
            let _ = note_map_skip::<()>(SITE, MapSkip::NoModels, None);
            return Vec::new();
        };
        if !self.map_source.is_installed() {
            if let Some(frame) = &mut diagnostic.selected {
                frame.source = FramedMapSource::Unavailable;
            }
            note_framed_map_gather(camera, diagnostic);
            let _ = note_map_skip::<()>(SITE, MapSkip::NoSource, None);
            return Vec::new();
        }
        // A frame's item type decides the full-size frame body, but only this
        // lookup proves that its corresponding MAP_ITEM_DATA has arrived. Group
        // by stable map id, not `Arc` allocation identity: a map patch swaps the
        // copy-on-write pixels but does not require frame geometry to rebuild.
        let mut inputs = Vec::new();
        let mut pictures = HashMap::new();
        for draw in &wanted {
            let Some(picture) = self.map_source.picture(None, Some(draw.id)) else {
                if diagnostic.selected.as_ref().is_some_and(|frame| frame.entity_id == draw.id) {
                    diagnostic.selected.as_mut().expect("checked selected frame").source =
                        FramedMapSource::Unresolved;
                }
                let _ = note_map_skip::<()>(SITE, MapSkip::NoContents, Some(draw.id));
                continue;
            };
            if diagnostic.selected.as_ref().is_some_and(|frame| frame.entity_id == draw.id) {
                let frame = diagnostic.selected.as_mut().expect("checked selected frame");
                frame.map_id = Some(picture.map_id.raw());
                frame.source = FramedMapSource::Resolved;
            }
            let input = FramedMapInput::new(
                draw.id,
                picture.map_id,
                draw.feet.to_array(),
                draw.yaw,
                draw.pitch,
                draw.item_frame_rotation,
                draw.invisible,
                framed_map_light(
                    super::entity_passes::item_frame_light(
                        &self.entity_light,
                        draw,
                        draw.type_path.as_ref() == GLOW_ITEM_FRAME_TYPE_PATH,
                    ),
                    draw.type_path.as_ref() == GLOW_ITEM_FRAME_TYPE_PATH,
                ),
            );
            if should_trace_candidate("framed_map", draw.id, draw.feet, camera.position) {
                // `map_quad_mesh` is centred at local `(-.5, -.5, 0)` rather
                // than corner-origin. Move the diagnostic unit square into
                // that same local range before reporting its final plane.
                let pose = input.pose();
                // `map_quad_mesh` is centred in X/Y already and lies at local
                // z=0. `unit_quad_plane` expects a 0..1 quad, so adapt only
                // that XY origin; translating z would invent a half-block
                // depth that the mesh never renders.
                let map_plane = pose * Mat4::from_translation(Vec3::new(-0.5, -0.5, 0.0));
                let facing = lodestone_render::entity::item_frame_facing(draw.yaw, draw.pitch)
                    .transform_vector3(Vec3::NEG_Z)
                    .to_array();
                tracing::debug!(
                    target: "pack_trace",
                    surface = "framed_map",
                    entity_id = draw.id,
                    protocol_type = %draw.type_path,
                    world_pos = ?draw.feet.to_array(),
                    yaw = draw.yaw,
                    pitch = draw.pitch,
                    invisible = draw.invisible,
                    attachment_facing = ?facing,
                    frame_rotation = draw.item_frame_rotation,
                    held_item = ?draw.item.as_ref().map(ToString::to_string),
                    item_model = ?draw.item_model.as_ref().map(ToString::to_string),
                    map_id = picture.map_id.raw(),
                    final_transform = ?pose.to_cols_array_2d(),
                    quad_plane = ?unit_quad_plane(map_plane),
                    quad_normal = ?unit_quad_normal(pose),
                    "nearby render candidate reached framed-map draw"
                );
            }
            pictures.entry(picture.map_id).or_insert(picture);
            inputs.push(input);
        }
        diagnostic.submitted_instances = inputs.len();
        diagnostic.selected_submitted = tracked_entity_id.is_some_and(|tracked| {
            inputs.iter().any(|input| input.entity_id == tracked)
        });
        let key = FramedMapsKey::new(inputs);
        let batch_inputs = key.0.clone();
        let mut cache = self.map_cache.borrow_mut();
        let batches = cache.framed_batches.get_or_insert_with(key, || {
            let mut merged: Vec<(MapId, ModelMesh)> = Vec::new();
            for input in &batch_inputs {
                let mesh = map_quad_mesh(input.pose(), input.light);
                if let Some((_, batch)) = merged
                    .iter_mut()
                    .find(|(map_id, _)| *map_id == input.map_id)
                {
                    batch.merge(&mesh);
                } else {
                    merged.push((input.map_id, mesh));
                }
            }
            merged
                .into_iter()
                .map(|(map_id, mesh)| CachedFramedBatch {
                    map_id,
                    mesh: Arc::new(
                        GpuModelMesh::upload(device, &mesh)
                            .expect("a framed map batch has at least one quad"),
                    ),
                })
                .collect()
        });
        diagnostic.submitted_batches = batches.len();
        let mut prepared = Vec::with_capacity(batches.len());
        for batch in batches.iter() {
            let Some(picture) = pictures.get(&batch.map_id) else {
                // The cache key and picture collection are built together, so
                // this is defensive rather than a normal decline.
                let _ = note_map_skip::<()>(SITE, MapSkip::NoContents, Some(batch.map_id.raw()));
                continue;
            };
            let texture = cache.texture(device, queue, &model.pipeline, picture);
            prepared.push((Arc::clone(&batch.mesh), texture));
        }
        if !prepared.is_empty() {
            note_map_drawn();
        }
        note_framed_map_gather(camera, diagnostic);
        prepared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_diagnostic_switches_only_enable_the_named_branches() {
        let switches = map_diagnostic_switches_from(|name| match name {
            "LODESTONE_MAP_DISABLE_FRUSTUM_CULL" => true,
            "LODESTONE_MAP_DISABLE_BACKFACE_CULL" => true,
            "LODESTONE_MAP_DISABLE_DEPTH" => true,
            _ => false,
        });

        assert!(switches.disable_frustum_cull);
        assert!(switches.disable_backface_cull);
        assert!(switches.disable_depth());
        assert_eq!(switches.depth, MapDepthDiagnostic::ALL_OFF);
    }

    /// Nothing set is the production path on every axis. This is the arm that
    /// makes the three below mean something: without it they only show that
    /// *some* switch moves *some* field.
    #[test]
    fn no_map_switch_leaves_every_depth_decision_in_place() {
        let switches = map_diagnostic_switches_from(|_| false);
        assert_eq!(switches.depth, MapDepthDiagnostic::PRODUCTION);
        assert!(!switches.disable_depth());
        assert!(!switches.disable_frustum_cull);
        assert!(!switches.disable_backface_cull);
    }

    /// Each narrow switch removes **exactly one** depth decision.
    ///
    /// The whole point of splitting `LODESTONE_MAP_DISABLE_DEPTH` is that a live
    /// run under it changed three things at once, so it settled nothing. These
    /// assert the separation itself: for each switch, the named axis is off and
    /// the other two are still on. A regression that re-coupled any pair would
    /// leave the switches individually "working" and collectively useless, and
    /// only the *other two* assertions in each arm can see that.
    #[test]
    fn each_narrow_depth_switch_removes_exactly_one_decision() {
        let only = |target: &'static str| {
            map_diagnostic_switches_from(move |name| name == target).depth
        };

        let test = only("LODESTONE_MAP_DISABLE_DEPTH_TEST");
        assert!(!test.compare, "the test switch must drop the comparison");
        assert!(test.write, "the test switch must keep the depth write");
        assert!(test.bias, "the test switch must keep the polygon offset");

        let write = only("LODESTONE_MAP_DISABLE_DEPTH_WRITE");
        assert!(write.compare);
        assert!(!write.write, "the write switch must drop the depth write");
        assert!(write.bias);

        let bias = only("LODESTONE_MAP_DISABLE_DEPTH_BIAS");
        assert!(bias.compare);
        assert!(bias.write);
        assert!(!bias.bias, "the bias switch must drop the polygon offset");

        // And all three are genuinely distinct configurations.
        assert_ne!(test, write);
        assert_ne!(write, bias);
        assert_ne!(test, bias);
    }

    /// The supplied live trace was not the reported in-view disappearance: all
    /// four frames are about 51 blocks away and cross the horizontal frustum
    /// edge during this 9.3° turn. Pin the actual coordinates so a later cull
    /// change cannot misclassify this ordinary transition as a map depth bug.
    #[test]
    fn traced_four_map_transition_is_a_frustum_edge_not_a_gpu_drop() {
        let position = Vec3::new(2039.1737, 88.62, 3743.3477);
        let feet = [
            Vec3::new(2025.0, 89.0, 3792.0),
            Vec3::new(2025.0, 89.0, 3791.0),
            Vec3::new(2025.0, 88.0, 3791.0),
            Vec3::new(2025.0, 88.0, 3792.0),
        ];
        // A 100° vertical FOV is within the live options range and brackets the
        // trace: it includes the group at -44.25° but not after -53.55°.
        let camera = |yaw, pitch| lodestone_render::Camera {
            position,
            yaw,
            pitch,
            fov_y_degrees: 100.0,
            aspect: 16.0 / 9.0,
            near: 0.05,
            far: 512.0,
        };
        let before = camera(-44.250_305, 33.300_018).frustum();
        let after = camera(-53.550_308, 32.250_019).frustum();
        assert!(
            feet.iter().all(|&point| framed_map_in_frustum(&before, point, 0.0, 0.0)),
            "trace precondition: all four maps must reach CPU gather before the turn"
        );
        assert!(
            feet.iter().all(|&point| !framed_map_in_frustum(&after, point, 0.0, 0.0)),
            "the turned trace must be culled before any GPU map draw is submitted"
        );
    }

    #[test]
    fn nearby_invisible_frame_from_live_trace_survives_the_tiny_camera_turn() {
        let frame = Vec3::new(1965.0, 73.0, 3806.0);
        let camera = lodestone_render::Camera {
            position: Vec3::new(1963.2434, 72.62, 3808.2954),
            yaw: -42.600_28,
            pitch: 8.100_004,
            fov_y_degrees: 110.0,
            aspect: 2.0,
            near: 0.05,
            far: 512.0,
        };

        assert!(
            framed_map_in_frustum(&camera.frustum(), frame, 90.0, 0.0),
            "the live frame remains partly visible after this turn; culling its whole map is wrong"
        );
    }

    #[test]
    fn framed_map_live_diagnostic_is_edge_triggered_for_appearance_and_disappearance() {
        let frame = FramedMapDiagnosticFrame {
            entity_id: 42,
            entity_type: "item_frame".to_owned(),
            feet: [0, 0, 0],
            yaw: 0,
            pitch: 0,
            rotation: 0,
            invisible: false,
            item: Some("minecraft:filled_map".to_owned()),
            map_id: Some(7),
            source: FramedMapSource::Resolved,
            quad_normal: [0, 0, 0],
        };
        let visible = FramedMapGatherDiagnostic {
            model_ready: true,
            map_source_installed: true,
            candidate_count: 1,
            tracked_entity_id: Some(42),
            selected: Some(frame),
            selected_in_frustum: true,
            selected_submitted: true,
            projection_state: None,
            projection: None,
            submitted_instances: 1,
            submitted_batches: 1,
        };
        let disappeared = FramedMapGatherDiagnostic {
            model_ready: true,
            map_source_installed: true,
            candidate_count: 1,
            tracked_entity_id: Some(42),
            selected: visible.selected.clone(),
            selected_in_frustum: false,
            selected_submitted: false,
            projection_state: None,
            projection: None,
            submitted_instances: 0,
            submitted_batches: 0,
        };
        let mut diagnostics = MapLiveDiagnostics::default();

        assert!(diagnostics.gather_changed(&visible));
        assert!(
            !diagnostics.gather_changed(&visible),
            "the unchanged gather state must not log every frame"
        );
        let mut unrelated_counts_changed = visible.clone();
        unrelated_counts_changed.submitted_instances = 191;
        unrelated_counts_changed.submitted_batches = 24;
        assert!(
            !diagnostics.gather_changed(&unrelated_counts_changed),
            "other maps entering the frustum must not retrigger the tracked-map observer"
        );
        assert!(
            diagnostics.gather_changed(&disappeared),
            "a visible candidate leaving gather must emit a disappearance edge"
        );
        assert!(diagnostics.draw_changed(FramedMapDrawDiagnostic {
            tracked_entity_id: Some(42),
            tracked_submitted: true,
            tracked_drawn: true,
            submitted_instances: 1,
            submitted_batches: 1,
            drawn_instances: 1,
        }));
        assert!(
            !diagnostics.draw_changed(FramedMapDrawDiagnostic {
                tracked_entity_id: Some(42),
                tracked_submitted: true,
                tracked_drawn: true,
                submitted_instances: 191,
                submitted_batches: 24,
                drawn_instances: 191,
            }),
            "other map batches must not retrigger an unchanged tracked draw"
        );
        assert!(
            diagnostics.draw_changed(FramedMapDrawDiagnostic {
                tracked_entity_id: Some(42),
                tracked_submitted: true,
                tracked_drawn: false,
                submitted_instances: 1,
                submitted_batches: 1,
                drawn_instances: 0,
            }),
            "a submitted-but-not-drawn transition must be observable"
        );
    }

    #[test]
    fn framed_map_live_diagnostic_tracks_screen_centre_with_hysteresis() {
        let mut diagnostics = MapLiveDiagnostics::default();
        let candidates = |pairs: &[(i32, f32)]| {
            pairs
                .iter()
                .map(|&(entity_id, score)| FramedMapTrackCandidate { entity_id, score })
                .collect::<Vec<_>>()
        };

        assert_eq!(
            diagnostics.track_entity(None, &candidates(&[(49812, 0.10), (49815, 0.30)])),
            Some(49812)
        );
        assert_eq!(
            diagnostics.track_entity(None, &candidates(&[(49812, 0.15), (49815, 0.14)])),
            Some(49812),
            "a tiny centre-score change must not make the observer flicker"
        );
        assert_eq!(
            diagnostics.track_entity(None, &candidates(&[(49812, 0.30), (49815, 0.01)])),
            Some(49815),
            "a decisively more central frame must become the repro subject"
        );
        assert_eq!(diagnostics.track_entity(None, &[]), None);

        let mut configured = MapLiveDiagnostics::default();
        assert_eq!(
            configured.track_entity(Some(49813), &candidates(&[(49812, 0.0)])),
            Some(49813),
            "LODESTONE_MAP_TRACE_ENTITY must win over nearest selection"
        );
    }

    /// `ItemFrameRenderer.submit` enters frame-local space, moves contents by
    /// `.4375`/`.5`, then its map branch's `scale(1/128)` carries the
    /// `translate(0, 0, -1)` plus `MapRenderer.MAP_Z_OFFSET (-.01)`. The map
    /// mesh here is already centred and unit-sized, so this asserts that same
    /// depth at its local origin for every wall orientation.
    #[test]
    /// A glow frame lights its map itself; a plain one passes the frame's own
    /// sampled light straight through. The two must differ, and the glow value
    /// must be a shade under a fully-bright framed *item*, which is the
    /// distinction the 26.2 renderer draws between its two content branches.
    #[test]
    fn only_a_glow_frame_lights_its_own_map() {
        let dark = 0x00;
        let dim = 0x37;
        assert_eq!(framed_map_light(dark, false), dark);
        assert_eq!(framed_map_light(dim, false), dim);
        let glow = framed_map_light(dark, true);
        assert_eq!(glow >> 4, 15, "a glow frame's map is at full sky light");
        assert_eq!(glow & 0x0F, GLOW_FRAME_MAP_BLOCK_LIGHT);
        assert!(
            glow & 0x0F < 15,
            "the map branch sits below the item branch's full block light, \
             got {glow:#04x}"
        );
        assert_eq!(
            framed_map_light(0xFF, true),
            glow,
            "a glow frame ignores the sampled value entirely"
        );
    }

    #[test]
    fn framed_map_plane_matches_the_26_2_frame_local_content_chain() {
        let anchor = Vec3::new(4.0, 65.0, -9.0);
        let block_centre = anchor + Vec3::splat(0.5);
        let map_depth = 1.01 / 128.0;
        for (yaw, pitch) in [
            (0.0f32, 0.0f32),
            (90.0, 0.0),
            (180.0, 0.0),
            (270.0, 0.0),
            (0.0, -90.0),
            (0.0, 90.0),
        ] {
            let facing = lodestone_render::entity::item_frame_facing_step(yaw, pitch);
            for (invisible, content_lift) in [(false, 0.4375), (true, 0.5)] {
                let actual = framed_map_pose(anchor, yaw, pitch, 0, invisible)
                    .transform_point3(Vec3::ZERO);
                let expected = block_centre + facing * (map_depth - content_lift);
                assert!(
                    (actual - expected).length() < 1.0e-5,
                    "yaw {yaw}, pitch {pitch}, invisible {invisible}: map centre {actual:?}, expected {expected:?}"
                );
            }
        }
    }

    /// Item-frame spawn packets carry the integer attachment block position.
    /// With vanilla's invisible `.5` content lift, the map plane reaches that
    /// block's wall face, then `MapRenderer` moves it outward by `1.01 / 128`.
    #[test]
    fn invisible_framed_map_sits_just_outside_the_packet_anchors_wall() {
        let anchor = Vec3::new(4.0, 65.0, -9.0);
        for (yaw, pitch) in [
            (0.0f32, 0.0f32),
            (90.0, 0.0),
            (180.0, 0.0),
            (270.0, 0.0),
            (0.0, -90.0),
            (0.0, 90.0),
        ] {
            let facing = lodestone_render::entity::item_frame_facing_step(yaw, pitch);
            let actual = framed_map_pose(anchor, yaw, pitch, 0, true)
                .transform_point3(Vec3::ZERO);
            let expected = anchor + Vec3::splat(0.5) + facing * (-0.5 + MAP_RENDERER_DEPTH);
            assert!(
                (actual - expected).length() < 1.0e-5,
                "yaw {yaw}, pitch {pitch}: invisible map must be just outside wall {expected}, got {actual}"
            );
        }
    }

    /// The picture's signed clearance from the face of the block it hangs on,
    /// at all six facings, against a **hand-written `Direction` table** rather
    /// than against [`lodestone_render::entity::item_frame_facing_step`].
    ///
    /// # Why the table, when a helper already answers this
    ///
    /// Every other pose gate in this file — and the board gate in
    /// `tests/framed_map_pixels.rs` — takes its outward direction from
    /// `item_frame_facing_step`, which is the same function
    /// [`framed_map_pose`] builds the pose out of. That is `decode(encode(x))
    /// == x`: if the facing were inverted, the pose and the expectation would
    /// invert together and every one of those gates would still pass, while the
    /// picture sat on the wrong side of its wall. The expectation here comes
    /// from `Direction`'s own definition instead — `get2DDataValue` orders
    /// south, west, north, east, and `ItemFrame.setDirection` writes
    /// `2D * 90` into the entity's `yRot` with `xRot` zero, or `yRot` zero and
    /// `xRot = -90 * axisDirection.getStep()` for the two vertical ones — so
    /// the two sources share no code.
    ///
    /// # What the number means
    ///
    /// A hanging entity's attachment block is the one it *occupies*; the wall is
    /// the neighbour at `-direction`, so the surface the picture contests is the
    /// plane `0.5` back along the facing from the block centre. An invisible
    /// frame's content lift lands exactly on that plane and `MapRenderer`'s own
    /// `-1.01` (after `scale(1/128)`) pulls it back out, so the clearance must be
    /// `+MAP_RENDERER_DEPTH` — **positive, meaning toward the room**. This is the
    /// quantity a live report of the picture losing to its wall is a claim
    /// about, and it is printed rather than only asserted so a run reads as a
    /// measurement.
    #[test]
    fn the_picture_stands_in_front_of_its_wall_at_every_facing_of_the_direction_table() {
        // (yaw, pitch, Direction, its step vector) — transcribed from
        // `Direction`'s own ordering, not from any helper in this workspace.
        let table: [(f32, f32, &str, Vec3); 6] = [
            (0.0, 0.0, "SOUTH", Vec3::new(0.0, 0.0, 1.0)),
            (90.0, 0.0, "WEST", Vec3::new(-1.0, 0.0, 0.0)),
            (180.0, 0.0, "NORTH", Vec3::new(0.0, 0.0, -1.0)),
            (270.0, 0.0, "EAST", Vec3::new(1.0, 0.0, 0.0)),
            (0.0, -90.0, "UP", Vec3::new(0.0, 1.0, 0.0)),
            (0.0, 90.0, "DOWN", Vec3::new(0.0, -1.0, 0.0)),
        ];
        // The live server's own block, so the measurement is taken where the
        // report is and not at an origin whose small magnitudes round
        // differently.
        let block = Vec3::new(1970.0, 76.0, 3811.0);
        let block_centre = block + Vec3::splat(0.5);
        // The tolerance is **derived from the coordinates**, not picked. At
        // `x/z ~ 3811` a single-precision position is representable only every
        // `2^-12` of a block — 0.24 mm — so a 7.9 mm clearance carries about 3%
        // of quantization before anything is wrong, and a fixed `1e-5` epsilon
        // fails here while passing at the origin. That quantum is a measurement
        // in its own right: it is what the picture's whole separation from the
        // wall is spelled in at this build's coordinates.
        let quantum = {
            let magnitude = block_centre.x.abs().max(block_centre.z.abs());
            f32::from_bits(magnitude.to_bits() + 1) - magnitude
        };
        let tolerance = 2.0 * quantum;
        eprintln!(
            "world coordinate quantum at {:?}: {quantum:.9} blocks; \
             MAP_RENDERER_DEPTH is {MAP_RENDERER_DEPTH:.9}, i.e. {:.1} representable steps",
            block_centre.to_array(),
            MAP_RENDERER_DEPTH / quantum
        );
        // Collected, not asserted in the loop: an `assert!` inside it proves one
        // facing and leaves the other five as an argument.
        let mut wrong: Vec<String> = Vec::new();
        for (yaw, pitch, name, step) in table {
            // What the renderer's own helper thinks the facing is. Reported
            // beside the clearance so a disagreement names itself rather than
            // showing up only as a sign.
            let helper = lodestone_render::entity::item_frame_facing_step(yaw, pitch);
            // The wall's room-facing plane, from the table alone.
            let wall_face = block_centre - step * 0.5;
            // The expectation is a **literal**, not `MAP_RENDERER_DEPTH`. Writing
            // the constant here reads as rigour and is a closed loop: flipping
            // its sign flips the expectation with it, so the gate passes with
            // the picture buried in the wall. Measured — the first version of
            // this test did exactly that, and its neuter printed six negative
            // clearances and still reported `ok`. The number below is vanilla's
            // own chain instead: `translate(0, 0, -1)` after `scale(1/128)`,
            // plus `MapRenderer`'s `-0.01` vertex z at that same scale, along a
            // frame-local `+z` that `Axis.YP.rotationDegrees(180 - toYRot)`
            // points into the wall.
            let vanilla_map_plane = 1.01 / 128.0;
            for (kind, invisible, expected) in [
                ("invisible", true, vanilla_map_plane),
                // A visible frame's picture stands its own body's front plate
                // clear of the wall as well: vanilla's `0.4375` content lift is
                // `1/16` short of the face, and `MapRenderer` adds its own step.
                ("visible", false, vanilla_map_plane + 1.0 / 16.0),
            ] {
                let picture = framed_map_pose_with_extra_lift(block, yaw, pitch, 0, invisible, 0.0)
                    .transform_point3(Vec3::ZERO);
                let clearance = (picture - wall_face).dot(step);
                eprintln!(
                    "{name:<5} {kind:<9} helper_facing {:?} clearance {clearance:+.7} blocks \
                     (want {expected:+.7})",
                    helper.to_array()
                );
                if (helper - step).length() > 1.0e-5 {
                    wrong.push(format!(
                        "{name}: item_frame_facing_step gave {:?}, the Direction table says {:?}",
                        helper.to_array(),
                        step.to_array()
                    ));
                }
                // Two separate claims, because the magnitude check alone is
                // satisfied by a correctly-sized clearance pointing the wrong
                // way once the expectation is allowed to move with it.
                if clearance <= 0.0 {
                    wrong.push(format!(
                        "{name} {kind}: the picture is {} blocks INSIDE its wall — the lift is \
                         pointing the wrong way",
                        -clearance
                    ));
                }
                if (clearance - expected).abs() > tolerance {
                    wrong.push(format!(
                        "{name} {kind}: the picture stands {clearance} blocks off its wall face, \
                         wanted {expected} (tolerance {tolerance}, one coordinate quantum is \
                         {quantum})"
                    ));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The live clearance probe must move the picture **out of** the wall.
    ///
    /// It is a ruler for a live report, and a ruler that reads backwards is
    /// worse than none: an inward probe would make every value make the defect
    /// worse, which reads as "the deficit is larger than anything I tried"
    /// rather than as a broken instrument. Measured at all six facings, because
    /// the outward direction is `item_frame_facing_step` and not any fixed axis
    /// — the same coincidence at yaw 0/180 that once hid a whole pose bug.
    #[test]
    fn the_live_lift_probe_moves_the_picture_away_from_the_wall() {
        let anchor = Vec3::new(4.0, 65.0, -9.0);
        let extra = 0.25f32;
        let mut wrong: Vec<String> = Vec::new();
        for (yaw, pitch) in [
            (0.0f32, 0.0f32),
            (90.0, 0.0),
            (180.0, 0.0),
            (270.0, 0.0),
            (0.0, -90.0),
            (0.0, 90.0),
        ] {
            let facing = lodestone_render::entity::item_frame_facing_step(yaw, pitch);
            let base = framed_map_pose_with_extra_lift(anchor, yaw, pitch, 0, true, 0.0)
                .transform_point3(Vec3::ZERO);
            let probed = framed_map_pose_with_extra_lift(anchor, yaw, pitch, 0, true, extra)
                .transform_point3(Vec3::ZERO);
            let moved = (probed - base).dot(facing);
            if (moved - extra).abs() > 1.0e-5 {
                wrong.push(format!(
                    "yaw {yaw}, pitch {pitch}: probe moved the picture {moved} along its own \
                     facing, wanted {extra}"
                ));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The trace's depth margin is positive when the picture is **nearer the
    /// eye**, which in this renderer's reversed-Z `[0, 1]` depth means a
    /// *greater* depth value.
    #[test]
    fn the_traced_depth_margin_is_positive_when_the_picture_is_in_front() {
        // Same `w`, so the two depths are the two `z` values directly.
        let ahead = depth_ulp_margin([0.0, 0.0, 0.95, 1.0], [0.0, 0.0, 0.90, 1.0]);
        let behind = depth_ulp_margin([0.0, 0.0, 0.90, 1.0], [0.0, 0.0, 0.95, 1.0]);
        assert!(ahead > 0.0, "a nearer picture must read positive, got {ahead}");
        assert!(behind < 0.0, "a farther picture must read negative, got {behind}");
        assert!(
            (ahead + behind).abs() < ahead.abs() * 0.01,
            "the two directions must be equal and opposite: {ahead} against {behind}"
        );
        // A margin of one representable step is exactly 1.0, which is what makes
        // the number comparable against the `-20` polygon-offset constant.
        let one_step = 0.95f32;
        let next = f32::from_bits(one_step.to_bits() + 1);
        let single = depth_ulp_margin([0.0, 0.0, next, 1.0], [0.0, 0.0, one_step, 1.0]);
        assert!(
            (single - 1.0).abs() < 1.0e-3,
            "one representable step must read as 1 ULP, got {single}"
        );
        assert!(
            depth_ulp_margin([0.0, 0.0, 0.5, -1.0], [0.0, 0.0, 0.5, 1.0]).is_nan(),
            "a point behind the eye has no meaningful margin"
        );
    }

    #[test]
    fn retained_entry_skips_the_second_build_and_rebuilds_for_new_content() {
        let mut cache = RetainedMapEntries::<MapTextureKey, usize>::default();
        let map_id = MapId::new(17).expect("fixed map id is valid");
        let first = MapTextureKey::new(map_id, 3);
        let changed = MapTextureKey::new(map_id, 4);
        let mut builds = 0;

        let first_value = cache.get_or_insert_with(first, || {
            builds += 1;
            builds
        });
        let unchanged_value = cache.get_or_insert_with(first, || {
            builds += 1;
            builds
        });
        assert_eq!(
            builds, 1,
            "an unchanged map must skip conversion, upload and bind-group build"
        );
        assert!(Arc::ptr_eq(&first_value, &unchanged_value));

        let changed_value = cache.get_or_insert_with(changed, || {
            builds += 1;
            builds
        });
        assert_eq!(builds, 2, "a changed map revision must rebuild its GPU entry");
        assert_ne!(*first_value, *changed_value);
    }

    #[test]
    fn retained_last_batch_reuses_an_unchanged_frame_and_rebuilds_on_pose_or_light_change() {
        let map_id = MapId::new(17).expect("fixed map id is valid");
        let stable =
            FramedMapInput::new(9, map_id, [4.0, 65.0, -9.0], 0.0, 0.0, 0, false, 0xF0);
        let moved =
            FramedMapInput::new(9, map_id, [5.0, 65.0, -9.0], 0.0, 0.0, 0, false, 0xF0);
        let relit =
            FramedMapInput::new(9, map_id, [5.0, 65.0, -9.0], 0.0, 0.0, 0, false, 0xE0);
        let mut cache = RetainedLast::<FramedMapsKey, usize>::default();
        let mut builds = 0;

        let first = cache.get_or_insert_with(FramedMapsKey::new(vec![stable.clone()]), || {
            builds += 1;
            builds
        });
        let unchanged = cache.get_or_insert_with(FramedMapsKey::new(vec![stable]), || {
            builds += 1;
            builds
        });
        assert_eq!(builds, 1, "an unchanged frame must skip mesh build and upload");
        assert!(Arc::ptr_eq(&first, &unchanged));

        cache.get_or_insert_with(FramedMapsKey::new(vec![moved]), || {
            builds += 1;
            builds
        });
        cache.get_or_insert_with(FramedMapsKey::new(vec![relit]), || {
            builds += 1;
            builds
        });
        assert_eq!(builds, 3, "pose and light mutations must each invalidate the batch");
    }

    /// Invisible frames have a different vanilla map-content lift (`.5` rather
    /// than `.4375` in frame-local z). It is geometry, so it belongs in the
    /// retained mesh key; otherwise a visibility metadata update reuses the
    /// visible frame's stale vertices until some unrelated movement rebuilds.
    #[test]
    fn invisible_framed_map_has_its_own_lift_and_cache_key() {
        let map_id = MapId::new(17).expect("fixed map id is valid");
        let visible =
            FramedMapInput::new(9, map_id, [4.0, 65.0, -9.0], 90.0, 0.0, 0, false, 0xF0);
        let invisible =
            FramedMapInput::new(9, map_id, [4.0, 65.0, -9.0], 90.0, 0.0, 0, true, 0xF0);
        assert_ne!(visible, invisible, "invisibility changes framed-map vertex positions");

        let facing = lodestone_render::entity::item_frame_facing_step(90.0, 0.0);
        let visible_centre = visible.pose().transform_point3(Vec3::ZERO);
        let invisible_centre = invisible.pose().transform_point3(Vec3::ZERO);
        assert!(
            ((invisible_centre - visible_centre).dot(facing) + 1.0 / 16.0).abs() < 1.0e-5,
            "the invisible map must move one sixteenth toward its wall, not along world z"
        );

        let mut cache = RetainedLast::<FramedMapsKey, usize>::default();
        let mut builds = 0;
        cache.get_or_insert_with(FramedMapsKey::new(vec![visible]), || {
            builds += 1;
            builds
        });
        cache.get_or_insert_with(FramedMapsKey::new(vec![invisible]), || {
            builds += 1;
            builds
        });
        assert_eq!(builds, 2, "a visibility change must not reuse the old lifted quad");
    }

    /// A framed map faces the way its frame does, stands off the wall along that
    /// same axis, and **points its front face out of the wall**.
    ///
    /// The last of the three is the one this gate used to omit, and omitting it
    /// is what let an east- or west-facing framed map draw zero pixels: the model
    /// pipeline culls back faces, so a picture whose normal points into the wall
    /// is simply not there. `Ry(yaw)`'s local `+z` and the frame's real
    /// `Direction` agree at yaw 0 and 180 and are opposite at 90 and 270 —
    /// exactly the coincidence the translation arms below already had to work
    /// around — so an orientation assertion at 0 and 180 alone proves nothing.
    #[test]
    fn a_framed_map_lifts_along_its_own_facing_and_faces_out_of_the_wall() {
        let visible_lift = 0.4375 - MAP_RENDERER_DEPTH;
        let anchor = Vec3::new(4.0, 65.0, -9.0);
        let block_centre = anchor + Vec3::splat(0.5);
        let pose = framed_map_pose(anchor, 0.0, 0.0, 0, false);
        let centre = pose.transform_point3(Vec3::ZERO);
        assert!((centre.x - block_centre.x).abs() < 1.0e-5);
        assert!((centre.y - block_centre.y).abs() < 1.0e-5);
        assert!((centre.z - (block_centre.z - visible_lift)).abs() < 1.0e-5);

        // Yaw 180 faces north, so the same lift must move the other way.
        let north = framed_map_pose(anchor, 180.0, 0.0, 0, false)
            .transform_point3(Vec3::ZERO);
        assert!(north.z > block_centre.z, "a north-facing frame lifts toward +z");

        // Yaw 90 and 270 are the discriminating inputs. Yaw 90 is west, so the
        // picture must move to `-x`.
        let west = framed_map_pose(anchor, 90.0, 0.0, 0, false)
            .transform_point3(Vec3::ZERO);
        assert!(
            (west.x - (block_centre.x + visible_lift)).abs() < 1.0e-5,
            "yaw 90 is west, so the picture lifts to +x; got x={} (a bare Ry(yaw) \
             would give {})",
            west.x,
            block_centre.x - visible_lift
        );
        let east = framed_map_pose(anchor, 270.0, 0.0, 0, false)
            .transform_point3(Vec3::ZERO);
        assert!(
            (east.x - (block_centre.x - visible_lift)).abs() < 1.0e-5,
            "yaw 270 is east, so the picture lifts to -x; got x={}",
            east.x
        );

        // The map's plane must sit ahead of `template_item_frame_map`'s
        // room-facing plate at every orientation. The source-derived depth is
        // MapRenderer's 1.01/128 plane bias plus the model's 0.001/16 gap
        // between its local 15.001/16 plate (minus the body's half-block
        // origin) and vanilla's .4375 content origin.
        for (yaw, pitch) in [
            (0.0f32, 0.0f32),
            (90.0, 0.0),
            (180.0, 0.0),
            (270.0, 0.0),
            (0.0, -90.0),
            (0.0, 90.0),
        ] {
            let feet = anchor;
            let facing = lodestone_render::entity::item_frame_facing_step(yaw, pitch);
            let plate = lodestone_render::entity::item_frame_body_matrix(feet, yaw, pitch)
                .transform_point3(Vec3::new(0.5, 0.5, 15.001 / 16.0));
            let picture = framed_map_pose(feet, yaw, pitch, 0, false).transform_point3(Vec3::ZERO);
            let separation = (picture - plate).dot(facing);
            let expected_separation = MAP_RENDERER_DEPTH + (15.001 / 16.0 - 0.5 - 0.4375);
            assert!(
                (separation - expected_separation).abs() < 1.0e-5,
                "yaw {yaw}, pitch {pitch}: map plane must be {expected_separation} \
                 ahead of the frame map plate, got {separation}"
            );
        }

        // --- the orientation, at every horizontal facing and both vertical ones
        //
        // Collected rather than asserted in the loop: an `assert!` inside it
        // aborts on the first bad facing and leaves the other five unmeasured,
        // so a run would prove one arm and argue the rest.
        let mut wrong: Vec<String> = Vec::new();
        for (yaw, pitch) in [
            (0.0f32, 0.0f32),
            (90.0, 0.0),
            (180.0, 0.0),
            (270.0, 0.0),
            (0.0, -90.0),
            (0.0, 90.0),
        ] {
            let facing = lodestone_render::entity::item_frame_facing_step(yaw, pitch);
            let normal = framed_map_pose(Vec3::ZERO, yaw, pitch, 0, false)
                .transform_vector3(Vec3::Z)
                .normalize();
            let dot = normal.dot(facing);
            if dot < 0.999 {
                wrong.push(format!(
                    "(yaw {yaw}, pitch {pitch}): quad normal {normal:?} against the \
                     frame's facing {facing:?}, dot {dot}"
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of 6 facings point the picture's front face into the wall, where \
             back-face culling discards it:\n  {}",
            wrong.len(),
            wrong.join("\n  ")
        );

        // --- the in-plane quarter turn ---------------------------------------
        //
        // `ItemFrameRenderer`'s map branch is `rotation % 4 * 2` eighths, so a
        // map only ever hangs at a right angle and the odd half-steps fold onto
        // the even ones. Asserted as a *pair* of claims a wrong reading would
        // separate: rotation 1 must move the picture's own `+x` corner, and
        // rotation 4 must land back on rotation 0.
        let up = |rotation: u8| {
            framed_map_pose(Vec3::ZERO, 180.0, 0.0, rotation, false).transform_vector3(Vec3::Y)
        };
        assert!(
            up(0).dot(up(1)).abs() < 1.0e-5,
            "one step is a quarter turn, so its up vector is perpendicular; got \
             {:?} against {:?}",
            up(0),
            up(1)
        );
        assert!(
            (up(4) - up(0)).length() < 1.0e-5,
            "four quarter turns is the identity; got {:?} against {:?}",
            up(4),
            up(0)
        );
        assert!(
            (up(5) - up(1)).length() < 1.0e-5,
            "the eighth-steps fold in fours for a map, unlike an ordinary framed \
             item's 45 degrees; got {:?} against {:?}",
            up(5),
            up(1)
        );
    }

    /// The held map sits in front of the camera, not behind it. `-z` is into the
    /// screen in this space, so a positive `z` would put the whole picture behind
    /// the near plane and draw nothing at all.
    #[test]
    fn a_held_map_is_in_front_of_the_camera() {
        let mesh = map_quad_mesh(held_map_pose(0.0), 15);
        assert!(mesh.vertices.iter().all(|v| v.position[2] < 0.0));
        // The dip lowers it and never raises it.
        let rested = map_quad_mesh(held_map_pose(0.0), 15).vertices[0].position[1];
        let dipped = map_quad_mesh(held_map_pose(1.0), 15).vertices[0].position[1];
        assert!(dipped < rested);
    }
}
