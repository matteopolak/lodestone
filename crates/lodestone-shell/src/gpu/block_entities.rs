//! GPU resources for the block-entity pass: the chest rigs vanilla's
//! `BlockEntityRenderer`s draw and no block model covers (issue #23).
//!
//! # It reuses the entity pipeline on purpose
//!
//! There is no new pipeline here. A chest is an alpha-cutout, double-sided,
//! depth-tested-and-written, per-part-instanced cuboid mesh with one texture
//! sheet and a per-instance lightmap sample — which is precisely
//! [`EntityPipeline`]'s contract. Building a second pipeline would duplicate the
//! entity shader (including its gamma-space shade/tint multiply and its
//! `Rgba8UnormSrgb` requirement) with nothing to gain and a second place for the
//! two to drift.
//!
//! It also keeps this pass **off** the model shader's bind-group budget.
//! `wgpu`'s default `max_bind_groups` is 4 and the model shader already spends
//! all four; `EntityPipeline` spends exactly two (camera+fog / texture), and this
//! module adds a *second bind group over the existing group-0 layout* rather than
//! a fifth group — the same trick `EntityRenderer::hand_cam_bind_group` uses and
//! for the same reason. A fifth group would compile on an 8-group M5 and crash at
//! startup for everybody at the floor.
//!
//! Its own group-0 buffer, rather than sharing `EntityRenderer::cam_buffer`, is
//! not defensive: both are rewritten once per frame with the same view-projection
//! and fog, so sharing would work today — and would silently break the first time
//! either pass wanted a different matrix, exactly as the first-person hand pass
//! already needs. One buffer per pass is 128 bytes.
//!
//! # Textures are keyed by *sheet*, not by model
//!
//! A trapped chest and a plain chest share the single-chest **mesh** and differ
//! only in bind group, so the map is keyed by texture stem
//! (`entity/chest/normal_left`) and the batch key is `(model, texture)`. Keying
//! textures by model name — the way `EntityRenderer::textures` correctly does,
//! because a mob's sheet *is* determined by its model — would draw every trapped
//! chest with the plain sheet.
//!
//! Missing sheets **draw nothing** rather than falling back to a synthetic
//! colour. That is the same asymmetry `EntityRenderer::armour_textures`
//! documents: a flat-magenta mob reads as "this sheet is missing", but a
//! flat-magenta chest-shaped box reads as a renderer bug, and the offline demo
//! world has no chests to draw in the first place.
//!
//! # How to change it
//!
//! * Adding a block-entity type: add its model to `lodestone-assets`, its sheets
//!   to [`chest_texture_stems`]'s equivalent, and a prepare arm. The pipeline,
//!   the sampler, the uniform and the draw loop are already generic over
//!   `(model, texture)`.
//! * The draw is issued **inside the block pass**, after the entity/armour/wool
//!   layers and before translucent water — see the call site in `gpu.rs` for why
//!   that position and not another.

use std::collections::HashMap;

use lodestone_render::{
    BlockEntityModelSet, CameraUniform, EntityCameraUniform, EntityPipeline, GpuEntityModel,
    block_entity_texture_stems, entity_camera_buffer, fog::FogUniform,
};

/// GPU resources for the block-entity pass: one uploaded mesh per model, one
/// texture bind group per sheet, and a persistent group-0 uniform rewritten each
/// frame.
#[derive(Debug)]
pub(super) struct BlockEntityRenderer {
    /// Borrowed contract, owned resources: the pipeline is `EntityPipeline`'s,
    /// but this pass keeps its own instance of it so the two never contend for
    /// the group-0 buffer. See the module doc.
    pub(super) pipeline: EntityPipeline,
    pub(super) models: BlockEntityModelSet,
    pub(super) gpu_models: HashMap<&'static str, GpuEntityModel>,
    /// Keyed by *texture stem*, not model name — see the module doc.
    pub(super) textures: HashMap<&'static str, wgpu::BindGroup>,
    pub(super) cam_buffer: wgpu::Buffer,
    pub(super) cam_bind_group: wgpu::BindGroup,
}

impl BlockEntityRenderer {
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
    ) -> Self {
        let pipeline = EntityPipeline::new(device, color_format);
        let models = BlockEntityModelSet::load();

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lodestone-block-entity-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let mut gpu_models = HashMap::new();
        for (name, mesh) in models.iter() {
            // `GpuEntityModel::upload` takes an `EntityMesh`; a block-entity mesh
            // is the same three buffers plus a different part-transform rule, so
            // it uploads through `upload_parts` rather than a second copy of the
            // buffer-creation code.
            if let Some(gpu) = GpuEntityModel::upload_parts(
                device,
                &mesh.vertices,
                &mesh.indices,
                mesh.parts.clone(),
            ) {
                gpu_models.insert(name, gpu);
            }
        }

        let real = crate::resources::load_block_entity_textures();
        let mut textures = HashMap::new();
        for stem in block_entity_texture_stems() {
            let Some(img) = real.get(stem) else {
                // Fail-open and *silent at draw time*: the warning was already
                // logged by the loader, and a chest with no sheet draws nothing
                // rather than a magenta box. See the module doc.
                continue;
            };
            let view = super::entities::entity_texture_from_image(device, queue, img);
            textures.insert(stem, pipeline.texture_bind_group(device, &view, &sampler));
        }

        let cam_buffer = entity_camera_buffer(
            device,
            EntityCameraUniform {
                camera: CameraUniform {
                    view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
                    section_origin: [0.0, 0.0, 0.0, 0.0],
                },
                fog: FogUniform::disabled(),
            },
        );
        let cam_bind_group = pipeline.camera_bind_group(device, &cam_buffer);

        Self {
            pipeline,
            models,
            gpu_models,
            textures,
            cam_buffer,
            cam_bind_group,
        }
    }

    /// How many sheets loaded — the counter that separates "no chests in view"
    /// from "no pack, so nothing can ever draw".
    pub(super) fn sheet_count(&self) -> usize {
        self.textures.len()
    }
}

/// One uploaded batch, ready to draw: the model to bind, the sheet to bind, one
/// instance buffer per part, and the instance count.
#[derive(Debug)]
pub(super) struct BlockEntityDrawBatch {
    pub(super) model: &'static str,
    pub(super) texture: &'static str,
    pub(super) count: u32,
    pub(super) parts: Vec<Option<wgpu::Buffer>>,
}
