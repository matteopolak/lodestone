//! Filled-map drawing: the per-map 128×128 texture and the quads that sample it.
//!
//! # No map pipeline and no map shader
//!
//! A map's picture is one textured quad, and the *model* pipeline already draws
//! textured quads with absolute baked UVs from a texture at group 1. So a map
//! draw is an ordinary model draw with **group 1 swapped** from the stitched block
//! atlas to the map's own texture. That is not a shortcut — the model shader is at
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
use lodestone_render::map_item::{MAP_SIZE, map_quad_mesh, map_texture_rgba};
use lodestone_render::texture::GpuAtlas;
use lodestone_render::{ENTITY_FULLBRIGHT, GpuModelMesh, ModelMesh, ModelPipeline};

use crate::entities::EntityDraw;

use super::{MapPicture, RenderState};

/// GPU resources that stay valid for this [`RenderState`] and are shared by a
/// held or framed map draw.
pub(super) type PreparedMap = (Arc<GpuModelMesh>, Arc<wgpu::BindGroup>);

/// Cumulative retained-map work for this render state. A stationary second
/// frame should leave every field unchanged; a map patch changes only the three
/// texture fields, while a moved/rotated/relit item frame changes only the
/// framed-mesh fields.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MapCacheCounters {
    /// Calls to [`map_texture_rgba`].
    pub texture_conversions: u64,
    /// `queue.write_texture` submissions for map pixels.
    pub texture_uploads: u64,
    /// Map texture/sampler bind groups constructed.
    pub texture_bind_groups: u64,
    /// Held-map mesh builds and GPU uploads.
    pub held_mesh_builds: u64,
    /// Framed-map batch mesh builds and GPU uploads.
    pub framed_mesh_builds: u64,
}

/// A map texture's exact content identity. The id scopes the revision: two
/// unrelated maps naturally both start at revision zero.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct MapTextureKey {
    map_id: i32,
    color_revision: u64,
}

impl MapTextureKey {
    const fn new(map_id: i32, color_revision: u64) -> Self {
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
    map_id: i32,
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
        map_id: i32,
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
    fn matches(&self, key: &K) -> bool {
        self.entry.as_ref().is_some_and(|(previous, _)| previous == key)
    }

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
    map_id: i32,
    mesh: Arc<GpuModelMesh>,
}

/// Per-device retained map resources. This is intentionally owned by
/// [`RenderState`]: a device or colour-format/session rebuild creates a new
/// state and therefore cannot reuse stale wgpu handles.
pub(super) struct MapRenderCache {
    textures: RetainedMapEntries<MapTextureKey, wgpu::BindGroup>,
    held_mesh: RetainedLast<u32, GpuModelMesh>,
    framed_batches: RetainedLast<FramedMapsKey, Vec<CachedFramedBatch>>,
    counters: MapCacheCounters,
}

impl Default for MapRenderCache {
    fn default() -> Self {
        Self {
            textures: RetainedMapEntries::default(),
            held_mesh: RetainedLast::default(),
            framed_batches: RetainedLast::default(),
            counters: MapCacheCounters::default(),
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
            self.counters.texture_conversions += 1;
            self.counters.texture_uploads += 1;
            self.counters.texture_bind_groups += 1;
        }
        self.textures.get_or_insert_with(key, || {
            map_texture_bind_group(device, queue, pipeline, picture.colors.as_slice())
        })
    }

    pub(super) const fn counters(&self) -> MapCacheCounters {
        self.counters
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

/// A map's picture is drawn one block across, matching the item frame it hangs in.
const FRAMED_MAP_SCALE: f32 = 1.0;

/// How far in front of the frame's own plane the picture sits, along the frame's
/// real facing.
///
/// The room-facing plane of `template_item_frame_map`: its first element begins
/// at local `z = 15.001 / 16`, and the frame body starts half a block behind
/// `item_frame_space`. Measured outward from the entity position.
const ITEM_FRAME_MAP_FRONT_LIFT: f32 = 0.46875 - (15.001 / 16.0 - 0.5);

/// Explicit geometric separation between the frame's room-facing map plate and
/// the map picture. The previous relative depth bias still flickered on live
/// frames; this is a real 1/64-block displacement along the frame normal, small
/// enough to remain visually flush but large enough not to collapse at grazing
/// angles.
const FRAMED_MAP_FRONT_SEPARATION: f32 = 1.0 / 64.0;

/// The picture's distance from the item-frame entity along its outward normal.
const FRAMED_MAP_LIFT: f32 = ITEM_FRAME_MAP_FRONT_LIFT + FRAMED_MAP_FRONT_SEPARATION;

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

/// The world pose for a map hanging in an item frame at `feet`, facing along the
/// frame's real `Direction`.
///
/// ```text
/// T(feet + dir · FRAMED_MAP_LIFT) · item_frame_facing(yaw, pitch) · Rz(rot · 90°) · Ry(180°) · S
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
/// same turn `Axis.ZP.rotationDegrees(180)` because it draws through a no-cull
/// render type and only needs the *image* the right way round; it also lays its
/// quad out with `v` growing **up** where [`map_quad_mesh`] grows it down. Those
/// two differences compose: on the `z == 0` plane every vertex of this quad
/// lands, `Rz(180) · diag(1, -1, 1)` and `Ry(180)` are the same map, and only
/// `Ry(180)` also turns the face outward. Substituting `Rz(180)` here draws the
/// picture upside-down *and* back-to-front.
///
/// # The in-plane rotation is quartered, not eighthed
///
/// `ItemFrame.getRotation()` counts eighths and an ordinary framed item is drawn
/// at `rotation · 45°`, but the map branch is `rotation % 4 * 2` eighths — i.e.
/// `(rotation % 4) · 90°`. A map only ever hangs at a right angle, and the odd
/// half-steps fold onto the even ones rather than tilting the picture.
#[must_use]
fn framed_map_pose(feet: Vec3, yaw: f32, pitch: f32, rotation: u8, invisible: bool) -> Mat4 {
    let step = lodestone_render::entity::item_frame_facing_step(yaw, pitch);
    let quarter_turns = f32::from(rotation % 4) * 90.0;
    // `ItemFrameRenderer` moves visible contents to local z=.4375 and
    // invisible-frame contents to z=.5. Local +z points into the wall after
    // the frame transform, so the latter is one sixteenth *less* far outward.
    // Keep our fixed front separation from the frame plate on top of that
    // vanilla distinction rather than treating invisibility as material-only.
    let lift = FRAMED_MAP_LIFT - if invisible { 1.0 / 16.0 } else { 0.0 };
    Mat4::from_translation(feet + step * lift)
        * lodestone_render::entity::item_frame_facing(yaw, pitch)
        * Mat4::from_rotation_z(quarter_turns.to_radians())
        * Mat4::from_rotation_y(std::f32::consts::PI)
        * Mat4::from_scale(Vec3::splat(FRAMED_MAP_SCALE))
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
        if !cache.held_mesh.matches(&inverse_arm_height.to_bits()) {
            cache.counters.held_mesh_builds += 1;
        }
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
    ) -> Vec<PreparedMap> {
        const SITE: &str = "item frame";
        let frustum = camera.frustum();
        let wanted = entities.iter().any(|draw| {
            ITEM_FRAME_TYPES.contains(&draw.type_path.as_ref())
                && draw.item.as_ref().is_some_and(|id| id.path() == FILLED_MAP_ITEM)
                // A framed map is a block across, so a one-block box around the
                // hanging point covers it however the frame is turned.
                && frustum.intersects_aabb(draw.feet - Vec3::splat(1.0), draw.feet + Vec3::splat(1.0))
        });
        if !wanted {
            // No framed map is in view. Not a decline, and deliberately silent:
            // this is every frame of ordinary play.
            return Vec::new();
        }
        let Some(model) = self.model.as_ref() else {
            let _ = note_map_skip::<()>(SITE, MapSkip::NoModels, None);
            return Vec::new();
        };
        if !self.map_source.is_installed() {
            let _ = note_map_skip::<()>(SITE, MapSkip::NoSource, None);
            return Vec::new();
        }
        // A frame's item type decides the full-size frame body, but only this
        // lookup proves that its corresponding MAP_ITEM_DATA has arrived. Group
        // by stable map id, not `Arc` allocation identity: a map patch swaps the
        // copy-on-write pixels but does not require frame geometry to rebuild.
        let mut inputs = Vec::new();
        let mut pictures = HashMap::new();
        for draw in entities {
            if !ITEM_FRAME_TYPES.contains(&draw.type_path.as_ref())
                || draw.item.as_ref().is_none_or(|id| id.path() != FILLED_MAP_ITEM)
                || !frustum.intersects_aabb(draw.feet - Vec3::splat(1.0), draw.feet + Vec3::splat(1.0))
            {
                continue;
            }
            let Some(picture) = self.map_source.picture(None, Some(draw.id)) else {
                let _ = note_map_skip::<()>(SITE, MapSkip::NoContents, Some(draw.id));
                continue;
            };
            let input = FramedMapInput::new(
                draw.id,
                picture.map_id,
                draw.feet.to_array(),
                draw.yaw,
                draw.pitch,
                draw.item_frame_rotation,
                draw.invisible,
                self.entity_light.sample(draw.feet),
            );
            pictures.entry(picture.map_id).or_insert(picture);
            inputs.push(input);
        }
        let key = FramedMapsKey::new(inputs);
        let batch_inputs = key.0.clone();
        let mut cache = self.map_cache.borrow_mut();
        if !cache.framed_batches.matches(&key) {
            cache.counters.framed_mesh_builds += 1;
        }
        let batches = cache.framed_batches.get_or_insert_with(key, || {
            let mut merged: Vec<(i32, ModelMesh)> = Vec::new();
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
        let mut prepared = Vec::with_capacity(batches.len());
        for batch in batches.iter() {
            let Some(picture) = pictures.get(&batch.map_id) else {
                // The cache key and picture collection are built together, so
                // this is defensive rather than a normal decline.
                let _ = note_map_skip::<()>(SITE, MapSkip::NoContents, Some(batch.map_id));
                continue;
            };
            let texture = cache.texture(device, queue, &model.pipeline, picture);
            prepared.push((Arc::clone(&batch.mesh), texture));
        }
        if !prepared.is_empty() {
            note_map_drawn();
        }
        prepared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_entry_skips_the_second_build_and_rebuilds_for_new_content() {
        let mut cache = RetainedMapEntries::<MapTextureKey, usize>::default();
        let first = MapTextureKey::new(17, 3);
        let changed = MapTextureKey::new(17, 4);
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
        let stable = FramedMapInput::new(9, 17, [4.0, 65.0, -9.0], 0.0, 0.0, 0, false, 0xF0);
        let moved = FramedMapInput::new(9, 17, [5.0, 65.0, -9.0], 0.0, 0.0, 0, false, 0xF0);
        let relit = FramedMapInput::new(9, 17, [5.0, 65.0, -9.0], 0.0, 0.0, 0, false, 0xE0);
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
        let visible = FramedMapInput::new(9, 17, [4.0, 65.0, -9.0], 90.0, 0.0, 0, false, 0xF0);
        let invisible = FramedMapInput::new(9, 17, [4.0, 65.0, -9.0], 90.0, 0.0, 0, true, 0xF0);
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
        let pose = framed_map_pose(Vec3::new(4.0, 65.0, -9.0), 0.0, 0.0, 0, false);
        let centre = pose.transform_point3(Vec3::ZERO);
        assert!((centre.x - 4.0).abs() < 1.0e-5);
        assert!((centre.y - 65.0).abs() < 1.0e-5);
        assert!((centre.z - (-9.0 + FRAMED_MAP_LIFT)).abs() < 1.0e-5);

        // Yaw 180 faces north, so the same lift must move the other way.
        let north = framed_map_pose(Vec3::new(4.0, 65.0, -9.0), 180.0, 0.0, 0, false)
            .transform_point3(Vec3::ZERO);
        assert!(north.z < -9.0, "a north-facing frame lifts toward -z");

        // Yaw 90 and 270 are the discriminating inputs. Yaw 90 is west, so the
        // picture must move to `-x`.
        let west = framed_map_pose(Vec3::new(4.0, 65.0, -9.0), 90.0, 0.0, 0, false)
            .transform_point3(Vec3::ZERO);
        assert!(
            (west.x - (4.0 - FRAMED_MAP_LIFT)).abs() < 1.0e-5,
            "yaw 90 is west, so the picture lifts to -x; got x={} (a bare Ry(yaw) \
             would give {})",
            west.x,
            4.0 + FRAMED_MAP_LIFT
        );
        let east = framed_map_pose(Vec3::new(4.0, 65.0, -9.0), 270.0, 0.0, 0, false)
            .transform_point3(Vec3::ZERO);
        assert!(
            (east.x - (4.0 + FRAMED_MAP_LIFT)).abs() < 1.0e-5,
            "yaw 270 is east, so the picture lifts to +x; got x={}",
            east.x
        );

        // The map's plane must have a real, fixed positive gap ahead of the
        // actual `template_item_frame_map` room-facing plane — not merely a
        // relative pipeline bias. Check every orientation below because a
        // world-z-only displacement can pass this at south and fail on the
        // east/west walls (or ceilings).
        for (yaw, pitch) in [
            (0.0f32, 0.0f32),
            (90.0, 0.0),
            (180.0, 0.0),
            (270.0, 0.0),
            (0.0, -90.0),
            (0.0, 90.0),
        ] {
            let feet = Vec3::new(4.0, 65.0, -9.0);
            let facing = lodestone_render::entity::item_frame_facing_step(yaw, pitch);
            let plate = lodestone_render::entity::item_frame_body_matrix(feet, yaw, pitch)
                .transform_point3(Vec3::new(0.5, 0.5, 15.001 / 16.0));
            let picture = framed_map_pose(feet, yaw, pitch, 0, false).transform_point3(Vec3::ZERO);
            let separation = (picture - plate).dot(facing);
            assert!(
                (separation - FRAMED_MAP_FRONT_SEPARATION).abs() < 1.0e-5,
                "yaw {yaw}, pitch {pitch}: map plane must be {FRAMED_MAP_FRONT_SEPARATION} \
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
