//! GPU-requiring integration tests. All are `#[ignore]`d so the default
//! `cargo test` run stays hermetic and headless; run them with
//! `cargo test -p lodestone-render -- --ignored` on a machine with a real
//! adapter.

use lodestone_render::{
    ArenaBuffer, DrawRegion, DrawStrategy, GpuContext, HeadlessTarget, PerDraw, RenderTarget,
    Renderer, Submission, select_strategy,
};

fn ctx() -> Option<GpuContext> {
    match GpuContext::new_headless_blocking() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipping: no GPU adapter available: {e}");
            None
        }
    }
}

/// Prints the real capability probe for this machine so we can compare it
/// against documented expectations.
#[test]
#[ignore = "requires a GPU adapter"]
fn probe_reports_capabilities() {
    let Some(ctx) = ctx() else { return };
    let caps = ctx.capabilities();
    eprintln!("=== lodestone-render capability probe ===");
    eprintln!("adapter: {} ({:?})", caps.adapter_name, caps.backend);
    eprintln!(
        "indirect_first_instance          = {}",
        caps.indirect_first_instance
    );
    eprintln!(
        "indirect_execution (INDIRECT_EXECUTION) = {}",
        caps.indirect_execution
    );
    eprintln!(
        "multi_draw_indirect_count        = {}",
        caps.multi_draw_indirect_count
    );
    eprintln!(
        "timestamp_query                  = {}",
        caps.timestamp_query
    );
    eprintln!(
        "timestamp_inside_encoders        = {}",
        caps.timestamp_inside_encoders
    );
    eprintln!(
        "texture_binding_array            = {}",
        caps.texture_binding_array
    );
    eprintln!(
        "nonuniform_binding_array_indexing= {}",
        caps.nonuniform_binding_array_indexing
    );
    eprintln!("subgroup                         = {}", caps.subgroup);
    eprintln!("shader_int64                     = {}", caps.shader_int64);
    eprintln!(
        "experimental_mesh_shader         = {}",
        caps.experimental_mesh_shader
    );
    eprintln!(
        "supports_bindless_atlas          = {}",
        caps.supports_bindless_atlas()
    );
    eprintln!(
        "max_buffer_size                  = {}",
        caps.max_buffer_size
    );
    eprintln!(
        "max_bind_groups                  = {}",
        caps.max_bind_groups
    );
    eprintln!(
        "max_texture_array_layers         = {}",
        caps.max_texture_array_layers
    );
    eprintln!(
        "max_storage_buffer_binding_size  = {}",
        caps.max_storage_buffer_binding_size
    );
    eprintln!(
        "max_storage_buffers_per_stage    = {}",
        caps.max_storage_buffers_per_shader_stage
    );
    eprintln!(
        "selected draw strategy           = {}",
        select_strategy(caps).name()
    );
}

/// End-to-end: render the trivial test triangle to a headless target and verify
/// the centre pixel is the triangle colour and a corner is the clear colour.
#[test]
#[ignore = "requires a GPU adapter"]
fn triangle_renders_end_to_end() {
    let Some(ctx) = ctx() else { return };
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (64u32, 64u32);
    let mut target = HeadlessTarget::new(ctx.device(), w, h, format);
    let renderer = Renderer::new(ctx.device(), format);

    let outcome = renderer.render_frame(ctx.device(), ctx.queue(), &mut target);
    assert!(
        matches!(outcome, lodestone_render::FrameOutcome::Presented { .. }),
        "expected a presented frame, got {outcome:?}"
    );

    let pixels = target.read_texels(ctx.device(), ctx.queue());
    let at = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    let centre = at(w / 2, h / 2);
    assert_eq!(
        centre,
        Renderer::TRIANGLE_RGBA,
        "centre should be triangle colour"
    );
    // Top-left corner is outside the centred triangle -> clear colour (dark).
    let corner = at(0, 0);
    assert!(
        corner != Renderer::TRIANGLE_RGBA,
        "corner should be clear colour"
    );
}

/// Exercises the arena suballocator and the `PerDraw` strategy against a real
/// device: build an indexed triangle in arena buffers and render it.
#[test]
#[ignore = "requires a GPU adapter"]
fn per_draw_strategy_with_arena_buffers() {
    let Some(ctx) = ctx() else { return };
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (64u32, 64u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    // Vertex = vec2 position (8 bytes). One centred triangle.
    let verts: [[f32; 2]; 3] = [[-0.8, -0.8], [0.8, -0.8], [0.0, 0.8]];
    let indices: [u32; 3] = [0, 1, 2];

    let mut vbuf = ArenaBuffer::new(device, "verts", 4096, 256, wgpu::BufferUsages::VERTEX);
    let mut ibuf = ArenaBuffer::new(device, "indices", 4096, 256, wgpu::BufferUsages::INDEX);
    let valloc = vbuf.allocate(std::mem::size_of_val(&verts) as u64).unwrap();
    let ialloc = ibuf
        .allocate(std::mem::size_of_val(&indices) as u64)
        .unwrap();
    vbuf.write(queue, &valloc, bytemuck::cast_slice(&verts))
        .unwrap();
    ibuf.write(queue, &ialloc, bytemuck::cast_slice(&indices))
        .unwrap();

    // Minimal indexed pipeline.
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("indexed"),
        source: wgpu::ShaderSource::Wgsl(
            r"
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(p, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(1.0, 0.5019608, 0.0, 1.0); }
"
            .into(),
        ),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 8,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2],
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
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
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    let region = DrawRegion {
        first_index: 0,
        index_count: 3,
        base_vertex: 0,
        instance: 0,
        visible: true,
    };
    let submission = Submission {
        regions: &[region],
        indirect: None,
        count: None,
        draw_capacity: 1,
    };

    let frame = target.acquire().unwrap();
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: frame.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_vertex_buffer(0, vbuf.buffer().slice(valloc.offset()..));
        pass.set_index_buffer(
            ibuf.buffer().slice(ialloc.offset()..),
            wgpu::IndexFormat::Uint32,
        );
        PerDraw.record(&submission, &mut pass).unwrap();
    }
    queue.submit(std::iter::once(encoder.finish()));

    let pixels = target.read_texels(device, queue);
    let i = (((h / 2) * w + (w / 2)) * 4) as usize;
    assert_eq!(
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]],
        [255, 128, 0, 255],
        "centre pixel should be the triangle colour drawn via PerDraw"
    );

    vbuf.free(valloc).unwrap();
    ibuf.free(ialloc).unwrap();
    assert_eq!(vbuf.stats().used, 0);
}

/// End-to-end block pass: mesh a single textured cube, render it with the real
/// depth-tested block pipeline sampling a green atlas sprite, and verify the
/// centre pixel is green (texture + camera transform + depth all exercised) and
/// a corner is the background clear colour.
#[test]
#[ignore = "requires a GPU adapter"]
fn block_pass_renders_a_textured_cube() {
    use lodestone_render::block::{camera_buffer, sprite_uv_buffer};
    use lodestone_render::{
        BlockPipeline, Camera, CameraUniform, Cell, DepthBuffer, GpuAtlas, GpuMesh,
        SectionNeighborhood, SectionView, SpriteId, mesh_simple,
    };

    let Some(ctx) = ctx() else { return };
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (64u32, 64u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    // A section: one solid block at (8,8,8); air cells carry full sky light.
    struct OneBlock;
    impl SectionView for OneBlock {
        fn cell(&self, x: usize, y: usize, z: usize) -> Cell {
            if (x, y, z) == (8, 8, 8) {
                Cell::solid(SpriteId(0))
            } else {
                Cell {
                    occludes: false,
                    surface: None,
                    block_light: 0,
                    sky_light: 15,
                }
            }
        }
    }
    let section = OneBlock;
    let hood = SectionNeighborhood::centre_only(&section);
    let mesh = mesh_simple(&hood);
    assert_eq!(mesh.quad_count(), 6, "isolated cube has six faces");
    let gpu_mesh = GpuMesh::upload(device, &mesh).expect("non-empty mesh");

    // A 16×16 solid green atlas, one sprite covering the whole texture.
    let mut rgba = vec![0u8; 16 * 16 * 4];
    for px in rgba.chunks_exact_mut(4) {
        px[1] = 255;
        px[3] = 255;
    }
    let atlas = GpuAtlas::from_rgba(device, queue, 16, 16, &rgba, &[]);
    let uv = sprite_uv_buffer(device, &[[0.0, 0.0, 1.0, 1.0]]);

    // Camera at z = -5 looking +Z straight at the cube's −Z face.
    let camera = Camera {
        position: glam::Vec3::new(8.5, 8.5, -5.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: 100.0,
    };
    let cam_buf = camera_buffer(device, CameraUniform::new(&camera, [0.0, 0.0, 0.0]));

    let pipeline = BlockPipeline::new(device, format);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas, &uv);
    let depth = DepthBuffer::new(device, w, h);

    let frame = target.acquire().unwrap();
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("block pass test"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: frame.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.04,
                        g: 0.0,
                        b: 0.08,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
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
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[]);
        pass.set_bind_group(1, &atlas_bg, &[]);
        pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
        pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
    }
    queue.submit(std::iter::once(encoder.finish()));

    let pixels = target.read_texels(device, queue);
    let at = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    let centre = at(w / 2, h / 2);
    assert!(
        centre[1] > 180 && centre[0] < 60 && centre[2] < 60,
        "centre should be the green cube face, got {centre:?}"
    );
    let corner = at(1, 1);
    assert!(
        corner[1] < 60,
        "corner should be the dark background, got {corner:?}"
    );
}

/// Bug 2 regression: a *greedy-merged* quad that spans many tiles must keep
/// sampling **its own** atlas sprite, not bleed into neighbouring sprites.
///
/// A merged NxM quad carries per-vertex tile coordinates running `0..N`/`0..M`
/// rather than `0..1`. If the shader maps those straight onto the sprite's
/// atlas sub-rect (`rect.xy + tile * rect.zw`), the coordinate runs off the end
/// of the sprite and (with clamp-to-edge) samples whatever sits at the atlas
/// border — a different sprite. The fix tiles per fragment (`fract`) into the
/// sprite rect, so every tile of the span repeats the correct sprite.
///
/// Setup: a fully solid section (greedy merges each outward face into one
/// 16×16 quad) drawn against a two-sprite atlas — sprite 0 red on the atlas'
/// left half, sprite 1 blue on the right half. The visible face uses sprite 0,
/// so with a correct shader the whole face is red; the pre-fix shader bleeds
/// into the blue sprite across most of the span.
#[test]
#[ignore = "requires a GPU adapter"]
fn greedy_merged_quad_stays_within_its_sprite() {
    use lodestone_render::block::{camera_buffer, sprite_uv_buffer};
    use lodestone_render::{
        BlockPipeline, Camera, CameraUniform, Cell, DepthBuffer, GpuAtlas, GpuMesh,
        SectionNeighborhood, SectionView, SpriteId, mesh_greedy,
    };

    let Some(ctx) = ctx() else { return };
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (64u32, 64u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    // A fully solid 16³ section using sprite 0 on every face. Interior faces are
    // culled by their solid neighbours; each of the six outward faces greedy-
    // merges into a single 16×16 quad whose tile coords run 0..16.
    struct FullSolid;
    impl SectionView for FullSolid {
        fn cell(&self, _x: usize, _y: usize, _z: usize) -> Cell {
            Cell::solid(SpriteId(0))
        }
    }
    let section = FullSolid;
    let hood = SectionNeighborhood::centre_only(&section);
    let mesh = mesh_greedy(&hood);
    assert_eq!(
        mesh.quad_count(),
        6,
        "a solid section merges to six outward faces"
    );
    let gpu_mesh = GpuMesh::upload(device, &mesh).expect("non-empty mesh");

    // 32×16 atlas: sprite 0 = red (left half), sprite 1 = blue (right half).
    let mut rgba = vec![0u8; 32 * 16 * 4];
    for y in 0..16 {
        for x in 0..32 {
            let i = (y * 32 + x) * 4;
            if x < 16 {
                rgba[i] = 255; // red
            } else {
                rgba[i + 2] = 255; // blue
            }
            rgba[i + 3] = 255;
        }
    }
    let atlas = GpuAtlas::from_rgba(device, queue, 32, 16, &rgba, &[]);
    let uv = sprite_uv_buffer(device, &[[0.0, 0.0, 0.5, 1.0], [0.5, 0.0, 0.5, 1.0]]);

    // Camera at z = -5 looking +Z straight at the block's −Z face.
    let camera = Camera {
        position: glam::Vec3::new(8.0, 8.0, -5.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: 100.0,
    };
    let cam_buf = camera_buffer(device, CameraUniform::new(&camera, [0.0, 0.0, 0.0]));

    let pipeline = BlockPipeline::new(device, format);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas, &uv);
    let depth = DepthBuffer::new(device, w, h);

    let frame = target.acquire().unwrap();
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("greedy atlas bleed test"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: frame.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.2,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
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
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[]);
        pass.set_bind_group(1, &atlas_bg, &[]);
        pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
        pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
    }
    queue.submit(std::iter::once(encoder.finish()));

    let pixels = target.read_texels(device, queue);
    let at = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    // Sample a horizontal sweep across the middle of the face. Every face pixel
    // must read the red sprite (its own); none may bleed into the blue neighbour.
    // Absolute brightness is irrelevant here — the exposed face samples the empty
    // neighbour's zero light, so red arrives dim — what matters is red vs blue.
    let mut face_samples = 0usize;
    for sx in (8..w - 8).step_by(4) {
        let p = at(sx, h / 2);
        let is_background = p[1] > p[0] && p[1] > p[2] && p[0] < 80;
        if is_background {
            continue;
        }
        face_samples += 1;
        assert!(
            p[0] > 20 && p[0] > p[2] * 4,
            "face pixel at x={sx} sampled the wrong sprite (bled into blue): {p:?}"
        );
    }
    assert!(
        face_samples >= 4,
        "expected several on-face samples, got {face_samples}"
    );
}

/// Phase-5 gate: render a frame from **real `lodestone-world` chunk storage**.
///
/// Builds a real paletted [`ChunkSection`], adapts it through the renderer's
/// `ChunkSectionView`/`BlockClassifier` seam, greedy-meshes it into packed
/// vertices, uploads and draws it with the depth-tested block pipeline, then
/// reads back pixels and asserts the terrain wall is visible. Also prints the
/// numbers the report asks for: vertices, draw calls, frame time, VRAM.
#[test]
#[ignore = "requires a GPU adapter"]
fn real_chunk_section_renders_terrain() {
    use lodestone_render::block::{camera_buffer, sprite_uv_buffer};
    use lodestone_render::vertex::vram_bytes;
    use lodestone_render::{
        BlockClassifier, BlockPipeline, Camera, CameraUniform, Cell, ChunkSectionView, DepthBuffer,
        GpuAtlas, GpuMesh, SectionNeighborhood, SectionView, SpriteId, UniformLight, mesh_greedy,
    };
    use lodestone_world::{ChunkSection, PaletteKind};

    let Some(ctx) = ctx() else { return };
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (96u32, 96u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    const AIR: u32 = 0;
    const STONE: u32 = 1;

    // A real paletted section: a solid stone box inset from the section
    // boundaries so its exposed faces border lit in-section air (not the absent,
    // unlit neighbour sections).
    let mut section = ChunkSection::new(PaletteKind::block_states(), PaletteKind::biomes(), AIR, 0);
    for y in 0..8 {
        for z in 4..12 {
            for x in 0..16 {
                section.set_block(x, y, z, STONE);
            }
        }
    }
    assert_eq!(section.non_air_count(), 16 * 8 * 8);

    // The renderer never sees block-state ids: a classifier resolves them.
    #[derive(Debug)]
    struct Classifier;
    impl BlockClassifier for Classifier {
        fn classify(&self, state_id: u32, block_light: u8, sky_light: u8) -> Cell {
            if state_id == AIR {
                // Air renders nothing but still carries its light, so the faces
                // of neighbouring blocks are lit by it.
                Cell {
                    occludes: false,
                    surface: None,
                    block_light,
                    sky_light,
                }
            } else {
                let mut c = Cell::solid(SpriteId(0));
                c.block_light = block_light;
                c.sky_light = sky_light;
                c
            }
        }
    }

    let light = UniformLight::default();
    let view = ChunkSectionView::new(&section, &Classifier, &light);
    // Smooth lighting reads corner neighbours across the section boundary, so
    // faces of the box that touch an edge (x=0/15, y=0) need lit neighbour data
    // or the merge fragments along that edge. Surround with lit air, as the real
    // pipeline's populated neighbourhood does.
    #[derive(Debug)]
    struct AirLit;
    impl SectionView for AirLit {
        fn cell(&self, _x: usize, _y: usize, _z: usize) -> Cell {
            Cell {
                occludes: false,
                surface: None,
                block_light: 0,
                sky_light: 15,
            }
        }
    }
    let air = AirLit;
    let mut hood = SectionNeighborhood::centre_only(&view);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                if (dx, dy, dz) != (0, 0, 0) {
                    hood.set(dx, dy, dz, Some(&air));
                }
            }
        }
    }
    let mesh = mesh_greedy(&hood);
    // Greedy on a solid box with lit-air neighbours: the 6 outer faces of the
    // 16×8×8 box, each merged to one quad.
    assert_eq!(mesh.quad_count(), 6, "greedy should merge the box shell");
    let quad_count = mesh.quad_count();
    let vertex_count = mesh.vertices.len();
    let gpu_mesh = GpuMesh::upload(device, &mesh).expect("non-empty mesh");

    // Solid green atlas sprite.
    let mut rgba = vec![0u8; 16 * 16 * 4];
    for px in rgba.chunks_exact_mut(4) {
        px[1] = 255;
        px[3] = 255;
    }
    let atlas = GpuAtlas::from_rgba(device, queue, 16, 16, &rgba, &[]);
    let uv = sprite_uv_buffer(device, &[[0.0, 0.0, 1.0, 1.0]]);

    // Camera in front of the −Z wall of the slab, centred on its mid-height.
    let camera = Camera {
        position: glam::Vec3::new(8.0, 4.0, -6.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(32, 0),
    };
    let cam_buf = camera_buffer(device, CameraUniform::new(&camera, [0.0, 0.0, 0.0]));

    let pipeline = BlockPipeline::new(device, format);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas, &uv);
    let depth = DepthBuffer::new(device, w, h);

    let start = std::time::Instant::now();
    let frame = target.acquire().unwrap();
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("real chunk pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: frame.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.04,
                        g: 0.0,
                        b: 0.08,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
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
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[]);
        pass.set_bind_group(1, &atlas_bg, &[]);
        pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
        pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
    }
    queue.submit(std::iter::once(encoder.finish()));
    let pixels = target.read_texels(device, queue);
    let frame_ms = start.elapsed().as_secs_f64() * 1000.0;

    let at = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    // The lower-centre of the frame looks at the stone wall → green.
    let wall = at(w / 2, 2 * h / 3);
    assert!(
        wall[1] > 150 && wall[0] < 80 && wall[2] < 80,
        "wall pixel should be green terrain, got {wall:?}"
    );

    eprintln!("=== real chunk render (phase-5 gate) ===");
    eprintln!("blocks (non-air)  = {}", section.non_air_count());
    eprintln!("quads             = {quad_count}");
    eprintln!("vertices          = {vertex_count}");
    eprintln!("draw calls        = 1");
    eprintln!("index count       = {}", gpu_mesh.index_count);
    eprintln!("mesh VRAM (bytes) = {}", vram_bytes(quad_count));
    eprintln!("frame time (ms)   = {frame_ms:.3}");
}

/// Translucency ordering, proven by the **blended pixel**, not the index array.
///
/// Two overlapping half-alpha quads — red at z=2 (near the camera) and blue at
/// z=14 (far) — are drawn through the translucent pipeline (alpha blend on,
/// depth-write off). Drawn back-to-front (blue then red) the centre blends to a
/// red-dominant colour; drawn front-to-back (red then blue) it blends
/// blue-dominant. We sort with [`TranslucentMesh`] and assert the readback is
/// red-dominant, then render the reversed order and assert it genuinely differs
/// — so the test fails if the sort is wrong *or* if blending is broken.
#[test]
#[ignore = "requires a GPU adapter"]
fn translucent_quads_blend_in_sorted_order() {
    use lodestone_render::block::{camera_buffer, sprite_uv_buffer};
    use lodestone_render::vertex::VertexFields;
    use lodestone_render::{
        BlockPipeline, Camera, CameraUniform, DepthBuffer, Face, GpuAtlas, GpuMesh, Mesh,
        PackedVertex, RenderLayer, TranslucentMesh,
    };

    let Some(ctx) = ctx() else { return };
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (64u32, 64u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    // Two overlapping quads filling the view, each a distinct sprite. Quad 0
    // (sprite 0, red) sits at z=2; quad 1 (sprite 1, blue) at z=14.
    let mut mesh = Mesh::default();
    for (qi, z) in [2u32, 14u32].into_iter().enumerate() {
        let base = (qi * 4) as u32;
        let sprite = qi as u16;
        for (x, y) in [(0u32, 0u32), (16, 0), (16, 16), (0, 16)] {
            mesh.vertices.push(PackedVertex::pack(VertexFields {
                pos: [x, y, z],
                normal: Face::NegZ,
                ao: 255,
                sky_light: 255,
                block_light: 0,
                sprite,
                u: 0,
                v: 0,
            }));
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base + 2, base + 3, base]);
    }

    // 2×1 atlas: sprite 0 = red α½, sprite 1 = blue α½.
    let rgba: Vec<u8> = vec![255, 0, 0, 128, 0, 0, 255, 128];
    let atlas = GpuAtlas::from_rgba(device, queue, 2, 1, &rgba, &[]);
    let uv = sprite_uv_buffer(device, &[[0.0, 0.0, 0.5, 1.0], [0.5, 0.0, 0.5, 1.0]]);

    // Camera at z=-10 looking +Z, centred on the quads.
    let camera = Camera {
        position: glam::Vec3::new(8.0, 8.0, -10.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: 100.0,
    };
    let cam_buf = camera_buffer(device, CameraUniform::new(&camera, [0.0, 0.0, 0.0]));
    let pipeline = BlockPipeline::for_layer(device, format, RenderLayer::Translucent);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas, &uv);

    let mut render_centre = |indices: Vec<u32>| -> [u8; 4] {
        let ordered = Mesh {
            vertices: mesh.vertices.clone(),
            indices,
        };
        let gpu_mesh = GpuMesh::upload(device, &ordered).expect("non-empty mesh");
        let depth = DepthBuffer::new(device, w, h);
        let frame = target.acquire().unwrap();
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("translucent pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: frame.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth.view,
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
            pass.set_pipeline(&pipeline.pipeline);
            pass.set_bind_group(0, &cam_bg, &[]);
            pass.set_bind_group(1, &atlas_bg, &[]);
            pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
            pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
        let pixels = target.read_texels(device, queue);
        let i = (((h / 2) * w + (w / 2)) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    // Sort back-to-front for the real camera: farther (blue, z=14) drawn first.
    let mut tm = TranslucentMesh::from_mesh(&mesh, [0, 0, 0]);
    assert!(
        tm.update([8.0, 8.0, -10.0]),
        "camera drives an initial sort"
    );
    let sorted = render_centre(tm.indices());

    // The naive (submission-order) draw would be front-to-back: red then blue.
    let unsorted = render_centre(vec![0, 1, 2, 2, 3, 0, 4, 5, 6, 6, 7, 4]);

    eprintln!("=== translucency blend ===");
    eprintln!("sorted   (back-to-front) centre = {sorted:?}");
    eprintln!("unsorted (front-to-back) centre = {unsorted:?}");

    // Correct order → red on top → red-dominant (~128, 0, 64).
    assert!(
        sorted[0] > sorted[2] + 30,
        "sorted centre should be red-dominant, got {sorted:?}"
    );
    // Wrong order → blue on top → blue-dominant, provably different.
    assert!(
        unsorted[2] > unsorted[0] + 30,
        "unsorted centre should be blue-dominant, got {unsorted:?}"
    );
    assert_ne!(sorted, unsorted, "draw order must change the blended pixel");
}
