//! The model render pass: a depth-tested pipeline that consumes wide
//! [`ModelVertex`](crate::models::ModelVertex) geometry produced by
//! [`mesh_models`](crate::models::mesh_models) and samples the stitched block
//! atlas with the *baked* UVs each quad already carries.
//!
//! This is the counterpart to [`BlockPipeline`](crate::block::BlockPipeline).
//! The packed path exists for the full-cube majority; this path renders
//! everything a baked model can be — stairs, fences, cross-plants, fluids and
//! tinted cubes — because in the real asset pipeline *every* block is a model
//! (see the D1 note in [`crate::models`]). It is deliberately simpler than the
//! packed pipeline in one respect: [`BakedQuad`](lodestone_assets::BakedQuad)
//! UVs are already absolute normalised atlas coordinates, so there is no
//! sprite-rect indirection buffer — the vertex UV maps straight onto the atlas.
//!
//! # Tint
//!
//! A baked quad with a `tint_index` carries a **palette index** rather than the
//! raw model index: [`BlockModels::build`](crate::BlockModels::build) resolves
//! each tinted quad's real default (plains) colour — grass, foliage, dry-foliage,
//! water, the fixed constants, redstone levels — and interns it into a small
//! palette ([`BlockModels::tint_palette`](crate::BlockModels::tint_palette)). The
//! shader multiplies the sampled texel by `palette[tint]` (slot 255 is white, so
//! untinted quads pass through). This replaces the earlier single hardcoded
//! green, which collapsed every tinted source to one colour — the defect that
//! made leaves render grass-green and grass side-overlays over-saturated.
//! Per-*biome* tint (sampling the live biome instead of plains) is still a
//! follow-up; the palette is the plains default the colormaps resolve to.
//!
//! # Render layer
//!
//! The pipeline is built per [`RenderLayer`](crate::translucency::RenderLayer)
//! just like the packed one. A fragment whose sampled alpha is below a cutout
//! threshold is discarded, so cutout sprites (cross-plants, leaves) render
//! correctly on the opaque pass without a separate material.

use wgpu::util::DeviceExt;

use crate::anim::AnimSlotUniform;
use crate::block::{CameraUniform, DEPTH_FORMAT};
use crate::models::{ModelMesh, ModelVertex};
use crate::texture::GpuAtlas;
use crate::translucency::RenderLayer;

/// GPU-resident wide-model geometry: a vertex buffer, an index buffer and the
/// index count.
#[derive(Debug)]
pub struct GpuModelMesh {
    /// Vertex buffer of [`ModelVertex`].
    pub vertices: wgpu::Buffer,
    /// `u32` index buffer.
    pub indices: wgpu::Buffer,
    /// Number of indices to draw.
    pub index_count: u32,
}

impl GpuModelMesh {
    /// Upload a [`ModelMesh`], or `None` if it is empty (nothing to draw).
    #[must_use]
    pub fn upload(device: &wgpu::Device, mesh: &ModelMesh) -> Option<Self> {
        if mesh.indices.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-model-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-model-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuModelMesh {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
        })
    }
}

/// A depth-tested pipeline for wide baked-model geometry.
#[derive(Debug)]
pub struct ModelPipeline {
    /// The render pipeline.
    pub pipeline: wgpu::RenderPipeline,
    /// Bind-group layout for the camera uniform (group 0).
    pub camera_layout: wgpu::BindGroupLayout,
    /// Bind-group layout for the atlas texture + sampler (group 1).
    pub atlas_layout: wgpu::BindGroupLayout,
    /// Bind-group layout for the tint palette uniform (group 2). Present only on
    /// the model pipeline; the fluid pipeline carries its own tint and has none.
    pub palette_layout: Option<wgpu::BindGroupLayout>,
    /// Bind-group layout for the per-slot animation uniform array. Its group
    /// index depends on the pipeline: **3** on the model pipeline (after the
    /// palette), **2** on the fluid pipeline (which has no palette). Both shaders
    /// declare the matching `@group`.
    pub anim_layout: wgpu::BindGroupLayout,
}

/// The name of `model.wgsl`'s pipeline-overridable alpha-test threshold.
/// `alpha_cutout_override_is_declared_by_the_model_shader` keeps this from
/// drifting away from the shader, which would otherwise fail at pipeline
/// creation on a real device and nowhere else.
const ALPHA_CUTOUT_OVERRIDE: &str = "alpha_cutout";

/// Vanilla `RenderPipelines.CUTOUT_TERRAIN`'s
/// `withShaderDefine("ALPHA_CUTOUT", 0.5F)`. Also the shader's declared
/// default, and the value the opaque pass uses — that pass carries solid and
/// cutout geometry in one mesh, so it must take the stricter of the two.
const ALPHA_CUTOUT_CUTOUT: f32 = 0.5;

/// Vanilla `RenderPipelines.TRANSLUCENT_TERRAIN`'s
/// `withShaderDefine("ALPHA_CUTOUT", 0.1F)` — five times looser than the
/// cutout pass, and the whole reason this is a per-pipeline value: real
/// stained glass is a partial alpha in the low 0.4s, which a `0.5` test
/// deletes outright.
const ALPHA_CUTOUT_TRANSLUCENT: f32 = 0.1;

/// Polygon offset that moves a coincident world-space primitive toward the
/// camera in this renderer's ordinary `[0, 1]` depth convention.
///
/// Vanilla's depth convention is reversed, so its `(+1.0, +10)` polygon
/// offset becomes this sign-flipped `(slope, constant)` pair.  Keep the
/// magnitude in depth-buffer units rather than replacing it with a
/// world-space epsilon: the latter stops working at grazing angles and changes
/// meaning with camera distance.  Opaque overlays and geometry that shares a
/// physical contact plane (text, item-frame bodies, entity feet, selection
/// lines) use this same policy; translucent terrain deliberately does not.
pub const CAMERA_DEPTH_BIAS: wgpu::DepthBiasState = wgpu::DepthBiasState {
    constant: -10,
    slope_scale: -1.0,
    clamp: 0.0,
};

/// A second constant [`CAMERA_DEPTH_BIAS`] step toward the camera for a texture
/// that must win over the surface immediately behind it, such as a filled-map
/// picture over an item frame's front texture. Its slope term stays identical
/// to the frame body's: multiplying it would make the *relative* ordering vary
/// with projected slope, which shows as a curved/triangular floating edge at
/// grazing angles.
pub const MAP_SURFACE_DEPTH_BIAS: wgpu::DepthBiasState = wgpu::DepthBiasState {
    constant: CAMERA_DEPTH_BIAS.constant * 2,
    slope_scale: CAMERA_DEPTH_BIAS.slope_scale,
    clamp: CAMERA_DEPTH_BIAS.clamp,
};

impl ModelPipeline {
    /// Build the opaque (`Solid`) model pipeline targeting `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        Self::for_layer(device, color_format, RenderLayer::Solid)
    }

    /// Build the pipeline for a specific [`RenderLayer`]. `Solid`/`Cutout` use
    /// an opaque target with depth writes and back-face culling; `Translucent`
    /// enables alpha blending, disables depth writes, expects the caller to
    /// have sorted quads back-to-front, and **keeps back-face culling on** —
    /// see [`build`](Self::build)'s `cull_back_face` doc for why that is the
    /// vanilla-faithful choice here and not, e.g., for
    /// [`for_fluid`](Self::for_fluid).
    #[must_use]
    pub fn for_layer(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        layer: RenderLayer,
    ) -> Self {
        let translucent = layer == RenderLayer::Translucent;
        Self::build(
            device,
            color_format,
            MODEL_WGSL,
            translucent,
            true,
            true,
            Some(if translucent { ALPHA_CUTOUT_TRANSLUCENT } else { ALPHA_CUTOUT_CUTOUT }),
            wgpu::DepthBiasState::default(),
        )
    }

    /// Build the opaque/cutout pipeline for geometry that intentionally shares
    /// a surface with previously submitted world geometry, such as an item
    /// frame's block-model body or a moving block overlay. Layered pictures
    /// that must win over this surface use [`Self::for_map_surface`].
    #[must_use]
    pub fn for_surface(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        Self::build(
            device,
            color_format,
            MODEL_WGSL,
            false,
            true,
            true,
            Some(ALPHA_CUTOUT_CUTOUT),
            CAMERA_DEPTH_BIAS,
        )
    }

    /// Build the opaque/cutout pipeline for a map picture layered over an
    /// item-frame surface. It is one depth-bias step farther toward the eye
    /// than [`Self::for_surface`], so the picture wins against the frame's
    /// front texture without moving either mesh in world space.
    #[must_use]
    pub fn for_map_surface(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        Self::for_map_surface_diagnostic(device, color_format, true, true)
    }

    /// Build a map-surface variant for a narrowly scoped live diagnostic.
    ///
    /// The caller selects this only when one of Lodestone's `LODESTONE_MAP_*`
    /// switches is set. It exists to eliminate a single GPU boundary from a
    /// report; normal item-frame maps always use [`Self::for_map_surface`].
    #[must_use]
    pub fn for_map_surface_diagnostic(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        cull_back_face: bool,
        use_depth: bool,
    ) -> Self {
        Self::build_with_depth(
            device,
            color_format,
            MODEL_WGSL,
            false,
            true,
            cull_back_face,
            Some(ALPHA_CUTOUT_CUTOUT),
            MAP_SURFACE_DEPTH_BIAS,
            use_depth,
        )
    }

    /// Build a map-surface pipeline with the normal depth state and a
    /// back-face culling diagnostic selected by [`Self::for_map_surface_diagnostic`].
    #[must_use]
    pub fn for_map_surface_no_cull(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        Self::for_map_surface_diagnostic(device, color_format, false, true)
    }

    /// Build a map-surface pipeline that neither tests nor writes depth, for
    /// diagnosis only. It is intentionally never used by default because it
    /// paints through the frame and intervening world geometry.
    #[must_use]
    pub fn for_map_surface_no_depth(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        Self::for_map_surface_diagnostic(device, color_format, true, false)
    }

    /// Build the combined no-cull/no-depth map diagnostic variant.
    #[must_use]
    pub fn for_map_surface_no_cull_no_depth(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
    ) -> Self {
        Self::for_map_surface_diagnostic(device, color_format, false, false)
    }

    /// Build the translucent **fluid** pipeline: like a `Translucent` model
    /// pipeline (alpha blending, no depth writes) but with a shader that does
    /// **not** cutout-discard (water is a smooth alpha, not a mask) and tints
    /// `tint_index` quads with the water colour instead of foliage green.
    /// Drawn after opaque terrain so the sea floor already in the depth
    /// buffer shows through.
    ///
    /// Back-face culling stays **off** here, unlike [`for_layer`](Self::for_layer)'s
    /// `Translucent` — unverified against vanilla's own fluid geometry and
    /// deliberately left unchanged rather than folded into the [`for_layer`]
    /// fix below.
    #[must_use]
    pub fn for_fluid(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        // `None`: `fluid.wgsl` declares no `alpha_cutout` override (water is a
        // smooth alpha, not a mask, and it runs no discard at all), and wgpu
        // rejects a constant the module does not declare.
        Self::build(
            device,
            color_format,
            FLUID_WGSL,
            true,
            false,
            false,
            None,
            wgpu::DepthBiasState::default(),
        )
    }

    /// `cull_back_face` diverges from `translucent` on purpose — they used to
    /// be the same flag, which was the bug.
    ///
    /// Vanilla's `RenderPipelines.TRANSLUCENT_TERRAIN`/`TRANSLUCENT_BLOCK`
    /// both build on `TERRAIN_SNIPPET`/`BLOCK_SNIPPET`, neither of which ever
    /// calls `.withCull(false)` — and `RenderPipeline.Builder`'s own default
    /// is `this.cull.orElse(true)`. So real translucent terrain, ice and
    /// glass included, renders **single-sided** exactly like opaque terrain;
    /// nothing in the real pipeline chain disables culling for them. This
    /// pipeline used to set `cull_mode: None` whenever `translucent` was
    /// true, which draws **both** faces of a solid cube (e.g. ice's `Up` and
    /// `Down` quads) along any view ray that passes through it, double-
    /// compositing the same partial alpha and reading as far more opaque
    /// than a single vanilla-correct blend — the owner's report that ice
    /// "shows no opacity at all" looking down through it.
    ///
    /// This is safe to flip for the model path specifically because
    /// non-cube translucent geometry here is already baked **two-sided at
    /// the model level**, not relying on the GPU state at all — vanilla's
    /// own pattern for thin planes. Measured: `nether_portal_ew.json` (the
    /// real 26.2 model) bakes explicit `east` *and* `west` quads with no
    /// `cullface` on either, so single-sided culling still shows the swirl
    /// from both sides; it was never the disabled cull state doing that
    /// work. `for_fluid` is deliberately left at its prior `cull_mode: None`
    /// — fluid geometry's own two-sidedness (`docs/fluid-rendering.md`'s
    /// `addBackFace`) was not audited here and this fix does not touch it.
    ///
    /// `alpha_cutout` is the value bound to `model.wgsl`'s `alpha_cutout`
    /// pipeline-overridable constant — vanilla's per-pipeline
    /// `withShaderDefine("ALPHA_CUTOUT", ..)`. `None` is for a shader that does
    /// not declare it (the fluid one); wgpu rejects a constant the module has
    /// no override for.
    #[allow(clippy::too_many_arguments)]
    fn build(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        shader_src: &str,
        translucent: bool,
        with_palette: bool,
        cull_back_face: bool,
        alpha_cutout: Option<f32>,
        depth_bias: wgpu::DepthBiasState,
    ) -> Self {
        Self::build_with_depth(
            device,
            color_format,
            shader_src,
            translucent,
            with_palette,
            cull_back_face,
            alpha_cutout,
            depth_bias,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_with_depth(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        shader_src: &str,
        translucent: bool,
        with_palette: bool,
        cull_back_face: bool,
        alpha_cutout: Option<f32>,
        depth_bias: wgpu::DepthBiasState,
        use_depth: bool,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-model-shader"),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        // Two bindings, not one: binding 0 is the *shared* per-frame half
        // (view-projection + fog), identical for every section and every other
        // consumer of this pipeline (dropped items, the held item); binding 1
        // is the per-section world origin, selected per draw by a **dynamic
        // offset** into one physically resident buffer. Splitting them is the
        // fix for issue #75 — profiling a live session found `render_inner`
        // rewriting *every* section's whole camera uniform (view_proj bytes
        // included) every frame, ~4000 `queue.write_buffer` calls landing in
        // `RenderState::render`'s hot path (52.9% of main-thread CPU, mostly
        // `StagingBuffer::new`/`create_buffer`). `section_origin` is constant
        // for a section's life, so it only needs writing once, at upload; only
        // `view_proj`/fog actually change per frame, and there is exactly one
        // of those. See `docs/section-camera-uniform.md`.
        //
        // This still fits the pipeline's four-bind-group floor: it is a second
        // *binding* inside the existing group 0, not a fifth group.
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-model-camera-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // Vertex reads the view-projection; fragment reads the folded
                    // fog block (eye, colour, range), so the group must be
                    // visible to both.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    // The origin only ever feeds `world = position + origin.xyz`
                    // in the vertex stage.
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(
                            core::mem::size_of::<SectionOriginUniform>() as u64,
                        ),
                    },
                    count: None,
                },
            ],
        });

        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-model-atlas-bgl"),
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

        let palette_layout = with_palette.then(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("lodestone-model-palette-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            })
        });

        let anim_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-model-anim-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // The vertex stage reads the per-slot V offsets and passes them
                // (flat) to the fragment stage, so both stages bind it.
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let mut bind_group_layouts = vec![Some(&camera_layout), Some(&atlas_layout)];
        if let Some(palette_layout) = &palette_layout {
            bind_group_layouts.push(Some(palette_layout));
        }
        bind_group_layouts.push(Some(&anim_layout));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-model-layout"),
            bind_group_layouts: &bind_group_layouts,
            immediate_size: 0,
        });

        // A slice rather than an `Option` at the use site: an empty slice is
        // exactly "leave every override at its declared default", which is what
        // a shader with no override wants.
        let constants: Vec<(&str, f64)> = alpha_cutout
            .map(|v| vec![(ALPHA_CUTOUT_OVERRIDE, f64::from(v))])
            .unwrap_or_default();

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-model-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(ModelVertex::vertex_layout_with_biome_tint())],
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: if translucent {
                        Some(wgpu::BlendState::ALPHA_BLENDING)
                    } else {
                        None
                    },
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: if cull_back_face {
                    Some(wgpu::Face::Back)
                } else {
                    None
                },
                ..Default::default()
            },
            depth_stencil: use_depth.then_some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(!translucent),
                // Vanilla's terrain pipelines all inherit
                // `DepthStencilState.DEFAULT = (GREATER_THAN_OR_EQUAL, true)`
                // (26.2 `RenderPipelines.TERRAIN_SNIPPET` →
                // `GENERIC_BLOCKS_SNIPPET.withDepthStencilState(DEFAULT)`), and
                // under reversed-Z that comparison *includes* equality. Our depth
                // is `[0, 1]` DirectX-style, so the faithful port is `LessEqual`,
                // not `Less`.
                //
                // This is not cosmetic. A model may place two elements at exactly
                // the same coordinates and rely on the later one winning:
                // `grass_block.json` puts `#overlay` on top of `#side` at the same
                // `[0,0,0]..[16,16,16]` box, so a strict `Less` rejects every
                // overlay quad and a grass block's sides lose their tinted fringe.
                // Measured: `grass_block` bakes 10 quads, 4 of which are tinted
                // overlays coplanar with the base cube's sides.
                //
                // The translucent variant keeps `Less` deliberately, because we
                // diverge from vanilla on the *other* field: vanilla's
                // `TRANSLUCENT_TERRAIN` writes depth and we do not
                // (`depth_write_enabled: !translucent`). `LessEqual` without a
                // depth write lets two coplanar translucent quads both blend,
                // double-darkening a water surface — an artefact vanilla's depth
                // write suppresses. Restore `LessEqual` here if translucent depth
                // writes are ever restored too.
                depth_compare: Some(if translucent {
                    wgpu::CompareFunction::Less
                } else {
                    wgpu::CompareFunction::LessEqual
                }),
                stencil: wgpu::StencilState::default(),
                bias: depth_bias,
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        ModelPipeline {
            pipeline,
            camera_layout,
            atlas_layout,
            palette_layout,
            anim_layout,
        }
    }

    /// Create the group-0 bind group from the shared per-frame buffer
    /// (binding 0: view-projection + fog, built with
    /// [`model_shared_camera_buffer`] or [`model_shared_camera_buffer_with_fog`])
    /// and an origin buffer (binding 1, dynamic offset).
    ///
    /// `origin_buffer` may be a single [`SectionOriginUniform`] slot (one-off
    /// draws: a dropped item, the held item, a test's synthetic section) or a
    /// large arena backing many sections at different offsets — the *window*
    /// bound here is always one `SectionOriginUniform` (16 bytes); a caller
    /// addressing many sections through one arena builds this bind group
    /// **once** and picks a section by the dynamic offset passed to
    /// `set_bind_group`, not by rebuilding the bind group.
    #[must_use]
    pub fn camera_bind_group(
        &self,
        device: &wgpu::Device,
        shared_buffer: &wgpu::Buffer,
        origin_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-model-camera-bg"),
            layout: &self.camera_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: shared_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: origin_buffer,
                        offset: 0,
                        size: wgpu::BufferSize::new(
                            core::mem::size_of::<SectionOriginUniform>() as u64
                        ),
                    }),
                },
            ],
        })
    }

    /// Create the atlas bind group from a [`GpuAtlas`] (texture + sampler only).
    #[must_use]
    pub fn atlas_bind_group(&self, device: &wgpu::Device, atlas: &GpuAtlas) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-model-atlas-bg"),
            layout: &self.atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&atlas.sampler),
                },
            ],
        })
    }

    /// Create the tint-palette bind group (group 2) from a palette uniform
    /// buffer built with [`model_palette_buffer`].
    ///
    /// # Panics
    ///
    /// Panics if called on a pipeline built without a palette (the fluid
    /// pipeline); only the model pipeline carries group 2.
    #[must_use]
    pub fn palette_bind_group(
        &self,
        device: &wgpu::Device,
        palette_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let layout = self
            .palette_layout
            .as_ref()
            .expect("palette_bind_group requires a pipeline built with a palette");
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-model-palette-bg"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: palette_buffer.as_entire_binding(),
            }],
        })
    }

    /// Create the animation bind group from the per-slot uniform buffer built
    /// with [`model_anim_buffer`]. Group **3** on the model pipeline, group
    /// **2** on the fluid pipeline (see [`Self::anim_layout`]).
    #[must_use]
    pub fn anim_bind_group(
        &self,
        device: &wgpu::Device,
        anim_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-model-anim-bg"),
            layout: &self.anim_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: anim_buffer.as_entire_binding(),
            }],
        })
    }
}

/// Build the tint-palette uniform buffer the model shader looks up per tinted
/// quad. `palette` is [`BlockModels::tint_palette`](crate::BlockModels::tint_palette)
/// — one straight RGBA multiplier per palette index.
#[must_use]
pub fn model_palette_buffer(device: &wgpu::Device, palette: &[[f32; 4]]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lodestone-model-palette-uniform"),
        contents: bytemuck::cast_slice(palette),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

/// The fixed number of animation slots the shaders' uniform array holds. A quad's
/// one-byte `anim` (0 = static, `1..=255` = a sprite slot) indexes this array, so
/// it must cover the whole `u8` range plus the static sentinel at index 0.
pub const ANIM_SLOT_UNIFORM_LEN: usize = 256;

/// Build the per-slot animation uniform buffer the model/fluid shaders sample.
///
/// `slots` is [`BlockModels::anim_slot_uniforms`](crate::BlockModels::anim_slot_uniforms)
/// for the current tick: index 0 is the static sentinel, index `s` is slot `s`.
/// It is padded to [`ANIM_SLOT_UNIFORM_LEN`] (extra slots are static no-ops) so
/// the buffer size is fixed and a quad's `anim` byte can never index out of
/// range. Rewrite it each frame via [`update_model_anim_buffer`].
#[must_use]
pub fn model_anim_buffer(device: &wgpu::Device, slots: &[AnimSlotUniform]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lodestone-model-anim-uniform"),
        contents: bytemuck::cast_slice(&padded_anim_slots(slots)),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

/// Rewrite an existing animation uniform buffer with a new tick's slot values.
pub fn update_model_anim_buffer(
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    slots: &[AnimSlotUniform],
) {
    queue.write_buffer(buffer, 0, bytemuck::cast_slice(&padded_anim_slots(slots)));
}

/// Pad (or truncate) a slot list to the fixed [`ANIM_SLOT_UNIFORM_LEN`], filling
/// the tail with static no-op slots.
fn padded_anim_slots(slots: &[AnimSlotUniform]) -> [AnimSlotUniform; ANIM_SLOT_UNIFORM_LEN] {
    let mut out = [AnimSlotUniform::static_slot(); ANIM_SLOT_UNIFORM_LEN];
    let n = slots.len().min(ANIM_SLOT_UNIFORM_LEN);
    out[..n].copy_from_slice(&slots[..n]);
    out
}

/// Reuse the packed pass's camera uniform buffer builder for the model pass; the
/// [`CameraUniform`] layout is identical.
///
/// The buffer is sized for a [`ModelCameraUniform`] — camera data followed by a
/// **disabled** [`FogUniform`] — so the model/fluid shaders' single group-0
/// binding carries both the view-projection and the distance fog. Callers that
/// want fog build the buffer with [`model_camera_buffer_with_fog`] or overwrite
/// it each frame with a full `ModelCameraUniform`.
///
/// **Legacy, single-binding shape.** [`ModelPipeline::camera_layout`] now has
/// *two* bindings (see [`ModelPipeline::camera_bind_group`]), so this builder
/// and [`ModelCameraUniform`] are no longer wired to that bind group. They
/// remain for [`CrackPipeline`](crate::crack_pipeline::CrackPipeline), whose own
/// (unrelated) single-binding layout still expects one buffer carrying camera,
/// origin and fog together, and whose crack overlay never has more than one
/// draw's worth of state to write, so there was nothing to fix there.
#[must_use]
pub fn model_camera_buffer(device: &wgpu::Device, uniform: CameraUniform) -> wgpu::Buffer {
    model_camera_buffer_with_fog(device, uniform, crate::fog::FogUniform::disabled())
}

/// Build the group-0 uniform buffer for the model/fluid pass with an explicit
/// fog block. `fog` fades distant fragments toward the fog colour; pass
/// [`FogUniform::disabled`](crate::fog::FogUniform::disabled) to turn fog off.
///
/// See [`model_camera_buffer`]'s doc: legacy shape, kept for
/// [`CrackPipeline`](crate::crack_pipeline::CrackPipeline).
#[must_use]
pub fn model_camera_buffer_with_fog(
    device: &wgpu::Device,
    camera: CameraUniform,
    fog: crate::fog::FogUniform,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lodestone-model-camera-uniform"),
        contents: bytemuck::bytes_of(&ModelCameraUniform { camera, fog }),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

/// The group-0 uniform for the model and fluid pipelines: the per-section
/// [`CameraUniform`] followed by the per-frame [`FogUniform`]. Folding fog into
/// the camera group (rather than giving it its own bind group) keeps the model
/// shader within the portable `max_bind_groups` floor of 4 — camera, atlas,
/// palette and animation already occupy four groups on Metal's guaranteed
/// minimum. Rewrite the whole struct each frame via [`queue.write_buffer`].
///
/// See [`model_camera_buffer`]'s doc: legacy shape, kept for
/// [`CrackPipeline`](crate::crack_pipeline::CrackPipeline). Live terrain uses
/// [`ModelSharedCameraUniform`] + [`SectionOriginUniform`] instead.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ModelCameraUniform {
    /// View-projection and section origin (group 0, first 80 bytes).
    pub camera: CameraUniform,
    /// Distance fog for this frame (eye position, colour, start/end).
    pub fog: crate::fog::FogUniform,
}

/// The *shared* half of the model/fluid group-0 uniform (binding 0):
/// view-projection plus this frame's fog, identical for every section drawn
/// this frame. Paired with [`SectionOriginUniform`] at binding 1, which varies
/// per section and is addressed by a dynamic offset instead of being part of
/// this struct.
///
/// This split is the fix for issue #75: a live-play profile found
/// `RenderState::render_inner` rewriting a *whole* per-section camera uniform
/// (view_proj bytes included) via `queue.write_buffer` once per section, every
/// frame — up to ~4000 calls/frame at the measured `sections=3880`, and 52.9%
/// of main-thread CPU (mostly `StagingBuffer::new` → `create_buffer`). Only
/// `view_proj`/fog actually change frame to frame; `section_origin` is the
/// section's fixed world position and is constant for its whole lifetime. This
/// struct is written **once per frame**, not once per section.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ModelSharedCameraUniform {
    /// Column-major view-projection matrix, shared by every section this
    /// frame.
    pub view_proj: [[f32; 4]; 4],
    /// Distance fog for this frame (eye position, colour, start/end).
    pub fog: crate::fog::FogUniform,
}

/// Vanilla's `chunkSectionFadeInTime` default (`Options.java`'s
/// `OptionInstance<Double>` of that name: range `0.0..=2.0` seconds, shipped
/// default `0.75`). This client has no video-settings UI to expose the option
/// yet, so it is hardcoded exactly like `model.wgsl`'s own `BRIGHTNESS_FACTOR`
/// — see that shader's matching constant, which must move with this one.
pub const SECTION_FADE_DURATION_SECS: f32 = 0.75;

/// Sentinel `build_time` for a section that must never fade: far enough in the
/// past that `now - sentinel` clears [`SECTION_FADE_DURATION_SECS`] for any
/// clock value a real session reaches, including `now == 0.0` at startup —
/// unlike `f32::NEG_INFINITY`, arithmetic on it cannot produce `inf`/`NaN`
/// further down the shader. [`SectionOriginUniform::new`] defaults to this, so
/// every one-off caller (dropped items, the first-person held item, the
/// arena's reserved zero slot, tests) renders at full visibility exactly as it
/// did before the fade existed.
pub const SECTION_FADE_ALREADY_VISIBLE: f32 = -1.0e6;

/// Byte-for-byte the mix `model.wgsl`/`fluid.wgsl` compute per section:
/// vanilla's `SectionRenderDispatcher.RenderSection.getVisibility` —
/// `elapsed >= duration ? 1.0 : elapsed / duration`, written here as an
/// equivalent clamp so the CPU-side prediction in this crate's tests and the
/// shader's own arithmetic can be checked against the same formula.
#[must_use]
pub fn section_visibility(now_secs: f32, build_time_secs: f32) -> f32 {
    let elapsed = now_secs - build_time_secs;
    (elapsed / SECTION_FADE_DURATION_SECS).clamp(0.0, 1.0)
}

/// Vanilla's `isNearby` test from `LevelRenderer.compileSections`
/// (`double distSqr = center.distSqr(cameraPosition); boolean isNearby =
/// distSqr < 768.0;`) — a section within this squared-distance of the camera
/// never fades, regardless of whether it is a genuinely new build.
///
/// `768.0` is a squared block distance (not a radius), so the true cutoff is
/// `sqrt(768) ≈ 27.7` blocks from the section's centre. Deliberately integer,
/// block-granularity input on both sides, exactly matching vanilla's own
/// `BlockPos`-typed `center`/`cameraPosition` — there is no sub-block
/// precision to lose by rounding the camera down first.
#[must_use]
pub fn section_is_nearby(section_origin: [i32; 3], camera_block_pos: [i32; 3]) -> bool {
    // `SectionPos.of(pos).center()`: the section's own middle block, i.e. its
    // minimum corner plus half of its 16-block span.
    let center = [
        section_origin[0] + 8,
        section_origin[1] + 8,
        section_origin[2] + 8,
    ];
    let dx = i64::from(center[0] - camera_block_pos[0]);
    let dy = i64::from(center[1] - camera_block_pos[1]);
    let dz = i64::from(center[2] - camera_block_pos[2]);
    let dist_sqr = dx * dx + dy * dy + dz * dz;
    dist_sqr < 768
}

/// A section's world-space origin (group 0 binding 1): `vec4(origin.xyz,
/// build_time)`, added to the section-local vertex position in the vertex
/// shader; `build_time` feeds the per-section fade-in (see
/// [`section_visibility`]).
///
/// Bound with a **dynamic offset**, so one physically resident buffer (an
/// arena of these, or a single slot for a one-off draw) serves every section:
/// written once when the section is uploaded — the origin never changes for a
/// section's lifetime — and selected per draw by the offset passed to
/// `wgpu::RenderPass::set_bind_group`, not by rebuilding a bind group.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SectionOriginUniform {
    /// `xyz` = the section's world-space origin; `w` = this section's fade
    /// `build_time`, in the same clock units as `Camera.fog_ambient_light.w`
    /// (seconds). [`SECTION_FADE_ALREADY_VISIBLE`] for a section that must
    /// never fade.
    pub origin: [f32; 4],
}

impl SectionOriginUniform {
    /// Build from a plain `[x, y, z]` world origin, defaulted to
    /// [`SECTION_FADE_ALREADY_VISIBLE`] — the safe choice for every caller
    /// that does not know or care about the fade (see that constant's doc).
    #[must_use]
    pub const fn new(origin: [f32; 3]) -> Self {
        Self {
            origin: [origin[0], origin[1], origin[2], SECTION_FADE_ALREADY_VISIBLE],
        }
    }

    /// Build with an explicit fade `build_time`, for a genuinely new section
    /// that should fade in from the fog colour. See [`section_visibility`].
    #[must_use]
    pub const fn with_build_time(origin: [f32; 3], build_time_secs: f32) -> Self {
        Self {
            origin: [origin[0], origin[1], origin[2], build_time_secs],
        }
    }
}

/// Build the shared group-0 buffer (binding 0) with fog disabled. Rewrite it
/// each frame via [`update_model_shared_camera_buffer`] — this is now the
/// **only** per-frame write the model/fluid camera group needs, however many
/// sections are resident.
#[must_use]
pub fn model_shared_camera_buffer(device: &wgpu::Device, view_proj: [[f32; 4]; 4]) -> wgpu::Buffer {
    model_shared_camera_buffer_with_fog(device, view_proj, crate::fog::FogUniform::disabled())
}

/// Build the shared group-0 buffer (binding 0) with an explicit fog block.
#[must_use]
pub fn model_shared_camera_buffer_with_fog(
    device: &wgpu::Device,
    view_proj: [[f32; 4]; 4],
    fog: crate::fog::FogUniform,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lodestone-model-shared-camera-uniform"),
        contents: bytemuck::bytes_of(&ModelSharedCameraUniform { view_proj, fog }),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

/// Rewrite the shared group-0 buffer for a new frame. One call replaces what
/// used to be one `queue.write_buffer` per **section**, per frame.
pub fn update_model_shared_camera_buffer(
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    view_proj: [[f32; 4]; 4],
    fog: crate::fog::FogUniform,
) {
    queue.write_buffer(
        buffer,
        0,
        bytemuck::bytes_of(&ModelSharedCameraUniform { view_proj, fog }),
    );
}

/// Build a single-slot origin buffer (binding 1) for a one-off draw that does
/// not need the shared multi-section arena: a dropped item, the held item, or
/// a test's synthetic section. Real terrain instead shares one arena of many
/// slots across all resident sections — see `SectionOriginArena` in
/// `lodestone-shell`'s `gpu.rs`.
#[must_use]
pub fn section_origin_buffer(device: &wgpu::Device, origin: [f32; 3]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lodestone-model-section-origin"),
        contents: bytemuck::bytes_of(&SectionOriginUniform::new(origin)),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

/// Rewrite a section-origin slot (any buffer built to hold one or more
/// [`SectionOriginUniform`]s, at `offset`). Real sections call this exactly
/// once, at upload — the origin is constant for the section's lifetime — never
/// per frame.
///
/// `build_time_secs` is this section's fade start time (see
/// [`section_visibility`]); pass [`SECTION_FADE_ALREADY_VISIBLE`] for a
/// section that must never fade (the reserved zero slot, a one-off draw).
pub fn write_section_origin(
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    offset: u64,
    origin: [f32; 3],
    build_time_secs: f32,
) {
    queue.write_buffer(
        buffer,
        offset,
        bytemuck::bytes_of(&SectionOriginUniform::with_build_time(origin, build_time_secs)),
    );
}

const MODEL_WGSL: &str = include_str!("shaders/model.wgsl");

// The fluid (water) shader. Unlike the model shader it does **not** discard on
// low alpha — water is a smooth translucent surface, not a cutout mask — and it
// tints `tint_index` quads with the default water colour (#3F76E4) rather than
// foliage green. Water's greyscale texture becomes blue here.
const FLUID_WGSL: &str = include_str!("shaders/fluid.wgsl");

/// The fluid pass's anti-z-fight depth nudge, in window-depth (`z / w`) units —
/// the value `shaders/fluid.wgsl` adds to every fluid fragment's depth.
///
/// Restated here so a gate can measure it against the real
/// [`Camera::view_projection`](crate::Camera::view_projection) without parsing
/// WGSL; [`FLUID_DEPTH_NUDGE_LITERAL`] and
/// `fluid_depth_nudge_matches_the_shader` are what stop the two drifting. The
/// shader's own comment carries the derivation and the measurement — read that,
/// not this.
///
/// `2^-21`, so it is exactly 8 float32 ULPs at any depth in `[0.5, 1)` and more
/// below it. Positive is away from the camera under this project's `[0,1]`
/// depth convention.
pub const FLUID_DEPTH_NUDGE: f32 = 4.768_371_582_031_25e-7;

/// The exact text of [`FLUID_DEPTH_NUDGE`] as it appears in `fluid.wgsl`. Two
/// copies of a number in two languages is the drift this pairing exists to
/// catch.
const FLUID_DEPTH_NUDGE_LITERAL: &str = "4.76837158203125e-7";

#[cfg(test)]
mod tests {
    use super::*;

    /// The Rust constant and the WGSL constant must be the same number.
    ///
    /// `cargo check` never compiles a shader and no pixel gate can tell a
    /// slightly-too-small nudge from a correct one, so this string match is the
    /// only thing tying the measured value to the one that actually ships.
    #[test]
    fn fluid_depth_nudge_matches_the_shader() {
        let decl = format!("const FLUID_DEPTH_NUDGE: f32 = {FLUID_DEPTH_NUDGE_LITERAL};");
        assert!(
            FLUID_WGSL.contains(&decl),
            "fluid.wgsl does not declare `{decl}` — the shader and \
             `FLUID_DEPTH_NUDGE` have drifted apart"
        );
        // The literal really is this constant, so the string check above is not
        // comparing a typo against itself.
        assert_eq!(
            FLUID_DEPTH_NUDGE_LITERAL.parse::<f32>().expect("a float literal"),
            FLUID_DEPTH_NUDGE
        );
        // And it is the power of two the ULP guarantee depends on: exactly 8
        // ULPs at 1.0's exponent. Computed from `f32::EPSILON` (the gap between
        // 1.0 and its successor, i.e. 2 ULPs at the [0.5, 1) exponent) rather
        // than restated, so a hand-edited digit cannot pass.
        assert_eq!(FLUID_DEPTH_NUDGE, f32::EPSILON * 4.0);
        // Control: the assertion above would reject a value that merely looks
        // similar, so the nudge cannot be silently halved or doubled.
        assert_ne!(FLUID_DEPTH_NUDGE, f32::EPSILON * 2.0);
        assert_ne!(FLUID_DEPTH_NUDGE, f32::EPSILON * 8.0);
    }

    /// The nudge must push **away** from the camera, which is *positive* under
    /// this project's `[0,1]` depth. The two pipelines that pull toward the
    /// camera (crack, sign text) use negative biases; getting this sign
    /// backwards would make the water win every coplanar contest instead of
    /// losing it, and a screenshot of a blue film over a stair looks much like a
    /// z-fight that has stopped moving.
    #[test]
    fn fluid_depth_nudge_pushes_away_from_the_camera() {
        assert!(FLUID_DEPTH_NUDGE > 0.0);
        assert!(FLUID_WGSL.contains("out.clip.z = out.clip.z + FLUID_DEPTH_NUDGE * out.clip.w;"));
    }

    /// The Rust constant and both WGSL copies must be the same number, or the
    /// CPU-side prediction below describes a fade the GPU does not actually
    /// run. `cargo check` never compiles a shader (see `DESIGN.md`'s note on
    /// `no_wgsl_is_inlined_in_rust_sources`), so this string match is what
    /// ties the measured constant to what ships.
    #[test]
    fn section_fade_duration_matches_the_shader() {
        let decl = format!("const SECTION_FADE_DURATION_SECS: f32 = {SECTION_FADE_DURATION_SECS};");
        assert!(
            MODEL_WGSL.contains(&decl),
            "model.wgsl does not declare `{decl}` — the shader and \
             SECTION_FADE_DURATION_SECS have drifted apart"
        );
        assert!(
            FLUID_WGSL.contains(&decl),
            "fluid.wgsl does not declare `{decl}` — the shader and \
             SECTION_FADE_DURATION_SECS have drifted apart"
        );
        // Vanilla's real default (`Options.java`'s `chunkSectionFadeInTime`,
        // 0.75 of its 0.0..=2.0 range) — not a plausible round number typed
        // from memory.
        assert_eq!(SECTION_FADE_DURATION_SECS, 0.75);
    }

    /// `section_visibility` at three points: the instant a section is built
    /// (must read as the fog colour, not a snap-in), the middle of the fade
    /// (the discriminating instant — a gate that only samples the two ends
    /// cannot tell a real fade from a delayed pop), and past the duration
    /// (fully materialised). Exact equalities throughout: this is a plain
    /// linear mix with no alpha blending involved (see `model.wgsl`'s
    /// comment on the fade being an RGB mix, not an alpha fade), so unlike
    /// this codebase's `ALPHA_BLENDING` gates there is no backend-dependent
    /// compositing to bracket — the predicted value is the only value.
    #[test]
    fn section_visibility_at_start_middle_and_end() {
        let build_time = 100.0_f32;
        assert_eq!(section_visibility(build_time, build_time), 0.0, "t=0: pure fog colour");
        assert_eq!(
            section_visibility(build_time + SECTION_FADE_DURATION_SECS * 0.5, build_time),
            0.5,
            "t=duration/2: the mid-fade discriminator"
        );
        assert_eq!(
            section_visibility(build_time + SECTION_FADE_DURATION_SECS, build_time),
            1.0,
            "t=duration: fully materialised"
        );
        // Past the duration, visibility saturates rather than overshooting —
        // vanilla's `elapsed >= fadeDuration ? 1.0 : ...` branch, reproduced
        // here by `clamp` rather than by the branch itself.
        assert_eq!(
            section_visibility(build_time + SECTION_FADE_DURATION_SECS * 10.0, build_time),
            1.0
        );
        // A negative delta (clock skew, or a section reused before its build
        // time is written) must not go negative and invert the mix.
        assert_eq!(section_visibility(build_time - 1.0, build_time), 0.0);
    }

    /// [`section_is_nearby`] against vanilla's own boundary
    /// (`LevelRenderer.compileSections`'s `distSqr < 768.0`), predicted
    /// exactly rather than merely signed: `sqrt(768)` is irrational
    /// (`≈27.712...`), so the discriminating pair here is chosen on the
    /// **squared** distance directly — `767` (just inside) and `768` (exactly
    /// on the excluded boundary, since vanilla's test is strict `<`) — not on
    /// a rounded block count, which is exactly the "predict the plausible
    /// round number" trap this repo's own evidence standard warns against.
    #[test]
    fn section_is_nearby_matches_vanillas_exact_768_squared_boundary() {
        let origin = [0, 64, 0];
        // Section centre is [8, 72, 8]. A camera offset of [27, 8, 8] from the
        // centre gives dx=27 (squared 729) plus dy=dz=0 for a total of 729 —
        // comfortably inside. Use a mixed-axis offset instead so all three
        // axes contribute, matching how a real camera position looks.
        let camera_dist_sqr = |offset: [i32; 3]| -> i64 {
            let dx = i64::from(offset[0]);
            let dy = i64::from(offset[1]);
            let dz = i64::from(offset[2]);
            dx * dx + dy * dy + dz * dz
        };
        // 21^2 + 15^2 + 5^2 = 441 + 225 + 25 = 691 < 768: nearby.
        let close_offset = [21, 15, 5];
        assert_eq!(camera_dist_sqr(close_offset), 691);
        let camera_close = [
            origin[0] + 8 + close_offset[0],
            origin[1] + 8 + close_offset[1],
            origin[2] + 8 + close_offset[2],
        ];
        assert!(
            section_is_nearby(origin, camera_close),
            "distSqr=691 must read as nearby (< 768)"
        );

        // 24^2 + 16^2 + 8^2 = 576 + 256 + 64 = 896 >= 768: not nearby.
        let far_offset = [24, 16, 8];
        assert_eq!(camera_dist_sqr(far_offset), 896);
        let camera_far = [
            origin[0] + 8 + far_offset[0],
            origin[1] + 8 + far_offset[1],
            origin[2] + 8 + far_offset[2],
        ];
        assert!(
            !section_is_nearby(origin, camera_far),
            "distSqr=896 must read as not-nearby (>= 768)"
        );

        // The camera standing exactly at the section's own centre: distSqr=0,
        // the least-ambiguous "nearby" case there is.
        let center = [origin[0] + 8, origin[1] + 8, origin[2] + 8];
        assert!(section_is_nearby(origin, center));
    }

    /// Vanilla's test is strict `<`, so a section sitting **exactly** on the
    /// boundary (`distSqr == 768`) must NOT be treated as nearby — the
    /// off-by-one a `<=` typo would introduce, and a case the mixed-offset
    /// test above does not exercise since 767/896 both land strictly off the
    /// line.
    #[test]
    fn section_is_nearby_excludes_the_exact_boundary() {
        // 16^2 + 16^2 + 16^2 = 256*3 = 768 exactly, all on one axis pair
        // chosen so the arithmetic is easy to re-derive by hand.
        let origin = [0, 0, 0];
        let camera = [8 + 16, 8 + 16, 8 + 16];
        let dx = i64::from(camera[0] - 8);
        let dy = i64::from(camera[1] - 8);
        let dz = i64::from(camera[2] - 8);
        assert_eq!(dx * dx + dy * dy + dz * dz, 768);
        assert!(
            !section_is_nearby(origin, camera),
            "distSqr==768 sits exactly on vanilla's excluded boundary (strict <)"
        );

        // A point just inside the boundary must flip to nearby. 767 itself is
        // not expressible as a sum of three integer squares (it is
        // `8*95 + 7`, the Legendre-excluded residue), so this uses the
        // nearest reachable integer offset below 768: `1² + 6² + 27² = 766`.
        let camera_just_inside = [origin[0] + 8 + 1, origin[1] + 8 + 6, origin[2] + 8 + 27];
        let dx2 = i64::from(camera_just_inside[0] - 8);
        let dy2 = i64::from(camera_just_inside[1] - 8);
        let dz2 = i64::from(camera_just_inside[2] - 8);
        assert_eq!(dx2 * dx2 + dy2 * dy2 + dz2 * dz2, 766);
        assert!(section_is_nearby(origin, camera_just_inside));
    }

    /// [`SECTION_FADE_ALREADY_VISIBLE`] must clear the fade for *any* `now`
    /// a real session reaches — this is the sentinel every one-off caller
    /// ([`SectionOriginUniform::new`]) relies on to render at full visibility
    /// unconditionally, including `now == 0.0` at startup, which is exactly
    /// when a naive `build_time = 0.0` default would have read as "just
    /// built" and flashed a fade on the dropped-item/held-item pass.
    #[test]
    fn already_visible_sentinel_clears_at_any_plausible_clock_value() {
        for now in [0.0_f32, 1.0, 60.0, 3600.0, 1_000_000.0] {
            assert_eq!(
                section_visibility(now, SECTION_FADE_ALREADY_VISIBLE),
                1.0,
                "now={now} did not read as fully visible"
            );
        }
    }

    /// [`SectionOriginUniform::new`] (every one-off caller: dropped items,
    /// the held item, the arena's reserved zero slot, tests) must default to
    /// the always-visible sentinel, not `0.0` — this is the regression the
    /// test above's doc names directly.
    #[test]
    fn section_origin_uniform_new_defaults_to_already_visible() {
        let u = SectionOriginUniform::new([1.0, 2.0, 3.0]);
        assert_eq!(u.origin, [1.0, 2.0, 3.0, SECTION_FADE_ALREADY_VISIBLE]);
    }

    /// [`SectionOriginUniform::with_build_time`] must place the build time in
    /// the `w` lane the shader actually reads (`origin.section_origin.w`),
    /// not silently drop it — a transposition here is invisible to
    /// `decode(encode(x)) == x` against our own constructor, so the fixture
    /// uses pairwise-distinct values.
    #[test]
    fn section_origin_uniform_with_build_time_places_it_in_w() {
        let u = SectionOriginUniform::with_build_time([11.0, 1.0, 4.0], 42.0);
        assert_eq!(u.origin, [11.0, 1.0, 4.0, 42.0]);
    }

    #[test]
    fn empty_model_mesh_uploads_to_none() {
        // The empty check short-circuits before any GPU call, so this needs no
        // device.
        let mesh = ModelMesh::default();
        assert!(mesh.indices.is_empty());
    }

    #[test]
    fn model_vertex_layout_is_32_bytes_over_four_attributes_no_location_4() {
        // `vertex_layout` (not `_with_biome_tint`) is the one `crate::
        // entity_pipeline` builds its instance-buffer attributes on top of,
        // starting at location 4 — this must never claim it, or every entity
        // pipeline build fails a wgpu validation check (measured: "Two or
        // more vertex attributes were assigned to the same location in the
        // shader: 4", the exact regression this test exists to catch).
        let layout = ModelVertex::vertex_layout();
        assert_eq!(layout.array_stride, 32, "stride is the real struct size");
        assert_eq!(layout.attributes.len(), 4);
        assert!(
            layout.attributes.iter().all(|a| a.shader_location != 4),
            "location 4 is reserved for the entity pipeline's instance buffer"
        );
        // Packed light/tint/anim tail starts at offset 24.
        assert_eq!(layout.attributes[3].offset, 24);
    }

    #[test]
    fn model_vertex_layout_with_biome_tint_is_32_bytes_over_five_attributes() {
        let layout = ModelVertex::vertex_layout_with_biome_tint();
        assert_eq!(layout.array_stride, 32);
        assert_eq!(layout.attributes.len(), 5);
        // Packed light/tint/anim tail starts at offset 24.
        assert_eq!(layout.attributes[3].offset, 24);
        // The real-colour biome-tint override (additive, location 4) starts
        // right after it at offset 28.
        assert_eq!(layout.attributes[4].offset, 28);
    }

    /// The override name and the two thresholds are three separate claims that
    /// nothing else checks, and each fails in a different place: a drifted name
    /// is a pipeline-creation error on a real device only, and a drifted value
    /// is silent everywhere.
    ///
    /// This greps `model.wgsl` from a *different* file, deliberately — a
    /// source-grep gate placed inside the file it greps matches its own
    /// assertion string and passes with the real line deleted.
    #[test]
    fn the_model_shader_declares_the_alpha_cutout_override_this_file_binds() {
        let src = MODEL_WGSL;
        assert!(
            src.contains(&format!("override {ALPHA_CUTOUT_OVERRIDE}: f32 = 0.5;")),
            "model.wgsl must declare `override {ALPHA_CUTOUT_OVERRIDE}: f32 = 0.5;` — the name is \
             what `build` binds by string and the default is the cutout threshold the opaque pass \
             relies on"
        );
        assert!(
            src.contains(&format!("tex.a < {ALPHA_CUTOUT_OVERRIDE}")),
            "model.wgsl's cutout discard must test against `{ALPHA_CUTOUT_OVERRIDE}`, not against \
             a literal — a literal is how this was wrong for the translucent pass"
        );
        // Vanilla `RenderPipelines`: CUTOUT_TERRAIN 0.5F, TRANSLUCENT_TERRAIN
        // 0.1F. Transcribed from the 26.2 source, not from each other.
        assert_eq!(ALPHA_CUTOUT_CUTOUT, 0.5);
        assert_eq!(ALPHA_CUTOUT_TRANSLUCENT, 0.1);
    }

    #[test]
    fn camera_depth_bias_pulls_coplanar_world_geometry_toward_the_eye() {
        assert_eq!(CAMERA_DEPTH_BIAS.constant, -10);
        assert_eq!(CAMERA_DEPTH_BIAS.slope_scale, -1.0);
        assert_eq!(CAMERA_DEPTH_BIAS.clamp, 0.0);
    }

    #[test]
    fn map_surface_depth_bias_adds_a_constant_step_without_changing_slope() {
        assert_eq!(
            MAP_SURFACE_DEPTH_BIAS.constant,
            CAMERA_DEPTH_BIAS.constant * 2
        );
        assert_eq!(
            MAP_SURFACE_DEPTH_BIAS.slope_scale,
            CAMERA_DEPTH_BIAS.slope_scale,
            "the frame plate and its parallel map must receive the same grazing-angle term; \
             only their relative constant depth step may differ"
        );
        assert_eq!(MAP_SURFACE_DEPTH_BIAS.clamp, CAMERA_DEPTH_BIAS.clamp);
    }
}
