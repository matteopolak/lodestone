//! Particles: simulation ownership plus the billboard render pass.
//!
//! [`lodestone_particle`] reproduces vanilla's per-tick particle physics but has
//! no opinion about pixels — it emits [`ParticleQuad`]s in camera-relative space
//! with *sprite-local* UVs. This module is the other half: it owns the live
//! [`ParticleEngine`], resolves each quad's sprite into absolute atlas UVs, and
//! draws the result as camera-facing billboards.
//!
//! # Why the shell owns sprite resolution
//!
//! A [`SpriteSource::BlockState`] names a block state, not a texture. Turning it
//! into UVs needs the baked model set — vanilla's `BakedModel.particleIcon()`,
//! which is the model's `#particle` variable and is emphatically **not** the
//! texture of any of its faces (`grass_block` declares `block/dirt`). Only the
//! shell holds both the engine and the atlas, so the join happens here.
//!
//! # What is not implemented yet
//!
//! [`SpriteSource::Sheet`] particles — smoke, flame, crits, splashes — need the
//! stitched `particles.png` sheet, which nothing builds. Rather than drop them
//! silently, [`Particles::extract`] counts them into
//! [`ParticleFrame::unresolved`] so the gap is visible in the HUD instead of
//! looking like a working system that emits nothing.

use std::sync::Arc;

use lodestone_particle::{Layer, ParticleEngine, ParticleQuad, SpriteSource, emit};
use lodestone_physics::{CollisionView, Vec3d};
use lodestone_render::{BlockModels, Camera};
use wgpu::util::DeviceExt;

/// One particle's GPU instance. Four vertices are generated per instance from
/// `vertex_index`, so there is no vertex or index buffer.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ParticleInstance {
    /// Camera-relative centre, `w` = half-extent in blocks.
    centre_size: [f32; 4],
    /// Absolute atlas UVs `[u0, v0, u1, v1]`.
    uv: [f32; 4],
    /// Linear RGBA tint, already multiplied by the light term.
    colour: [f32; 4],
    /// `x` = roll about the view axis in radians; `yzw` padding.
    roll: [f32; 4],
}

/// The particle camera uniform. Positions are camera-relative, so the matrix is
/// the view-projection pre-translated by the camera position — that keeps the
/// f32 precision win of camera-relative extraction instead of undoing it by
/// adding the world position back in the shader.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ParticleUniform {
    view_proj: [[f32; 4]; 4],
    /// World-space camera right vector (`w` unused).
    right: [f32; 4],
    /// World-space camera up vector (`w` unused).
    up: [f32; 4],
}

/// What one frame's extraction produced. Reported so a frame that draws nothing
/// says *why*.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParticleFrame {
    /// Live particles in the engine.
    pub alive: usize,
    /// Quads that resolved to a sprite and were uploaded.
    pub drawn: usize,
    /// Quads dropped because their sprite could not be resolved — sheet-based
    /// particles (no particle atlas yet) or a block state with no `#particle`.
    pub unresolved: usize,
}

/// The live particle simulation plus its per-frame extraction scratch.
///
/// Sprite resolution is precomputed into a per-state table at construction: the
/// alternative is a `BlockModels` borrow held across the frame, and the models
/// live inside the renderer while the engine ticks in the simulation.
#[derive(Debug)]
pub struct Particles {
    engine: ParticleEngine,
    /// Per-block-state atlas UV rect, indexed by state id. Empty when no vanilla
    /// model set is loaded (the offline demo world), which is why
    /// [`ParticleFrame::unresolved`] exists rather than a silent no-op.
    state_uv: Arc<Vec<Option<[f32; 4]>>>,
    quads: Vec<ParticleQuad>,
    instances: Vec<ParticleInstance>,
    last: ParticleFrame,
}

impl Particles {
    /// Build the simulation. `models`, when present, supplies each block state's
    /// `#particle` sprite; without it terrain particles still *simulate* but
    /// resolve to nothing and are counted as unresolved.
    #[must_use]
    pub fn new(models: Option<&BlockModels>) -> Self {
        let state_uv = match models {
            Some(m) => (0..m.state_count() as u32)
                .map(|id| m.particle_uv(id))
                .collect(),
            None => Vec::new(),
        };
        Self {
            engine: ParticleEngine::new(),
            state_uv: Arc::new(state_uv),
            quads: Vec::new(),
            instances: Vec::new(),
            last: ParticleFrame::default(),
        }
    }

    /// Build the simulation over the offline demo palette, whose sprites are
    /// indexed per block rather than per baked model.
    ///
    /// The demo block table has no `#particle` variable, so the closest faithful
    /// stand-in is the **bottom** face sprite. That is not an arbitrary pick: it
    /// reproduces vanilla's answer for the one block where the choice is
    /// visible, since `grass_block` declares `"particle": "block/dirt"` and its
    /// bottom face is dirt. For a uniformly-textured block every face agrees, so
    /// the rule is right there too.
    ///
    /// `uv_table` is [`crate::blocks::AtlasData::uv_table`], whose entries are
    /// `[u_min, v_min, u_size, v_size]` — an origin-plus-size form, unlike the
    /// baked models' min/max corners, so it is converted here rather than at the
    /// sample site.
    #[must_use]
    pub fn with_demo_palette(uv_table: &[[f32; 4]]) -> Self {
        let mut state_uv: Vec<Option<[f32; 4]>> = Vec::new();
        for id in 0..64u32 {
            let uv = crate::blocks::block(id)
                .and_then(|b| uv_table.get(b.sprites[2] as usize))
                .map(|r| [r[0], r[1], r[0] + r[2], r[1] + r[3]]);
            state_uv.push(uv);
        }
        Self {
            engine: ParticleEngine::new(),
            state_uv: Arc::new(state_uv),
            quads: Vec::new(),
            instances: Vec::new(),
            last: ParticleFrame::default(),
        }
    }

    /// The engine, for emitters that need direct access.
    pub fn engine_mut(&mut self) -> &mut ParticleEngine {
        &mut self.engine
    }

    /// The last frame's extraction report.
    #[must_use]
    pub fn frame(&self) -> ParticleFrame {
        self.last
    }

    /// Emit vanilla's block-destruction burst — `ClientLevel.addDestroyBlockEffect`.
    ///
    /// The shape is passed in rather than queried because vanilla reads the
    /// block's *outline* shape, not its collision shape, and the two differ for
    /// exactly the blocks that matter: `short_grass` has an outline and no
    /// collision at all, so driving this from collision geometry would emit
    /// nothing when a player breaks grass.
    pub fn destroy_block(&mut self, block: [i32; 3], state: u32, tint: [f32; 3]) {
        emit::destroy_block_effect(
            &mut self.engine,
            (block[0], block[1], block[2]),
            state,
            tint,
            &[emit::FULL_CUBE],
        );
    }

    /// Emit the single fragment vanilla throws each time a mining hit lands on a
    /// face — `ClientLevel.addBreakingBlockEffect`.
    pub fn breaking_block(&mut self, block: [i32; 3], state: u32, tint: [f32; 3], face: emit::Face) {
        emit::breaking_block_effect(
            &mut self.engine,
            (block[0], block[1], block[2]),
            state,
            tint,
            face,
            emit::FULL_CUBE,
        );
    }

    /// Advance every live particle one tick against `view`.
    pub fn tick(&mut self, view: &dyn CollisionView) {
        self.engine.tick(view);
    }

    /// This frame's extracted instances, ready for upload.
    #[must_use]
    pub fn instances(&self) -> &[ParticleInstance] {
        &self.instances
    }

    /// Rebuild the GPU instance list for this frame. `light` returns packed
    /// block/sky light coords at a block position, matching
    /// [`ParticleEngine::extract`].
    pub fn extract(
        &mut self,
        camera: &Camera,
        partial_tick: f32,
        light: &dyn Fn(i32, i32, i32) -> Option<u32>,
    ) -> ParticleFrame {
        self.quads.clear();
        self.instances.clear();
        let eye = Vec3d::new(
            f64::from(camera.position.x),
            f64::from(camera.position.y),
            f64::from(camera.position.z),
        );
        self.engine
            .extract(eye, partial_tick, light, &mut self.quads);

        let mut unresolved = 0usize;
        for q in &self.quads {
            // Translucent-layer ordering is not implemented; every particle
            // draws in one blended pass with depth writes off, which is correct
            // for the additive-looking terrain debris and slightly wrong for
            // overlapping alpha sprites. Recorded rather than hidden.
            let _ = matches!(q.layer, Layer::Translucent);
            let Some(rect) = self.sprite_rect(q.sprite) else {
                unresolved += 1;
                continue;
            };
            // Sprite-local UVs -> absolute atlas UVs.
            let (u0, v0) = (rect[0], rect[1]);
            let (du, dv) = (rect[2] - rect[0], rect[3] - rect[1]);
            let uv = [
                q.uv[0].mul_add(du, u0),
                q.uv[2].mul_add(dv, v0),
                q.uv[1].mul_add(du, u0),
                q.uv[3].mul_add(dv, v0),
            ];
            // Match the model shader exactly: `0.2 + 0.8 * max(sky, block)`.
            // Vanilla packs block light at bit 4 and sky light at bit 20.
            let block = ((q.light >> 4) & 15) as f32 / 15.0;
            let sky = ((q.light >> 20) & 15) as f32 / 15.0;
            let shade = 0.8f32.mul_add(block.max(sky), 0.2);
            self.instances.push(ParticleInstance {
                centre_size: [q.position[0], q.position[1], q.position[2], q.size],
                uv,
                colour: [
                    q.colour[0] * shade,
                    q.colour[1] * shade,
                    q.colour[2] * shade,
                    q.colour[3],
                ],
                roll: [q.roll, 0.0, 0.0, 0.0],
            });
        }

        let frame = ParticleFrame {
            alive: self.engine.particles().len(),
            drawn: self.instances.len(),
            unresolved,
        };
        self.last = frame;
        frame
    }

    fn sprite_rect(&self, sprite: SpriteSource) -> Option<[f32; 4]> {
        match sprite {
            SpriteSource::BlockState(id) => {
                self.state_uv.get(id as usize).copied().flatten()
            }
            // No stitched particle sheet exists yet; see the module docs.
            SpriteSource::Sheet { .. } => None,
        }
    }
}

/// The billboard render pass: one pipeline, one growable instance buffer, one
/// camera uniform. Binds whichever atlas the terrain draws from, so a fragment's
/// UVs address the same texture its parent block does.
#[derive(Debug)]
pub struct ParticleRenderer {
    pipeline: wgpu::RenderPipeline,
    cam_layout: wgpu::BindGroupLayout,
    tex_layout: wgpu::BindGroupLayout,
    cam_buffer: wgpu::Buffer,
    cam_bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    capacity: u32,
    count: u32,
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

        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-particle-atlas-bgl"),
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
            label: Some("lodestone-particle-pl"),
            bind_group_layouts: &[Some(&cam_layout), Some(&tex_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-particle-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<ParticleInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4
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
                // Tested against terrain so particles hide behind blocks, but
                // not written: overlapping particles would otherwise punch
                // holes in each other in draw order.
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
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
        });

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
            cam_layout,
            tex_layout,
            cam_buffer,
            cam_bind_group,
            instances,
            capacity: INITIAL_CAPACITY,
            count: 0,
        }
    }

    /// Build the atlas bind group this pass samples. Call once with the same
    /// atlas view the terrain pass binds.
    #[must_use]
    pub fn atlas_bind_group(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-particle-atlas-bg"),
            layout: &self.tex_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
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
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(instances));

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

    /// Record the draw. No-op when the last [`prepare`](Self::prepare) produced
    /// nothing.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, atlas: &wgpu::BindGroup) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_bind_group(1, atlas, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..4, 0..self.count);
    }

    /// The camera bind-group layout, exposed so a caller can rebuild the
    /// uniform binding if it owns the buffer.
    #[must_use]
    pub fn camera_layout(&self) -> &wgpu::BindGroupLayout {
        &self.cam_layout
    }
}

const SHADER: &str = r"
struct Camera {
    view_proj: mat4x4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;

struct Instance {
    @location(0) centre_size: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) colour: vec4<f32>,
    @location(3) roll: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) colour: vec4<f32>,
};

@vertex
fn vs_main(inst: Instance, @builtin(vertex_index) vi: u32) -> VsOut {
    // Triangle-strip corner order: (-1,-1) (-1,+1) (+1,-1) (+1,+1).
    let cx = select(-1.0, 1.0, vi >= 2u);
    let cy = select(-1.0, 1.0, (vi & 1u) == 1u);

    // Roll about the view axis, matching vanilla's `Particle.roll`.
    let s = sin(inst.roll.x);
    let c = cos(inst.roll.x);
    let rx = cx * c - cy * s;
    let ry = cx * s + cy * c;

    let size = inst.centre_size.w;
    let offset = camera.right.xyz * (rx * size) + camera.up.xyz * (ry * size);
    let world = inst.centre_size.xyz + offset;

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    // The atlas V axis grows downward, so the +Y corner takes v0.
    out.uv = vec2<f32>(
        select(inst.uv.x, inst.uv.z, cx > 0.0),
        select(inst.uv.w, inst.uv.y, cy > 0.0),
    );
    out.colour = inst.colour;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    let out = texel * in.colour;
    // Terrain fragments come from opaque sprites; discarding near-zero alpha
    // keeps a cutout parent block (leaves, grass) from throwing square debris.
    if (out.a < 0.02) {
        discard;
    }
    return out;
}
";

#[cfg(test)]
mod tests {
    use super::*;

    /// No models loaded (the offline demo world) must report unresolved rather
    /// than pretending the frame was empty — a silently-zero particle count is
    /// indistinguishable from "the emitter never fired", which is exactly the
    /// confusion this counter exists to prevent.
    #[test]
    fn terrain_particles_without_models_are_counted_unresolved() {
        let mut p = Particles::new(None);
        p.destroy_block([0, 64, 0], 1, [1.0, 1.0, 1.0]);
        assert!(
            p.engine.particles().len() >= 64,
            "a full cube throws 4^3 fragments; got {}",
            p.engine.particles().len()
        );

        let camera = Camera::default();
        let frame = p.extract(&camera, 0.0, &|_, _, _| Some(lodestone_particle::FULL_BRIGHT));
        assert_eq!(frame.drawn, 0, "no atlas, so nothing can be drawn");
        assert_eq!(
            frame.unresolved, frame.alive,
            "every live particle must be accounted for as unresolved, not dropped"
        );
    }

    /// With a sprite table present the same burst resolves and produces
    /// instances whose UVs land inside the declared sprite rect. This is the
    /// positive control for the test above: without it, an `extract` that
    /// resolved *nothing at all* would still satisfy the unresolved assertion.
    #[test]
    fn resolved_terrain_particles_produce_instances_inside_the_sprite_rect() {
        let rect = [0.25f32, 0.5, 0.3125, 0.5625];
        let mut p = Particles::new(None);
        p.state_uv = Arc::new(vec![None, Some(rect)]);
        p.destroy_block([0, 64, 0], 1, [1.0, 1.0, 1.0]);

        let camera = Camera::default();
        let frame = p.extract(&camera, 0.0, &|_, _, _| Some(lodestone_particle::FULL_BRIGHT));
        assert_eq!(frame.unresolved, 0);
        assert_eq!(frame.drawn, frame.alive);
        assert!(frame.drawn >= 64);

        for inst in &p.instances {
            for (i, uv) in inst.uv.iter().enumerate() {
                let (lo, hi) = if i % 2 == 0 {
                    (rect[0], rect[2])
                } else {
                    (rect[1], rect[3])
                };
                assert!(
                    *uv >= lo - 1e-5 && *uv <= hi + 1e-5,
                    "UV {uv} escaped the sprite rect {lo}..{hi} — a terrain fragment \
                     would sample a neighbouring block's texture"
                );
            }
            assert!(inst.centre_size[3] > 0.0, "a zero-size quad draws nothing");
        }
    }

    /// The light term must match the model shader's `0.2 + 0.8 * max(sky,
    /// block)`. A particle lit differently from the block it came from reads as
    /// a rendering bug in the terrain, not in the particle.
    #[test]
    fn light_term_matches_the_terrain_shader() {
        let rect = [0.0f32, 0.0, 1.0, 1.0];
        let mut p = Particles::new(None);
        p.state_uv = Arc::new(vec![None, Some(rect)]);
        p.destroy_block([0, 64, 0], 1, [1.0, 1.0, 1.0]);

        // Full bright first, to learn the particle's own base tint. Vanilla's
        // `TerrainParticle` scales the block colour by 0.6 in its constructor,
        // so the instance colour is `base * shade` rather than `shade`, and
        // asserting on the absolute value would be asserting on that 0.6.
        let frame = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        assert!(frame.drawn > 0);
        let base = p.instances[0].colour[0];
        assert!(base > 0.0, "a black particle makes the ratio meaningless");

        // Block light 0, sky light 0 -> the 0.2 floor.
        let _ = p.extract(&Camera::default(), 0.0, &|_, _, _| Some(0));
        let dark = p.instances[0].colour[0];
        assert!(
            (dark / base - 0.2).abs() < 1e-5,
            "unlit particle shade {} != the terrain shader's 0.2 floor",
            dark / base
        );

        // Sky-only and block-only must agree: the shader takes the max, so a
        // particle in full skylight is as bright as one beside a torch.
        let _ = p.extract(&Camera::default(), 0.0, &|_, _, _| Some(15 << 20));
        let sky_only = p.instances[0].colour[0];
        assert!(
            (sky_only - base).abs() < 1e-5,
            "sky-lit particle {sky_only} != block-lit {base}"
        );
    }

    /// Ticking must retire particles, or a single break leaks 64 quads for the
    /// rest of the session.
    #[test]
    fn particles_expire() {
        struct Air;
        impl CollisionView for Air {
            fn collision_boxes(
                &self,
                _x: i32,
                _y: i32,
                _z: i32,
                _out: &mut Vec<lodestone_physics::Aabb>,
            ) {
            }
        }

        let mut p = Particles::new(None);
        p.destroy_block([0, 64, 0], 1, [1.0, 1.0, 1.0]);
        let start = p.engine.particles().len();
        assert!(start >= 64);
        for _ in 0..200 {
            p.tick(&Air);
        }
        assert_eq!(
            p.engine.particles().len(),
            0,
            "every fragment's lifetime is well under 200 ticks"
        );
    }
}
