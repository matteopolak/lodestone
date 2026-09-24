//! GPU gate for state-gated AO and partial-face interpolation.
//!
//! The synthetic view is intentionally small, but it drives the same
//! `mesh_models` output and `ModelPipeline` that the live snapshot mesher uses.
//! A partial `South` face is projected to the whole target, one diagonal shade
//! sample is located at the bottom-left corner, and the readback checks that
//! corner rather than a frame average. The no-occluder and flat-AO scenes are
//! executed controls for the detector.

use lodestone_assets::{BakedQuad, Direction};
use lodestone_render::{
    GpuAtlas, GpuModelMesh, ModelPipeline, ModelSectionView, mesh_models, model_anim_buffer,
    model_palette_buffer, model_shared_camera_buffer, section_origin_buffer,
};

const W: u32 = 64;
const H: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const INSET: u32 = 3;
const OCCLUDER: [i32; 3] = [1, -1, 0];

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
                label: Some("emission_partial_face_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// South-face winding: the first and last vertices are the upper edge, while
/// vertex 1 is the screen-space bottom-left corner under `render_frame`'s
/// projection. X and Y are both inset to `[.25,.75]`, so shape weighting is
/// active; Z stays fixed as the face plane.
fn partial_quad() -> BakedQuad {
    BakedQuad {
        positions: [
            [0.25, 0.75, 0.5],
            [0.25, 0.25, 0.5],
            [0.75, 0.25, 0.5],
            [0.75, 0.75, 0.5],
        ],
        uvs: [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
        direction: Direction::South,
        cullface: None,
        tint_index: None,
        shade: true,
        layer: 0,
        anim: 0,
        sprite: 0,
    }
}

struct PartialOccluder;
impl ModelSectionView for PartialOccluder {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        static QUAD: std::sync::OnceLock<Vec<BakedQuad>> = std::sync::OnceLock::new();
        if (x, y, z) == (0, 0, 0) {
            QUAD.get_or_init(|| vec![partial_quad()])
        } else {
            &[]
        }
    }

    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }

    fn ao_occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        [x, y, z] == OCCLUDER
    }
}

struct PartialNoOccluder;
impl ModelSectionView for PartialNoOccluder {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        static QUAD: std::sync::OnceLock<Vec<BakedQuad>> = std::sync::OnceLock::new();
        if (x, y, z) == (0, 0, 0) {
            QUAD.get_or_init(|| vec![partial_quad()])
        } else {
            &[]
        }
    }

    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

struct PartialAoFlat;
impl ModelSectionView for PartialAoFlat {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        static QUAD: std::sync::OnceLock<Vec<BakedQuad>> = std::sync::OnceLock::new();
        if (x, y, z) == (0, 0, 0) {
            QUAD.get_or_init(|| vec![partial_quad()])
        } else {
            &[]
        }
    }

    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }

    fn ao_occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        [x, y, z] == OCCLUDER
    }

    fn ambient_occlusion_at(&self, _x: usize, _y: usize, _z: usize) -> bool {
        false
    }
}

/// Render one view through the real model pipeline and return its red channel.
/// The atlas and palette are white, while the default full-bright light makes
/// the output byte a direct location-specific shade measurement.
fn render_frame(gpu: &Gpu, view: &dyn ModelSectionView) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let pipeline = ModelPipeline::new(device, FORMAT);
    let atlas = GpuAtlas::from_rgba(device, queue, 4, 4, &[255, 255, 255, 255].repeat(16), &[]);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);
    let palette = vec![[1.0_f32, 1.0, 1.0, 1.0]; 256];
    let palette_buffer = model_palette_buffer(device, &palette);
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    let anim_buffer = model_anim_buffer(device, &[]);
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);
    // Map local [.25,.75] onto NDC [-1,1], leaving the model's fixed depth at
    // .5. This keeps the shape extents in block-local coordinates while every
    // quad corner lands at a known framebuffer location.
    let view_proj = glam::Mat4::from_scale_rotation_translation(
        glam::Vec3::new(4.0, 4.0, 1.0),
        glam::Quat::IDENTITY,
        glam::Vec3::new(-2.0, -2.0, 0.0),
    );
    let cam_buffer = model_shared_camera_buffer(device, view_proj.to_cols_array_2d());
    let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);

    let mesh = mesh_models(view);
    assert_eq!(mesh.quad_count(), 1, "the gate view must emit exactly one quad");
    let gpu_mesh = GpuModelMesh::upload(device, &mesh).expect("non-empty partial face");
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("emission partial face target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("emission partial face depth"),
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
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("emission partial face gate"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
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
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[0]);
        pass.set_bind_group(1, &atlas_bg, &[]);
        pass.set_bind_group(2, &palette_bg, &[]);
        pass.set_bind_group(3, &anim_bg, &[]);
        pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
        pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
    }
    let padded = (W * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("emission partial face readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
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
    let mut red = vec![0u8; (W * H) as usize];
    for y in 0..H {
        for x in 0..W {
            red[(y * W + x) as usize] = data[(y * padded + x * 4) as usize];
        }
    }
    red
}

fn at(frame: &[u8], x: u32, y: u32) -> u8 {
    frame[(y * W + x) as usize]
}

fn changed_dark_bbox(
    frame: &[u8],
    bright_control: &[u8],
    dark_threshold: u8,
    bright_threshold: u8,
) -> Option<(u32, u32, u32, u32)> {
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for y in 0..H {
        for x in 0..W {
            if at(frame, x, y) >= dark_threshold || at(bright_control, x, y) < bright_threshold {
                continue;
            }
            bbox = Some(match bbox {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    bbox
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to observe the location controls"]
fn partial_face_emission_lighting_reaches_the_expected_pixels() {
    let Some(gpu) = setup() else {
        panic!("emission_partial_face_gate: no GPU adapter");
    };
    let occluded = render_frame(&gpu, &PartialOccluder);
    let bare = render_frame(&gpu, &PartialNoOccluder);
    let flat = render_frame(&gpu, &PartialAoFlat);
    let occluded_corner = at(&occluded, INSET, H - 1 - INSET);
    let bare_corner = at(&bare, INSET, H - 1 - INSET);
    let flat_corner = at(&flat, INSET, H - 1 - INSET);
    let bbox = changed_dark_bbox(&occluded, &bare, 190, 195);
    println!(
        "=== PARTIAL-FACE TERRAIN PIXEL GATE ===\n  bottom-left bytes: weighted={occluded_corner}, no-occluder={bare_corner}, flat-AO={flat_corner}\n  weighted dark bbox (<190): {bbox:?}"
    );

    // Independent prediction: the located vertex is an inset x/y corner. The
    // diagonal shade sample contributes weight .5625, so AO is
    // `1 - .2*.5625 = .8875`; South's directional shade is .8, predicting a
    // framebuffer byte near `255 * .8875 * .8 = 181`.
    assert!((165..=190).contains(&occluded_corner));
    assert!(bare_corner >= 195, "no-occluder control unexpectedly dark: {bare_corner}");
    assert!(flat_corner >= 195, "flat-AO control unexpectedly dark: {flat_corner}");
    let (x0, y0, x1, y1) = bbox.expect("the weighted corner must produce a dark region");
    assert!(
        x1 <= W / 2 + 4 && y0 >= H / 2 - 4,
        "the dark region must stay at the located bottom-left corner, got {bbox:?}"
    );
    assert!(
        x0 <= INSET && y1 >= H - 1 - INSET,
        "the dark region must touch the measured vertex location, got {bbox:?}"
    );
}
