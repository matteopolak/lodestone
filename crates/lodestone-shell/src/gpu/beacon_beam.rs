//! The beacon light beam's GPU pass — `beacon_beam.wgsl`, uploaded from
//! [`lodestone_render::beacon_beam_vertices`]'s pure geometry. Its own module
//! for the reason [`super::sign_text`] is its own module and not folded into
//! [`super::block_entities`]: this is procedural, camera-facing-free geometry
//! with a scrolling texture, not an instanced cuboid rig, so it shares no
//! mesh, no bake and no batch key with that family.
//!
//! # Two pipelines, one shader, ported from vanilla's own two `RenderType`s
//!
//! `BeaconRenderer.submitBeaconBeam` submits through
//! `RenderTypes.beaconBeam(texture, translucent)` **twice** per section — once
//! `false` (the solid inner core) and once `true` (the outer glow) — and the
//! two resolve to genuinely different pipelines in
//! `RenderPipelines.BEACON_BEAM_OPAQUE`/`BEACON_BEAM_TRANSLUCENT`:
//!
//! | | `BEACON_BEAM_OPAQUE` (solid) | `BEACON_BEAM_TRANSLUCENT` (glow) |
//! |---|---|---|
//! | blend | `ColorTargetState.DEFAULT` — `Optional.empty()`, i.e. **no blending at all** (an opaque overwrite; the texture's own alpha never reaches the framebuffer) | `BlendFunction.TRANSLUCENT` |
//! | depth write | `true` (`DepthStencilState.DEFAULT`) | `false` |
//! | depth compare | `GREATER_THAN_OR_EQUAL`, which this reversed-Z engine spells [`lodestone_render::DEPTH_COMPARE_NEARER_OR_EQUAL`] with no sign flip | same |
//!
//! So the solid core is drawn like ordinary opaque geometry (it still writes
//! depth and occludes what is behind it — the reason it is submitted with the
//! opaque block-entity batch in `gpu/frame.rs`, not the translucent group),
//! and only the *glow* square is genuinely alpha-blended. One shader module
//! backs both pipelines; only the `RenderPipelineDescriptor`'s blend/depth
//! fields differ, exactly as the two Java pipelines share
//! `BEACON_BEAM_SNIPPET` and differ only in the same two fields.
//!
//! # What this pass does not do
//!
//! * **Cull mode: `None`.** Vanilla's own `BEACON_BEAM_SNIPPET` sets no
//!   explicit cull state, and a player routinely stands *inside* a beam
//!   (walking through the pyramid) where a single-sided quad's back face
//!   would otherwise vanish. Disabling culling is the safe, cheap answer for
//!   four quads per section rather than re-deriving vanilla's exact
//!   `RenderPipeline` default.
//! * **No fog term.** `rendertype_beacon_beam.fsh` applies vanilla's
//!   `apply_fog`; this pass's own `beacon_beam.wgsl` does not, the identical
//!   simplification `gpu/sign_text.rs` already makes for its own
//!   jar-sourced-texture pass (see that module's doc). A beam is meant to
//!   read as a bright, distance-visible effect, so an un-fogged one is the
//!   least visible of this pass's gaps.
//! * **Not folded into the model shader's bind-group budget.** Two groups
//!   only (camera / texture), well inside wgpu's 4-group floor — see
//!   `gpu/block_entities.rs`'s module doc for why that ceiling matters at
//!   all on this backend.

use lodestone_render::{
    BeaconSpawn, DEPTH_COMPARE_NEARER_OR_EQUAL, DEPTH_FORMAT, beacon_beam_vertices,
};

/// Mirrors [`lodestone_render::BeamVertex`] field-for-field as a GPU-layout
/// type, the same "own vertex type per pass" idiom
/// `gpu/sign_text.rs`/`gpu/outline.rs`/`gpu/debug_lines.rs` already use.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BeamGpuVertex {
    position: [f32; 3],
    color: [f32; 4],
    uv: [f32; 2],
}

impl From<lodestone_render::BeamVertex> for BeamGpuVertex {
    fn from(v: lodestone_render::BeamVertex) -> Self {
        Self {
            position: v.position,
            color: v.color,
            uv: v.uv,
        }
    }
}

/// Fixed vertex capacity for each of the solid/glow buffers. A beam section
/// contributes 4 quads × 6 vertices = 24 per pass; even a beacon with every
/// glass colour stacked ten deep comfortably fits.
const MAX_BEAM_VERTICES: usize = 24_000;

/// Fixed vertex capacity for the end gateway's own solid/glow buffers — one
/// beam, one section, so the bare `4 quads × 6 vertices = 24` is exact
/// rather than a generous cap.
const MAX_GATEWAY_BEAM_VERTICES: usize = 24;

/// Draws the beacon light beam — see the module doc for the two-pipeline
/// shape.
#[derive(Debug)]
pub(super) struct BeaconBeamRenderer {
    solid_pipeline: wgpu::RenderPipeline,
    glow_pipeline: wgpu::RenderPipeline,
    cam_bind_group: wgpu::BindGroup,
    cam_uniform: wgpu::Buffer,
    /// `None` off a jar-less run — same fail-open contract as
    /// [`super::sign_text::SignTextRenderer::font`].
    texture: Option<wgpu::BindGroup>,
    solid_vertices: wgpu::Buffer,
    glow_vertices: wgpu::Buffer,
    /// The end gateway teleport beam's own texture — a **second** bind
    /// group over the identical `texture_layout`, reusing
    /// [`solid_pipeline`](Self::solid_pipeline)/[`glow_pipeline`](Self::glow_pipeline)
    /// unchanged: a `wgpu::RenderPipeline` embeds no texture data, only a
    /// bind-group-layout compatibility contract, so a second texture needs
    /// no second pipeline pair. `None` off a jar-less run.
    gateway_texture: Option<wgpu::BindGroup>,
    /// A single beam's worth of vertices — at most one section, unlike the
    /// beacon's own (up to `MAX_BEAM_VERTICES`) accumulated-sections buffer.
    gateway_solid_vertices: wgpu::Buffer,
    gateway_glow_vertices: wgpu::Buffer,
}

impl BeaconBeamRenderer {
    pub(super) fn new(device: &wgpu::Device, queue: &wgpu::Queue, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-beacon-beam-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/beacon_beam.wgsl").into()),
        });

        let cam_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-beacon-beam-cam-bgl"),
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
        let cam_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-beacon-beam-cam-uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cam_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-beacon-beam-cam-bg"),
            layout: &cam_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: cam_uniform.as_entire_binding(),
            }],
        });

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-beacon-beam-texture-bgl"),
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

        let texture = crate::resources::load_beacon_beam_texture().map(|img| {
            let view = super::entities::entity_texture_from_image(device, queue, &img);
            // The beam scrolls its V coordinate well outside `0..1` (a tall
            // section repeats the texture many times), so V must wrap;
            // U never leaves `0..1` so its address mode does not matter.
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("lodestone-beacon-beam-sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::Repeat,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("lodestone-beacon-beam-texture-bg"),
                layout: &texture_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            })
        });

        // The end gateway's own beam texture, over the identical
        // `texture_layout` — see [`BeaconBeamRenderer::gateway_texture`]'s
        // doc for why this needs no second pipeline pair.
        let gateway_texture = crate::resources::load_end_gateway_beam_texture().map(|img| {
            let view = super::entities::entity_texture_from_image(device, queue, &img);
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("lodestone-end-gateway-beam-sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::Repeat,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("lodestone-end-gateway-beam-texture-bg"),
                layout: &texture_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            })
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-beacon-beam-layout"),
            bind_group_layouts: &[Some(&cam_layout), Some(&texture_layout)],
            immediate_size: 0,
        });

        let vertex_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<BeamGpuVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4, 2 => Float32x2],
        })];

        let make_pipeline = |label: &str, blend: Option<wgpu::BlendState>, depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &vertex_buffers,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: color_format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    // See the module doc: a player routinely stands inside
                    // the beam, where single-sided culling would remove the
                    // near-side faces the camera actually needs.
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(DEPTH_COMPARE_NEARER_OR_EQUAL),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        // `BEACON_BEAM_OPAQUE`: no blend function at all, depth write true.
        let solid_pipeline = make_pipeline("lodestone-beacon-beam-solid-pipeline", None, true);
        // `BEACON_BEAM_TRANSLUCENT`: standard alpha blend, depth write false.
        let glow_pipeline = make_pipeline(
            "lodestone-beacon-beam-glow-pipeline",
            Some(wgpu::BlendState::ALPHA_BLENDING),
            false,
        );

        let solid_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-beacon-beam-solid-vertices"),
            size: (MAX_BEAM_VERTICES * std::mem::size_of::<BeamGpuVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let glow_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-beacon-beam-glow-vertices"),
            size: (MAX_BEAM_VERTICES * std::mem::size_of::<BeamGpuVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let gateway_solid_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-end-gateway-beam-solid-vertices"),
            size: (MAX_GATEWAY_BEAM_VERTICES * std::mem::size_of::<BeamGpuVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let gateway_glow_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-end-gateway-beam-glow-vertices"),
            size: (MAX_GATEWAY_BEAM_VERTICES * std::mem::size_of::<BeamGpuVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            solid_pipeline,
            glow_pipeline,
            cam_bind_group,
            cam_uniform,
            texture,
            solid_vertices,
            glow_vertices,
            gateway_texture,
            gateway_solid_vertices,
            gateway_glow_vertices,
        }
    }

    /// Uploads this frame's view-projection and beam geometry. Must run
    /// before the render pass opens. Returns `(solid_count, glow_count)`,
    /// each capped at [`MAX_BEAM_VERTICES`] — pass to [`draw_solid`](Self::draw_solid)
    /// / [`draw_glow`](Self::draw_glow).
    pub(super) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        beacons: &[BeaconSpawn],
    ) -> (u32, u32) {
        queue.write_buffer(&self.cam_uniform, 0, bytemuck::bytes_of(view_proj));
        if self.texture.is_none() {
            return (0, 0);
        }

        let mut solid = Vec::new();
        let mut glow = Vec::new();
        for beacon in beacons {
            if beacon.sections.is_empty() {
                continue;
            }
            let (s, g) = beacon_beam_vertices(
                beacon.pos,
                &beacon.sections,
                beacon.animation_time,
                beacon.beam_radius_scale,
            );
            solid.extend(s.into_iter().map(BeamGpuVertex::from));
            glow.extend(g.into_iter().map(BeamGpuVertex::from));
        }

        let solid_len = solid.len().min(MAX_BEAM_VERTICES);
        if solid_len > 0 {
            queue.write_buffer(&self.solid_vertices, 0, bytemuck::cast_slice(&solid[..solid_len]));
        }
        let glow_len = glow.len().min(MAX_BEAM_VERTICES);
        if glow_len > 0 {
            queue.write_buffer(&self.glow_vertices, 0, bytemuck::cast_slice(&glow[..glow_len]));
        }
        (solid_len as u32, glow_len as u32)
    }

    /// Records the solid-core draw — opaque, depth-writing. Belongs with the
    /// other opaque/cutout geometry in the pass, before translucent water.
    pub(super) fn draw_solid(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
        let Some(texture) = &self.texture else {
            return;
        };
        if count == 0 {
            return;
        }
        pass.set_pipeline(&self.solid_pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_bind_group(1, texture, &[]);
        pass.set_vertex_buffer(0, self.solid_vertices.slice(..));
        pass.draw(0..count, 0..1);
    }

    /// Records the glow draw — alpha-blended, depth-test only. Belongs with
    /// the other translucent geometry, after opaque terrain and water.
    pub(super) fn draw_glow(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
        let Some(texture) = &self.texture else {
            return;
        };
        if count == 0 {
            return;
        }
        pass.set_pipeline(&self.glow_pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_bind_group(1, texture, &[]);
        pass.set_vertex_buffer(0, self.glow_vertices.slice(..));
        pass.draw(0..count, 0..1);
    }

    /// Uploads this frame's end gateway teleport-beam geometry. Does **not**
    /// rewrite `cam_uniform` — [`prepare`](Self::prepare) already does that
    /// unconditionally every frame, and this is always called immediately
    /// after it (see `gpu/frame.rs`), so a second write here would only be
    /// redundant, not incorrect if ever reordered.
    pub(super) fn prepare_gateway(
        &self,
        queue: &wgpu::Queue,
        gateways: &[lodestone_render::EndGatewayBeamSpawn],
    ) -> (u32, u32) {
        if self.gateway_texture.is_none() {
            return (0, 0);
        }
        let mut solid = Vec::new();
        let mut glow = Vec::new();
        for gateway in gateways {
            let (s, g) = lodestone_render::end_gateway_beam_vertices(
                gateway.pos,
                gateway.scale,
                gateway.animation_time,
                gateway.height,
                gateway.color,
            );
            solid.extend(s.into_iter().map(BeamGpuVertex::from));
            glow.extend(g.into_iter().map(BeamGpuVertex::from));
        }
        let solid_len = solid.len().min(MAX_GATEWAY_BEAM_VERTICES);
        if solid_len > 0 {
            queue.write_buffer(
                &self.gateway_solid_vertices,
                0,
                bytemuck::cast_slice(&solid[..solid_len]),
            );
        }
        let glow_len = glow.len().min(MAX_GATEWAY_BEAM_VERTICES);
        if glow_len > 0 {
            queue.write_buffer(
                &self.gateway_glow_vertices,
                0,
                bytemuck::cast_slice(&glow[..glow_len]),
            );
        }
        (solid_len as u32, glow_len as u32)
    }

    /// Records the end gateway beam's solid-core draw — same pipeline as
    /// [`draw_solid`](Self::draw_solid), a different texture and vertex
    /// buffer.
    pub(super) fn draw_gateway_solid(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
        let Some(texture) = &self.gateway_texture else {
            return;
        };
        if count == 0 {
            return;
        }
        pass.set_pipeline(&self.solid_pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_bind_group(1, texture, &[]);
        pass.set_vertex_buffer(0, self.gateway_solid_vertices.slice(..));
        pass.draw(0..count, 0..1);
    }

    /// Records the end gateway beam's glow draw — same pipeline as
    /// [`draw_glow`](Self::draw_glow), a different texture and vertex
    /// buffer.
    pub(super) fn draw_gateway_glow(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
        let Some(texture) = &self.gateway_texture else {
            return;
        };
        if count == 0 {
            return;
        }
        pass.set_pipeline(&self.glow_pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_bind_group(1, texture, &[]);
        pass.set_vertex_buffer(0, self.gateway_glow_vertices.slice(..));
        pass.draw(0..count, 0..1);
    }
}
