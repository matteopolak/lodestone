//! The world-space plugin-billboard pass — the render half of issue #161
//! (`docs/plugin-api.md`'s `ExtractSet::Debug` billboard channel), and the
//! polled source that feeds it. `lodestone_ecs::plugin_draw` (the ECS/API
//! half) landed with no render consumer; this module is the brokered hunk
//! that document names, mirroring [`super::debug_lines`] file-for-file the
//! way its own doc asks for.
use lodestone_render::DEPTH_FORMAT;

/// One instance of a camera-facing world-space billboard quad — the render
/// half of [`lodestone_ecs::PluginBillboard`]. A separate, `bytemuck`-friendly
/// type rather than reusing the ECS one directly, for the identical reason
/// [`super::debug_lines::DebugLineVertex`] is not
/// `lodestone_ecs::player::DebugLine`: this crate converts the ECS's `f64`
/// world position and resolves [`lodestone_ecs::PluginTexture`] to a concrete
/// atlas UV rect (or the "untextured" sentinel), neither of which the ECS
/// type carries.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PluginBillboardInstance {
    /// World-space centre (`xyz`); `w` unused.
    pub position: [f32; 4],
    /// Width/height in blocks (`xy`); `z` is `1.0` when this instance samples
    /// the atlas, `0.0` for a flat tint (`w` unused).
    pub size_textured: [f32; 4],
    /// Atlas UV rect (`uv_min.xy`, `uv_max.xy`) — meaningless when
    /// `size_textured.z == 0.0`.
    pub uv: [f32; 4],
    /// The billboard's own tint — see
    /// [`lodestone_ecs::PluginBillboard::color`]'s doc for the gamma-space
    /// multiply this feeds `fs_main`.
    pub color: [f32; 4],
}

/// Lower a plugin's world-space billboards
/// ([`lodestone_ecs::PluginBillboard`]) into the per-instance data
/// [`PluginBillboardRenderer`] draws, resolving
/// [`lodestone_ecs::PluginTexture::Named`] against `atlas` — the block
/// atlas's own sprite table (a `ResourceLocation` string, e.g.
/// `"minecraft:block/stone"`, to its UV rect), the same stitched texture
/// [`PluginBillboardRenderer`] binds as group 1 (see that type's doc). Build
/// `atlas` with [`crate::gpu::RenderState::plugin_atlas_sprites`].
///
/// # What "unresolved" covers, and why that is not a bug
///
/// The block atlas keys sprites by their full jar path
/// (`minecraft:block/stone`, not `minecraft:stone`), so a **item** id like
/// `"minecraft:diamond"` — a legal [`lodestone_ecs::PluginTexture::Named`]
/// value, and the one `lodestone_ecs::plugin_draw`'s own test uses — has no
/// entry here: item sprites live in a separate, unstitched
/// `lodestone_assets::ItemAtlas` this pass does not bind (binding a second
/// atlas here would cost a second multi-megabyte upload for a handful of
/// billboards, the same objection `crate::particles`' module doc records for
/// its own two-atlas design, and unlike particles a plugin billboard has no
/// vanilla precedent forcing that cost). An unresolved name therefore
/// degrades to the same flat tint [`lodestone_ecs::PluginTexture::Solid`]
/// draws — real, visible pixels, just untextured — rather than dropping the
/// billboard, matching `docs/plugin-api.md`'s own documented fallback ("an
/// unresolved name" behaves like `Solid`).
#[must_use]
pub fn plugin_billboard_vertices(
    billboards: &[lodestone_ecs::PluginBillboard],
    atlas: &std::collections::HashMap<String, [f32; 4]>,
) -> Vec<PluginBillboardInstance> {
    billboards
        .iter()
        .map(|b| {
            let (textured, uv) = match &b.texture {
                lodestone_ecs::PluginTexture::Solid => (0.0, [0.0, 0.0, 1.0, 1.0]),
                lodestone_ecs::PluginTexture::Named(name) => atlas
                    .get(name)
                    .map_or((0.0, [0.0, 0.0, 1.0, 1.0]), |&rect| (1.0, rect)),
            };
            PluginBillboardInstance {
                position: [
                    b.position.x as f32,
                    b.position.y as f32,
                    b.position.z as f32,
                    0.0,
                ],
                size_textured: [b.size[0], b.size[1], textured, 0.0],
                uv,
                color: b.color,
            }
        })
        .collect()
}

/// Fixed instance capacity, mirroring
/// [`super::debug_lines::MAX_DEBUG_LINE_SEGMENTS`]'s reasoning exactly: a
/// plugin overlay does not need to grow without bound, and a fixed buffer is
/// what lets [`PluginBillboardRenderer::prepare`] take `&self` —
/// [`RenderState::render`](super::RenderState::render) itself takes `&self`,
/// so a `prepare` that needed to reallocate would need `&mut self` and a
/// second, `app.rs`-level call before every frame, exactly the wiring this
/// crate cannot add (see [`PluginBillboardsSource`]). Beyond this many
/// billboards, [`PluginBillboardRenderer::prepare`] truncates rather than
/// growing.
pub(super) const MAX_PLUGIN_BILLBOARDS: usize = 512;

/// This pass's group-0 uniform: the view-projection plus the camera's own
/// right/up basis vectors, which is what turns a centre+size instance into a
/// camera-facing quad in `vs_main` — byte-for-byte
/// `crate::particles`' `ParticleUniform`, minus the field this pass has no
/// use for.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PluginBillboardCamera {
    view_proj: [[f32; 4]; 4],
    /// World-space camera right vector (`w` unused).
    right: [f32; 4],
    /// World-space camera up vector (`w` unused).
    up: [f32; 4],
}

/// Draws a plugin's world-space billboards — the render half of issue #161
/// (`docs/plugin-api.md`). A dedicated pipeline entirely outside the model
/// shader's four bind groups (camera / atlas / palette / anim), so this
/// addition has no bearing on the 4-bind-group floor `CLAUDE.md` warns about:
/// group 0 is this pass's own camera uniform, group 1 its own texture +
/// sampler — two groups total, spending none of the model pipeline's four.
#[derive(Debug)]
pub(super) struct PluginBillboardRenderer {
    pipeline: wgpu::RenderPipeline,
    cam_buffer: wgpu::Buffer,
    cam_bind_group: wgpu::BindGroup,
    tex_bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
}

impl PluginBillboardRenderer {
    /// Build the pipeline for a target of `color_format`, binding
    /// `atlas_view`/`atlas_sampler` as group 1 — the **same** block atlas the
    /// terrain pass samples (`RenderState::new`'s `atlas` field), borrowed
    /// rather than re-uploaded for the identical reason
    /// `crate::particles::ParticleRenderer` borrows it: a second stitch would
    /// cost tens of megabytes to draw a handful of billboards.
    pub(super) fn new(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-plugin-billboards-shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../shaders/plugin_billboards.wgsl").into(),
            ),
        });

        let cam_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-plugin-billboards-camera-bgl"),
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
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-plugin-billboards-atlas-bgl"),
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

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-plugin-billboards-layout"),
            bind_group_layouts: &[Some(&cam_layout), Some(&tex_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-plugin-billboards-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<PluginBillboardInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4
                    ],
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
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                // A billboard is built from the camera basis, so its winding
                // flips as the camera passes it — same reasoning as
                // `crate::particles::ParticleRenderer`.
                cull_mode: None,
                ..Default::default()
            },
            // Tested against terrain, not written — a translucent billboard
            // must not punch a depth hole for whatever draws after it, same
            // treatment as `DebugLineRenderer`.
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

        let cam_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-plugin-billboards-camera"),
            size: std::mem::size_of::<PluginBillboardCamera>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cam_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-plugin-billboards-camera-bg"),
            layout: &cam_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: cam_buffer.as_entire_binding(),
            }],
        });
        let tex_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-plugin-billboards-atlas-bg"),
            layout: &tex_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(atlas_sampler),
                },
            ],
        });

        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-plugin-billboards-instances"),
            size: (MAX_PLUGIN_BILLBOARDS * std::mem::size_of::<PluginBillboardInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            cam_buffer,
            cam_bind_group,
            tex_bind_group,
            instances,
        }
    }

    /// Upload this frame's camera basis and billboard instances. Must run
    /// before the render pass opens — buffers cannot be written mid-pass.
    /// Returns the instance count actually written, capped at
    /// [`MAX_PLUGIN_BILLBOARDS`] — pass it to [`draw`](Self::draw).
    ///
    /// Takes `&self`, not `&mut self`: see [`MAX_PLUGIN_BILLBOARDS`]'s docs
    /// for why a fixed buffer is what makes that possible.
    pub(super) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        camera: &lodestone_render::Camera,
        instances: &[PluginBillboardInstance],
    ) -> u32 {
        let view = camera.view_matrix();
        let uniform = PluginBillboardCamera {
            view_proj: *view_proj,
            // The view matrix's rows are the camera basis in world space; in
            // glam's column-major `Mat4` that is one component from each
            // column — identical extraction to `ParticleRenderer::prepare`.
            right: [view.x_axis.x, view.y_axis.x, view.z_axis.x, 0.0],
            up: [view.x_axis.y, view.y_axis.y, view.z_axis.y, 0.0],
        };
        queue.write_buffer(&self.cam_buffer, 0, bytemuck::bytes_of(&uniform));

        let capped = &instances[..instances.len().min(MAX_PLUGIN_BILLBOARDS)];
        if capped.is_empty() {
            return 0;
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(capped));
        u32::try_from(capped.len()).unwrap_or(u32::MAX)
    }

    /// Record the draw. No-op when `count` (the last [`prepare`](Self::prepare)'s
    /// return value) is zero.
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
        if count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_bind_group(1, &self.tex_bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..4, 0..count);
    }
}

/// Polled source for this frame's plugin billboards — the render half of
/// `ExtractSet::Debug`'s billboard channel (`docs/plugin-api.md`, issue
/// #161). Same idiom as [`super::debug_lines::DebugLinesSource`]: the
/// renderer cannot reach the ECS `PluginBillboards` resource directly (this
/// crate has no dependency edge back to whoever owns the `World`), and
/// threading it through [`RenderState::render`](super::RenderState::render)'s
/// signature would touch every call site.
///
/// Unset — the default, and the state until someone installs a source —
/// samples to nothing, so [`RenderState::render`](super::RenderState::render)'s
/// behaviour is unchanged from before this existed: zero pixels from this
/// pass until a caller installs a real source with
/// [`RenderState::set_plugin_billboards_source`](super::RenderState::set_plugin_billboards_source).
#[derive(Default)]
pub struct PluginBillboardsSource(
    #[allow(clippy::type_complexity)]
    pub(super) Option<Box<dyn Fn() -> Vec<PluginBillboardInstance> + Send + Sync>>,
);

impl PluginBillboardsSource {
    #[must_use]
    pub(super) fn sample(&self) -> Vec<PluginBillboardInstance> {
        self.0.as_ref().map_or_else(Vec::new, |f| f())
    }
}

impl std::fmt::Debug for PluginBillboardsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PluginBillboardsSource")
            .field(&if self.0.is_some() {
                "installed"
            } else {
                "empty"
            })
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> lodestone_ecs::PluginBillboard {
        lodestone_ecs::PluginBillboard {
            position: lodestone_physics::Vec3d::new(1.0, 2.0, 3.0),
            size: [0.5, 0.75],
            color: [1.0, 0.5, 0.25, 1.0],
            texture: lodestone_ecs::PluginTexture::Solid,
        }
    }

    /// A [`lodestone_ecs::PluginTexture::Solid`] billboard lowers to an
    /// untextured instance carrying its own position/size/colour unchanged.
    #[test]
    fn solid_billboard_lowers_untextured() {
        let atlas = std::collections::HashMap::new();
        let out = plugin_billboard_vertices(std::slice::from_ref(&sample()), &atlas);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].position, [1.0, 2.0, 3.0, 0.0]);
        assert_eq!(out[0].size_textured, [0.5, 0.75, 0.0, 0.0]);
        assert_eq!(out[0].color, [1.0, 0.5, 0.25, 1.0]);
    }

    /// A [`lodestone_ecs::PluginTexture::Named`] billboard whose id is present
    /// in the atlas table resolves to a textured instance carrying that
    /// exact UV rect — the "reaches the block atlas" half of the render-side
    /// contract.
    #[test]
    fn named_billboard_resolves_against_the_atlas_table() {
        let mut atlas = std::collections::HashMap::new();
        atlas.insert(
            "minecraft:block/stone".to_owned(),
            [0.25, 0.5, 0.375, 0.625],
        );
        let billboards = [lodestone_ecs::PluginBillboard {
            position: lodestone_physics::Vec3d::new(0.0, 0.0, 0.0),
            size: [1.0, 1.0],
            color: [1.0, 1.0, 1.0, 1.0],
            texture: lodestone_ecs::PluginTexture::Named("minecraft:block/stone".to_owned()),
        }];
        let out = plugin_billboard_vertices(&billboards, &atlas);
        assert_eq!(out[0].size_textured[2], 1.0, "should be marked textured");
        assert_eq!(out[0].uv, [0.25, 0.5, 0.375, 0.625]);
    }

    /// The negative control for the test above: a [`Named`](lodestone_ecs::PluginTexture::Named)
    /// id absent from the atlas table — e.g. an item id like
    /// `"minecraft:diamond"`, which lives in a different atlas this pass does
    /// not bind — degrades to the same untextured instance `Solid` produces,
    /// rather than being silently dropped.
    #[test]
    fn unresolved_named_billboard_falls_back_to_untextured() {
        let atlas = std::collections::HashMap::new();
        let billboards = [lodestone_ecs::PluginBillboard {
            position: lodestone_physics::Vec3d::new(0.0, 0.0, 0.0),
            size: [1.0, 1.0],
            color: [1.0, 1.0, 1.0, 1.0],
            texture: lodestone_ecs::PluginTexture::Named("minecraft:diamond".to_owned()),
        }];
        let out = plugin_billboard_vertices(&billboards, &atlas);
        assert_eq!(out.len(), 1, "an unresolved name must not drop the billboard");
        assert_eq!(out[0].size_textured[2], 0.0);
    }
}
