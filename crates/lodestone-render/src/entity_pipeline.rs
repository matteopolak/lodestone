//! The entity render pass: an **instanced**, depth-tested pipeline that draws a
//! baked [`EntityMesh`] once per model type and reads
//! each visible entity's world transform from a per-instance matrix.
//!
//! This is the entity counterpart to [`ModelPipeline`](crate::model_pipeline).
//! It reuses the wide [`ModelVertex`] layout for the
//! mesh — so a baked mob shares vertex plumbing with baked blocks — but differs
//! in the one way entities require: the vertex position is transformed by a
//! per-instance `mat4x4` supplied through a second, `Instance`-step vertex
//! buffer. That is what makes a mob farm of hundreds of the same model a single
//! instanced draw with one small matrix per mob, rather than hundreds of
//! meshes.
//!
//! # Bindings and buffers
//!
//! * **Group 0**: [`EntityCameraUniform`] — the camera ([`CameraUniform`];
//!   only `view_proj` is read, `section_origin` is left zero because an entity's
//!   world position lives in its instance matrix) **followed by this frame's
//!   [`FogUniform`]**. Fog is folded in here rather than given its own bind
//!   group, matching [`ModelCameraUniform`](crate::model_pipeline::ModelCameraUniform):
//!   the fog block must travel with the camera anyway, and one uniform means
//!   the entity pass can never drift out of step with the terrain pass's fog.
//! * **Group 1**: the entity's texture sheet + sampler.
//! * **Vertex buffer 0**: [`ModelVertex`] (locations 0–3; the shader reads
//!   position and UV).
//! * **Vertex buffer 1**: [`EntityInstanceRaw`] (locations 4–7 = the four columns
//!   of the model matrix, location 8 = the packed light byte), stepped per
//!   instance.
//!
//! # Shading: world light per instance, direction per fragment
//!
//! A mob's brightness has two independent factors and they are applied in
//! different spaces for a reason:
//!
//! 1. **World light**, one packed sky/block byte per *instance*. Vanilla samples
//!    the lightmap once per entity at its block position, so a mob is uniformly
//!    lit by the block it stands in; this shader reproduces terrain's light term
//!    (vanilla's `lightmap.fsh` curve — see [`crate::light`]) from that byte.
//!    Without it a mob renders full-bright and out-shines the terrain around it
//!    by up to an order of magnitude at night — the reported "mobs are super
//!    bright, blocks are dark" defect, in which nothing was wrong with the
//!    blocks.
//! 2. **Direction.** [`ModelVertex`] carries no normal, so the fragment shader
//!    reconstructs a face normal from screen-space derivatives of the
//!    interpolated world position (`-cross(dpdx, dpdy)`, negated to point at the
//!    eye) and applies **vanilla's two-light diffuse**: `min(1, (max(0, n·L0) +
//!    max(0, n·L1)) * 0.6 + 0.4)` with `L0/L1` from
//!    `blaze3d.platform.Lighting.DIFFUSE_LIGHT_0/1`. The negation is what makes
//!    the double-sided raster state below safe — entity meshes are drawn without
//!    back-face culling (robust visibility while per-model winding parity is
//!    still being pixel-verified), and taking the eye-facing side is exactly what
//!    vanilla's front/back pair in `entity.vsh` resolves to.
//!
//!    This was **one** light and an `abs()` until issue #383. That formula lights
//!    a face pointing away from the light as brightly as one pointing into it
//!    (up and down both `0.9085`, vanilla `1.0` and `0.4`) and bottoms out at
//!    `0.4` on every normal *perpendicular* to its single direction. Box faces
//!    never land on that band, so standing mobs looked passable; the rotated
//!    first-person arm sat at `0.497` across 97% of its pixels, which is the dark
//!    side a player reported. See `entity_diffuse_two_lights_pixels.rs` and
//!    `docs/entity-rendering.md`.
//!
//! Their product is multiplied into the texel in **gamma space**, through the
//! same `srgb_to_linear(linear_to_srgb(rgb) * shade)` round-trip the model
//! shader uses. Vanilla is not colour-managed and multiplies shade into gamma
//! byte values; doing it in linear light and re-encoding pulls every factor
//! toward 1.0 (a shade of 0.6 reads as 0.79), which is the washed-out look
//! `4e8f058` removed from terrain. Entities carried the same bug afterwards.
//!
//! # Texture format is part of the brightness
//!
//! The sheet bound to group 1 must be an **`_srgb`** format, like the block
//! atlas. A vanilla PNG holds gamma-encoded bytes; binding it as plain `Unorm`
//! hands the shader `0.50` where the linear value is `0.21`, and the sRGB render
//! target then encodes it a *second* time — a measured **+48%** on every mob
//! pixel, enough on its own to make a mob brighter than the brightest sunlit
//! block face.

use wgpu::util::DeviceExt;

use crate::block::{CameraUniform, DEPTH_FORMAT};
use crate::entity::EntityMesh;
use crate::models::ModelVertex;

/// A per-instance entity record for the instance vertex buffer: a column-major
/// `mat4x4<f32>` laid out as four `vec4` attributes, the entity's packed
/// sky/block light byte, and a per-instance tint.
///
/// Light rides the *instance* buffer, not the vertex buffer, because the vertex
/// buffer is shared by every instance of a model type — a per-vertex light byte
/// could only ever say one thing for all mobs of that kind. Vanilla's own
/// lightmap sample is per entity, so this is also the faithful granularity. The
/// tint rides here for the same reason and at the same granularity: vanilla's
/// `submitModel(model, state, pose, renderType, light, overlay, color, …)` takes
/// one `color` per submitted model, and dyed leather armour is the case that
/// needs it.
///
/// # Why the instance buffer and not a fifth bind group
///
/// Because a bind group is the one resource this pass cannot afford. The model
/// shader is at wgpu's default `max_bind_groups` of 4 and a fifth group compiles
/// on an M5 (which reports 8) while crashing at startup on any 4-group adapter —
/// see `CLAUDE.md`. A vertex attribute has no such ceiling: this adds location 9
/// to a buffer that already exists.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EntityInstanceRaw {
    /// The model→world matrix, column-major (four columns of four floats).
    pub model: [[f32; 4]; 4],
    /// Packed sky/block light, `sky << 4 | block` (`0..=15` each), widened to
    /// `u32` for the `Uint32` vertex attribute. Same encoding as
    /// [`ModelVertex::light`](crate::models::ModelVertex::light), so the entity
    /// and model shaders unpack it with identical code.
    pub light: u32,
    /// Packed `AARRGGBB`: bits 0–23 are the **gamma-space** tint, multiplied
    /// into the texel exactly as before; bits 24–31 are the hurt/death overlay
    /// alpha, added on top ([`HURT_OVERLAY_ALPHA_BYTE`] when set, `0` when not).
    /// [`NO_TINT`] (white, no overlay) is what every mob passes by default.
    ///
    /// The overlay byte rides in the tint word's previously-unused top byte
    /// rather than a new vertex attribute, for the same reason fog rides in the
    /// group-0 camera uniform: this shader is at wgpu's 4-bind-group floor (see
    /// `CLAUDE.md`), and this instance buffer already has a spare byte sitting
    /// idle in every existing tint value, so widening the *meaning* of one
    /// `Uint32` costs nothing a new attribute would.
    ///
    /// Gamma space, not linear: vanilla is not colour-managed and its vertex
    /// colour multiplies the gamma-encoded texel byte. The shader therefore
    /// folds the tint multiply into the *same* `srgb_to_linear(linear_to_srgb(rgb)
    /// * …)` round-trip the directional and world-light shades already use, and
    /// blends the overlay in that same gamma-space stage. Doing either in linear
    /// light pulls the factor toward 1.0 and washes the result out.
    pub tint: u32,
}

/// The `tint` value meaning "leave the texel alone": opaque white, no overlay.
pub const NO_TINT: u32 = 0x00FF_FFFF;

/// The hurt/death overlay's alpha byte, packed into `tint`'s bits 24–31.
///
/// Vanilla's `OverlayTexture` (`net/minecraft/client/renderer/texture/
/// OverlayTexture.java`) bakes a 16×16 lookup texture whose `y < 8` (red) row is
/// a flat `ARGB.color(...)` of `-1291911168` for every `x` — i.e. `(178, 255, 0,
/// 0)` — sampled whenever `LivingEntityRenderer.java:281` sets
/// `state.hasRedOverlay = entity.hurtTime > 0 || entity.deathTime > 0`. `178` is
/// that overlay's alpha byte; `255, 0, 0` is pure red, which is why the blend
/// below mixes against a literal `vec3(1.0, 0.0, 0.0)` rather than reading a
/// colour out of this word.
///
/// **`178` is how much of the entity's own colour survives, not how much red is
/// added.** Vanilla's `entity.fsh:57` is
/// `mix(overlayColor.rgb, color.rgb, overlayColor.a)`, so the alpha is the weight
/// on `color`, giving roughly 30% red. This comment previously described the
/// constant correctly and its *role* not at all, and the shader below was written
/// with the arguments the other way round — 70% red (issue #371). If you are
/// tempted to tune this number because the flash looks wrong, check the argument
/// order first.
pub const HURT_OVERLAY_ALPHA_BYTE: u32 = 178;

impl EntityInstanceRaw {
    /// Pack a [`glam::Mat4`] into the instance format (column-major), lit
    /// full-bright and untinted. Kept for callers with no world to sample.
    #[must_use]
    pub fn from_mat4(m: glam::Mat4) -> Self {
        Self::new(m, u32::from(crate::entity::ENTITY_FULLBRIGHT))
    }

    /// Pack a transform and a packed sky/block light byte into the instance
    /// format (column-major), untinted.
    #[must_use]
    pub fn new(m: glam::Mat4, light: u32) -> Self {
        Self {
            model: m.to_cols_array_2d(),
            light,
            tint: NO_TINT,
        }
    }

    /// Set this instance's packed `0x00RRGGBB` gamma-space tint.
    ///
    /// Builder-style for the same reason [`EntityInstance::with_light`] is: only
    /// dyed armour has anything to pass, and every other caller wants
    /// [`NO_TINT`].
    ///
    /// [`EntityInstance::with_light`]: crate::entity::EntityInstance::with_light
    #[must_use]
    pub fn with_tint(mut self, rgb: [u8; 3]) -> Self {
        self.tint = (self.tint & 0xFF00_0000)
            | (u32::from(rgb[0]) << 16)
            | (u32::from(rgb[1]) << 8)
            | u32::from(rgb[2]);
        self
    }

    /// Set or clear the hurt/death red overlay (bits 24–31 of `tint`).
    ///
    /// Vanilla's gate is boolean, not a fade: `hasRedOverlay = entity.hurtTime
    /// > 0 || entity.deathTime > 0` (`LivingEntityRenderer.java:281`) — no
    /// interpolation by how much of `hurtTime` remains, so this takes a `bool`
    /// rather than a `0.0..=1.0` strength. Builder-style, like [`with_tint`],
    /// so a caller that also dyes leather can chain both without either
    /// clobbering the other's bits.
    ///
    /// [`with_tint`]: Self::with_tint
    #[must_use]
    pub fn with_hurt_overlay(mut self, active: bool) -> Self {
        let alpha = if active { HURT_OVERLAY_ALPHA_BYTE } else { 0 };
        self.tint = (self.tint & 0x00FF_FFFF) | (alpha << 24);
        self
    }

    /// The instance-stepped vertex-buffer layout: four `Float32x4` columns at
    /// shader locations 4–7, then the packed light `Uint32` at location 8.
    #[must_use]
    pub const fn instance_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 6] = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 0,
                shader_location: 4,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 16,
                shader_location: 5,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 32,
                shader_location: 6,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 48,
                shader_location: 7,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 64,
                shader_location: 8,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 68,
                shader_location: 9,
            },
        ];
        wgpu::VertexBufferLayout {
            array_stride: core::mem::size_of::<EntityInstanceRaw>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }
}

/// GPU-resident geometry for one entity model type: a vertex buffer, an index
/// buffer, and the index count. Uploaded once; every instance of the model
/// reuses it.
#[derive(Debug)]
pub struct GpuEntityModel {
    /// Vertex buffer of [`ModelVertex`].
    pub vertices: wgpu::Buffer,
    /// `u32` index buffer.
    pub indices: wgpu::Buffer,
    /// Number of indices to draw (all parts).
    pub index_count: u32,
    /// One index sub-range per skeleton part, in mesh part order. Drawing part
    /// `p` instanced over that part's matrices is what animates a limb.
    pub parts: Vec<crate::entity::PartRange>,
}

impl GpuEntityModel {
    /// Upload an [`EntityMesh`], or `None` if it is empty (nothing to draw).
    #[must_use]
    pub fn upload(device: &wgpu::Device, mesh: &EntityMesh) -> Option<Self> {
        if mesh.indices.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuEntityModel {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
            parts: mesh.parts.clone(),
        })
    }

    /// Upload a [`BlockEntityMesh`](crate::block_entity::BlockEntityMesh), or
    /// `None` if empty.
    ///
    /// Takes the three pieces rather than the mesh type because a block-entity
    /// mesh differs from an [`EntityMesh`] only in what lives *beside* the
    /// buffers on the CPU (a part hierarchy with pose overrides instead of a
    /// slot-based [`Skeleton`](crate::entity_anim::Skeleton)). The GPU-resident
    /// shape is identical, so the alternative would be either a second copy of
    /// this buffer-creation code or a `BlockEntityMesh → EntityMesh` conversion
    /// that fabricates a skeleton nothing reads.
    #[must_use]
    pub fn upload_parts(
        device: &wgpu::Device,
        vertices: &[crate::models::ModelVertex],
        indices: &[u32],
        parts: Vec<crate::entity::PartRange>,
    ) -> Option<Self> {
        if indices.is_empty() {
            return None;
        }
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-block-entity-vertices"),
            contents: bytemuck::cast_slice(vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-block-entity-indices"),
            contents: bytemuck::cast_slice(indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuEntityModel {
            vertices: vertex_buffer,
            indices: index_buffer,
            index_count: indices.len() as u32,
            parts,
        })
    }

    /// Upload an [`ArmourMesh`](crate::entity::ArmourMesh), or `None` if empty.
    ///
    /// `parts` carries the ranges in the mesh's own order, with the part *names*
    /// left behind on the CPU side: an armour draw gets its ranges from
    /// [`ArmourMesh::attach`](crate::entity::ArmourMesh::attach), which pairs
    /// each range with the wearer's part index, so the GPU struct never needs to
    /// be indexed by name.
    #[must_use]
    pub fn upload_armour(device: &wgpu::Device, mesh: &crate::entity::ArmourMesh) -> Option<Self> {
        if mesh.indices.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-armour-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-armour-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuEntityModel {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
            parts: mesh.parts.iter().map(|(_, r)| *r).collect(),
        })
    }

    /// Upload a [`WoolMesh`](crate::entity::WoolMesh), or `None` if empty.
    ///
    /// Mirrors [`upload_armour`](Self::upload_armour) exactly — the same
    /// `parts`-carries-ranges-only, names-left-on-the-CPU shape, since a wool
    /// draw likewise gets its ranges from
    /// [`WoolMesh::attach`](crate::entity::WoolMesh::attach), which pairs each
    /// range with the wearer's part index.
    #[must_use]
    pub fn upload_wool(device: &wgpu::Device, mesh: &crate::entity::WoolMesh) -> Option<Self> {
        if mesh.indices.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-wool-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-wool-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuEntityModel {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
            parts: mesh.parts.iter().map(|(_, r)| *r).collect(),
        })
    }
}

/// Build an instance buffer from a slice of model matrices and the matching
/// per-instance packed light bytes, or `None` if empty.
///
/// `lights` is indexed in lockstep with `transforms`
/// ([`EntityBatch::lights`](crate::entity::EntityBatch::lights) alongside any of
/// that batch's per-part matrix vectors). A short or missing `lights` entry
/// falls back to [`ENTITY_FULLBRIGHT`](crate::entity::ENTITY_FULLBRIGHT) rather
/// than panicking or rendering black: a light plumbing mistake should look like
/// the old behaviour, not like a crash mid-frame.
#[must_use]
pub fn upload_instances(
    device: &wgpu::Device,
    transforms: &[glam::Mat4],
    lights: &[u32],
) -> Option<wgpu::Buffer> {
    if transforms.is_empty() {
        return None;
    }
    let fallback = u32::from(crate::entity::ENTITY_FULLBRIGHT);
    let raw: Vec<EntityInstanceRaw> = transforms
        .iter()
        .enumerate()
        .map(|(i, m)| EntityInstanceRaw::new(*m, lights.get(i).copied().unwrap_or(fallback)))
        .collect();
    Some(
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-instances"),
            contents: bytemuck::cast_slice(&raw),
            usage: wgpu::BufferUsages::VERTEX,
        }),
    )
}

/// One instance's full colour state: the gamma-space dye tint **and** whether
/// the hurt/death red overlay applies to it.
///
/// # Why one type instead of two parallel slices
///
/// [`upload_instances_tinted`] used to take a bare `&[[u8; 3]]`, and the
/// obvious way to add the overlay was a second `&[bool]` beside it. That is the
/// shape this repo keeps getting bitten by: a lockstep invariant across two
/// arguments that nothing enforces, so a later edit that filters or reorders one
/// of them silently paints the wrong mob red. Bundling the two means a tint
/// physically cannot travel without its overlay flag — the same move that made
/// `sprite_rect` return its atlas alongside its rect rather than leaving the
/// pairing to the caller.
///
/// [`NONE`](Self::NONE) is what every undyed, unhurt instance passes, and it
/// packs to exactly [`NO_TINT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceTint {
    /// The gamma-space `[r, g, b]` multiplied into the texel — `[255, 255, 255]`
    /// for "leave the texel alone".
    pub rgb: [u8; 3],
    /// Whether this instance draws with the hurt/death red overlay
    /// ([`HURT_OVERLAY_ALPHA_BYTE`]). Boolean, not a fade, per
    /// [`EntityInstanceRaw::with_hurt_overlay`].
    pub hurt: bool,
}

impl InstanceTint {
    /// Untinted and unhurt: packs to [`NO_TINT`].
    pub const NONE: Self = Self {
        rgb: [255, 255, 255],
        hurt: false,
    };

    /// A dye tint with no overlay.
    #[must_use]
    pub const fn rgb(rgb: [u8; 3]) -> Self {
        Self { rgb, hurt: false }
    }

    /// The same tint with the hurt/death overlay set or cleared.
    #[must_use]
    pub const fn with_hurt(mut self, hurt: bool) -> Self {
        self.hurt = hurt;
        self
    }

    /// Fold both halves into one instance's packed `tint` word.
    #[must_use]
    fn apply(self, inst: EntityInstanceRaw) -> EntityInstanceRaw {
        inst.with_tint(self.rgb).with_hurt_overlay(self.hurt)
    }
}

impl Default for InstanceTint {
    fn default() -> Self {
        Self::NONE
    }
}

/// [`upload_instances`] with a per-instance gamma-space tint and hurt overlay.
///
/// `tints` is indexed in lockstep with `transforms`; a short or missing entry
/// falls back to [`InstanceTint::NONE`], for the same reason `lights` falls back
/// to full-bright — a plumbing mistake should render the *untinted* thing, not a
/// black one, because "grey leather" is a legible bug and "black leather" looks
/// like a lighting failure somewhere else entirely. A missing entry likewise
/// draws *unhurt*: a mob that should have flashed and did not is a missed frame,
/// where a mob reddened by an indexing slip looks like a damage event that never
/// happened.
#[must_use]
pub fn upload_instances_tinted(
    device: &wgpu::Device,
    transforms: &[glam::Mat4],
    lights: &[u32],
    tints: &[InstanceTint],
) -> Option<wgpu::Buffer> {
    if transforms.is_empty() {
        return None;
    }
    let fallback = u32::from(crate::entity::ENTITY_FULLBRIGHT);
    let raw: Vec<EntityInstanceRaw> = transforms
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let inst = EntityInstanceRaw::new(*m, lights.get(i).copied().unwrap_or(fallback));
            match tints.get(i) {
                Some(tint) => tint.apply(inst),
                None => inst,
            }
        })
        .collect();
    Some(
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-tinted-instances"),
            contents: bytemuck::cast_slice(&raw),
            usage: wgpu::BufferUsages::VERTEX,
        }),
    )
}

/// The one place the entity pipeline's raster/depth/vertex state is spelled out,
/// parameterised by the two things that vary: the label and the depth
/// comparison. Two pipelines share it — the mob pass and the armour pass — so a
/// change to the vertex layout or the colour target cannot land on one and miss
/// the other.
fn build_entity_pipeline(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
    camera_layout: &wgpu::BindGroupLayout,
    texture_layout: &wgpu::BindGroupLayout,
    label: &str,
    depth_compare: wgpu::CompareFunction,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&format!("{label}-shader")),
        source: wgpu::ShaderSource::Wgsl(ENTITY_WGSL.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(&format!("{label}-layout")),
        bind_group_layouts: &[Some(camera_layout), Some(texture_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&format!("{label}-pipeline")),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[
                Some(ModelVertex::vertex_layout()),
                Some(EntityInstanceRaw::instance_layout()),
            ],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            // Double-sided for now: robust visibility while per-model winding
            // parity is pixel-verified. See the module docs. Vanilla's armour
            // render type is `armorCutoutNoCull`, i.e. also double-sided.
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(depth_compare),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// A depth-tested, instanced pipeline for baked entity geometry.
#[derive(Debug)]
pub struct EntityPipeline {
    /// The render pipeline.
    pub pipeline: wgpu::RenderPipeline,
    /// Bind-group layout for the camera uniform (group 0).
    pub camera_layout: wgpu::BindGroupLayout,
    /// Bind-group layout for the entity texture + sampler (group 1).
    pub texture_layout: wgpu::BindGroupLayout,
}

impl EntityPipeline {
    /// Build the entity pipeline targeting `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-entity-camera-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // Vertex reads the view-projection; fragment reads the folded
                // fog block (eye, colour, range), so both stages bind it.
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-entity-texture-bgl"),
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

        let pipeline = build_entity_pipeline(
            device,
            color_format,
            &camera_layout,
            &texture_layout,
            "lodestone-entity",
            wgpu::CompareFunction::Less,
        );

        EntityPipeline {
            pipeline,
            camera_layout,
            texture_layout,
        }
    }

    /// A second render pipeline over **this** pipeline's own bind-group layouts,
    /// differing only in its depth comparison: `LessEqual` rather than `Less`.
    /// For the humanoid-armour layers.
    ///
    /// Sharing `self`'s layout objects rather than creating equivalent ones is
    /// deliberate: every camera and texture bind group already built through
    /// [`camera_bind_group`](Self::camera_bind_group) /
    /// [`texture_bind_group`](Self::texture_bind_group) is then valid here with
    /// no second set of uploads, and there is no reliance on wgpu deduplicating
    /// two structurally identical layout descriptors.
    ///
    /// # Why `LessEqual`, and why only here
    ///
    /// Vanilla's own entity depth state is
    /// `DepthStencilState.DEFAULT = (GREATER_THAN_OR_EQUAL, writeDepth = true)`
    /// (`DepthStencilState.java:6`), which under this engine's `[0,1]`
    /// DirectX-style depth — vanilla is reversed-Z — is `LessEqual`. The base
    /// entity pipeline above uses `Less`, so it is the one that departs from
    /// vanilla; that is left alone here rather than "fixed", because changing it
    /// would alter how *every* mob's coplanar geometry resolves and this change
    /// has no pixel gate to prove that safe.
    ///
    /// Armour needs the faithful value for a concrete reason: leather's
    /// `humanoid` layer list is **two coplanar layers** at one inflation — a
    /// greyscale dyeable base and an untinted `leather_overlay` detail pass
    /// drawn straight over it (`equipment/leather.json`). Under `Less` the
    /// second draw fails the depth test against the first at every texel and
    /// the overlay is silently invisible; under `LessEqual` it wins, which is
    /// what vanilla does.
    #[must_use]
    pub fn armour_pipeline(
        &self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        build_entity_pipeline(
            device,
            color_format,
            &self.camera_layout,
            &self.texture_layout,
            "lodestone-entity-armour",
            wgpu::CompareFunction::LessEqual,
        )
    }

    /// Build the group-0 uniform buffer for the entity pass with fog
    /// **disabled**. `view_proj` is taken from the camera; `section_origin` is
    /// unused (zero) because an entity's world position lives in its instance
    /// matrix.
    ///
    /// The buffer is sized for the whole [`EntityCameraUniform`], so a caller
    /// that later wants fog can overwrite it in place with
    /// [`queue.write_buffer`](wgpu::Queue::write_buffer) — see
    /// [`camera_buffer_with_fog`](Self::camera_buffer_with_fog).
    #[must_use]
    pub fn camera_buffer(
        &self,
        device: &wgpu::Device,
        camera: &crate::camera::Camera,
    ) -> wgpu::Buffer {
        self.camera_buffer_with_fog(device, camera, crate::fog::FogUniform::disabled())
    }

    /// Build the group-0 uniform buffer for the entity pass with an explicit fog
    /// block, so mobs fade into the distance (or into water fog) on exactly the
    /// same curve as the terrain behind them.
    #[must_use]
    pub fn camera_buffer_with_fog(
        &self,
        device: &wgpu::Device,
        camera: &crate::camera::Camera,
        fog: crate::fog::FogUniform,
    ) -> wgpu::Buffer {
        entity_camera_buffer(
            device,
            EntityCameraUniform {
                camera: CameraUniform::new(camera, [0.0, 0.0, 0.0]),
                fog,
            },
        )
    }

    /// Create the camera bind group from a uniform buffer.
    #[must_use]
    pub fn camera_bind_group(
        &self,
        device: &wgpu::Device,
        camera_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-entity-camera-bg"),
            layout: &self.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        })
    }

    /// Create the texture bind group from a texture view and sampler (one
    /// entity sheet).
    #[must_use]
    pub fn texture_bind_group(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-entity-texture-bg"),
            layout: &self.texture_layout,
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
}

/// The group-0 uniform for the entity pipeline: the [`CameraUniform`] followed
/// by this frame's [`FogUniform`](crate::fog::FogUniform).
///
/// Byte-compatible with
/// [`ModelCameraUniform`](crate::model_pipeline::ModelCameraUniform) on purpose
/// — same layout, same shader-side `Camera` struct, same `fog_amount` — so a
/// mob and the block behind it can never be fogged by different math. Rewrite
/// the whole struct each frame via [`queue.write_buffer`](wgpu::Queue::write_buffer).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EntityCameraUniform {
    /// View-projection (and an unused zero section origin).
    pub camera: CameraUniform,
    /// Distance fog for this frame (eye position, colour, start/end) — plus,
    /// in its one spare lane, this frame's sky darkening. See
    /// [`with_sky_darken`](Self::with_sky_darken).
    pub fog: crate::fog::FogUniform,
}

/// Which lane of `FogUniform::end_enabled` carries the sky-darken factor.
/// `end_enabled` is documented as `x = end`, `y = enabled`, `zw` unused; this is
/// the `z`.
const SKY_DARKEN_LANE: usize = 2;

impl EntityCameraUniform {
    /// Set this frame's **sky darkening**: the factor vanilla's `LightTexture`
    /// scales the *sky* half of the lightmap by, `1.0` at noon down to `0.24` at
    /// midnight. See [`sky_darken`](Self::sky_darken) for the read side.
    ///
    /// # Why this term has to exist at all
    ///
    /// A server's sky-light array is time-**invariant**: it encodes how much sky
    /// reaches a block, not how bright the sky currently is. Measured live
    /// against a vanilla 26.2 oracle at one position, with the server clock as
    /// the control:
    ///
    /// ```text
    /// noon     clock= 6000  packed=0xF0  sky=15 block=0  light_term=1.000
    /// midnight clock=18000  packed=0xF0  sky=15 block=0  light_term=1.000
    /// ```
    ///
    /// So sampling world light correctly — which `52f109f` did — cannot darken a
    /// mob at night, because the sampled byte is the same byte. Vanilla darkens
    /// purely client-side, in `LightTexture.updateLightTexture`, by scaling the
    /// sky contribution by `Level.getSkyDarken(partialTick) * 0.95 + 0.05`.
    /// `crate::entity::sky_darken_for_time_of_day` is that curve.
    ///
    /// # Why a spare fog lane and not a new field
    ///
    /// [`EntityCameraUniform`] is byte-identical to
    /// [`ModelCameraUniform`](crate::model_pipeline::ModelCameraUniform) on
    /// purpose, and the model shader is at wgpu's 4-bind-group floor, so neither
    /// growing the struct nor adding a bind group is free. `end_enabled.zw` were
    /// already unused and the model shader does not read them, so terrain is
    /// unaffected until it opts in.
    ///
    /// # Why `0.0` reads as full daylight
    ///
    /// Every path that builds this uniform derives its fog from
    /// [`FogUniform::new`](crate::fog::FogUniform::new) or
    /// [`FogUniform::disabled`](crate::fog::FogUniform::disabled), both of which
    /// zero the lane. Taking `0.0` literally would render every mob in every
    /// existing caller at the `0.2` floor — a silent, global regression of
    /// exactly the shape [`ENTITY_FULLBRIGHT`](crate::entity::ENTITY_FULLBRIGHT)
    /// exists to prevent. Vanilla's factor is floored at `0.24`, so `0.0` is
    /// never a legitimate value and is safe as the "not wired yet" sentinel: the
    /// shader reads it as `1.0`, i.e. today's behaviour.
    #[must_use]
    pub const fn with_sky_darken(mut self, sky_darken: f32) -> Self {
        self.fog.end_enabled[SKY_DARKEN_LANE] = sky_darken;
        self
    }

    /// This frame's sky-darken factor as the shader will interpret it: the raw
    /// lane, or `1.0` when the lane is the unset `0.0` sentinel.
    #[must_use]
    pub fn sky_darken(&self) -> f32 {
        let raw = self.fog.end_enabled[SKY_DARKEN_LANE];
        if raw <= 0.0 { 1.0 } else { raw }
    }
}

/// Create the entity pass's group-0 uniform buffer from a full
/// [`EntityCameraUniform`]. For callers holding a [`Camera`](crate::camera::Camera),
/// [`EntityPipeline::camera_buffer_with_fog`] is the convenient wrapper.
#[must_use]
pub fn entity_camera_buffer(
    device: &wgpu::Device,
    uniform: EntityCameraUniform,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lodestone-entity-camera-uniform"),
        contents: bytemuck::bytes_of(&uniform),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

const ENTITY_WGSL: &str = include_str!("shaders/entity.wgsl");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_raw_is_four_columns_plus_a_light_and_a_tint_word() {
        assert_eq!(core::mem::size_of::<EntityInstanceRaw>(), 72);
        let layout = EntityInstanceRaw::instance_layout();
        assert_eq!(layout.array_stride, 72);
        assert_eq!(layout.step_mode, wgpu::VertexStepMode::Instance);
        assert_eq!(layout.attributes.len(), 6);
        // Instance attributes start at location 4, past ModelVertex's 0..=3.
        assert_eq!(layout.attributes[0].shader_location, 4);
        assert_eq!(layout.attributes[3].shader_location, 7);
        assert_eq!(layout.attributes[3].offset, 48);
        // The light word sits immediately after the matrix, the tint after it.
        assert_eq!(layout.attributes[4].shader_location, 8);
        assert_eq!(layout.attributes[4].offset, 64);
        assert_eq!(layout.attributes[4].format, wgpu::VertexFormat::Uint32);
        assert_eq!(layout.attributes[5].shader_location, 9);
        assert_eq!(layout.attributes[5].offset, 68);
        assert_eq!(layout.attributes[5].format, wgpu::VertexFormat::Uint32);
    }

    /// A tint must round-trip its bytes in `0x00RRGGBB` order, and an instance
    /// built without one must be **white**, not zero. Zero would be black, and
    /// every mob in the game goes through [`EntityInstanceRaw::new`].
    #[test]
    fn tint_defaults_to_white_and_packs_rgb_in_order() {
        let m = glam::Mat4::IDENTITY;
        assert_eq!(EntityInstanceRaw::new(m, 0).tint, NO_TINT);
        assert_eq!(EntityInstanceRaw::from_mat4(m).tint, NO_TINT);
        assert_eq!(NO_TINT, 0x00FF_FFFF);
        let leather = EntityInstanceRaw::new(m, 0)
            .with_tint(lodestone_assets::equipment::UNDYED_LEATHER_RGB);
        assert_eq!(leather.tint, 0x00A0_6540);
        // R in the high byte: a byte-order slip would make leather blue.
        assert_eq!((leather.tint >> 16) & 0xFF, 0xA0);
        assert_eq!(leather.tint & 0xFF, 0x40);
    }

    /// The overlay byte lives in bits 24-31, and setting or clearing it must
    /// never disturb the tint's own bits 0-23 (or vice versa) — dyed leather
    /// worn by a hurt mob needs both at once.
    #[test]
    fn hurt_overlay_shares_the_tint_word_without_colliding() {
        let m = glam::Mat4::IDENTITY;

        // Off by default, same as tint.
        let plain = EntityInstanceRaw::new(m, 0);
        assert_eq!(plain.tint, NO_TINT);

        // Overlay alone: RGB bits untouched (still opaque white), alpha byte set
        // to vanilla's 178 (`-1291911168`'s alpha channel, `OverlayTexture`'s red
        // row, `LivingEntityRenderer.java:281`).
        let hurt = EntityInstanceRaw::new(m, 0).with_hurt_overlay(true);
        assert_eq!(hurt.tint & 0x00FF_FFFF, NO_TINT);
        assert_eq!((hurt.tint >> 24) & 0xFF, HURT_OVERLAY_ALPHA_BYTE);
        assert_eq!(HURT_OVERLAY_ALPHA_BYTE, 178);

        // Setting then clearing must return to exactly the untouched value, not
        // merely a "no visible effect" value — a stray bit here would silently
        // change the packed word's meaning for any future consumer.
        let cleared = hurt.with_hurt_overlay(false);
        assert_eq!(cleared.tint, NO_TINT);

        // Tint and overlay compose: dyed leather (bits 0-23) plus hurt (bits
        // 24-31) must both read back correctly regardless of call order.
        let leather_hurt = EntityInstanceRaw::new(m, 0)
            .with_tint(lodestone_assets::equipment::UNDYED_LEATHER_RGB)
            .with_hurt_overlay(true);
        let hurt_leather = EntityInstanceRaw::new(m, 0)
            .with_hurt_overlay(true)
            .with_tint(lodestone_assets::equipment::UNDYED_LEATHER_RGB);
        assert_eq!(leather_hurt.tint, hurt_leather.tint);
        assert_eq!(leather_hurt.tint & 0x00FF_FFFF, 0x00A0_6540);
        assert_eq!((leather_hurt.tint >> 24) & 0xFF, HURT_OVERLAY_ALPHA_BYTE);
    }

    /// [`InstanceTint`] is the thing that stops the overlay flag from being a
    /// second, parallel slice nothing keeps in step with the tints. Both halves
    /// must survive the fold into one packed word, in both orders, and
    /// [`InstanceTint::NONE`] must be indistinguishable from the pre-overlay
    /// `NO_TINT` — otherwise every undyed mob in the world changes colour the
    /// day this type lands.
    #[test]
    fn instance_tint_carries_both_halves_into_one_packed_word() {
        let m = glam::Mat4::IDENTITY;
        let raw = |t: InstanceTint| t.apply(EntityInstanceRaw::new(m, 0)).tint;

        assert_eq!(raw(InstanceTint::NONE), NO_TINT);
        assert_eq!(InstanceTint::default(), InstanceTint::NONE);

        let leather = lodestone_assets::equipment::UNDYED_LEATHER_RGB;
        assert_eq!(raw(InstanceTint::rgb(leather)) & 0x00FF_FFFF, 0x00A0_6540);
        assert_eq!(raw(InstanceTint::rgb(leather)) >> 24, 0);

        // The case a parallel `&[bool]` gets wrong: a dyed *and* hurt instance.
        let both = InstanceTint::rgb(leather).with_hurt(true);
        assert_eq!(raw(both) & 0x00FF_FFFF, 0x00A0_6540);
        assert_eq!(raw(both) >> 24, HURT_OVERLAY_ALPHA_BYTE);

        // Hurt with no dye still leaves the texel's own colour alone.
        let hurt_only = InstanceTint::NONE.with_hurt(true);
        assert_eq!(raw(hurt_only) & 0x00FF_FFFF, NO_TINT);
        assert_eq!(raw(hurt_only) >> 24, HURT_OVERLAY_ALPHA_BYTE);
        assert_eq!(raw(hurt_only.with_hurt(false)), NO_TINT);
    }

    /// The uniform the entity shader's `Camera` struct maps onto: 80 bytes of
    /// camera (a `mat4x4` plus a `vec4`) then 48 of fog (three `vec4`s). If this
    /// ever stops matching the model pipeline's uniform, the two passes would fog
    /// differently and a mob would visibly detach from its background.
    #[test]
    fn camera_uniform_matches_the_model_pipelines_layout() {
        assert_eq!(core::mem::size_of::<EntityCameraUniform>(), 128);
        assert_eq!(
            core::mem::size_of::<EntityCameraUniform>(),
            core::mem::size_of::<crate::model_pipeline::ModelCameraUniform>()
        );
        assert_eq!(core::mem::size_of::<CameraUniform>(), 80);
    }

    /// A light byte supplied per instance must survive packing unchanged, and a
    /// caller that supplies none must get the full-bright fallback rather than
    /// black.
    #[test]
    fn instance_light_packs_and_defaults_full_bright() {
        let m = glam::Mat4::IDENTITY;
        assert_eq!(EntityInstanceRaw::new(m, 0).light, 0);
        assert_eq!(EntityInstanceRaw::new(m, 0xF0).light, 0xF0);
        assert_eq!(
            EntityInstanceRaw::from_mat4(m).light,
            u32::from(crate::entity::ENTITY_FULLBRIGHT)
        );
    }

    /// The sky-darken lane must round-trip, must read as full daylight when
    /// unset, and must not disturb any fog field — it rides a *spare* lane
    /// precisely so entities and terrain keep fogging on identical numbers.
    #[test]
    fn sky_darken_rides_a_spare_fog_lane_without_touching_fog() {
        let fog = crate::fog::FogUniform::new(
            &crate::fog::FogSettings::for_view_distance([0.1, 0.2, 0.3], 128.0, 0.5),
            [1.0, 2.0, 3.0],
        );
        let base = EntityCameraUniform {
            camera: CameraUniform {
                view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
                section_origin: [0.0; 4],
            },
            fog,
        };
        // Unset is the 0.0 sentinel, which reads as full daylight — not as the
        // 0.2 floor, which would black out every existing caller's mobs.
        assert_eq!(base.fog.end_enabled[SKY_DARKEN_LANE], 0.0);
        assert_eq!(base.sky_darken(), 1.0);

        let dark = base.with_sky_darken(0.24);
        assert!((dark.sky_darken() - 0.24).abs() < 1e-6);
        // Everything else is byte-identical: same eye, same colour+start, same
        // end and enabled flag.
        assert_eq!(dark.fog.eye, base.fog.eye);
        assert_eq!(dark.fog.color_start, base.fog.color_start);
        assert_eq!(dark.fog.end_enabled[0], base.fog.end_enabled[0]);
        assert_eq!(dark.fog.end_enabled[1], base.fog.end_enabled[1]);
        assert_eq!(dark.camera.view_proj, base.camera.view_proj);
        // And the struct did not grow, so it still matches the model pipeline.
        assert_eq!(core::mem::size_of::<EntityCameraUniform>(), 128);
    }

    #[test]
    fn from_mat4_is_column_major() {
        // A translation matrix: glam stores translation in the 4th column, so
        // the packed [3] row must carry it (column-major → model[3] is col 3).
        let m = glam::Mat4::from_translation(glam::Vec3::new(1.0, 2.0, 3.0));
        let raw = EntityInstanceRaw::from_mat4(m);
        assert_eq!(raw.model[3][0], 1.0);
        assert_eq!(raw.model[3][1], 2.0);
        assert_eq!(raw.model[3][2], 3.0);
        assert_eq!(raw.model[3][3], 1.0);
    }
}
