//! Location-level controls for player skin texture edges and layer occlusion.
//!
//! These controls intentionally render a tiny synthetic model instead of
//! checking only sampler or pipeline descriptors.  The first control compares
//! a nearest and a linear sample at a UV boundary and reports the exact pixel
//! box that changes.  The second draws a nearer opaque body, then a larger
//! translucent layer behind it, and checks both the centre (self-occlusion)
//! and the ring (silhouette).  A third farther draw proves that a surviving
//! translucent layer still owns depth where the contract says it should.
//!
//! The tests are ignored because they require a GPU adapter.  They are useful
//! while changing the player skin path because a unit test can prove that a
//! sampler is nearest or that depth writes are enabled without proving which
//! pixels those choices affect.

use glam::{Mat4, Vec3};
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{PartRange, entity_model_matrix};
use lodestone_render::entity_pipeline::{
    EntityPipeline, GpuEntityModel, upload_instances,
};
use lodestone_render::model_pipeline::CAMERA_DEPTH_BIAS;
use lodestone_render::models::ModelVertex;

const W: u32 = 128;
const H: u32 = 128;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn setup() -> Option<Gpu> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
                apply_limit_buckets: false,
            })
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("player-skin-artifact-controls"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

fn camera() -> Camera {
    Camera {
        position: Vec3::new(0.0, 0.0, -2.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    }
}

fn vertex(position: [f32; 3], uv: [f32; 2]) -> ModelVertex {
    ModelVertex {
        position,
        uv,
        ao: 1.0,
        light: 0xFF,
        tint: 255,
        anim: 0,
        cutout_bypass: 0,
        tint_rgb_override: [0, 0, 0, 0],
    }
}

fn quad_model_on(
    device: &wgpu::Device,
    half_width: f32,
    half_height: f32,
    uv_min: [f32; 2],
    uv_max: [f32; 2],
) -> GpuEntityModel {
    let vertices = [
        vertex([-half_width, -half_height, 0.0], [uv_min[0], uv_max[1]]),
        vertex([half_width, -half_height, 0.0], [uv_max[0], uv_max[1]]),
        vertex([half_width, half_height, 0.0], uv_max),
        vertex([-half_width, half_height, 0.0], [uv_min[0], uv_min[1]]),
    ];
    // The camera looks down +Z. Clip-space Y is upward while framebuffer Y is
    // downward, so this order is the front-facing `Ccw` winding seen by the
    // render pipeline after projection.
    let indices = [0u32, 2, 1, 0, 3, 2];
    GpuEntityModel::upload_parts(
        device,
        &vertices,
        &indices,
        vec![PartRange {
            index_start: 0,
            index_count: 6,
            vertex_start: 0,
            vertex_count: 4,
        }],
    )
    .expect("the quad is non-empty")
}

fn texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    pixels: &[u8],
    filter: wgpu::FilterMode,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("player-skin-control-sheet"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
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
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
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
        label: Some("player-skin-control-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: filter,
        min_filter: filter,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    (texture, view, sampler)
}

fn render_one(
    gpu: &Gpu,
    model: &GpuEntityModel,
    texture_data: (wgpu::Texture, wgpu::TextureView, wgpu::Sampler),
    uv_label: &str,
) -> Vec<u8> {
    render_one_at(
        gpu,
        model,
        texture_data,
        uv_label,
        Mat4::IDENTITY,
        camera(),
        None,
    )
}

fn render_one_at(
    gpu: &Gpu,
    model: &GpuEntityModel,
    texture_data: (wgpu::Texture, wgpu::TextureView, wgpu::Sampler),
    uv_label: &str,
    model_matrix: Mat4,
    camera: Camera,
    cull_mode: Option<wgpu::Face>,
) -> Vec<u8> {
    let device = &gpu.device;
    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let player_pipeline = player_skin_pipeline_for_test(device, &pipeline, cull_mode);
    let camera_buffer = pipeline.camera_buffer(device, &camera);
    let camera_bind_group = pipeline.camera_bind_group(device, &camera_buffer);
    let texture_bind_group = pipeline.texture_bind_group(device, &texture_data.1, &texture_data.2);
    let instances = upload_instances(
        device,
        &[model_matrix],
        &[u32::from(lodestone_render::entity::ENTITY_FULLBRIGHT)],
    )
    .expect("one instance is non-empty");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("player-skin-control-color"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = DepthBuffer::new(device, W, H);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("player-skin-control-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(uv_label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(lodestone_render::DEPTH_CLEAR),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&player_pipeline);
        pass.set_bind_group(0, &camera_bind_group, &[]);
        pass.set_bind_group(1, &texture_bind_group, &[]);
        pass.set_vertex_buffer(0, model.vertices.slice(..));
        pass.set_vertex_buffer(1, instances.slice(..));
        pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..model.index_count, 0, 0..1);
    }
    readback(gpu, encoder, &color)
}

fn player_skin_pipeline_for_test(
    device: &wgpu::Device,
    pipeline: &EntityPipeline,
    cull_mode: Option<wgpu::Face>,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("player-skin-control-shader"),
        source: wgpu::ShaderSource::Wgsl(
            include_str!("../../src/shaders/entity.wgsl").into(),
        ),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("player-skin-control-layout"),
        bind_group_layouts: &[Some(&pipeline.camera_layout), Some(&pipeline.texture_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("player-skin-control-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[
                Some(ModelVertex::vertex_layout()),
                Some(lodestone_render::entity_pipeline::EntityInstanceRaw::instance_layout()),
            ],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main_player_skin"),
            targets: &[Some(wgpu::ColorTargetState {
                format: COLOR_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: lodestone_render::DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(lodestone_render::DEPTH_COMPARE_NEARER_OR_EQUAL),
            stencil: wgpu::StencilState::default(),
            bias: CAMERA_DEPTH_BIAS,
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn render_three(
    gpu: &Gpu,
    first_model: &GpuEntityModel,
    later_model: &GpuEntityModel,
    first: (wgpu::Texture, wgpu::TextureView, wgpu::Sampler),
    second: (wgpu::Texture, wgpu::TextureView, wgpu::Sampler),
    third: Option<(wgpu::Texture, wgpu::TextureView, wgpu::Sampler)>,
) -> Vec<u8> {
    let device = &gpu.device;
    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let player_pipeline = pipeline.player_skin_pipeline(device, COLOR_FORMAT);
    let camera = camera();
    let camera_buffer = pipeline.camera_buffer(device, &camera);
    let camera_bind_group = pipeline.camera_bind_group(device, &camera_buffer);
    let first_bind_group = pipeline.texture_bind_group(device, &first.1, &first.2);
    let second_bind_group = pipeline.texture_bind_group(device, &second.1, &second.2);
    let third_bind_group = third
        .as_ref()
        .map(|value| pipeline.texture_bind_group(device, &value.1, &value.2));
    let instances = [
        upload_instances(
            device,
            &[Mat4::from_translation(Vec3::new(0.0, 0.0, 0.0))],
            &[u32::from(lodestone_render::entity::ENTITY_FULLBRIGHT)],
        )
        .expect("first instance is non-empty"),
        upload_instances(
            device,
            &[Mat4::from_translation(Vec3::new(0.0, 0.0, 0.05))],
            &[u32::from(lodestone_render::entity::ENTITY_FULLBRIGHT)],
        )
        .expect("second instance is non-empty"),
        upload_instances(
            device,
            &[Mat4::from_translation(Vec3::new(0.0, 0.0, 0.1))],
            &[u32::from(lodestone_render::entity::ENTITY_FULLBRIGHT)],
        )
        .expect("third instance is non-empty"),
    ];
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("player-skin-depth-control-color"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = DepthBuffer::new(device, W, H);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("player-skin-depth-control-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("player-skin-depth-control"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(lodestone_render::DEPTH_CLEAR),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&player_pipeline);
        pass.set_bind_group(0, &camera_bind_group, &[]);
        pass.set_index_buffer(first_model.indices.slice(..), wgpu::IndexFormat::Uint32);

        pass.set_bind_group(1, &first_bind_group, &[]);
        pass.set_vertex_buffer(0, first_model.vertices.slice(..));
        pass.set_vertex_buffer(1, instances[0].slice(..));
        pass.draw_indexed(0..first_model.index_count, 0, 0..1);

        pass.set_bind_group(1, &second_bind_group, &[]);
        pass.set_vertex_buffer(0, later_model.vertices.slice(..));
        pass.set_index_buffer(later_model.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.set_vertex_buffer(1, instances[1].slice(..));
        pass.draw_indexed(0..later_model.index_count, 0, 0..1);

        if let Some(third_bind_group) = &third_bind_group {
            pass.set_bind_group(1, third_bind_group, &[]);
            pass.set_vertex_buffer(1, instances[2].slice(..));
            pass.draw_indexed(0..later_model.index_count, 0, 0..1);
        }
    }
    readback(gpu, encoder, &color)
}

fn readback(gpu: &Gpu, mut encoder: wgpu::CommandEncoder, color: &wgpu::Texture) -> Vec<u8> {
    let padded = (W * 4).next_multiple_of(256);
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("player-skin-control-readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: color,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(std::iter::once(encoder.finish()));
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("GPU poll failed");
    let bytes = readback
        .slice(..)
        .get_mapped_range()
        .expect("mapped readback range");
    let mut out = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        let src = (y * padded) as usize;
        let dst = (y * W * 4) as usize;
        out[dst..dst + (W * 4) as usize].copy_from_slice(&bytes[src..src + (W * 4) as usize]);
    }
    drop(bytes);
    readback.unmap();
    out
}

fn bbox_of_difference(a: &[u8], b: &[u8]) -> Option<(u32, u32, u32, u32, usize)> {
    let mut min_x = W;
    let mut min_y = H;
    let mut max_x = 0;
    let mut max_y = 0;
    let mut count = 0;
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if a[i..i + 4] != b[i..i + 4] {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                count += 1;
            }
        }
    }
    (count > 0).then_some((min_x, min_y, max_x, max_y, count))
}

fn sheet_edge_control(gpu: &Gpu) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let mut pixels = vec![0u8; 8 * 8 * 4];
    for y in 0..8 {
        for x in 0..8 {
            let color = if x < 4 {
                [220, 0, 0, 255]
            } else {
                [255, 80, 80, 255]
            };
            let i = ((y * 8 + x) * 4) as usize;
            pixels[i..i + 4].copy_from_slice(&color);
        }
    }
    let nearest = texture(device, queue, 8, 8, &pixels, wgpu::FilterMode::Nearest);
    let linear = texture(device, queue, 8, 8, &pixels, wgpu::FilterMode::Linear);
    let exact_model = quad_model_on(device, 0.9, 0.9, [0.0, 0.0], [0.5, 1.0]);
    let inset_model = quad_model_on(device, 0.9, 0.9, [0.0625, 0.0625], [0.4375, 0.9375]);
    (
        render_one(gpu, &exact_model, nearest, "nearest exact UV"),
        render_one(gpu, &exact_model, linear, "linear exact UV"),
        render_one(
            gpu,
            &inset_model,
            texture(device, queue, 8, 8, &pixels, wgpu::FilterMode::Nearest),
            "nearest half-texel inset UV",
        ),
    )
}

fn flattened_player_model(device: &wgpu::Device) -> GpuEntityModel {
    let quads = lodestone_assets::entity::bake_entity(&lodestone_assets::entity::player_model(false));
    let mut vertices = Vec::with_capacity(quads.len() * 4);
    let mut indices = Vec::with_capacity(quads.len() * 6);
    for quad in quads {
        let base = vertices.len() as u32;
        for i in 0..4 {
            vertices.push(vertex(quad.positions[i], quad.uvs[i]));
        }
        let p0 = Vec3::from(quad.positions[0]);
        let p1 = Vec3::from(quad.positions[1]);
        let p2 = Vec3::from(quad.positions[2]);
        let n = Vec3::from(quad.normal);
        if (p1 - p0).cross(p2 - p0).dot(n) >= 0.0 {
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        } else {
            indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
        }
    }
    GpuEntityModel::upload_parts(
        device,
        &vertices,
        &indices,
        vec![PartRange {
            index_start: 0,
            index_count: indices.len() as u32,
            vertex_start: 0,
            vertex_count: vertices.len() as u32,
        }],
    )
    .expect("the player model is non-empty")
}

fn solid_sheet(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    color: [u8; 4],
) -> (wgpu::Texture, wgpu::TextureView, wgpu::Sampler) {
    texture(
        device,
        queue,
        64,
        64,
        &color.repeat(64 * 64),
        wgpu::FilterMode::Nearest,
    )
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly for player skin pixel controls"]
fn player_skin_sampler_control_separates_filter_from_uv_inset() {
    let Some(gpu) = setup() else {
        panic!("player skin controls require a GPU adapter");
    };
    let (nearest, linear, inset) = sheet_edge_control(&gpu);
    let Some(linear_diff) = bbox_of_difference(&nearest, &linear) else {
        panic!("linear sampler control was vacuous: exact UV edge produced no changed pixels");
    };
    let inset_diff = bbox_of_difference(&nearest, &inset);
    println!("sampler/UV raw diff: linear={linear_diff:?}; inset={inset_diff:?}");
    let centre = ((H / 2 * W + W / 2) * 4) as usize;
    assert_eq!(nearest[centre + 1], 0, "nearest interior sample crossed the sheet edge");
    println!(
        "sampler/UV controls: linear diff bbox=({},{})..({},{}) count={}; inset diff={inset_diff:?}",
        linear_diff.0,
        linear_diff.1,
        linear_diff.2,
        linear_diff.3,
        linear_diff.4,
    );
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly for player skin depth controls"]
fn player_skin_overlay_control_separates_self_occlusion_from_silhouette() {
    let Some(gpu) = setup() else {
        panic!("player skin controls require a GPU adapter");
    };
    let device = &gpu.device;
    let queue = &gpu.queue;
    let red = texture(
        device,
        queue,
        2,
        2,
        &vec![220, 0, 0, 255].repeat(4),
        wgpu::FilterMode::Nearest,
    );
    let blue_half = texture(
        device,
        queue,
        2,
        2,
        &vec![0, 0, 220, 128].repeat(4),
        wgpu::FilterMode::Nearest,
    );
    let green = texture(
        device,
        queue,
        2,
        2,
        &vec![0, 220, 0, 255].repeat(4),
        wgpu::FilterMode::Nearest,
    );
    let body = quad_model_on(device, 0.45, 0.75, [0.0, 0.0], [1.0, 1.0]);
    let outer = quad_model_on(device, 0.75, 0.95, [0.0, 0.0], [1.0, 1.0]);
    let pixels = render_three(&gpu, &body, &outer, red, blue_half, Some(green));

    let centre = ((H / 2 * W + W / 2) * 4) as usize;
    assert!(
        pixels[centre] > pixels[centre + 2],
        "far translucent layer showed through the nearer body at centre: {:?}",
        &pixels[centre..centre + 4]
    );
    let ring = (((H / 2) * W + (W / 2 + 35)) * 4) as usize;
    assert!(
        pixels[ring + 2] > pixels[ring],
        "outer layer did not reach the silhouette ring: {:?}",
        &pixels[ring..ring + 4]
    );
    println!(
        "overlay depth controls: centre RGBA={:?}; silhouette ring RGBA={:?}",
        &pixels[centre..centre + 4],
        &pixels[ring..ring + 4],
    );
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly for the uniform-sheet lighting control"]
fn player_skin_uniform_sheet_reports_lighting_bands_without_texture_changes() {
    let Some(gpu) = setup() else {
        panic!("player skin controls require a GPU adapter");
    };
    let model = flattened_player_model(&gpu.device);
    let model_matrix = entity_model_matrix(Vec3::ZERO, 0.0, 1.0);
    let model_camera = Camera {
        position: Vec3::new(0.0, 0.5, -4.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 45.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    };
    let pixels = render_one_at(
        &gpu,
        &model,
        solid_sheet(&gpu.device, &gpu.queue, [220, 0, 0, 255]),
        "player uniform skin",
        model_matrix,
        model_camera,
        None,
    );
    let back_culled = render_one_at(
        &gpu,
        &model,
        solid_sheet(&gpu.device, &gpu.queue, [220, 0, 0, 255]),
        "player uniform skin back-cull control",
        model_matrix,
        model_camera,
        Some(wgpu::Face::Back),
    );
    let Some(cull_diff) = bbox_of_difference(&pixels, &back_culled) else {
        panic!("back-face culling control was vacuous: no player pixels changed");
    };
    let count_non_background = |image: &[u8]| {
        image
            .chunks_exact(4)
            .filter(|pixel| pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0)
            .count()
    };
    let mut colors = std::collections::BTreeMap::<[u8; 3], (usize, u32, u32, u32, u32)>::new();
    for (index, pixel) in pixels.chunks_exact(4).enumerate() {
        if pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0 {
            let x = (index as u32) % W;
            let y = (index as u32) / W;
            let entry = colors
                .entry([pixel[0], pixel[1], pixel[2]])
                .or_insert((0, x, y, x, y));
            entry.0 += 1;
            entry.1 = entry.1.min(x);
            entry.2 = entry.2.min(y);
            entry.3 = entry.3.max(x);
            entry.4 = entry.4.max(y);
        }
    }
    let top: Vec<_> = colors.iter().rev().take(12).collect();
    println!(
        "uniform solid-red sheet non-background colours (top): {top:?}; cull diff={cull_diff:?}; pixels none/back={}/{}",
        count_non_background(&pixels),
        count_non_background(&back_culled),
    );
    assert!(!colors.is_empty(), "flattened player model rendered no pixels");
}
