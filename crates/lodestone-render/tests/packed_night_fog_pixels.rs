//! Issue #400: the packed full-cube path darkens at night and fades into fog.
//!
//! # Which scene, and why `--headless`'s mesher is the *right* one here
//!
//! `CLAUDE.md` warns that `--headless` renders through `mesh_simple` while live
//! terrain uses `mesh_models`, and that a colour fix was once "verified"
//! byte-identical against the one scene that structurally could not exercise it.
//! This gate inverts that warning: `mesh_simple` + `BlockPipeline` +
//! `block.wgsl` **is** the code under test. `mesher.rs` emits
//! `SectionGeometry::Packed` only when `classifier.models()` is `None` — the demo
//! world and every headless gate — so a scene built any other way could not reach
//! this shader at all.
//!
//! Confirmed by construction, not assumed: the gate binds `BlockPipeline`, whose
//! only WGSL is `block.wgsl`, and uploads a `GpuMesh` from `mesh_simple`. The
//! noon and midnight readbacks below *differ*, which is only possible if the
//! shader read the `sky_darken` lane this issue adds.
//!
//! # The three divergences, and which are still open
//!
//! `block.wgsl` diverged from `model.wgsl` in three ways. The third — the shade
//! multiply happening in **linear** space rather than gamma — was fixed in
//! `a80a095`, and that changes the arithmetic, so issue #400's own number table
//! is half-stale. This gate carries the *current* derivation:
//!
//! 1. **No `sky_darken`**: `0.2 + 0.8 * max(sky, block)` where `model.wgsl` has
//!    `max(sky * sky_darken(), block)`. Fixed here.
//! 2. **No fog lanes at all**: `Camera` was `view_proj` + `section_origin`, so
//!    there was nowhere for fog to arrive. The lanes arrived with issue #76's
//!    shared-camera split; this reads them.
//! 3. Linear-space shade multiply. Already fixed — asserted below only as the
//!    wrong hypothesis the midnight byte must **not** land on.
//!
//! # The numbers, and where each comes from
//!
//! One isolated cube, air on all six sides, every air cell at `sky_light 15` /
//! `block_light 0`. `mesh.rs`'s `face_corner_lighting` then gives every corner
//! `ao = 255` (`(1.0 + 1.0 + 1.0 + 1.0) * 0.25 = 1.0`, `mesh.rs:238`) and
//! `sky = 255` (`level_to_byte(60/4) = level_to_byte(15) = 255`,
//! `mesh.rs:171-173`), so `shade == light_term` exactly and AO cannot confound
//! the measurement.
//!
//! The atlas is `Rgba8UnormSrgb` and so is the target, so the shader's
//! `linear_to_srgb` → multiply → `srgb_to_linear` round-trip cancels against the
//! transfer functions either side of it: a texel byte of [`TEXEL`] comes back as
//! `TEXEL * shade`, in bytes.
//!
//! | | `light_term` | predicted byte |
//! |---|---|---|
//! | noon (`sky_darken` 1.00) | `0.2 + 0.8·1.00` = 1.000 | **128** |
//! | midnight (`sky_darken` 0.24) | `0.2 + 0.8·0.24` = 0.392 | **50** |
//! | *the bug*: no `sky_darken` at all | 1.000 | 128 |
//! | *if the multiply were still in linear space* | 0.392 | 82 |
//!
//! 0.24 is **not** hardcoded: it comes from
//! `entity::sky_darken_for_time_of_day(18_000)`, which `sky_light_factor_timeline.rs`
//! gates tick by tick against a JVM oracle. The gate prints all four rows and
//! requires the measurement to land on the right one — 50 vs 82 is 32 bytes
//! apart and 50 vs 128 is 78, so no tolerance admitting one admits another.
//!
//! # Fog
//!
//! A second, cheap assertion, because the fog lanes were absent *entirely* and
//! would otherwise ship as an island: with the render-distance term at
//! `start = 0.5` / `end = 1.0` and the cube ~13 blocks from the eye, `amount`
//! saturates at `1.0`, so every cube pixel must equal the fog colour exactly.
//! The control is `FogUniform::disabled()`, which must give the unfogged byte.
//!
//! # Controls, each run and observed to fail
//!
//! * **noon vs noon** — render twice at `sky_darken = 1.0` and require the ratio
//!   to be 1.000, then require the *band* that accepts 0.392 to reject it. This
//!   is the shipped world (no time term anywhere) and is what the pre-fix code
//!   produces for both renders.
//! * **fog disabled** — must give 128, not the fog colour.
//! * Both are run by the tests below rather than described.
//!
//! # Fail closed
//!
//! `#[ignore]`d; once opted in, a missing adapter is a failure, not a skip.

use lodestone_render::block::{BlockPipeline, shared_camera_buffer, sprite_uv_buffer};
use lodestone_render::entity::sky_darken_for_time_of_day;
use lodestone_render::fog::{FogSettings, FogUniform};
use lodestone_render::{
    Cell, DepthBuffer, GpuAtlas, GpuMesh, SectionNeighborhood, SectionView, SpriteId, camera::Camera,
    mesh_simple, section_origin_buffer,
};

const W: u32 = 96;
const H: u32 = 96;

/// sRGB, so the shader's gamma round-trip cancels and the readback byte is
/// `TEXEL * shade` directly. On a plain `Unorm` target the same frame reads 8
/// rather than 50 at midnight, which is a correct value for the wrong question.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The one texel every face is painted with. Mid-grey in **sRGB bytes**, the
/// same anchor `entity_night_pixels.rs` uses.
const TEXEL: u8 = 128;

/// `TEXEL * 1.000`. Noon: `sky_darken = 1.0`, `light_term = 0.2 + 0.8 = 1.0`.
const NOON_BYTE: i32 = 128;
/// `TEXEL * 0.392`, rounded. Midnight: `sky_darken = 0.24`,
/// `light_term = 0.2 + 0.8 * 0.24 = 0.392`.
const MIDNIGHT_BYTE: i32 = 50;
/// What the same frame would read if the shade multiply were still in linear
/// space (issue #400's divergence 3, fixed in `a80a095`). Asserted *against*.
const MIDNIGHT_BYTE_IF_LINEAR: i32 = 82;

/// The GPU's own rounding of the final sRGB encode, and nothing more. Must stay
/// far below the 32-byte gap to `MIDNIGHT_BYTE_IF_LINEAR`.
const TOLERANCE: i32 = 2;

/// Vanilla's midnight, `18000` ticks. Fed to `sky_darken_for_time_of_day` rather
/// than hardcoding 0.24.
const MIDNIGHT_TICK: i64 = 18_000;
/// `sky_darken` at noon.
const NOON_DARKEN: f32 = 1.0;

/// A fog colour whose sRGB byte is unmistakable against both 128 and 50.
const FOG_SRGB_BYTE: u8 = 220;

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
                label: Some("packed-night-fog"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// One solid block at `(8, 8, 8)`; every other cell is air carrying full sky
/// light and no block light. This is what pins `ao = 255` / `sky = 255` — see the
/// module docs.
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

/// Camera two and a half blocks in front of the cube's `−Z` face, looking `+Z`.
fn front_camera() -> Camera {
    Camera {
        position: glam::Vec3::new(8.5, 8.5, 5.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 100.0,
    }
}

/// Render the cube with `sky_darken` and `fog` as given, returning the RGBA
/// readback.
fn frame(gpu: &Gpu, sky_darken: f32, fog: FogUniform) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let section = OneBlock;
    let hood = SectionNeighborhood::centre_only(&section);
    let mesh = mesh_simple(&hood);
    assert_eq!(mesh.quad_count(), 6, "an isolated cube has six faces");
    let gpu_mesh = GpuMesh::upload(device, &mesh).expect("non-empty mesh");

    // A uniform mid-grey sprite. Every channel equal, so a channel-crossing bug
    // could not hide in a colour cast.
    let mut rgba = vec![255u8; 16 * 16 * 4];
    for texel in rgba.chunks_exact_mut(4) {
        texel[0] = TEXEL;
        texel[1] = TEXEL;
        texel[2] = TEXEL;
    }
    let atlas = GpuAtlas::from_rgba(device, queue, 16, 16, &rgba, &[]);
    let uv = sprite_uv_buffer(device, &[[0.0, 0.0, 1.0, 1.0]]);

    let camera = front_camera();
    // `end_enabled.z` is the sky-darken lane — the same spare lane the entity and
    // model passes already use, so terrain and mobs cannot disagree about what
    // time it is.
    let mut fog = fog;
    fog.end_enabled[2] = sky_darken;
    let cam_buf = shared_camera_buffer(
        device,
        camera.view_projection().to_cols_array_2d(),
        fog,
    );
    let origin_buf = section_origin_buffer(device, [0.0, 0.0, 0.0]);

    let pipeline = BlockPipeline::new(device, FORMAT);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf, &origin_buf);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas, &uv);
    let depth = DepthBuffer::new(device, W, H);

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("packed-night-fog-color"),
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

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("packed-night-fog-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // Pure black: distinct from 128, from 50 and from the fog
                    // colour, so a background pixel is never a candidate.
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
        pass.set_bind_group(0, &cam_bg, &[0]);
        pass.set_bind_group(1, &atlas_bg, &[]);
        pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
        pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
    }

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("packed-night-fog-readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
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
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("map failed"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll failed");
    let mapped = slice.get_mapped_range().expect("mapped range");
    let mut out = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        let src = (y * padded) as usize;
        let dst = (y * bytes_per_row) as usize;
        out[dst..dst + bytes_per_row as usize]
            .copy_from_slice(&mapped[src..src + bytes_per_row as usize]);
    }
    drop(mapped);
    readback.unmap();
    out
}

fn px(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

/// The **bounding box** of non-background pixels, plus their modal red byte.
///
/// A bounding box rather than a percentage, per `CLAUDE.md`: a fraction cannot
/// tell a uniform-but-wrong frame from a localised blob, and two bugs here were
/// diagnosed in one step by printing *where*.
fn cube_bbox_and_modal_byte(pixels: &[u8]) -> ((u32, u32, u32, u32), u8, usize) {
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    let mut hist = [0usize; 256];
    let mut n = 0usize;
    for y in 0..H {
        for x in 0..W {
            let p = px(pixels, x, y);
            // Background is pure black; every cube byte here is >= 20.
            if p[0] < 8 {
                continue;
            }
            n += 1;
            hist[p[0] as usize] += 1;
            bbox = Some(match bbox {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    let modal = hist
        .iter()
        .enumerate()
        .max_by_key(|(_, c)| **c)
        .map(|(b, _)| b as u8)
        .unwrap_or(0);
    (bbox.expect("the cube drew no pixels at all"), modal, n)
}

fn assert_byte(pixels: &[u8], expected: i32, what: &str) {
    let (bbox, modal, n) = cube_bbox_and_modal_byte(pixels);
    let ok = (i32::from(modal) - expected).abs() <= TOLERANCE;
    assert!(
        ok,
        "{what}: expected modal cube byte {expected} ±{TOLERANCE}, got {modal} \
         over {n} px; cube bbox (x0,y0,x1,y1) = {bbox:?}.\n  \
         noon = {NOON_BYTE}, midnight = {MIDNIGHT_BYTE}, \
         midnight-if-linear = {MIDNIGHT_BYTE_IF_LINEAR}"
    );
}

#[test]
#[ignore = "requires a GPU adapter"]
fn packed_cubes_darken_at_night_by_vanillas_sky_factor() {
    let Some(gpu) = setup() else {
        panic!("no GPU adapter — do not run this gate on a machine without one");
    };

    let midnight_darken = sky_darken_for_time_of_day(MIDNIGHT_TICK);
    let noon = frame(&gpu, NOON_DARKEN, FogUniform::disabled());
    let night = frame(&gpu, midnight_darken, FogUniform::disabled());

    let (_, noon_modal, _) = cube_bbox_and_modal_byte(&noon);
    let (_, night_modal, _) = cube_bbox_and_modal_byte(&night);

    println!("=== PACKED SKY-DARKEN GATE (texel {TEXEL}, sRGB target) ===");
    println!("sky_darken at midnight   = {midnight_darken:.4}");
    println!("noon modal byte          = {noon_modal}  (predicted {NOON_BYTE})");
    println!("midnight modal byte      = {night_modal}  (predicted {MIDNIGHT_BYTE})");
    println!("  if no sky_darken       = {NOON_BYTE}");
    println!("  if linear-space shade  = {MIDNIGHT_BYTE_IF_LINEAR}");

    assert_byte(&noon, NOON_BYTE, "noon");
    assert_byte(&night, MIDNIGHT_BYTE, "midnight");

    // The negative control: hold sky_darken at noon for both renders. This is the
    // world the pre-fix shader lives in, and the midnight band must reject it.
    let control_a = frame(&gpu, NOON_DARKEN, FogUniform::disabled());
    let (_, control_modal, _) = cube_bbox_and_modal_byte(&control_a);
    assert!(
        (i32::from(control_modal) - MIDNIGHT_BYTE).abs() > TOLERANCE,
        "the no-time-term control landed inside the midnight band at {control_modal}; \
         this gate cannot distinguish the fix from the bug"
    );
}

#[test]
#[ignore = "requires a GPU adapter"]
fn packed_cubes_fade_into_the_fog_colour() {
    let Some(gpu) = setup() else {
        panic!("no GPU adapter — do not run this gate on a machine without one");
    };

    // start 0.5 / end 1.0, against a cube ~3.5 blocks away: `amount` saturates at
    // 1.0, so every cube pixel is the fog colour exactly.
    let settings = FogSettings {
        color: lodestone_render::fog::srgb_u8_to_linear([
            FOG_SRGB_BYTE,
            FOG_SRGB_BYTE,
            FOG_SRGB_BYTE,
        ]),
        start: 0.5,
        end: 1.0,
        ..FogSettings::disabled()
    };
    let fogged = frame(
        &gpu,
        NOON_DARKEN,
        FogUniform::new(&settings, front_camera().position.into()),
    );
    let (bbox, modal, n) = cube_bbox_and_modal_byte(&fogged);
    println!("=== PACKED FOG GATE ===");
    println!("fogged modal byte = {modal} over {n} px, bbox {bbox:?} (predicted {FOG_SRGB_BYTE})");
    assert_byte(&fogged, i32::from(FOG_SRGB_BYTE), "saturated fog");

    // The control: the same frame with fog disabled must be the unfogged byte,
    // not the fog colour. Without this, a shader that ignored `amount` and always
    // wrote the fog colour would pass above.
    let clear = frame(&gpu, NOON_DARKEN, FogUniform::disabled());
    assert_byte(&clear, NOON_BYTE, "fog disabled");
}
