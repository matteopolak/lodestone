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

/// A map's picture is drawn one block across, matching the item frame it hangs in.
const FRAMED_MAP_SCALE: f32 = 1.0;

/// How far in front of the frame's own plane the picture sits, so it wins the
/// depth test against the wall behind it without a bias.
const FRAMED_MAP_LIFT: f32 = 0.03;

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
/// frame's `yaw`/`pitch`.
///
/// Yaw is vanilla's, so `0` faces **south** (`+z`) and the rotation is about `+y`;
/// pitch tips the picture for a frame on the floor or ceiling. The quad's own local
/// `+z` is its front, so the picture faces the way the frame does with no extra
/// flip — and the lift along that same axis is what keeps it off the wall.
#[must_use]
fn framed_map_pose(feet: Vec3, yaw: f32, pitch: f32) -> Mat4 {
    let facing = Mat4::from_rotation_y(yaw.to_radians()) * Mat4::from_rotation_x(-pitch.to_radians());
    Mat4::from_translation(feet)
        * facing
        * Mat4::from_translation(Vec3::new(0.0, 0.0, FRAMED_MAP_LIFT))
        * Mat4::from_scale(Vec3::splat(FRAMED_MAP_SCALE))
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
            return None;
        }
        let model = self.model.as_ref()?;
        let colors = self.map_source.picture(None)?;
        // Full bright, as the GUI item path nails every vertex: the map is drawn
        // in its own camera-space pass with no world position to sample.
        let mesh = map_quad_mesh(held_map_pose(inverse_arm_height), ENTITY_FULLBRIGHT);
        let gpu = GpuModelMesh::upload(device, &mesh)?;
        Some((
            gpu,
            map_texture_bind_group(device, queue, &model.pipeline, &colors),
        ))
    }

    /// Every filled map hanging in an item frame this frame, as one world-space
    /// mesh plus the texture it samples.
    ///
    /// One mesh rather than one per frame-entity: every framed map resolves to the
    /// same picture while `minecraft:map_id` is undecoded (see
    /// `Sim::map_source`), so they share a bind group and concatenate into a single
    /// draw. When the id lands this becomes a group-by-id and returns one pair per
    /// distinct map.
    pub(super) fn prepare_framed_maps(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &lodestone_render::Camera,
        entities: &[EntityDraw],
    ) -> Option<(GpuModelMesh, wgpu::BindGroup)> {
        let model = self.model.as_ref()?;
        let frustum = camera.frustum();
        let mut combined = ModelMesh::default();
        for draw in entities {
            if !ITEM_FRAME_TYPES.contains(&draw.type_path.as_ref()) {
                continue;
            }
            if draw.item.as_ref().is_none_or(|id| id.path() != FILLED_MAP_ITEM) {
                continue;
            }
            // A framed map is a block across, so a one-block box around the
            // hanging point covers it however the frame is turned.
            if !frustum.intersects_aabb(draw.feet - Vec3::splat(1.0), draw.feet + Vec3::splat(1.0)) {
                continue;
            }
            combined.merge(&map_quad_mesh(
                framed_map_pose(draw.feet, draw.yaw, draw.pitch),
                self.entity_light.sample(draw.feet),
            ));
        }
        let colors = self.map_source.picture(None)?;
        let gpu = GpuModelMesh::upload(device, &combined)?;
        Some((
            gpu,
            map_texture_bind_group(device, queue, &model.pipeline, &colors),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A framed map faces the way its frame does and stands off the wall along
    /// that same axis. Yaw `0` is south (`+z`) in vanilla, so a frame at yaw `0`
    /// must lift toward `+z` — lifting the other way buries the picture in the
    /// block behind it, which reads as "the map does not draw".
    #[test]
    fn a_framed_map_lifts_along_its_own_facing() {
        let pose = framed_map_pose(Vec3::new(4.0, 65.0, -9.0), 0.0, 0.0);
        let centre = pose.transform_point3(Vec3::ZERO);
        assert!((centre.x - 4.0).abs() < 1.0e-5);
        assert!((centre.y - 65.0).abs() < 1.0e-5);
        assert!((centre.z - (-9.0 + FRAMED_MAP_LIFT)).abs() < 1.0e-5);

        // Yaw 180 faces north, so the same lift must move the other way.
        let north = framed_map_pose(Vec3::new(4.0, 65.0, -9.0), 180.0, 0.0)
            .transform_point3(Vec3::ZERO);
        assert!(north.z < -9.0, "a north-facing frame lifts toward -z");
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
