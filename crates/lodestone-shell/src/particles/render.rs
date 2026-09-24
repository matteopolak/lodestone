//! GPU billboard pipeline and atlas-backed instance uploads.

use super::*;
use wgpu::util::DeviceExt;

/// The billboard render pass: one pipeline, one growable instance buffer, one
/// camera uniform.
///
/// # Two atlases, one pass
///
/// Group 1 binds **both** stitches — the block-model atlas the terrain samples
/// *and* the stitched particle sheet — and each instance carries a
/// [`SpriteAtlas`] selector saying which of them its UVs address. Before that
/// this pass bound one texture and every sheet particle sampled
/// block texels at particle-sheet coordinates: `/particle minecraft:flame`
/// drew fragments of arbitrary block textures, and nothing observed it because
/// the UVs *did* resolve.
///
/// The alternative shape — a second bind group plus two draws, block-atlas
/// instances then sheet instances — was rejected because it makes correctness
/// depend on the instance list staying **partitioned by atlas**, an invariant
/// nothing in the type system holds and which any future sort (by depth, say)
/// would silently break, reintroducing exactly this bug. Sampling both
/// textures and selecting costs one extra tap per particle fragment and makes
/// a mis-pairing unrepresentable. Two bind groups total also keeps this pass
/// far below the 4-group floor `CLAUDE.md` warns about.
#[derive(Debug)]
pub struct ParticleRenderer {
    pipeline: wgpu::RenderPipeline,
    opaque_pipeline: wgpu::RenderPipeline,
    cam_layout: wgpu::BindGroupLayout,
    tex_layout: wgpu::BindGroupLayout,
    cam_buffer: wgpu::Buffer,
    cam_bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    /// The upload staging list, opaque-layer instances first. Held across
    /// frames so the partition is not a per-frame allocation.
    ordered: Vec<ParticleInstance>,
    capacity: u32,
    count: u32,
    /// How many of the leading [`Self::count`] instances are opaque-layer, i.e.
    /// the split point between [`ParticleRenderer::draw_opaque`] and
    /// [`ParticleRenderer::draw`].
    opaque_count: u32,
    /// Of [`Self::count`], how many address the particle sheet. Kept so a
    /// caller that never installed a sheet texture can *notice* it is
    /// submitting sheet instances instead of drawing nothing — see
    /// [`ParticleFrame::sheet_drawn`] for why that distinction is the whole
    /// point of that fix.
    sheet_count: u32,
}

/// Instances allocated up front; the buffer grows (never shrinks) past this.
const INITIAL_CAPACITY: u32 = 4096;

impl ParticleRenderer {
    /// Build the pipeline for a target of `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-particle-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let cam_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-particle-camera-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // The uniform is read in the vertex stage only, but naming the
                // wrong stage set here fails at *bind* time rather than compile
                // time, so it is spelled out deliberately.
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        // Bindings 0/1 are the block-model atlas + its sampler; 2/3 are the
        // particle sheet + *its* sampler. Two samplers, not one: the two
        // stitches are separate textures with separate mip pyramids, and
        // sharing a sampler object across them would only work by accident.
        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-particle-atlas-bgl"),
            entries: &[
                texture_entry(0),
                sampler_entry(1),
                texture_entry(2),
                sampler_entry(3),
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-particle-pl"),
            bind_group_layouts: &[Some(&cam_layout), Some(&tex_layout)],
            immediate_size: 0,
        });

        // Two pipelines over one shader and one layout, differing **only** in
        // depth write. They are vanilla's `OPAQUE_PARTICLE` and
        // `TRANSLUCENT_PARTICLE`, both built from
        // the same `PARTICLE_SNIPPET`.
        //
        // One deliberate deviation: vanilla's opaque pipeline has no blending at
        // all, and this one keeps `ALPHA_BLENDING`. `Behaviour::layer()` assigns
        // every `Terrain` particle to `Layer::Opaque` unconditionally, where
        // vanilla's own by-sprite layer selection consults the sprite's own transparency and
        // sends a translucent block texture to `TRANSLUCENT_TERRAIN` instead. So
        // a broken glass or ice block reaches this pipeline here and would not
        // in vanilla, and a non-blending pipeline would draw it as opaque
        // squares. For a genuinely opaque texel the two are identical, so
        // blending is the strictly safer of the two until `layer()` learns about
        // sprite transparency.
        let make_pipeline = |label: &str, depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<ParticleInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4,
                            4 => Uint32, 5 => Uint32
                        ],
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    // A billboard is built from the camera basis, so its winding
                    // flips as the camera passes it. Culling would blink particles
                    // out; vanilla draws them double-sided too.
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(depth_write),
                    // Strictly nearer wins. Note this is *not* vanilla's
                    // `GREATER_THAN_OR_EQUAL`, which admits an exact tie; the
                    // difference is inert for a billboard, which is never
                    // coplanar with the surface behind it, and the divergence
                    // predates the reversed-Z conversion rather than being
                    // introduced by it.
                    depth_compare: Some(lodestone_render::DEPTH_COMPARE_NEARER),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: color_format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        // Depth **write on**, which is the whole mechanism behind the water fix:
        // water draws with depth test on and depth write off, so it can only
        // blend over a submerged particle if that particle is already in the
        // depth buffer. Without the write, water passes against the sea floor
        // and the particle stays in the framebuffer untinted *and* a particle in
        // front of the surface gets tinted anyway.
        let opaque_pipeline = make_pipeline("lodestone-particle-pipeline-opaque", true);
        // Depth write off: these draw after translucent terrain, and overlapping
        // blended sprites would punch holes in each other in draw order.
        let pipeline = make_pipeline("lodestone-particle-pipeline", false);

        let cam_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-particle-camera"),
            contents: bytemuck::bytes_of(&ParticleUniform {
                view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
                right: [1.0, 0.0, 0.0, 0.0],
                up: [0.0, 1.0, 0.0, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let cam_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-particle-camera-bg"),
            layout: &cam_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: cam_buffer.as_entire_binding(),
            }],
        });

        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-particle-instances"),
            size: u64::from(INITIAL_CAPACITY) * std::mem::size_of::<ParticleInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            opaque_pipeline,
            cam_layout,
            tex_layout,
            cam_buffer,
            cam_bind_group,
            instances,
            ordered: Vec::new(),
            capacity: INITIAL_CAPACITY,
            count: 0,
            opaque_count: 0,
            sheet_count: 0,
        }
    }

    /// Build the atlas bind group this pass samples.
    ///
    /// `block_*` must be the **same** atlas view the terrain pass binds, so a
    /// terrain fragment is textured from the same pixels as the block it came
    /// off. `sheet_*` is the stitched [`ParticleAtlas`] upload, which is a
    /// wholly separate texture with its own packing — passing the block atlas
    /// twice is what the renderer effectively did before this was fixed,
    /// and it draws block texels for flame and smoke. See
    /// [`crate::gpu::RenderState::install_particle_sheet_atlas`] for the
    /// jar-less fallback, which binds a 1×1 transparent texture instead so an
    /// unresolvable sheet particle draws *nothing* rather than garbage.
    #[must_use]
    pub fn atlas_bind_group(
        &self,
        device: &wgpu::Device,
        block_view: &wgpu::TextureView,
        block_sampler: &wgpu::Sampler,
        sheet_view: &wgpu::TextureView,
        sheet_sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-particle-atlas-bg"),
            layout: &self.tex_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(block_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(block_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(sheet_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(sheet_sampler),
                },
            ],
        })
    }

    /// Upload this frame's already-extracted instances. Must run **before** the
    /// render pass opens — buffers cannot be created mid-pass.
    ///
    /// Extraction deliberately happens in the simulation
    /// ([`Particles::extract`]), not here: resolving each particle's light needs
    /// the world, and taking `&mut Particles` alongside a world-reading closure
    /// would force the caller to hand out two borrows of the same owner.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[ParticleInstance],
        camera: &Camera,
    ) {
        self.count = u32::try_from(instances.len()).unwrap_or(u32::MAX);
        // Counted here rather than plumbed down from `ParticleFrame` because
        // this is the last place that sees the bytes actually being uploaded:
        // a caller that extracted one list and uploaded another would make the
        // frame report a lie, and this counter is the thing `gpu.rs` uses to
        // warn about a missing sheet texture.
        self.sheet_count = u32::try_from(
            instances
                .iter()
                .filter(|i| i.atlas == SpriteAtlas::Sheet as u32)
                .count(),
        )
        .unwrap_or(u32::MAX);

        // Partition here rather than asking `Particles::extract` to emit the two
        // layers in order. Vanilla splits the same particle group across two
        // draws — the `solid` phase before translucent terrain and `afterTerrain`
        // after it — and this pass reproduces that with one buffer and two
        // instance ranges, which only works if the buffer is partitioned.
        //
        // Doing it at the last place that sees the uploaded bytes means the
        // invariant cannot be broken by a producer: the module doc above rejects
        // an atlas-partitioned instance list for exactly the reason that a
        // future sort would silently undo it, and the same objection would apply
        // to a layer-partitioned one built upstream. `atlas` stays per-instance,
        // so nothing about that fix depends on this ordering either.
        self.ordered.clear();
        self.ordered
            .extend(instances.iter().filter(|i| i.translucent == 0));
        self.opaque_count = u32::try_from(self.ordered.len()).unwrap_or(u32::MAX);
        self.ordered
            .extend(instances.iter().filter(|i| i.translucent != 0));

        if self.count == 0 {
            return;
        }

        if self.count > self.capacity {
            self.capacity = self.count.next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lodestone-particle-instances"),
                size: u64::from(self.capacity) * std::mem::size_of::<ParticleInstance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&self.ordered));

        // Camera-relative positions, so fold the camera translation into the
        // matrix rather than adding it back per vertex.
        let view = camera.view_matrix();
        let uniform = ParticleUniform {
            view_proj: (camera.projection_matrix()
                * view
                * glam::Mat4::from_translation(camera.position))
            .to_cols_array_2d(),
            // The view matrix's rows are the camera basis in world space; in
            // glam's column-major `Mat4` that is one component from each column.
            right: [view.x_axis.x, view.y_axis.x, view.z_axis.x, 0.0],
            up: [view.x_axis.y, view.y_axis.y, view.z_axis.y, 0.0],
        };
        queue.write_buffer(&self.cam_buffer, 0, bytemuck::bytes_of(&uniform));
    }

    /// Billboards uploaded by the last [`prepare`](Self::prepare) — i.e. what
    /// [`draw`](Self::draw) will submit.
    pub fn count(&self) -> usize {
        self.count as usize
    }

    /// Of [`count`](Self::count), how many sample the **particle sheet**.
    ///
    /// Non-zero here with no sheet texture installed is a wiring defect, not a
    /// quiet frame — see [`ParticleFrame::sheet_drawn`].
    pub fn sheet_count(&self) -> usize {
        self.sheet_count as usize
    }

    /// Of [`count`](Self::count), how many are [`Layer::Opaque`] — i.e. what
    /// [`draw_opaque`](Self::draw_opaque) will submit. The remainder is
    /// [`draw`](Self::draw)'s.
    pub fn opaque_count(&self) -> usize {
        self.opaque_count as usize
    }

    /// Record the **opaque-layer** draw, which must run *before* translucent
    /// water. No-op when the last [`prepare`](Self::prepare) produced no opaque
    /// instances.
    ///
    /// This is vanilla's own opaque-layer submission of the particle group: block-break
    /// debris, crits, flame, bubbles and the rest of [`Layer::Opaque`] go in
    /// here, with depth write on, so the water surface blends over the ones
    /// beneath it and depth-rejects over the ones in front of it. See
    /// [`ParticleRenderer::new`] on the pipelines and `gpu/frame.rs`'s module
    /// doc on the ordering rule.
    ///
    /// Returns the number of instances it actually submitted, which the caller
    /// is expected to total into `RenderStats::particles_drawn` rather than
    /// reading [`count`](Self::count). That is not bookkeeping pedantry: this
    /// pass shipped for two weeks with the opaque half sitting inside a
    /// `if let Some(model)` gate it did not need, so on the packed (demo) path
    /// nothing was ever submitted while the counter — sourced from `count` —
    /// reported the full 64. A submitted-instance total makes a dropped draw
    /// visible in the debug overlay instead of indistinguishable from a healthy
    /// frame.
    pub fn draw_opaque(&self, pass: &mut wgpu::RenderPass<'_>, atlas: &wgpu::BindGroup) -> usize {
        self.draw_range(pass, atlas, &self.opaque_pipeline, 0, self.opaque_count)
    }

    /// Record the **translucent-layer** draw, which runs after translucent
    /// water as vanilla's own after-terrain draw phase does. No-op when the last
    /// [`prepare`](Self::prepare) produced no translucent instances.
    /// Returns the number of instances submitted, for the reason
    /// [`draw_opaque`](Self::draw_opaque) documents.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, atlas: &wgpu::BindGroup) -> usize {
        self.draw_range(pass, atlas, &self.pipeline, self.opaque_count, self.count)
    }

    /// The half of a draw both layers share.
    ///
    /// The instance range is expressed as a **vertex-buffer byte offset** rather
    /// than as a non-zero `first_instance`, because `first_instance` interacts
    /// with backend feature gates (`INDIRECT_FIRST_INSTANCE`) and an offset
    /// slice does not.
    fn draw_range(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        atlas: &wgpu::BindGroup,
        pipeline: &wgpu::RenderPipeline,
        first: u32,
        end: u32,
    ) -> usize {
        if end <= first {
            return 0;
        }
        let stride = std::mem::size_of::<ParticleInstance>() as u64;
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_bind_group(1, atlas, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(u64::from(first) * stride..));
        pass.draw(0..4, 0..(end - first));
        (end - first) as usize
    }

    /// The camera bind-group layout, exposed so a caller can rebuild the
    /// uniform binding if it owns the buffer.
    #[must_use]
    pub fn camera_layout(&self) -> &wgpu::BindGroupLayout {
        &self.cam_layout
    }
}

const SHADER: &str = include_str!("../shaders/particles.wgsl");
