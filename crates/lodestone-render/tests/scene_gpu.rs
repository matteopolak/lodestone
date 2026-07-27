//! GPU end-to-end multi-section frame-time benchmark.
//!
//! `live_gate` proves *one* section reaches pixels. This proves a **world**
//! does: it builds a flat terrain at a stated view distance, culls it through
//! [`WorldScene::plan_frame`], and rasterises the surviving sections through the
//! real [`DrawStrategy`] trait into a headless target — reporting GPU frame time
//! and draw counts for the strategy the adapter actually selects.
//!
//! It is `#[ignore]`d and fails closed: running it is an explicit request for
//! the full GPU path (see `live_gate` for the same discipline). Because it is
//! `#[ignore]`d it never runs in the default headless suite; the CPU-only
//! `scene_bench` covers culling there.
//!
//! ## What it measures, and how it avoids being a vacuous benchmark
//!
//! A renderer that is "fast" because it culled everything is the pixel-domain
//! version of a gate that passes while asserting nothing. So this test asserts,
//! on real pixels, that the frame both **drew** geometry and **culled**
//! geometry ([`CullStats::is_meaningful`]) *and* that the framebuffer is
//! non-uniform (sky and terrain both present). A regression to draw-all or
//! draw-none fails here rather than reporting an impressive millisecond.
//!
//! ## Strategy comparison on Metal
//!
//! The primary number is [`StrategyKind::PerDraw`] — the strategy
//! [`select_strategy`] returns on this Metal target, because
//! `multi_draw_indexed_indirect` is emulated as a per-draw CPU loop by wgpu-hal
//! here (see `caps.rs`). When the adapter grants `INDIRECT_FIRST_INSTANCE` the
//! test *also* times [`StrategyKind::MdiZeroInstance`] over the identical region
//! list, substantiating "measure, don't assume": the emulated multi-draw is not
//! faster than per-draw on this backend. Both strategies draw the same
//! geometry, so the pixels are identical — only the submission cost differs.

use std::hint::black_box;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use lodestone_render::{
    Camera, DrawRegion, DrawStrategy, GpuCapabilities, MdiZeroInstance, PerDraw, SectionVisibility,
    StrategyKind, Submission, WorldScene, build_strategy, section_of, select_strategy,
};
use wgpu::util::DeviceExt;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const SECTION: f32 = 16.0;
const INDICES_PER_QUAD: u32 = 6;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewProj {
    view_proj: [[f32; 4]; 4],
}

/// A cube vertex: section-local position plus face normal.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CubeVertex {
    pos: [f32; 3],
    normal: [f32; 3],
}

/// A unit-section cube (edge = 16), 24 vertices / 36 indices, per-face normals.
fn cube() -> (Vec<CubeVertex>, Vec<u32>) {
    // (normal, four corners CCW when viewed from outside)
    let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        (
            [0.0, 0.0, 1.0],
            [[0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]],
        ),
        (
            [0.0, 0.0, -1.0],
            [[1., 0., 0.], [0., 0., 0.], [0., 1., 0.], [1., 1., 0.]],
        ),
        (
            [1.0, 0.0, 0.0],
            [[1., 0., 1.], [1., 0., 0.], [1., 1., 0.], [1., 1., 1.]],
        ),
        (
            [-1.0, 0.0, 0.0],
            [[0., 0., 0.], [0., 0., 1.], [0., 1., 1.], [0., 1., 0.]],
        ),
        (
            [0.0, 1.0, 0.0],
            [[0., 1., 1.], [1., 1., 1.], [1., 1., 0.], [0., 1., 0.]],
        ),
        (
            [0.0, -1.0, 0.0],
            [[0., 0., 0.], [1., 0., 0.], [1., 0., 1.], [0., 0., 1.]],
        ),
    ];
    let mut verts = Vec::with_capacity(24);
    let mut idx = Vec::with_capacity(36);
    for (normal, corners) in faces {
        let base = verts.len() as u32;
        for c in corners {
            verts.push(CubeVertex {
                pos: [c[0] * SECTION, c[1] * SECTION, c[2] * SECTION],
                normal,
            });
        }
        idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (verts, idx)
}

/// A flat terrain scene at render distance `rd`: two solid layers (buried +
/// surface) under an air layer the camera occupies. Returns the scene plus a
/// per-instance origin table indexed by [`DrawRegion::instance`].
fn flat_terrain(rd: i32) -> (WorldScene, Vec<[f32; 4]>) {
    let mut scene = WorldScene::new();
    let mut origins: Vec<[f32; 4]> = Vec::new();
    let add = |scene: &mut WorldScene,
               origins: &mut Vec<[f32; 4]>,
               coord: (i32, i32, i32),
               quads: u32,
               vis: SectionVisibility| {
        let instance = origins.len() as u32;
        origins.push([
            coord.0 as f32 * SECTION,
            coord.1 as f32 * SECTION,
            coord.2 as f32 * SECTION,
            0.0,
        ]);
        scene.insert_section(
            coord,
            DrawRegion {
                first_index: 0,
                index_count: quads * INDICES_PER_QUAD,
                base_vertex: 0,
                instance,
                visible: true,
            },
            vis,
        );
    };
    for x in -rd..=rd {
        for z in -rd..=rd {
            add(
                &mut scene,
                &mut origins,
                (x, 0, z),
                6,
                SectionVisibility::solid(),
            );
            add(
                &mut scene,
                &mut origins,
                (x, 1, z),
                6,
                SectionVisibility::solid(),
            );
            add(
                &mut scene,
                &mut origins,
                (x, 2, z),
                0,
                SectionVisibility::all(),
            );
        }
    }
    (scene, origins)
}

const SHADER: &str = r#"
struct VP { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> vp: VP;

struct VsIn {
  @location(0) pos: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) origin: vec3<f32>,
};
struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) shade: f32,
};

@vertex
fn vs(in: VsIn) -> VsOut {
  var out: VsOut;
  out.clip = vp.view_proj * vec4<f32>(in.pos + in.origin, 1.0);
  let l = normalize(vec3<f32>(0.35, 1.0, 0.25));
  out.shade = 0.35 + 0.65 * max(dot(in.normal, l), 0.0);
  return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  return vec4<f32>(0.15 * in.shade, 0.55 * in.shade, 0.20 * in.shade, 1.0);
}
"#;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    caps: GpuCapabilities,
}

/// Bring up an adapter/device, requesting `INDIRECT_FIRST_INSTANCE` when the
/// adapter offers it so the indirect strategy's non-zero `first_instance` is
/// legal. Returns `None` if no adapter is available.
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
        let caps = GpuCapabilities::probe(&adapter);
        let mut features = wgpu::Features::empty();
        if adapter
            .features()
            .contains(wgpu::Features::INDIRECT_FIRST_INSTANCE)
        {
            features |= wgpu::Features::INDIRECT_FIRST_INSTANCE;
        }
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("scene_gpu device"),
                required_features: features,
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu {
            device,
            queue,
            caps,
        })
    })
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly for a real frame-time number"]
fn gpu_multi_section_frame_time() {
    let Some(gpu) = setup() else {
        panic!(
            "scene_gpu: no GPU adapter. This test is #[ignore]d, so running it is an explicit \
             request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let device = &gpu.device;
    let queue = &gpu.queue;

    // --- stated configuration ---
    const RD: i32 = 10; // render distance in chunks
    const W: u32 = 256;
    const H: u32 = 256;
    let (scene, origins) = flat_terrain(RD);

    // Camera on the surface (section (0,2,0)) looking out and slightly down.
    let camera = Camera {
        position: glam::Vec3::new(8.0, 40.0, 8.0),
        yaw: 0.0,
        pitch: 22.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD as u32, 0),
    };
    assert_eq!(section_of(camera.position), (0, 2, 0));

    let plan = scene.plan_frame(&camera);
    let stats = plan.stats;
    assert!(
        stats.is_meaningful(),
        "GPU frame must draw AND cull, not draw-all/draw-none: {stats:?}"
    );

    // --- GPU resources ---
    let (verts, indices) = cube();
    let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cube-vertices"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cube-indices"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    let inst = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("section-origins"),
        contents: bytemuck::cast_slice(&origins),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let vp = ViewProj {
        view_proj: camera.view_projection().to_cols_array_2d(),
    };
    let vp_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("view-proj"),
        contents: bytemuck::bytes_of(&vp),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    // Indirect args for every region (visible → instance_count 1, culled → 0).
    let indirect_args: Vec<wgpu::util::DrawIndexedIndirectArgs> = plan
        .regions
        .iter()
        .map(DrawRegion::to_indirect_args)
        .collect();
    let mut indirect_bytes = Vec::with_capacity(indirect_args.len() * 20);
    for a in &indirect_args {
        indirect_bytes.extend_from_slice(a.as_bytes());
    }
    let indirect_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("indirect-args"),
        contents: &indirect_bytes,
        usage: wgpu::BufferUsages::INDIRECT,
    });
    let draw_capacity = plan.regions.len() as u32;

    let cam_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("cam-layout"),
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
    let cam_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cam-bg"),
        layout: &cam_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: vp_buf.as_entire_binding(),
        }],
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("scene_gpu-shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let pl_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("scene_gpu-pl"),
        bind_group_layouts: &[Some(&cam_layout)],
        immediate_size: 0,
    });
    let vertex_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
    let inst_attrs = wgpu::vertex_attr_array![2 => Float32x3];
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("scene_gpu-pipeline"),
        layout: Some(&pl_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[
                Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CubeVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &vertex_attrs,
                }),
                Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<[f32; 4]>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &inst_attrs,
                }),
            ],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    });

    // Color + depth targets.
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scene_gpu-color"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scene_gpu-depth"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

    let sky = wgpu::Color {
        r: 0.40,
        g: 0.60,
        b: 0.95,
        a: 1.0,
    };

    // One frame's worth of recording through a strategy.
    let render = |strategy: &dyn DrawStrategy, submission: &Submission<'_>| {
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene_gpu-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(sky),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &cam_bg, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.set_vertex_buffer(1, inst.slice(..));
            pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
            strategy
                .record(submission, &mut pass)
                .expect("strategy records with the buffers it needs");
        }
        queue.submit(std::iter::once(encoder.finish()));
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    };

    // Time a strategy over `iters` frames (GPU idle between frames — a
    // conservative per-frame wall-clock figure, not a pipelined throughput).
    let bench = |strategy: &dyn DrawStrategy, submission: &Submission<'_>, iters: u32| -> f64 {
        render(strategy, submission); // warm-up
        let start = Instant::now();
        for _ in 0..iters {
            render(black_box(strategy), submission);
        }
        start.elapsed().as_secs_f64() * 1e3 / f64::from(iters)
    };

    let selected = select_strategy(&gpu.caps);
    println!("=== GPU multi-section frame benchmark ===");
    println!("adapter selects:    {}", selected.name());
    println!(
        "view distance:      {RD} chunks ({} loaded sections)",
        scene.loaded_len()
    );
    println!("target:             {W}x{H}");
    println!(
        "drawable: {}  drawn: {} ({} quads)  culled f/o: {}/{}",
        stats.drawable,
        stats.drawn,
        stats.drawn_quads,
        stats.culled_frustum,
        stats.culled_occlusion
    );

    // --- PerDraw: the strategy actually selected on Metal ---
    let per = PerDraw;
    let per_sub = Submission {
        regions: &plan.regions,
        indirect: None,
        count: None,
        draw_capacity,
    };
    let per_ms = bench(&per, &per_sub, 120);
    println!(
        "PerDraw:            {per_ms:.3} ms/frame  ({} draw calls)",
        stats.drawn
    );

    // --- MdiZeroInstance: only if the device granted first-instance ---
    // On Metal wgpu-hal emulates the multi-draw as a per-draw CPU loop, so this
    // is expected to be no faster than PerDraw — measuring it is the point.
    if gpu.caps.indirect_execution
        && gpu
            .device
            .features()
            .contains(wgpu::Features::INDIRECT_FIRST_INSTANCE)
    {
        let mdi = MdiZeroInstance;
        let mdi_sub = Submission {
            regions: &plan.regions,
            indirect: Some(&indirect_buf),
            count: None,
            draw_capacity,
        };
        let mdi_ms = bench(&mdi, &mdi_sub, 120);
        println!(
            "MdiZeroInstance:    {mdi_ms:.3} ms/frame  ({draw_capacity} indirect slots, \
             emulated per-draw on Metal)"
        );
        assert_eq!(
            mdi.kind(),
            StrategyKind::MdiZeroInstance,
            "sanity: strategy identity"
        );
    } else {
        println!(
            "MdiZeroInstance:    skipped (indirect_execution={}, first_instance not granted)",
            gpu.caps.indirect_execution
        );
    }
    // Reference the boxed builder so the whole strategy surface is exercised.
    assert_eq!(build_strategy(selected).kind(), selected);

    // --- read back once and assert the frame is non-vacuous on real pixels ---
    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    render(&per, &per_sub);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &color,
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
    queue.submit(std::iter::once(enc.finish()));
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = readback.slice(..).get_mapped_range().expect("mapped range");

    let mut sky_px = 0u32;
    let mut terrain_px = 0u32;
    for y in 0..H {
        let row = (y * padded) as usize;
        for x in 0..W {
            let i = row + (x * 4) as usize;
            let (r, g, b) = (data[i], data[i + 1], data[i + 2]);
            let is_sky = (i32::from(r) - 102).abs() < 24
                && (i32::from(g) - 153).abs() < 24
                && (i32::from(b) - 242).abs() < 24;
            if is_sky {
                sky_px += 1;
            } else {
                terrain_px += 1;
            }
        }
    }
    drop(data);
    readback.unmap();

    let total = f64::from(W * H);
    println!(
        "pixels:             sky {:.1}% / terrain {:.1}%",
        100.0 * f64::from(sky_px) / total,
        100.0 * f64::from(terrain_px) / total
    );
    println!("=== GPU MULTI-SECTION GATE PASSED ===");

    assert!(
        sky_px > 0,
        "some sky must remain — the frame did not draw-all"
    );
    assert!(
        terrain_px > (W * H) / 20,
        "terrain must cover a real fraction of the frame, not a sliver: {terrain_px} px"
    );
    assert!(
        stats.drawn < stats.drawable,
        "a meaningful frame never draws every drawable section: {stats:?}"
    );
}
