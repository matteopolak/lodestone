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
//! # Why the texture is rebuilt rather than cached
//!
//! [`map_texture_bind_group`] creates a texture, uploads it and builds a bind group
//! every frame a map is on screen. A cache keyed by map id would need `&mut self`
//! (the whole render path is `&self`) *and* would miss almost every frame anyway:
//! the server streams a fresh column patch as the holder walks, so a walking
//! player's map changes on most ticks. At 64 KB and one or two visible maps this is
//! not worth the invalidation bug.
//!
//! # What is not drawn
//!
//! Vanilla's `map_background` frame sprite and the `MapDecoration` icons (the
//! player arrow, banner markers) — both want the map-decorations atlas, which the
//! asset layer does not stitch. `SessionMaps` carries the decorations already, so
//! this is an asset job rather than a wiring one.

use std::sync::Arc;

use glam::{Mat4, Vec3};
use lodestone_render::map_item::{MAP_SIZE, map_quad_mesh, map_texture_rgba};
use lodestone_render::texture::GpuAtlas;
use lodestone_render::{ENTITY_FULLBRIGHT, GpuModelMesh, ModelMesh, ModelPipeline};

use crate::entities::EntityDraw;

use super::RenderState;

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
/// **Derived, not tuned.** `ItemFrameRenderer.submit`'s map branch translates
/// `0.4375` forward, then `-1.0` at the `0.0078125` map scale — a further
/// `-0.0078125` — putting the picture at local `z = 0.4296875` inside a frame
/// whose own space starts `0.46875` in front of the entity
/// (`ITEM_FRAME_WALL_STEP`). The picture therefore sits `0.46875 - 0.4296875`
/// from the entity's own position, which is also 1/128 of a block *in front of*
/// the `item_frame_map` model's back plate at local `0.4375`. The previous
/// hand-picked `0.03` was 1/1000 of a block behind that plate, which was
/// invisible only for as long as nothing drew the plate.
const FRAMED_MAP_LIFT: f32 = 0.46875 - 0.429_687_5;

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
fn framed_map_pose(feet: Vec3, yaw: f32, pitch: f32, rotation: u8) -> Mat4 {
    let step = lodestone_render::entity::item_frame_facing_step(yaw, pitch);
    let quarter_turns = f32::from(rotation % 4) * 90.0;
    Mat4::from_translation(feet + step * FRAMED_MAP_LIFT)
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
    /// The quad meshed but its vertex/index buffers could not be uploaded.
    NoUpload,
}

impl MapSkip {
    const fn reason(self) -> &'static str {
        match self {
            Self::NoModels => "no baked model set",
            Self::NoSource => "no map source installed for this frame",
            Self::NoContents => "no MAP_ITEM_DATA has arrived for this map yet",
            Self::NoUpload => "the map quad could not be uploaded",
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
    ) -> Option<(GpuModelMesh, wgpu::BindGroup)> {
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
        let Some(colors) = self.map_source.picture(None, None) else {
            return note_map_skip(SITE, MapSkip::NoContents, None);
        };
        // Full bright, as the GUI item path nails every vertex: the map is drawn
        // in its own camera-space pass with no world position to sample.
        let mesh = map_quad_mesh(held_map_pose(inverse_arm_height), ENTITY_FULLBRIGHT);
        let Some(gpu) = GpuModelMesh::upload(device, &mesh) else {
            return note_map_skip(SITE, MapSkip::NoUpload, None);
        };
        note_map_drawn();
        Some((
            gpu,
            map_texture_bind_group(device, queue, &model.pipeline, colors.as_slice()),
        ))
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
    ) -> Vec<(GpuModelMesh, wgpu::BindGroup)> {
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
        // lookup proves that its corresponding MAP_ITEM_DATA has arrived. Keep
        // separate meshes for distinct `Arc`s: each needs a different texture
        // bind group, and concatenating their vertices would sample all of them
        // through whichever texture happened to be bound last.
        let mut batches: Vec<(Arc<Vec<u8>>, ModelMesh)> = Vec::new();
        for draw in entities {
            if !ITEM_FRAME_TYPES.contains(&draw.type_path.as_ref())
                || draw.item.as_ref().is_none_or(|id| id.path() != FILLED_MAP_ITEM)
                || !frustum.intersects_aabb(draw.feet - Vec3::splat(1.0), draw.feet + Vec3::splat(1.0))
            {
                continue;
            }
            let Some(colors) = self.map_source.picture(None, Some(draw.id)) else {
                let _ = note_map_skip::<()>(SITE, MapSkip::NoContents, Some(draw.id));
                continue;
            };
            let mesh = map_quad_mesh(
                framed_map_pose(draw.feet, draw.yaw, draw.pitch, draw.item_frame_rotation),
                self.entity_light.sample(draw.feet),
            );
            if let Some((_, batch)) = batches
                .iter_mut()
                .find(|(picture, _)| Arc::ptr_eq(picture, &colors))
            {
                batch.merge(&mesh);
            } else {
                batches.push((colors, mesh));
            }
        }
        let mut prepared = Vec::with_capacity(batches.len());
        for (colors, mesh) in batches {
            let Some(gpu) = GpuModelMesh::upload(device, &mesh) else {
                let _ = note_map_skip::<()>(SITE, MapSkip::NoUpload, None);
                continue;
            };
            prepared.push((
                gpu,
                map_texture_bind_group(device, queue, &model.pipeline, colors.as_slice()),
            ));
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
        let pose = framed_map_pose(Vec3::new(4.0, 65.0, -9.0), 0.0, 0.0, 0);
        let centre = pose.transform_point3(Vec3::ZERO);
        assert!((centre.x - 4.0).abs() < 1.0e-5);
        assert!((centre.y - 65.0).abs() < 1.0e-5);
        assert!((centre.z - (-9.0 + FRAMED_MAP_LIFT)).abs() < 1.0e-5);

        // Yaw 180 faces north, so the same lift must move the other way.
        let north = framed_map_pose(Vec3::new(4.0, 65.0, -9.0), 180.0, 0.0, 0)
            .transform_point3(Vec3::ZERO);
        assert!(north.z < -9.0, "a north-facing frame lifts toward -z");

        // Yaw 90 and 270 are the discriminating inputs. Yaw 90 is west, so the
        // picture must move to `-x`.
        let west = framed_map_pose(Vec3::new(4.0, 65.0, -9.0), 90.0, 0.0, 0)
            .transform_point3(Vec3::ZERO);
        assert!(
            (west.x - (4.0 - FRAMED_MAP_LIFT)).abs() < 1.0e-5,
            "yaw 90 is west, so the picture lifts to -x; got x={} (a bare Ry(yaw) \
             would give {})",
            west.x,
            4.0 + FRAMED_MAP_LIFT
        );
        let east = framed_map_pose(Vec3::new(4.0, 65.0, -9.0), 270.0, 0.0, 0)
            .transform_point3(Vec3::ZERO);
        assert!(
            (east.x - (4.0 + FRAMED_MAP_LIFT)).abs() < 1.0e-5,
            "yaw 270 is east, so the picture lifts to +x; got x={}",
            east.x
        );

        // And the magnitude: the picture has to clear the `item_frame_map` body's
        // own back plate, which `item_frame_body_matrix` puts at local `0.4375`
        // — i.e. `0.46875 - 0.4375 = 0.03125` from the entity. A lift shorter
        // than that is *behind* the plate and draws nothing at all, which is what
        // the previous hand-picked `0.03` would now do.
        assert!(
            FRAMED_MAP_LIFT > 0.031_25,
            "the picture must clear the frame's back plate at 0.03125, got \
             {FRAMED_MAP_LIFT}"
        );

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
            let normal = framed_map_pose(Vec3::ZERO, yaw, pitch, 0)
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
            framed_map_pose(Vec3::ZERO, 180.0, 0.0, rotation).transform_vector3(Vec3::Y)
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
