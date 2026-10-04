//! Sparse terrain origins must preserve position, fade, and composition on a real GPU.

use lodestone_render::{
    ArenaMesh, GpuAtlas, GpuContext, GpuModelMesh, HeadlessTarget, ModelMesh, ModelMeshArena,
    ModelPipeline, ModelVertex, RenderLayer, SectionOriginUniform,
    fog::{FogSettings, FogUniform},
    model_anim_buffer, model_palette_buffer, model_shared_camera_buffer_with_fog,
};
use wgpu::util::DeviceExt;

const W: u32 = 96;
const H: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const NOW: f32 = 3.125;

struct Fixture {
    slot: u32,
    origin: [f32; 3],
    local_center: [f32; 3],
    world_bounds: [f32; 5],
    build_time: f32,
    visibility: f32,
    tint: [u8; 3],
    sample: [u32; 2],
}

// World bounds and visibility are literal fixture expectations, independent of slot addressing.
const FIXTURES: [Fixture; 3] = [
    Fixture {
        slot: 1, origin: [-0.43, 0.17, 0.19], local_center: [-0.10, -0.11, 0.28],
        world_bounds: [-0.91, -0.15, -0.35, 0.47, 0.47],
        build_time: 2.5625, visibility: 0.75, tint: [255, 96, 32], sample: [12, 30],
    },
    Fixture {
        slot: 4, origin: [0.37, -0.29, 0.31], local_center: [0.13, 0.18, 0.38],
        world_bounds: [0.12, 0.88, -0.52, 0.30, 0.69],
        build_time: 2.9375, visibility: 0.25, tint: [64, 255, 128], sample: [82, 35],
    },
    Fixture {
        slot: 9, origin: [0.11, 0.23, -0.17], local_center: [-0.09, -0.13, 0.40],
        world_bounds: [-0.36, 0.40, -0.31, 0.51, 0.23],
        build_time: 2.75, visibility: 0.50, tint: [96, 160, 255], sample: [48, 19],
    },
];
const ORDER: [usize; 3] = [2, 0, 1];

#[derive(Clone, Copy, Debug)]
enum Material { Opaque, Translucent, Fluid }

impl Material {
    fn alpha(self) -> u8 {
        match self { Self::Opaque => 255, Self::Translucent => 102, Self::Fluid => 180 }
    }

    fn pipeline(self, device: &wgpu::Device, stride: u64, instance: bool) -> ModelPipeline {
        match (self, instance) {
            (Self::Fluid, false) => ModelPipeline::for_fluid(device, FORMAT),
            (Self::Fluid, true) => ModelPipeline::for_terrain_fluid(device, FORMAT, stride)
                .expect("GPU must support the terrain origin stream"),
            (_, false) => ModelPipeline::for_layer(device, FORMAT, self.layer()),
            (_, true) => ModelPipeline::for_terrain(device, FORMAT, self.layer(), stride)
                .expect("GPU must support the terrain origin stream"),
        }
    }

    fn layer(self) -> RenderLayer {
        match self { Self::Opaque => RenderLayer::Solid, _ => RenderLayer::Translucent }
    }
}

#[derive(Clone, Copy)]
enum OriginPath { Uniform, Instance, ExplicitWorld, ForcedZero }

fn quad(fixture: &Fixture, world: bool) -> ModelMesh {
    let [left, right, bottom, top, z] = if world {
        fixture.world_bounds
    } else {
        let [x, y, z] = fixture.local_center;
        [x - 0.38, x + 0.38, y - 0.41, y + 0.41, z]
    };
    let v = |x, y, u, v| ModelVertex {
        position: [x, y, z], uv: [u, v], ao: 1.0, light: 0xff, tint: 255,
        anim: 0, cutout_bypass: 0,
        tint_rgb_override: [fixture.tint[0], fixture.tint[1], fixture.tint[2], 255],
    };
    ModelMesh {
        vertices: vec![v(left, bottom, 0.0, 1.0), v(right, bottom, 1.0, 1.0),
            v(right, top, 1.0, 0.0), v(left, top, 0.0, 0.0)],
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

enum Mesh { Arena(ArenaMesh), Dedicated(GpuModelMesh) }

fn render(gpu: &GpuContext, material: Material, path: OriginPath) -> Vec<u8> {
    let device = gpu.device();
    let queue = gpu.queue();
    let stride = u64::from(device.limits().min_uniform_buffer_offset_alignment).max(16);
    let instance = matches!(path, OriginPath::Instance | OriginPath::ForcedZero);
    let world = matches!(path, OriginPath::ExplicitWorld);
    let pipeline = material.pipeline(device, stride, instance);
    let mut origin_bytes = vec![0_u8; (10 * stride) as usize];
    for fixture in &FIXTURES {
        let offset = fixture.slot as usize * stride as usize;
        let origin = SectionOriginUniform::with_build_time(fixture.origin, fixture.build_time);
        origin_bytes[offset..offset + 16].copy_from_slice(bytemuck::bytes_of(&origin));
    }
    let origins = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("sparse terrain origin fixture"), contents: &origin_bytes,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::VERTEX,
    });
    let settings = FogSettings {
        color: [0.0; 3], sky_color: [0.0; 3], start: 0.0, end: 0.0,
        environmental_start: 0.0, environmental_end: 0.0,
        open_air: false,
    };
    let mut fog = FogUniform::new(&settings, [0.0; 3]);
    fog.ambient_light[3] = NOW;
    let camera = model_shared_camera_buffer_with_fog(
        device, glam::Mat4::IDENTITY.to_cols_array_2d(), fog,
    );
    let camera_group = pipeline.camera_bind_group(device, &camera, &origins);
    let reference_groups: Vec<_> = FIXTURES.iter().map(|fixture| {
        let origin = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("explicit world reference origin"),
            contents: bytemuck::bytes_of(&SectionOriginUniform::with_build_time(
                [0.0; 3], fixture.build_time,
            )),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        pipeline.camera_bind_group(device, &camera, &origin)
    }).collect();
    let atlas = GpuAtlas::from_rgba(
        device, queue, 4, 4, &[255, 255, 255, material.alpha()].repeat(16), &[],
    );
    let atlas_group = pipeline.atlas_bind_group(device, &atlas);
    let palette = model_palette_buffer(device, &[[1.0; 4]; 256]);
    let palette_group = pipeline.palette_layout.as_ref()
        .map(|_| pipeline.palette_bind_group(device, &palette));
    let animation = model_anim_buffer(device, &[]);
    let animation_group = pipeline.anim_bind_group(device, &animation);

    let mut arena = ModelMeshArena::with_block_sizes(4096, 1024);
    let _padding = arena.upload(device, queue, &quad(&FIXTURES[0], world)).expect("arena padding");
    let meshes: Vec<_> = FIXTURES.iter().enumerate().map(|(i, fixture)| {
        let mesh = quad(fixture, world);
        if i == 0 {
            Mesh::Dedicated(GpuModelMesh::upload(device, &mesh).expect("dedicated fixture"))
        } else {
            let allocation = arena.upload(device, queue, &mesh).expect("arena fixture");
            assert!(allocation.base_vertex > 0 && allocation.first_index > 0);
            Mesh::Arena(allocation)
        }
    }).collect();
    let target = HeadlessTarget::new(device, W, H, FORMAT);
    let view = target.texture().create_view(&wgpu::TextureViewDescriptor::default());
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terrain origin depth"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT, view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("terrain origin pixels"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view, depth_slice: None, resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(lodestone_render::DEPTH_CLEAR),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
        });
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(1, &atlas_group, &[]);
        if let Some(palette) = &palette_group {
            pass.set_bind_group(2, palette, &[]);
            pass.set_bind_group(3, &animation_group, &[]);
        } else {
            pass.set_bind_group(2, &animation_group, &[]);
        }
        if instance {
            pass.set_bind_group(0, &camera_group, &[0]);
            pass.set_vertex_buffer(1, origins.slice(..));
        }
        for i in ORDER {
            let fixture = &FIXTURES[i];
            if world {
                pass.set_bind_group(0, &reference_groups[i], &[0]);
            } else if !instance {
                pass.set_bind_group(0, &camera_group, &[(fixture.slot as u64 * stride) as u32]);
            }
            let first_instance = if matches!(path, OriginPath::Instance) { fixture.slot } else { 0 };
            let (indices, base_vertex) = match &meshes[i] {
                Mesh::Arena(mesh) => {
                    pass.set_vertex_buffer(0, arena.vertex_buffer(mesh.block).unwrap().slice(..));
                    pass.set_index_buffer(arena.index_buffer(mesh.block).unwrap().slice(..), wgpu::IndexFormat::Uint32);
                    (mesh.first_index..mesh.first_index + mesh.index_count, mesh.base_vertex)
                },
                Mesh::Dedicated(mesh) => {
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    (0..mesh.index_count, 0)
                },
            };
            pass.draw_indexed(indices, base_vertex, first_instance..first_instance + 1);
        }
    }
    queue.submit([encoder.finish()]);
    target.read_texels(device, queue)
}

fn mismatch(a: &[u8], b: &[u8]) -> (usize, Option<[u32; 4]>) {
    assert_eq!(a.len(), (W * H * 4) as usize);
    assert_eq!(a.len(), b.len());
    let mut count = 0;
    let mut bounds = [W, H, 0, 0];
    for (i, (a, b)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
        if a.iter().zip(b).any(|(&a, &b)| a.abs_diff(b) > 2) {
            let x = i as u32 % W;
            let y = i as u32 / W;
            bounds = [bounds[0].min(x), bounds[1].min(y), bounds[2].max(x), bounds[3].max(y)];
            count += 1;
        }
    }
    (count, (count > 0).then_some(bounds))
}

fn srgb_decode(value: f32) -> f32 {
    if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
}

fn srgb_encode(value: f32) -> f32 {
    if value <= 0.0031308 { value * 12.92 } else { 1.055 * value.powf(1.0 / 2.4) - 0.055 }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn sparse_instance_origins_match_uniform_and_independent_world_pixels() {
    let gpu = GpuContext::new_headless_blocking().expect("GPU adapter for terrain origin pixels");
    for material in [Material::Opaque, Material::Translucent, Material::Fluid] {
        let reference = render(&gpu, material, OriginPath::ExplicitWorld);
        let uniform = render(&gpu, material, OriginPath::Uniform);
        let instance = render(&gpu, material, OriginPath::Instance);
        for (label, a, b) in [
            ("uniform/world", &uniform, &reference),
            ("instance/uniform", &instance, &uniform),
            ("instance/world", &instance, &reference),
        ] {
            let difference = mismatch(a, b);
            assert_eq!(difference.0, 0, "{material:?} {label}: mismatches and bounds {difference:?}");
        }
        for fixture in &FIXTURES {
            let [x, y] = fixture.sample;
            let offset = ((y * W + x) * 4) as usize;
            for channel in 0..3 {
                let faded = fixture.tint[channel] as f32 / 255.0 * fixture.visibility;
                let predicted = (srgb_encode(srgb_decode(faded) * material.alpha() as f32 / 255.0)
                    * 255.0).round() as u8;
                let actual = instance[offset + channel];
                assert!(actual.abs_diff(predicted) <= 3,
                    "{material:?} slot {} channel {channel}: expected {predicted}, got {actual}, bounds [{x},{y},{x},{y}]",
                    fixture.slot);
            }
        }
        let [x, y] = [36, 25];
        let overlap = ((y * W + x) * 4) as usize;
        let alpha = material.alpha() as f32 / 255.0;
        for channel in 0..3 {
            let near = FIXTURES[0].tint[channel] as f32 / 255.0 * 0.75;
            let far = FIXTURES[2].tint[channel] as f32 / 255.0 * 0.50;
            let composite = srgb_decode(near) * alpha
                + srgb_decode(far) * alpha * (1.0 - alpha);
            let predicted = (srgb_encode(composite) * 255.0).round() as u8;
            assert!(instance[overlap + channel].abs_diff(predicted) <= 3,
                "{material:?} ordered overlap channel {channel}: expected {predicted}, got {}, bounds [{x},{y},{x},{y}]",
                instance[overlap + channel]);
        }
        let wrong = render(&gpu, material, OriginPath::ForcedZero);
        let difference = mismatch(&wrong, &reference);
        println!("{material:?} forced-instance-zero: {} mismatched pixels, bounds {:?}", difference.0, difference.1);
        assert!(difference.0 > 100, "{material:?}: wrong-origin control must fail visibly: {difference:?}");
    }
}
