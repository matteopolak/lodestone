//! Order-independent transparency for translucent terrain: water, stained
//! glass, ice and the other translucent block layers composite in any draw
//! order, with no per-section or per-quad sorting.
//!
//! # How it works
//!
//! The translucent surfaces are drawn three times over the same geometry, none
//! of the draws writing depth, then resolved over the scene:
//!
//! 1. **bounds**: the nearest and farthest translucent distance per pixel,
//!    max-blended into an `Rg32Float` target.
//! 2. **transmittance**: each fragment adds its absorbance, as a Haar wavelet
//!    series over the pixel's bounded depth range, to eight additive
//!    coefficients (two `Rgba16Float` targets).
//! 3. **accumulate**: each fragment reads back how much light the surfaces in
//!    front of it absorb, and adds its colour weighted by that.
//! 4. **composite**: the accumulated colour, rescaled to the pixel's exact
//!    coverage, blends premultiplied over the scene.
//!
//! The math is in `shaders/oit_common.wgsl`; the per-surface stages are
//! `shaders/oit_stages.wgsl`, appended to the model shader, and the
//! resolve is `shaders/oit_composite.wgsl`.
//!
//! # How to change it
//!
//! * A new translucent surface kind needs the stage entry points: append
//!   `oit_common.wgsl` and `oit_stages.wgsl` to its shader (it must define
//!   `VsOut` with a `clip` position and `shade_fragment`) and add an
//!   [`OitStagePipelines`] built the way `OitPipelines::new` builds the others.
//! * Group 0 of the stage pipelines is the ordinary camera group plus texture
//!   bindings, because the model shader already uses all four bind groups: the
//!   transmittance stage adds the bounds texture at binding 2, the accumulate
//!   stage adds both coefficient textures at 3 and 4. A texture bound in a
//!   group is a usage conflict with the target of the same pass, so each stage
//!   has its own group-0 bind group.
//! * The composite gets no depth: translucent surfaces write none here, so
//!   particles and weather drawn afterwards test against the opaque scene only.
//!
//! # Gotchas
//!
//! * [`supported`] gates the whole path on `FLOAT32_BLENDABLE`, which
//!   `crate::device::GpuContext` requests when the adapter has it. Callers fall
//!   back to the sorted alpha path otherwise.
//! * Only the translucent terrain layers take part. Entities, particles, clouds
//!   and weather keep their ordinary blended draws.
//! * A translucent fragment that is nearly opaque does not tighten the depth
//!   range, so a pixel under such a fragment spends bins on surfaces behind it.

use crate::block::{DEPTH_COMPARE_NEARER, DEPTH_COMPARE_NEARER_OR_EQUAL, DEPTH_FORMAT};
use crate::model_pipeline::{
    ALPHA_CUTOUT_OVERRIDE, ALPHA_CUTOUT_TRANSLUCENT, ModelPipeline, USE_RGSS_OVERRIDE,
    TextureFiltering, camera_layout_entries, terrain_origin_vertex_layout,
};
use crate::models::ModelVertex;

/// The depth-bounds target: `(-nearest, farthest)` view distance, max-blended.
pub const BOUNDS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Float;
/// The two additive coefficient targets and the accumulation target.
pub const COEFFICIENT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The accumulation target.
pub const ACCUMULATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Coefficient targets: eight coefficients, four to a texel.
pub const COEFFICIENT_TARGETS: usize = 2;
/// Clears the bounds target so any real fragment wins the max blend.
pub const BOUNDS_CLEAR: wgpu::Color = wgpu::Color {
    r: -(f32::MAX as f64),
    g: 0.0,
    b: 0.0,
    a: 0.0,
};

const MODEL_OIT_WGSL: &str = concat!(
    include_str!("shaders/model.wgsl"),
    "\n",
    include_str!("shaders/oit_common.wgsl"),
    "\n",
    include_str!("shaders/oit_stages.wgsl"),
);

/// The model shader's override choosing which surface kind of a mixed mesh a
/// pipeline draws, and its two non-default values.
const SURFACE_FILTER_OVERRIDE: &str = "surface_filter";
const SURFACE_FILTER_BLOCKS: f64 = 1.0;
const SURFACE_FILTER_FLUID: f64 = 2.0;

const COMPOSITE_WGSL: &str = concat!(
    include_str!("shaders/oit_common.wgsl"),
    "\n",
    include_str!("shaders/oit_composite.wgsl"),
);

/// Whether the device can run the order-independent path at all.
#[must_use]
pub fn supported(device: &wgpu::Device) -> bool {
    device.features().contains(wgpu::Features::FLOAT32_BLENDABLE)
}

/// The three stage pipelines of one translucent surface kind.
#[derive(Debug)]
pub struct OitStagePipelines {
    /// Nearest and farthest distance per pixel.
    pub bounds: wgpu::RenderPipeline,
    /// Wavelet coefficient accumulation.
    pub transmittance: wgpu::RenderPipeline,
    /// Weighted colour accumulation.
    pub accumulate: wgpu::RenderPipeline,
}

/// Every size-independent pipeline and layout of the order-independent path.
#[derive(Debug)]
pub struct OitPipelines {
    /// Water.
    pub fluid: OitStagePipelines,
    /// Translucent blocks: stained glass, ice, portals.
    pub block: OitStagePipelines,
    /// The resolve over the scene.
    pub composite: wgpu::RenderPipeline,
    transmittance_camera_layout: wgpu::BindGroupLayout,
    accumulate_camera_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn camera_layout(device: &wgpu::Device, label: &str, textures: &[u32]) -> wgpu::BindGroupLayout {
    let mut entries = camera_layout_entries().to_vec();
    entries.extend(textures.iter().map(|&binding| texture_entry(binding)));
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &entries,
    })
}

const ADDITIVE: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
};

const MAX: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Max,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Max,
    },
};

/// Premultiplied source over the scene.
const PREMULTIPLIED_OVER: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
};

/// One surface kind's shader and the bind-group layouts shared with its
/// ordinary blended pipeline.
struct Surface<'a> {
    label: &'static str,
    source: &'static str,
    template: &'a ModelPipeline,
    /// The model shader's pipeline-overridable constants.
    constants: Vec<(&'static str, f64)>,
    /// Water keeps the strict comparison so coplanar faces cannot both
    /// contribute; blocks admit ties.
    compare: wgpu::CompareFunction,
}

impl Surface<'_> {
    fn stages(
        &self,
        device: &wgpu::Device,
        layouts: [&wgpu::BindGroupLayout; 3],
        origin_stride: Option<wgpu::BufferAddress>,
    ) -> OitStagePipelines {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(self.label),
            source: wgpu::ShaderSource::Wgsl(self.source.into()),
        });
        let mut buffers = vec![Some(ModelVertex::vertex_layout_with_biome_tint())];
        if let Some(stride) = origin_stride {
            buffers.push(Some(terrain_origin_vertex_layout(stride)));
        }
        let vertex_entry = if origin_stride.is_some() { "vs_terrain" } else { "vs_main" };
        let build = |stage: &str,
                     camera: &wgpu::BindGroupLayout,
                     fragment_entry: &str,
                     targets: &[Option<wgpu::ColorTargetState>]| {
            let mut group_layouts = vec![Some(camera), Some(&self.template.atlas_layout)];
            if let Some(palette) = &self.template.palette_layout {
                group_layouts.push(Some(palette));
            }
            group_layouts.push(Some(&self.template.anim_layout));
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("lodestone-oit-layout"),
                bind_group_layouts: &group_layouts,
                immediate_size: 0,
            });
            let options = wgpu::PipelineCompilationOptions {
                constants: &self.constants,
                ..Default::default()
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&format!("{}-{stage}", self.label)),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vertex_entry),
                    buffers: &buffers,
                    compilation_options: options.clone(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment_entry),
                    targets,
                    compilation_options: options,
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(self.compare),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let target = |format, blend| {
            Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            })
        };
        OitStagePipelines {
            bounds: build("bounds", layouts[0], "fs_oit_bounds", &[target(BOUNDS_FORMAT, MAX)]),
            transmittance: build(
                "transmittance",
                layouts[1],
                "fs_oit_transmittance",
                &[
                    target(COEFFICIENT_FORMAT, ADDITIVE),
                    target(COEFFICIENT_FORMAT, ADDITIVE),
                ],
            ),
            accumulate: build(
                "accumulate",
                layouts[2],
                "fs_oit_accumulate",
                &[target(ACCUMULATE_FORMAT, ADDITIVE)],
            ),
        }
    }
}

impl OitPipelines {
    /// Build every pipeline. `translucent` is the ordinary translucent
    /// pipeline whose atlas, palette and animation layouts the stages reuse;
    /// `origin_stride` is the terrain origin stream's stride, or `None` for the
    /// legacy dynamic-offset origin.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        translucent: &ModelPipeline,
        origin_stride: Option<wgpu::BufferAddress>,
    ) -> Self {
        let transmittance_camera_layout = camera_layout(device, "lodestone-oit-bounds-camera", &[2]);
        let accumulate_camera_layout =
            camera_layout(device, "lodestone-oit-full-camera", &[2, 3, 4]);
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-oit-composite"),
            entries: &[texture_entry(3), texture_entry(4), texture_entry(5)],
        });
        let layouts = [
            &translucent.camera_layout,
            &transmittance_camera_layout,
            &accumulate_camera_layout,
        ];
        let block_surface = Surface {
            label: "lodestone-oit-block",
            source: MODEL_OIT_WGSL,
            template: translucent,
            constants: vec![
                (ALPHA_CUTOUT_OVERRIDE, f64::from(ALPHA_CUTOUT_TRANSLUCENT)),
                (USE_RGSS_OVERRIDE, f64::from(TextureFiltering::selected().use_rgss())),
                (SURFACE_FILTER_OVERRIDE, SURFACE_FILTER_BLOCKS),
            ],
            compare: DEPTH_COMPARE_NEARER_OR_EQUAL,
        };
        let fluid_surface = Surface {
            label: "lodestone-oit-fluid",
            source: MODEL_OIT_WGSL,
            template: translucent,
            constants: vec![
                (ALPHA_CUTOUT_OVERRIDE, f64::from(ALPHA_CUTOUT_TRANSLUCENT)),
                (USE_RGSS_OVERRIDE, f64::from(TextureFiltering::selected().use_rgss())),
                (SURFACE_FILTER_OVERRIDE, SURFACE_FILTER_FLUID),
            ],
            compare: DEPTH_COMPARE_NEARER,
        };

        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-oit-composite-shader"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE_WGSL.into()),
        });
        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("lodestone-oit-composite-layout"),
                bind_group_layouts: &[Some(&composite_layout)],
                immediate_size: 0,
            });
        let composite = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-oit-composite"),
            layout: Some(&composite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &composite_shader,
                entry_point: Some("vs_composite"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
                entry_point: Some("fs_composite"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(PREMULTIPLIED_OVER),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            fluid: fluid_surface.stages(device, layouts, origin_stride),
            block: block_surface.stages(device, layouts, origin_stride),
            composite,
            transmittance_camera_layout,
            accumulate_camera_layout,
            composite_layout,
        }
    }
}

fn target_texture(
    device: &wgpu::Device,
    label: &str,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

fn camera_entries<'a>(
    shared_camera: &'a wgpu::Buffer,
    origin: &'a wgpu::Buffer,
    textures: &[(u32, &'a wgpu::TextureView)],
) -> Vec<wgpu::BindGroupEntry<'a>> {
    let mut entries = vec![
        wgpu::BindGroupEntry { binding: 0, resource: shared_camera.as_entire_binding() },
        wgpu::BindGroupEntry {
            binding: 1,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: origin,
                offset: 0,
                size: wgpu::BufferSize::new(
                    core::mem::size_of::<crate::model_pipeline::SectionOriginUniform>() as u64,
                ),
            }),
        },
    ];
    entries.extend(textures.iter().map(|&(binding, view)| wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }));
    entries
}

/// The size-dependent render targets and the bind groups that read them.
#[derive(Debug)]
pub struct OitTargets {
    width: u32,
    height: u32,
    bounds: wgpu::TextureView,
    coefficients: [wgpu::TextureView; COEFFICIENT_TARGETS],
    accumulate: wgpu::TextureView,
    transmittance_camera: wgpu::BindGroup,
    accumulate_camera: wgpu::BindGroup,
    composite: wgpu::BindGroup,
}

impl OitTargets {
    /// Create targets of `width` x `height`. `shared_camera` and `origin` are
    /// the same buffers the ordinary terrain camera group binds, so a stage's
    /// group 0 differs from it only by the textures.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        pipelines: &OitPipelines,
        width: u32,
        height: u32,
        shared_camera: &wgpu::Buffer,
        origin: &wgpu::Buffer,
    ) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        let bounds = target_texture(device, "lodestone-oit-bounds", width, height, BOUNDS_FORMAT);
        let coefficients = [
            target_texture(device, "lodestone-oit-coefficients-0", width, height, COEFFICIENT_FORMAT),
            target_texture(device, "lodestone-oit-coefficients-1", width, height, COEFFICIENT_FORMAT),
        ];
        let accumulate =
            target_texture(device, "lodestone-oit-accumulate", width, height, ACCUMULATE_FORMAT);

        let transmittance_camera = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-oit-bounds-camera-bg"),
            layout: &pipelines.transmittance_camera_layout,
            entries: &camera_entries(shared_camera, origin, &[(2, &bounds)]),
        });
        let accumulate_camera = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-oit-full-camera-bg"),
            layout: &pipelines.accumulate_camera_layout,
            entries: &camera_entries(
                shared_camera,
                origin,
                &[(2, &bounds), (3, &coefficients[0]), (4, &coefficients[1])],
            ),
        });
        let composite = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-oit-composite-bg"),
            layout: &pipelines.composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&coefficients[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&coefficients[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&accumulate),
                },
            ],
        });
        Self {
            width,
            height,
            bounds,
            coefficients,
            accumulate,
            transmittance_camera,
            accumulate_camera,
            composite,
        }
    }

    /// Whether these targets match a `width` x `height` frame.
    #[must_use]
    pub fn matches(&self, width: u32, height: u32) -> bool {
        self.width == width.max(1) && self.height == height.max(1)
    }

    /// Group 0 for the transmittance stage.
    #[must_use]
    pub fn transmittance_camera(&self) -> &wgpu::BindGroup {
        &self.transmittance_camera
    }

    /// Group 0 for the accumulate stage.
    #[must_use]
    pub fn accumulate_camera(&self) -> &wgpu::BindGroup {
        &self.accumulate_camera
    }

    fn pass<'e>(
        encoder: &'e mut wgpu::CommandEncoder,
        label: &str,
        colors: &[&wgpu::TextureView],
        clear: wgpu::Color,
        depth: &wgpu::TextureView,
    ) -> wgpu::RenderPass<'e> {
        let attachments: Vec<_> = colors
            .iter()
            .map(|&view| {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })
            })
            .collect();
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &attachments,
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: None,
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    /// Open the bounds pass, tested against the opaque scene's `depth`.
    pub fn bounds_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
        depth: &wgpu::TextureView,
    ) -> wgpu::RenderPass<'e> {
        Self::pass(encoder, "oit bounds", &[&self.bounds], BOUNDS_CLEAR, depth)
    }

    /// Open the transmittance pass.
    pub fn transmittance_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
        depth: &wgpu::TextureView,
    ) -> wgpu::RenderPass<'e> {
        Self::pass(
            encoder,
            "oit transmittance",
            &[&self.coefficients[0], &self.coefficients[1]],
            wgpu::Color::TRANSPARENT,
            depth,
        )
    }

    /// Open the accumulate pass.
    pub fn accumulate_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
        depth: &wgpu::TextureView,
    ) -> wgpu::RenderPass<'e> {
        Self::pass(encoder, "oit accumulate", &[&self.accumulate], wgpu::Color::TRANSPARENT, depth)
    }

    /// Resolve the accumulated translucent colour over `scene`, loading what is
    /// already there.
    pub fn composite(
        &self,
        pipelines: &OitPipelines,
        encoder: &mut wgpu::CommandEncoder,
        scene: &wgpu::TextureView,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("oit composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: scene,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipelines.composite);
        pass.set_bind_group(0, &self.composite, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bounds_clear_loses_to_every_real_distance() {
        // Max blending keeps the larger value of each channel; the nearest
        // distance rides negated in red, so the clear must be below any
        // `-distance` a fragment can produce.
        assert!(BOUNDS_CLEAR.r < -1.0e30);
        assert_eq!(BOUNDS_CLEAR.r as f32, -f32::MAX);
    }
}
