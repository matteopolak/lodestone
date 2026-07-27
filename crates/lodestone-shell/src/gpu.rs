//! GPU render state for the shell: owns the block pipeline, the atlas, a depth
//! buffer, and a per-section table of uploaded meshes + camera uniforms, and
//! draws them all in one pass.
//!
//! Every section carries its own camera-uniform buffer because the block
//! shader's uniform bundles `view_proj` *with* the section's world origin (the
//! packed vertex only stores a 0..16 local position). Each frame we rewrite all
//! section uniforms with the current `view_proj` *before* opening the render
//! pass — buffers can't be written mid-pass — then issue one draw per section.

use std::collections::HashMap;

use lodestone_render::{
    BlockAtlas, BlockPipeline, Camera, CameraUniform, DEPTH_FORMAT, DepthBuffer, EntityModelSet,
    EntityPipeline, GpuAtlas, GpuEntityModel, GpuMesh, Mesh,
    block::{camera_buffer, sprite_uv_buffer},
    plan_entities, upload_instances,
    vertex::vram_bytes,
};

use crate::entities::EntityDraw;
use crate::mesher::SectionKey;

/// The 12 edges of a unit cube as pairs of corner indices (line list).
const CUBE_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (1, 3),
    (3, 2),
    (2, 0), // bottom face
    (4, 5),
    (5, 7),
    (7, 6),
    (6, 4), // top face
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7), // verticals
];

/// Draws a black wireframe box around the targeted block. Its own pipeline
/// (line-list topology, `LessEqual` depth, no depth write, alpha-blended) so it
/// reads clearly over terrain without a second pass or z-fighting.
#[derive(Debug)]
struct OutlineRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    vertices: wgpu::Buffer,
}

impl OutlineRenderer {
    fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-outline-shader"),
            source: wgpu::ShaderSource::Wgsl(
                r"
struct Uniform { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> u: Uniform;

@vertex
fn vs_main(@location(0) pos: vec3<f32>) -> @builtin(position) vec4<f32> {
    return u.view_proj * vec4<f32>(pos, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 0.6);
}
"
                .into(),
            ),
        });

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-outline-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-outline-uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-outline-bg"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });

        // 24 vertices (12 edges × 2), 3 f32 each.
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-outline-vertices"),
            size: (24 * 3 * std::mem::size_of::<f32>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-outline-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-outline-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (3 * std::mem::size_of::<f32>()) as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    }],
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
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            bind_group,
            uniform,
            vertices,
        }
    }

    /// Upload the view-projection and the box vertices for `block` (slightly
    /// expanded so the lines sit just outside the block faces). Must be called
    /// before the render pass begins — buffers can't be written mid-pass.
    fn prepare(&self, queue: &wgpu::Queue, view_proj: &[[f32; 4]; 4], block: [i32; 3]) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(view_proj));

        const PAD: f32 = 0.002;
        let lo = [
            block[0] as f32 - PAD,
            block[1] as f32 - PAD,
            block[2] as f32 - PAD,
        ];
        let hi = [
            block[0] as f32 + 1.0 + PAD,
            block[1] as f32 + 1.0 + PAD,
            block[2] as f32 + 1.0 + PAD,
        ];
        // Corner index bit layout: x = bit0, y = bit1, z = bit2.
        let corner = |i: usize| {
            [
                if i & 1 == 0 { lo[0] } else { hi[0] },
                if i & 2 == 0 { lo[1] } else { hi[1] },
                if i & 4 == 0 { lo[2] } else { hi[2] },
            ]
        };
        let mut verts = [0f32; 24 * 3];
        for (e, &(a, b)) in CUBE_EDGES.iter().enumerate() {
            let ca = corner(a);
            let cb = corner(b);
            let base = e * 6;
            verts[base..base + 3].copy_from_slice(&ca);
            verts[base + 3..base + 6].copy_from_slice(&cb);
        }
        queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&verts));
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..24, 0..1);
    }
}

/// Aggregate numbers for one rendered frame, surfaced to the debug overlay.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderStats {
    /// Sections with non-empty geometry drawn this frame.
    pub sections_drawn: usize,
    /// Total merged quads across all drawn sections.
    pub total_quads: usize,
    /// Draw calls issued (one per non-empty section).
    pub draw_calls: usize,
    /// Approximate mesh VRAM in bytes.
    pub vram_bytes: usize,
    /// Entity instances drawn this frame (post-frustum-cull).
    pub entities_drawn: usize,
    /// Entity instances frustum-culled this frame.
    pub entities_culled: usize,
}

#[derive(Debug)]
struct SectionGpu {
    mesh: GpuMesh,
    quad_count: usize,
    origin: [f32; 3],
    cam_buffer: wgpu::Buffer,
    cam_bind_group: wgpu::BindGroup,
}

/// GPU resources for the entity pass: the instanced pipeline, one uploaded mesh
/// per model type, a per-model synthetic texture bind group, and a persistent
/// camera uniform rewritten each frame. Owns the version-free
/// [`EntityModelSet`] so it can resolve a live entity type into a renderable
/// instance without the shell naming a mob model directly.
///
/// Textures are **synthetic solid colours**, one per model type, matching the
/// shell's existing block atlas (which is also procedurally coloured, not
/// resource-pack art). Real mob-skin loading through the resource pack is a
/// follow-up shared with the block-atlas work, not something this pass fakes.
#[derive(Debug)]
struct EntityRenderer {
    pipeline: EntityPipeline,
    models: EntityModelSet,
    gpu_models: HashMap<&'static str, GpuEntityModel>,
    textures: HashMap<&'static str, wgpu::BindGroup>,
    cam_buffer: wgpu::Buffer,
    cam_bind_group: wgpu::BindGroup,
}

impl EntityRenderer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, color_format: wgpu::TextureFormat) -> Self {
        let pipeline = EntityPipeline::new(device, color_format);
        let models = EntityModelSet::load();

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lodestone-entity-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let mut gpu_models = HashMap::new();
        let mut textures = HashMap::new();
        for (name, mesh) in models.iter() {
            if let Some(gpu) = GpuEntityModel::upload(device, mesh) {
                gpu_models.insert(name, gpu);
            }
            let (view, _tex) = synthetic_entity_texture(device, queue, name);
            let bg = pipeline.texture_bind_group(device, &view, &sampler);
            textures.insert(name, bg);
        }

        // A persistent camera uniform, rewritten every frame before the pass.
        let cam_buffer = camera_buffer(
            device,
            CameraUniform {
                view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
                section_origin: [0.0, 0.0, 0.0, 0.0],
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
}

/// Build a 2×2 solid-colour RGBA texture for one entity model, tinted
/// deterministically from the model name so distinct mob types are
/// distinguishable on screen. Opaque, so the shader's alpha cutout keeps every
/// texel. Returns the view and the texture (kept alive by the caller).
fn synthetic_entity_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    model_name: &str,
) -> (wgpu::TextureView, wgpu::Texture) {
    let [r, g, b] = model_tint(model_name);
    const N: u32 = 2;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lodestone-entity-synthetic-sheet"),
        size: wgpu::Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let pixels: Vec<u8> = (0..N * N).flat_map(|_| [r, g, b, 255]).collect();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(N * 4),
            rows_per_image: Some(N),
        },
        wgpu::Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (view, texture)
}

/// A deterministic, reasonably-separated RGB tint from a model name (FNV-1a over
/// the bytes, spread across channels). Kept bright (each channel ≥ 80) so mobs
/// read against both sky and terrain.
fn model_tint(name: &str) -> [u8; 3] {
    let mut h: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        h ^= u32::from(byte);
        h = h.wrapping_mul(0x0100_0193);
    }
    let chan = |shift: u32| -> u8 { 80 + ((h >> shift) as u8 % 176) };
    [chan(0), chan(8), chan(16)]
}

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
    outline: OutlineRenderer,
    entities: EntityRenderer,
    clear: wgpu::Color,
}

impl RenderState {
    /// Build the pipeline and atlas for a target of `color_format` and size.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        vanilla: Option<&BlockAtlas>,
    ) -> Self {
        let pipeline = BlockPipeline::new(device, color_format);

        // The live world binds the real stitched vanilla atlas; the demo world
        // binds the procedural colour atlas. The two are disjoint id spaces, so
        // the choice is made once here and mirrors the mesh classifier.
        let (atlas, uv_buffer) = match vanilla {
            Some(va) => {
                let atlas = GpuAtlas::from_atlas(device, queue, va.atlas());
                let uv_buffer = sprite_uv_buffer(device, va.uv_table());
                (atlas, uv_buffer)
            }
            None => {
                let atlas_data = crate::blocks::build_atlas();
                let atlas = GpuAtlas::from_rgba(
                    device,
                    queue,
                    atlas_data.width,
                    atlas_data.height,
                    &atlas_data.rgba,
                    &atlas_data.sprite_rects,
                );
                let uv_buffer = sprite_uv_buffer(device, &atlas_data.uv_table);
                (atlas, uv_buffer)
            }
        };
        let atlas_bind_group = pipeline.atlas_bind_group(device, &atlas, &uv_buffer);
        let depth = DepthBuffer::new(device, width.max(1), height.max(1));
        let outline = OutlineRenderer::new(device, color_format);
        let entities = EntityRenderer::new(device, queue, color_format);

        Self {
            pipeline,
            atlas,
            uv_buffer,
            atlas_bind_group,
            depth,
            sections: HashMap::new(),
            outline,
            entities,
            // A calm sky blue, so terrain reads clearly against it.
            clear: wgpu::Color {
                r: 0.53,
                g: 0.71,
                b: 0.92,
                a: 1.0,
            },
        }
    }

    /// Recreate the depth buffer to match a resized target.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if self.depth.width != width || self.depth.height != height {
            self.depth = DepthBuffer::new(device, width, height);
        }
    }

    /// Upload (or replace) a section's mesh. An empty mesh removes the section.
    pub fn upload_section(&mut self, device: &wgpu::Device, key: SectionKey, mesh: &Mesh) {
        match GpuMesh::upload(device, mesh) {
            None => {
                self.sections.remove(&key);
            }
            Some(gpu_mesh) => {
                let origin = key.origin();
                let origin_f = [origin[0] as f32, origin[1] as f32, origin[2] as f32];
                // Placeholder uniform; overwritten every frame with the live camera.
                let cam_buffer = camera_buffer(
                    device,
                    CameraUniform {
                        view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
                        section_origin: [origin_f[0], origin_f[1], origin_f[2], 0.0],
                    },
                );
                let cam_bind_group = self.pipeline.camera_bind_group(device, &cam_buffer);
                self.sections.insert(
                    key,
                    SectionGpu {
                        mesh: gpu_mesh,
                        quad_count: mesh.quad_count(),
                        origin: origin_f,
                        cam_buffer,
                        cam_bind_group,
                    },
                );
            }
        }
    }

    /// Remove a section (e.g. an unloaded chunk).
    pub fn remove_section(&mut self, key: &SectionKey) {
        self.sections.remove(key);
    }

    /// Number of uploaded (non-empty) sections.
    #[must_use]
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }

    /// Total merged quads currently resident on the GPU.
    #[must_use]
    pub fn total_quads(&self) -> usize {
        self.sections.values().map(|s| s.quad_count).sum()
    }

    /// Render every section into `view` using `camera`. Writes all section
    /// camera uniforms first, then draws. If `outline` names a block, a
    /// wireframe box is drawn around it after the terrain.
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        camera: &Camera,
        outline: Option<[i32; 3]>,
        entities: &[EntityDraw],
    ) -> RenderStats {
        let view_proj = camera.view_projection().to_cols_array_2d();

        // Rewrite each section's uniform with the current view-projection.
        for section in self.sections.values() {
            let uniform = CameraUniform {
                view_proj,
                section_origin: [section.origin[0], section.origin[1], section.origin[2], 0.0],
            };
            queue.write_buffer(&section.cam_buffer, 0, bytemuck::bytes_of(&uniform));
        }

        // Outline vertices/uniform must be written before the pass opens.
        if let Some(block) = outline {
            self.outline.prepare(queue, &view_proj, block);
        }

        // Resolve, frustum-cull and upload entity instances *before* the pass —
        // buffers can't be created mid-pass, and the entity camera uniform (no
        // section origin; the world position lives in each instance matrix) must
        // be written first too.
        let mut stats = RenderStats::default();
        let entity_batches = self.prepare_entities(device, queue, camera, entities, &mut stats);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("block pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline.pipeline);
            pass.set_bind_group(1, &self.atlas_bind_group, &[]);
            for section in self.sections.values() {
                pass.set_bind_group(0, &section.cam_bind_group, &[]);
                pass.set_vertex_buffer(0, section.mesh.vertices.slice(..));
                pass.set_index_buffer(section.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..section.mesh.index_count, 0, 0..1);
                stats.sections_drawn += 1;
                stats.draw_calls += 1;
                stats.total_quads += section.quad_count;
            }

            // Entities share the terrain depth buffer (depth test + write on, so
            // a mob behind a wall is correctly occluded and vice versa), drawn
            // after terrain in the same pass so no second clear touches depth.
            if !entity_batches.is_empty() {
                pass.set_pipeline(&self.entities.pipeline.pipeline);
                pass.set_bind_group(0, &self.entities.cam_bind_group, &[]);
                for batch in &entity_batches {
                    let Some(model) = self.entities.gpu_models.get(batch.model) else {
                        continue;
                    };
                    let Some(texture) = self.entities.textures.get(batch.model) else {
                        continue;
                    };
                    pass.set_bind_group(1, texture, &[]);
                    pass.set_vertex_buffer(0, model.vertices.slice(..));
                    pass.set_vertex_buffer(1, batch.instances.slice(..));
                    pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..model.index_count, 0, 0..batch.count);
                    stats.draw_calls += 1;
                }
            }

            if outline.is_some() {
                self.outline.draw(&mut pass);
            }
        }
        queue.submit(std::iter::once(encoder.finish()));

        stats.vram_bytes = vram_bytes(stats.total_quads);
        stats
    }

    /// Resolve each interpolated entity into a renderable instance, frustum-cull
    /// and group them by model, upload one instance buffer per surviving model,
    /// and record draw/cull counts. Runs before the render pass so every GPU
    /// buffer it creates outlives the pass that reads it.
    fn prepare_entities(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<EntityDrawBatch> {
        if entities.is_empty() {
            return Vec::new();
        }

        // Rewrite the entity camera uniform (world position lives per-instance,
        // so the section origin stays zero).
        queue.write_buffer(
            &self.entities.cam_buffer,
            0,
            bytemuck::bytes_of(&CameraUniform {
                view_proj: camera.view_projection().to_cols_array_2d(),
                section_origin: [0.0, 0.0, 0.0, 0.0],
            }),
        );

        let instances: Vec<_> = entities
            .iter()
            .filter_map(|e| {
                self.entities
                    .models
                    .resolve(&e.type_path, e.feet, e.yaw, e.scale)
            })
            .collect();

        let frame = plan_entities(&instances, &camera.frustum());
        stats.entities_drawn = frame.stats.drawn;
        stats.entities_culled = frame.stats.culled_frustum;

        frame
            .batches
            .iter()
            .filter_map(|batch| {
                let count = u32::try_from(batch.transforms.len()).unwrap_or(u32::MAX);
                upload_instances(device, &batch.transforms).map(|instances| EntityDrawBatch {
                    model: batch.model,
                    count,
                    instances,
                })
            })
            .collect()
    }
}

/// One model type's uploaded instance buffer for a frame.
#[derive(Debug)]
struct EntityDrawBatch {
    model: &'static str,
    count: u32,
    instances: wgpu::Buffer,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_render::{HeadlessTarget, RenderTarget};

    /// Headless GPU test: generate a world, mesh + upload every section, render
    /// one frame, and read pixels back to prove terrain (not just sky) drew.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn world_renders_terrain_with_pixel_readback() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let mut target = HeadlessTarget::new(device, w, h, format);

        let world = crate::worldgen::generate(2);
        let classifier = crate::blocks::DemoClassifier;
        let mut state = RenderState::new(device, queue, format, w, h, None);

        let mut total_quads = 0usize;
        let mut sections = 0usize;
        let radius = 2;
        for cz in -radius..=radius {
            for cx in -radius..=radius {
                for si in 0..crate::worldgen::SECTION_COUNT {
                    let key = SectionKey {
                        cx,
                        cz,
                        si,
                        min_y: crate::worldgen::MIN_Y,
                    };
                    if let Some(snap) = crate::mesher::snapshot_section(&world, key) {
                        let mesh = crate::mesher::mesh_snapshot(&snap, &classifier);
                        total_quads += mesh.quad_count();
                        sections += 1;
                        state.upload_section(device, key, &mesh);
                    }
                }
            }
        }
        assert!(sections > 0, "some sections should have meshed");

        // Camera above the origin, backed off to the north, looking south and
        // angled down over the terrain.
        let feet = crate::worldgen::spawn_feet();
        let camera = Camera {
            position: glam::Vec3::new(feet[0] as f32, feet[1] as f32 + 6.0, feet[2] as f32 - 18.0),
            yaw: 0.0,
            pitch: 22.0,
            fov_y_degrees: 70.0,
            aspect: w as f32 / h as f32,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };

        let start = std::time::Instant::now();
        let frame = target.acquire().expect("headless acquire");
        // Draw with a block outline enabled to exercise the outline pipeline.
        let stats = state.render(
            device,
            queue,
            frame.view(),
            &camera,
            Some([0, feet[1] as i32, 0]),
            &[],
        );
        let pixels = target.read_texels(device, queue);
        let frame_ms = start.elapsed().as_secs_f64() * 1000.0;

        // Sky clear colour ≈ (135,181,235). Count pixels that clearly differ:
        // terrain sprites are green/brown/grey, far from sky blue.
        let sky = [135u8, 181, 235];
        let mut terrain_px = 0usize;
        for px in pixels.chunks_exact(4) {
            let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
                + (i32::from(px[1]) - i32::from(sky[1])).abs()
                + (i32::from(px[2]) - i32::from(sky[2])).abs();
            if d > 60 {
                terrain_px += 1;
            }
        }
        let coverage = terrain_px as f64 / (w * h) as f64;
        let sky_px = (w * h) as usize - terrain_px;
        let sky_coverage = sky_px as f64 / (w * h) as f64;

        eprintln!("=== shell world render (headless) ===");
        eprintln!("sections meshed   = {sections}");
        eprintln!("sections drawn    = {}", stats.sections_drawn);
        eprintln!("quads (meshed)    = {total_quads}");
        eprintln!("quads (drawn)     = {}", stats.total_quads);
        eprintln!("draw calls        = {}", stats.draw_calls);
        eprintln!("mesh VRAM (bytes) = {}", stats.vram_bytes);
        eprintln!("terrain coverage  = {:.1}%", coverage * 100.0);
        eprintln!("sky coverage      = {:.1}%", sky_coverage * 100.0);
        eprintln!("frame time (ms)   = {frame_ms:.3}");

        // Two-sided on purpose: a blank/all-sky frame fails the terrain guard,
        // and an all-terrain frame (camera stuck inside a block, full-screen
        // fog, a broken clear) fails the sky guard. "Correctly rendered nothing"
        // and "rendered one solid colour" must both be distinguishable from a
        // real horizon.
        assert!(
            coverage > 0.05,
            "expected visible terrain, only {:.1}% non-sky pixels",
            coverage * 100.0
        );
        assert!(
            sky_coverage > 0.05,
            "expected visible sky above the horizon, only {:.1}% sky pixels — \
             frame may be a solid fill rather than a rendered scene",
            sky_coverage * 100.0
        );
    }

    /// Headless proof that the block outline actually draws distinct pixels:
    /// render the same scene twice — once without an outline, once with one
    /// around a block squarely in view — and confirm the outline adds a modest
    /// number of near-black pixels where terrain used to be. Pixel readback is
    /// the project's evidence standard for "did it really render?".
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn block_outline_draws_visible_edges() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let mut target = HeadlessTarget::new(device, w, h, format);

        let world = crate::worldgen::generate(2);
        let classifier = crate::blocks::DemoClassifier;
        let mut state = RenderState::new(device, queue, format, w, h, None);
        for cz in -2..=2 {
            for cx in -2..=2 {
                for si in 0..crate::worldgen::SECTION_COUNT {
                    let key = SectionKey {
                        cx,
                        cz,
                        si,
                        min_y: crate::worldgen::MIN_Y,
                    };
                    if let Some(snap) = crate::mesher::snapshot_section(&world, key) {
                        let mesh = crate::mesher::mesh_snapshot(&snap, &classifier);
                        state.upload_section(device, key, &mesh);
                    }
                }
            }
        }

        // Outline a cube floating in the air with open sky behind it, so its
        // edges are crisp black lines on blue and can't be confused with dark
        // terrain. The outline is a pure wireframe at world coords — it draws
        // whether or not a block occupies the cell.
        let target_block = [0i32, crate::worldgen::surface_height(0, 0) + 12, 6];
        let camera = Camera {
            position: glam::Vec3::new(0.5, target_block[1] as f32 + 0.5, -2.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_y_degrees: 70.0,
            aspect: w as f32 / h as f32,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };

        let frame = target.acquire().expect("acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        let plain = target.read_texels(device, queue);

        let frame = target.acquire().expect("acquire");
        state.render(
            device,
            queue,
            frame.view(),
            &camera,
            Some(target_block),
            &[],
        );
        let outlined = target.read_texels(device, queue);

        // The only thing that changed between the two frames is the outline, so
        // count pixels whose colour moved. A blended 0.6-alpha black line darkens
        // whatever it covers; we detect the change directly rather than guessing
        // its final colour.
        let mut changed = 0usize;
        let mut darkened = 0usize;
        for (a, b) in plain.chunks_exact(4).zip(outlined.chunks_exact(4)) {
            let d = (i32::from(a[0]) - i32::from(b[0])).abs()
                + (i32::from(a[1]) - i32::from(b[1])).abs()
                + (i32::from(a[2]) - i32::from(b[2])).abs();
            if d > 20 {
                changed += 1;
                // The outline can only darken (black over colour).
                if i32::from(b[0]) + i32::from(b[1]) + i32::from(b[2])
                    < i32::from(a[0]) + i32::from(a[1]) + i32::from(a[2])
                {
                    darkened += 1;
                }
            }
        }

        eprintln!("=== outline pixel readback ===");
        eprintln!("pixels changed by outline = {changed}");
        eprintln!("of which darkened         = {darkened}");

        assert!(
            changed > 50,
            "outline should visibly change the frame, only {changed} px moved"
        );
        assert_eq!(
            changed, darkened,
            "an outline only darkens pixels it covers"
        );
    }

    /// Headless proof that HUD **text actually rasterizes to pixels**, not just
    /// that geometry is generated. Renders two frames over the same known clear
    /// colour: an empty HUD (no crosshair/debug/chat) and one carrying chat
    /// lines plus a prompt. The empty frame must stay essentially background;
    /// the chat frame must light a substantial run of glyph pixels. Two-sided on
    /// purpose — a stray clear or wrong `LoadOp` lights the empty frame, and a
    /// no-op text path leaves the chat frame dark, so neither degenerate outcome
    /// can pass.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn hud_chat_text_rasterizes_to_pixels() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let clear = wgpu::Color {
            r: 0.04,
            g: 0.04,
            b: 0.08,
            a: 1.0,
        };
        let bg = [10i32, 10, 20];

        // Clear a fresh target to `clear`, render one HUD frame over it (the HUD
        // draws with `LoadOp::Load`), and count pixels far from the background.
        let lit_pixels = |frame: &crate::hud::HudFrame| -> usize {
            let mut target = HeadlessTarget::new(device, w, h, format);
            let mut hud = crate::hud::HudRenderer::new(device, format);
            let ht_frame = target.acquire().expect("headless acquire");
            {
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("clear"),
                });
                {
                    let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("hud-clear"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: ht_frame.view(),
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(clear),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                }
                queue.submit(std::iter::once(enc.finish()));
            }
            hud.render(device, queue, ht_frame.view(), frame, w, h);
            let pixels = target.read_texels(device, queue);
            pixels
                .chunks_exact(4)
                .filter(|px| {
                    let d = (i32::from(px[0]) - bg[0]).abs()
                        + (i32::from(px[1]) - bg[1]).abs()
                        + (i32::from(px[2]) - bg[2]).abs();
                    d > 40
                })
                .count()
        };

        let stats = crate::hud::DebugStats::default();
        let empty_frame = crate::hud::HudFrame {
            crosshair: false,
            show_debug: false,
            ..crate::hud::HudFrame::new(&stats)
        };
        let empty_lit = lit_pixels(&empty_frame);

        let chat = [("<Steve> hello world", 0.0_f32), ("<Alex> hi there", 0.0)];
        let chat_frame = crate::hud::HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat,
            chat_input: Some("typing a message"),
            ..crate::hud::HudFrame::new(&stats)
        };
        let chat_lit = lit_pixels(&chat_frame);

        eprintln!("=== hud chat rasterization ===");
        eprintln!("empty HUD lit px = {empty_lit}");
        eprintln!("chat  HUD lit px = {chat_lit}");

        assert!(
            empty_lit < 20,
            "an empty HUD should read as background, but {empty_lit} px were lit — \
             a stray clear or wrong LoadOp is drawing something"
        );
        assert!(
            chat_lit > 200,
            "chat text should rasterize a substantial run of glyph pixels, only {chat_lit} lit — \
             the text path may be a no-op"
        );
    }

    /// The scoreboard sidebar must actually reach pixels. Same two-sided shape as
    /// the chat proof: an empty HUD stays background; a sidebar with two scored
    /// rows lights a substantial run of glyph pixels. A no-op fold, a panel drawn
    /// with no text, or a wrong `LoadOp` each fails one side.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn hud_sidebar_rasterizes_to_pixels() {
        use crate::overlay::{Sidebar, SidebarLine};
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let clear = wgpu::Color {
            r: 0.04,
            g: 0.04,
            b: 0.08,
            a: 1.0,
        };
        let bg = [10i32, 10, 20];

        let lit_pixels = |frame: &crate::hud::HudFrame| -> usize {
            let mut target = HeadlessTarget::new(device, w, h, format);
            let mut hud = crate::hud::HudRenderer::new(device, format);
            let ht_frame = target.acquire().expect("headless acquire");
            {
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("clear"),
                });
                {
                    let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("hud-clear"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: ht_frame.view(),
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(clear),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                }
                queue.submit(std::iter::once(enc.finish()));
            }
            hud.render(device, queue, ht_frame.view(), frame, w, h);
            let pixels = target.read_texels(device, queue);
            pixels
                .chunks_exact(4)
                .filter(|px| {
                    let d = (i32::from(px[0]) - bg[0]).abs()
                        + (i32::from(px[1]) - bg[1]).abs()
                        + (i32::from(px[2]) - bg[2]).abs();
                    d > 40
                })
                .count()
        };

        let stats = crate::hud::DebugStats::default();
        let empty_frame = crate::hud::HudFrame {
            crosshair: false,
            show_debug: false,
            ..crate::hud::HudFrame::new(&stats)
        };
        let empty_lit = lit_pixels(&empty_frame);

        let side = Sidebar {
            title: "Objectives".into(),
            lines: vec![
                SidebarLine {
                    label: "Kills".into(),
                    score: "7".into(),
                },
                SidebarLine {
                    label: "Deaths".into(),
                    score: "2".into(),
                },
            ],
        };
        let side_frame = crate::hud::HudFrame {
            crosshair: false,
            show_debug: false,
            sidebar: Some(&side),
            ..crate::hud::HudFrame::new(&stats)
        };
        let side_lit = lit_pixels(&side_frame);

        eprintln!("=== hud sidebar rasterization ===");
        eprintln!("empty   HUD lit px = {empty_lit}");
        eprintln!("sidebar HUD lit px = {side_lit}");

        assert!(
            empty_lit < 20,
            "an empty HUD should read as background, but {empty_lit} px were lit"
        );
        assert!(
            side_lit > 200,
            "the sidebar title, labels and scores should rasterize a substantial run \
             of glyph pixels, only {side_lit} lit — the fold or text path may be a no-op"
        );
    }

    /// Headless GPU test: render a single entity (no terrain) through the real
    /// [`RenderState::render`] path — the same call the live frame loop uses —
    /// and read pixels back to prove a mob reaches the screen. This is the
    /// shell-level analogue of `lodestone-render`'s `entity_gate`, but it drives
    /// the *shell's* wiring: `EntityDraw` → resolve → `plan_entities` → upload →
    /// instanced draw, sharing the terrain depth buffer.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn entity_renders_to_pixels_through_shell_path() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter), don't 'skip' — a silent pass \
             here would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let mut target = HeadlessTarget::new(device, w, h, format);

        let state = RenderState::new(device, queue, format, w, h, None);

        // A pig standing just in front of the camera, which looks south (+Z,
        // yaw 0) at eye level with the pig's body — mirrors the render-crate
        // gate's geometry so a regression there shows up here too.
        let pig_feet = glam::Vec3::new(0.0, 0.0, 4.0);
        let camera = Camera {
            position: glam::Vec3::new(0.0, 0.9, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_y_degrees: 60.0,
            aspect: w as f32 / h as f32,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };

        let draws = vec![
            EntityDraw {
                type_path: "pig".to_owned(),
                feet: pig_feet,
                yaw: 0.0,
                scale: 1.0,
            },
            // A second pig behind the camera so frustum culling has something
            // real to remove — the anti-vacuity guard on the cull path.
            EntityDraw {
                type_path: "pig".to_owned(),
                feet: glam::Vec3::new(0.0, 0.0, -12.0),
                yaw: 0.0,
                scale: 1.0,
            },
        ];

        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &draws);
        let pixels = target.read_texels(device, queue);

        assert_eq!(
            stats.entities_drawn, 1,
            "exactly the front pig should draw; the one behind the camera must cull \
             (drawn={}, culled={})",
            stats.entities_drawn, stats.entities_culled
        );
        assert!(
            stats.entities_culled >= 1,
            "the pig behind the camera should have been frustum-culled, but culled={}",
            stats.entities_culled
        );

        // The synthetic pig texture is a solid tint; count pixels that clearly
        // differ from the sky clear colour, and confirm they cluster in the
        // centre (where the pig is) rather than smeared across the frame.
        let sky = [135u8, 181, 235];
        let is_mob = |px: &[u8]| -> bool {
            let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
                + (i32::from(px[1]) - i32::from(sky[1])).abs()
                + (i32::from(px[2]) - i32::from(sky[2])).abs();
            d > 60
        };

        let mut mob_px = 0usize;
        let mut centre_px = 0usize;
        let mut corner_px = 0usize;
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            let x = (i as u32) % w;
            let y = (i as u32) / w;
            if is_mob(px) {
                mob_px += 1;
            }
            let cx = x >= w / 4 && x < 3 * w / 4;
            let cy = y >= h / 4 && y < 3 * h / 4;
            if cx && cy && is_mob(px) {
                centre_px += 1;
            }
            let corner = (x < w / 8 || x >= 7 * w / 8) && (y < h / 8 || y >= 7 * h / 8);
            if corner && is_mob(px) {
                corner_px += 1;
            }
        }
        let coverage = mob_px as f64 / (w * h) as f64;

        eprintln!("=== shell entity render (headless) ===");
        eprintln!("entities drawn  = {}", stats.entities_drawn);
        eprintln!("entities culled = {}", stats.entities_culled);
        eprintln!("mob coverage    = {:.2}%", coverage * 100.0);
        eprintln!("centre mob px   = {centre_px}");
        eprintln!("corner mob px   = {corner_px}");

        // Two-sided: the pig must reach pixels (not a blank frame) but not fill
        // the screen (a broken clear or a mob glued to the camera), and it must
        // be centred (the corners stay sky).
        assert!(
            mob_px > 200,
            "expected the pig to reach pixels, only {mob_px} non-sky px ({:.2}%)",
            coverage * 100.0
        );
        assert!(
            coverage < 0.6,
            "the pig should not fill the frame ({:.1}% non-sky) — a mob glued to the \
             near plane or a broken clear",
            coverage * 100.0
        );
        assert!(
            centre_px > 100,
            "the pig should sit in the centre of the frame, only {centre_px} centre px"
        );
        assert_eq!(
            corner_px, 0,
            "the frame corners should stay sky, but {corner_px} corner px read as mob"
        );
    }
}
