//! The weather pass: one instanced angled quad per rain/snow column.
//!
//! Pairs with [`crate::weather`], which owns every constant and all the geometry
//! arithmetic; this module owns only the GPU objects. `docs/weather.md` is the
//! subsystem doc.
//!
//! # Where it runs in the frame, and why
//!
//! Inside the shell's existing **block pass**, after the particles and before the
//! block outline. That placement is forced by two facts:
//!
//! * The pass must be **depth-tested** against a depth buffer that already holds
//!   every opaque surface, or rain draws through walls. Terrain, models, entities
//!   and water have all written depth by that point.
//! * It must not **write** depth, or overlapping columns punch holes in each
//!   other in draw order — the same reason the particle pass has
//!   `depth_write_enabled: false`.
//!
//! Vanilla runs it as its own render pass against a dedicated `WEATHER_TARGET`
//! (`WeatherEffectRenderer.render`, `:121-123`) because it composites weather
//! through its transparency-sorting chain. This client has no such chain, so a
//! separate pass would buy nothing and cost a second depth attachment.
//!
//! # The depth comparison is flipped from vanilla, deliberately
//!
//! Vanilla's `WEATHER_NO_DEPTH_WRITE` is
//! `DepthStencilState(CompareOp.GREATER_THAN_OR_EQUAL, false)`
//! (`RenderPipelines.java:635-640`). Vanilla uses reversed-Z; this renderer uses
//! DirectX-style `[0, 1]` depth, so the port is `LessEqual` — see `CLAUDE.md`'s
//! rendering constraints. `Less` would also *look* right (rain is never coplanar
//! with terrain, so the `Equal` half never decides anything here); `LessEqual` is
//! used because it is the faithful mapping and a future coplanar case should
//! behave as vanilla does.
//!
//! # Two bind groups, and why that matters
//!
//! Camera at group 0, the precipitation texture at group 1. That is two of
//! wgpu's guaranteed four (`CLAUDE.md`: the model shader is already at the
//! floor), and it is why the per-column light term is resolved on the **CPU**
//! into the instance rather than by binding a lightmap texture as vanilla does
//! (`WeatherEffectRenderer.java:152-154` binds it at `Sampler2`). One sample per
//! column per frame against the sampler the shell already owns for particles is
//! cheaper than a third bind group and leaves headroom.
//!
//! Rain and snow are two textures, so they are two **draws** over one buffer —
//! instances sorted rain-first by [`crate::weather::extract_columns`], exactly as
//! vanilla issues two `drawIndexed` calls over one mesh (`:157-158`). Nothing in
//! the shader branches on the kind.

use bytemuck::{Pod, Zeroable};
use lodestone_assets::{Image, ResourceManager};

use crate::Camera;
use crate::weather::WeatherInstance;

const WEATHER_WGSL: &str = include_str!("shaders/weather.wgsl");

/// Jar path of the rain sheet.
pub const RAIN_TEXTURE: &str = "assets/minecraft/textures/environment/rain.png";

/// Jar path of the snow sheet.
pub const SNOW_TEXTURE: &str = "assets/minecraft/textures/environment/snow.png";

/// Why the weather textures could not be loaded.
#[derive(Debug)]
pub enum WeatherAssetError {
    /// The pack has no such texture.
    Missing {
        /// The jar path that was looked for.
        path: &'static str,
    },
    /// The texture is present but is not a decodable PNG.
    Decode {
        /// The jar path that failed.
        path: &'static str,
        /// The decoder's own error.
        source: lodestone_assets::TextureError,
    },
}

impl std::fmt::Display for WeatherAssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { path } => write!(f, "no {path} in the resource pack"),
            Self::Decode { path, source } => write!(f, "decoding {path}: {source}"),
        }
    }
}

impl std::error::Error for WeatherAssetError {}

/// The two precipitation sheets, decoded but not yet uploaded.
#[derive(Debug)]
pub struct WeatherTextures {
    /// `textures/environment/rain.png` — 32×32 in 26.2, a vertical streak sheet.
    pub rain: Image,
    /// `textures/environment/snow.png` — much smaller (256 bytes on disk), a
    /// flake pattern.
    pub snow: Image,
}

/// Load both sheets out of the resource pack.
///
/// Not stitched into any atlas, for the same reason
/// [`lodestone_assets::sky::load_cloud_texture`] is not: both tile with
/// wraparound UVs across the *whole* texture (rain's V scrolls past 32 tiles,
/// snow's U is a per-column random walk), and an atlas's padding or non-zero
/// origin breaks that seam.
///
/// # Errors
///
/// [`WeatherAssetError`] when either texture is absent or undecodable. A caller
/// with no jar should treat that as "no weather pass", not as fatal — see
/// [`WeatherRenderer::new`].
pub fn load_weather_textures(
    manager: &ResourceManager,
) -> Result<WeatherTextures, WeatherAssetError> {
    let read = |path: &'static str| -> Result<Image, WeatherAssetError> {
        let bytes = manager
            .read(path)
            .ok_or(WeatherAssetError::Missing { path })?;
        Image::decode_png(&bytes).map_err(|source| WeatherAssetError::Decode { path, source })
    };
    Ok(WeatherTextures {
        rain: read(RAIN_TEXTURE)?,
        snow: read(SNOW_TEXTURE)?,
    })
}

/// Instances allocated up front; the buffer grows (never shrinks) past this.
///
/// A radius-10 square is 441 columns and vanilla's option maxes below the 32×32
/// table, so 1024 covers every legal radius with no reallocation at all.
const INITIAL_CAPACITY: u32 = 1024;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WeatherUniform {
    view_proj: [[f32; 4]; 4],
}

/// The weather pass.
pub struct WeatherRenderer {
    pipeline: wgpu::RenderPipeline,
    cam_buffer: wgpu::Buffer,
    cam_bind_group: wgpu::BindGroup,
    rain_bind_group: wgpu::BindGroup,
    snow_bind_group: wgpu::BindGroup,
    // Held to keep the GPU textures alive for the pass's lifetime; never read.
    _rain_texture: wgpu::Texture,
    _snow_texture: wgpu::Texture,
    instances: wgpu::Buffer,
    capacity: u32,
    /// Total instances uploaded by the last `prepare`.
    count: u32,
    /// Of [`Self::count`], how many are rain — the split point for the two draws.
    rain_count: u32,
}

impl std::fmt::Debug for WeatherRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeatherRenderer")
            .field("count", &self.count)
            .field("rain_count", &self.rain_count)
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl WeatherRenderer {
    /// Build the pass for a target of `color_format`, sampling `textures`.
    ///
    /// `depth_format` must be the format of the depth buffer the caller's block
    /// pass binds; passing a different one is a pipeline-vs-pass mismatch wgpu
    /// rejects at draw time rather than here.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        textures: &WeatherTextures,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-weather-shader"),
            source: wgpu::ShaderSource::Wgsl(WEATHER_WGSL.into()),
        });

        let cam_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-weather-camera-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // Vertex only: the fragment stage reads nothing but the
                // interpolated instance data. Naming the wrong stage here fails
                // at bind time, not compile time, so it is spelled out.
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
            label: Some("lodestone-weather-texture-bgl"),
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
            label: Some("lodestone-weather-pl"),
            bind_group_layouts: &[Some(&cam_layout), Some(&tex_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-weather-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<WeatherInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x4, 1 => Float32x4, 2 => Float32x4
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                // Vanilla's `WEATHER_SNIPPET` is `.withCull(false)`
                // (`RenderPipelines.java:143`), and it has to be: the ribbon's
                // winding flips as the camera crosses the column, so culling
                // would blink half the rain out on every pass.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                depth_write_enabled: Some(false),
                // Vanilla's GREATER_THAN_OR_EQUAL under reversed-Z; see the
                // module doc.
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
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
                    // `BlendFunction.TRANSLUCENT` (`RenderPipelines.java:142`).
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let cam_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-weather-camera"),
            size: std::mem::size_of::<WeatherUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cam_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-weather-camera-bg"),
            layout: &cam_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: cam_buffer.as_entire_binding(),
            }],
        });

        // `AddressMode::Repeat` on both axes is load-bearing, not a default: the
        // whole animation is a U/V offset that runs far outside `0..1` (rain's V
        // is `y * 0.25 + scroll` with `scroll` wrapping at 32), so a clamped
        // sampler would smear one row of texels down every column. `Nearest`
        // matches vanilla's pixel-art filtering.
        let (rain_texture, rain_view, rain_sampler) = upload(
            device,
            queue,
            "lodestone-weather-rain",
            &textures.rain,
        );
        let (snow_texture, snow_view, snow_sampler) = upload(
            device,
            queue,
            "lodestone-weather-snow",
            &textures.snow,
        );
        let bind_texture = |label: &str, view: &wgpu::TextureView, sampler: &wgpu::Sampler| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &tex_layout,
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
        };
        let rain_bind_group = bind_texture("lodestone-weather-rain-bg", &rain_view, &rain_sampler);
        let snow_bind_group = bind_texture("lodestone-weather-snow-bg", &snow_view, &snow_sampler);

        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-weather-instances"),
            size: u64::from(INITIAL_CAPACITY) * std::mem::size_of::<WeatherInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            cam_buffer,
            cam_bind_group,
            rain_bind_group,
            snow_bind_group,
            _rain_texture: rain_texture,
            _snow_texture: snow_texture,
            instances,
            capacity: INITIAL_CAPACITY,
            count: 0,
            rain_count: 0,
        }
    }

    /// Upload this frame's instances. Must run **before** the render pass opens —
    /// buffers cannot be created mid-pass.
    ///
    /// `rain_count` is how many leading instances are rain
    /// ([`crate::weather::rain_count`] over the same sorted column list); it is
    /// clamped to `instances.len()` so a caller that miscounts draws every
    /// instance with the rain texture rather than skipping the tail.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[WeatherInstance],
        rain_count: usize,
        camera: &Camera,
    ) {
        self.count = u32::try_from(instances.len()).unwrap_or(u32::MAX);
        self.rain_count = u32::try_from(rain_count.min(instances.len())).unwrap_or(u32::MAX);
        if self.count == 0 {
            return;
        }
        if self.count > self.capacity {
            self.capacity = self.count.next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lodestone-weather-instances"),
                size: u64::from(self.capacity) * std::mem::size_of::<WeatherInstance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(instances));

        // Camera-relative positions, so the eye translation folds into the matrix
        // rather than being added back per vertex — the identical trick
        // `ParticleRenderer::prepare` uses, and for the identical reason: a column
        // 30 blocks away expressed in absolute world coordinates loses float
        // precision far from the origin.
        let uniform = WeatherUniform {
            view_proj: (camera.projection_matrix()
                * camera.view_matrix()
                * glam::Mat4::from_translation(camera.position))
            .to_cols_array_2d(),
        };
        queue.write_buffer(&self.cam_buffer, 0, bytemuck::bytes_of(&uniform));
    }

    /// Instances uploaded by the last [`prepare`](Self::prepare).
    #[must_use]
    pub fn count(&self) -> usize {
        self.count as usize
    }

    /// Of [`count`](Self::count), how many are rain rather than snow.
    #[must_use]
    pub fn rain_count(&self) -> usize {
        self.rain_count as usize
    }

    /// Record the draws. No-op when the last [`prepare`](Self::prepare) produced
    /// nothing.
    ///
    /// Two draws over one buffer: `0..rain_count` with the rain sheet, then
    /// `rain_count..count` with the snow sheet. Snow is second so it blends over
    /// rain in a mixed frame, matching vanilla's order (`:157-158`).
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        if self.rain_count > 0 {
            pass.set_bind_group(1, &self.rain_bind_group, &[]);
            pass.draw(0..4, 0..self.rain_count);
        }
        if self.count > self.rain_count {
            pass.set_bind_group(1, &self.snow_bind_group, &[]);
            pass.draw(0..4, self.rain_count..self.count);
        }
    }
}

/// Upload one precipitation sheet as a repeating, nearest-filtered texture.
fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    image: &Image,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::Sampler) {
    let width = image.width.max(1);
    let height = image.height.max(1);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
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
        &image.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * width),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (texture, view, sampler)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instance layout the `vertex_attr_array!` above declares must match the
    /// struct the CPU writes, and nothing else in the tree checks it: a mismatch
    /// is a silently garbled quad, not a compile error.
    #[test]
    fn the_instance_stride_matches_three_vec4s() {
        assert_eq!(std::mem::size_of::<WeatherInstance>(), 48);
        assert_eq!(std::mem::align_of::<WeatherInstance>(), 4);
    }

    #[test]
    fn the_camera_uniform_is_one_mat4() {
        assert_eq!(std::mem::size_of::<WeatherUniform>(), 64);
    }
}
