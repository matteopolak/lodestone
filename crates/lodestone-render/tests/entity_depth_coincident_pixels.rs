//! Issue #21: the base entity pipeline's depth comparison, measured.
//!
//! # What vanilla actually says
//!
//! Every entity render type in 26.2 is built from `ENTITY_SNIPPET`, and that
//! snippet pins the depth state explicitly:
//!
//! ```text
//! RenderPipelines.java:49-56
//!   private static final RenderPipeline.Snippet ENTITY_SNIPPET = RenderPipeline.builder(...)
//!      ...
//!      .withDepthStencilState(DepthStencilState.DEFAULT)
//!      .buildSnippet();
//!
//! DepthStencilState.java:5-6
//!   public record DepthStencilState(CompareOp depthTest, boolean writeDepth,
//!                                   float depthBiasScaleFactor, float depthBiasConstant) {
//!      public static final DepthStencilState DEFAULT =
//!          new DepthStencilState(CompareOp.GREATER_THAN_OR_EQUAL, true);
//! ```
//!
//! `ENTITY_SOLID` (`RenderPipelines.java:232`), `ENTITY_CUTOUT`
//! (`:245`), `ENTITY_CUTOUT_CULL` (`:238`) and `ENTITY_TRANSLUCENT` (`:274`)
//! all inherit it — none of them overrides `withDepthStencilState`. Vanilla is
//! reversed-Z and this engine is `[0,1]` DirectX-style, so
//! `GREATER_THAN_OR_EQUAL` translates to `CompareFunction::LessEqual` here (the
//! standing translation in `CLAUDE.md`). The base pipeline shipped `Less`, which
//! is the one value that is *not* vanilla: coincident geometry resolves to the
//! **first** draw instead of the last.
//!
//! Note the record's field order while reading that transcription: it is
//! `(depthTest, writeDepth, depthBiasScaleFactor, depthBiasConstant)`, so a
//! two-argument `DEFAULT` sets no bias at all.
//!
//! # The measurement, and both hypotheses
//!
//! Two **coincident** unit quads at exactly `z = 0`, drawn in two separate draw
//! calls through `EntityPipeline::pipeline`, differing only in their per-instance
//! tint: one pure red `[255, 0, 0]`, one pure blue `[0, 0, 255]`. A tint channel
//! of `0` multiplies to `0` in the shader, so "which quad won" is readable as an
//! exact zero in two of the three channels — no threshold, no ratio.
//!
//! | draw order | `LessEqual` (vanilla) | `Less` (the bug) |
//! |---|---|---|
//! | red, then blue | **blue** survives | red survives |
//! | blue, then red | **red** survives | blue survives |
//!
//! The two hypotheses are complements of each other, so this cannot be satisfied
//! by both — which is the failure mode `CLAUDE.md`'s *magnitude* species warns
//! about (a predicate both the right and the wrong code satisfy).
//!
//! # The surviving byte is predicted, not merely non-zero
//!
//! For a quad facing the camera with `ENTITY_FULLBRIGHT` (`0xF0`: sky 15, block
//! 0) and fog disabled, every term is fixed by constants that live outside this
//! crate:
//!
//! * `lightmap_color(1.0, 0.0)` saturates to `(1, 1, 1)` — ambient `0x0A0A0A`
//!   plus a full sky contribution clamps at 1.0, and `not_gamma` is the identity
//!   at 1.0.
//! * The derived normal is `+Z`. Vanilla's two diffuse lights are
//!   `Lighting.java:17-18`, `(0.2, 1.0, -0.7)` and `(-0.2, 1.0, 0.7)`
//!   normalised, so `d0 = max(-0.7/√1.53, 0) = 0` and `d1 = 0.7/√1.53 =
//!   0.565910`.
//! * `light.glsl:3-4` gives `MINECRAFT_LIGHT_POWER 0.6` /
//!   `MINECRAFT_AMBIENT_LIGHT 0.4`, so
//!   `diffuse = min(1, 0.565910 * 0.6 + 0.4) = 0.739546`.
//! * A white texel on an sRGB atlas is `1.0` after `linear_to_srgb`, so the
//!   surviving channel is `0.739546` in gamma space → **`188.585`**, i.e. byte
//!   `188` or `189` after the target's own sRGB encode.
//!
//! So the assertion is `188 ± 2` in the winner's channel and an **exact `0`** in
//! the other two, and the wrong hypothesis lands at exact `0` in the channel
//! that should read `188`. [`each_tint_rendered_alone_is_the_byte_the_survivor_equals`]
//! additionally pins the composite to be *byte-identical* to that tint drawn on
//! its own, so a survivor that is really a blend of the two fails.
//!
//! # Controls
//!
//! * [`a_farther_coincident_draw_still_loses`] separates `LessEqual` from
//!   `Always`. If the depth test were not consulted at all, last-drawn would win
//!   regardless of depth; here the second draw is pushed a block *behind* the
//!   first and must lose.
//! * Both orderings of the coincident pair are measured, so "the blue draw is
//!   just brighter/always wins" cannot pass either.
//! * The negative control that cannot live in the tree: reverting
//!   `EntityPipeline::new`'s `depth_compare` to `Less` and re-running. Done by
//!   hand while landing this gate; both `coincident_*` cases fail with the
//!   complementary colour and the bounding box printed below.
//!
//! # Fail closed
//!
//! `#[ignore]`d, so running it is opt-in; once opted in a missing adapter is a
//! failure, not a skip.

use glam::{Mat4, Vec3};
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::PartRange;
use lodestone_render::entity_pipeline::{
    EntityPipeline, GpuEntityModel, InstanceTint, upload_instances_tinted,
};
use lodestone_render::models::ModelVertex;

const W: u32 = 128;
const H: u32 = 128;

/// sRGB, matching the real swapchain — the entity shader's shade/tint multiply
/// happens in gamma space and is only correct against a target that re-encodes
/// on write. See `entity_gate.rs`'s own note.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Mid-green: distinct from both tints in every channel, so a background pixel
/// can never be mistaken for a survivor.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.35,
    b: 0.0,
    a: 1.0,
};

const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];

/// `0.739546 * 255`, derived in the module docs from `Lighting.java:17-18` and
/// `light.glsl:3-4`. The GPU's own rounding of the final sRGB encode is the only
/// slack allowed.
const EXPECTED_SURVIVOR_BYTE: i32 = 188;
const SURVIVOR_TOLERANCE: i32 = 2;

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
                label: Some("entity-depth-coincident"),
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

/// A camera at `z = -2` looking down `+Z` (yaw `0` faces `+Z`, see
/// [`Camera::yaw`]), so a quad in the `z = 0` plane faces it head-on and the
/// derived normal is exactly `+Z`.
fn front_camera() -> Camera {
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
        // The entity shader reads its light from the *instance*, not the vertex
        // (see `push_part_quads`' doc), so this byte is inert here.
        light: 0xFF,
        tint: 255,
        anim: 0,
        cutout_bypass: 0,
        tint_rgb_override: [0, 0, 0, 0],
    }
}

/// A single unit quad in the `z = 0` plane, wound counter-clockwise as seen from
/// `-Z` (the camera side). `cull_mode` is `None` on this pipeline, so the winding
/// only has to be consistent, not correct.
fn quad_model(device: &wgpu::Device) -> GpuEntityModel {
    let vertices = [
        vertex([-0.5, -0.5, 0.0], [0.0, 1.0]),
        vertex([0.5, -0.5, 0.0], [1.0, 1.0]),
        vertex([0.5, 0.5, 0.0], [1.0, 0.0]),
        vertex([-0.5, 0.5, 0.0], [0.0, 0.0]),
    ];
    let indices = [0u32, 1, 2, 0, 2, 3];
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

/// A 2×2 fully-opaque white sheet. White so `linear_to_srgb(texel)` is exactly
/// `1.0` and the surviving byte is the shade term alone; opaque so the shader's
/// `a < 0.5` cutout never discards.
fn white_sheet(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("entity-depth-white"),
        size: wgpu::Extent3d {
            width: 2,
            height: 2,
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
        &[255u8; 16],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("entity-depth-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

/// Draw the quad once per `(z, tint)` entry, **in the order given**, each as its
/// own draw call so the ordering is the pass's primitive order rather than an
/// instance index. Returns the RGBA readback.
fn render(gpu: &Gpu, draws: &[(f32, [u8; 3])]) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = front_camera();

    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let (tex_view, sampler) = white_sheet(device, queue);
    let cam_buf = pipeline.camera_buffer(device, &camera);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);
    let model = quad_model(device);

    let light = u32::from(lodestone_render::entity::ENTITY_FULLBRIGHT);
    let buffers: Vec<wgpu::Buffer> = draws
        .iter()
        .map(|(z, tint)| {
            upload_instances_tinted(
                device,
                &[Mat4::from_translation(Vec3::new(0.0, 0.0, *z))],
                &[light],
                &[InstanceTint::rgb(*tint)],
            )
            .expect("one instance is non-empty")
        })
        .collect();

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("entity-depth-color"),
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

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("entity-depth-pass"),
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
        pass.set_bind_group(1, &tex_bg, &[]);
        for buf in &buffers {
            pass.set_vertex_buffer(0, model.vertices.slice(..));
            pass.set_vertex_buffer(1, buf.slice(..));
            pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..model.index_count, 0, 0..1);
        }
    }

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("entity-depth-readback"),
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

/// The bounding box of pixels satisfying `pred`, as `(x0, y0, x1, y1)`
/// inclusive. Failure output says *where*, per `CLAUDE.md` — a bare percentage
/// cannot tell a uniform-but-wrong frame from a localised blob.
fn bbox(pixels: &[u8], pred: impl Fn([u8; 4]) -> bool) -> Option<(u32, u32, u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..H {
        for x in 0..W {
            if pred(px(pixels, x, y)) {
                found = Some(match found {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    found
}

fn count(pixels: &[u8], pred: impl Fn([u8; 4]) -> bool) -> usize {
    (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .filter(|(x, y)| pred(px(pixels, *x, *y)))
        .count()
}

/// The rect the quad actually covers, derived from the same projection the draw
/// uses rather than from a hardcoded guess: a unit quad two blocks from a
/// 60°-vertical-FOV camera spans `1 / (2 * 2 * tan(30°)) = 0.4330` of the
/// viewport height either side of centre. Sampled well inside that, at the
/// centre ±16 px, so an off-by-one at the silhouette edge never decides the
/// verdict.
fn interior_samples() -> Vec<(u32, u32)> {
    let c = (W / 2, H / 2);
    vec![
        c,
        (c.0 - 16, c.1),
        (c.0 + 16, c.1),
        (c.0, c.1 - 16),
        (c.0, c.1 + 16),
    ]
}

/// `channel` is the index the winner's tint should light up; the other two must
/// be exactly `0`, because a tint channel of `0` multiplies to `0`.
fn assert_survivor(pixels: &[u8], channel: usize, what: &str) {
    let others: Vec<usize> = (0..3).filter(|c| *c != channel).collect();
    for (x, y) in interior_samples() {
        let p = px(pixels, x, y);
        let lit = i32::from(p[channel]);
        let ok = (lit - EXPECTED_SURVIVOR_BYTE).abs() <= SURVIVOR_TOLERANCE
            && others.iter().all(|c| p[*c] == 0);
        assert!(
            ok,
            "{what}: at ({x}, {y}) expected channel {channel} = {EXPECTED_SURVIVOR_BYTE} \
             ±{SURVIVOR_TOLERANCE} with the other two exactly 0, got {p:?}.\n  \
             bbox of pixels lit in channel {channel}: {:?}\n  \
             bbox of pixels lit in the other channels: {:?}\n  \
             lit-in-{channel} count: {} of {}",
            bbox(pixels, |p| p[channel] > 32),
            bbox(pixels, |p| others.iter().any(|c| p[*c] > 32)),
            count(pixels, |p| p[channel] > 32),
            W * H,
        );
    }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn coincident_second_draw_wins_at_equal_depth() {
    let Some(gpu) = setup() else {
        panic!("no GPU adapter — do not run this gate on a machine without one");
    };

    // Red first, blue second. `LessEqual` (vanilla) → blue. `Less` → red.
    let blue_last = render(&gpu, &[(0.0, RED), (0.0, BLUE)]);
    assert_survivor(&blue_last, 2, "red then blue at equal depth");

    // And the complement, so "blue always wins" cannot pass.
    let red_last = render(&gpu, &[(0.0, BLUE), (0.0, RED)]);
    assert_survivor(&red_last, 0, "blue then red at equal depth");
}

#[test]
#[ignore = "requires a GPU adapter"]
fn a_farther_coincident_draw_still_loses() {
    let Some(gpu) = setup() else {
        panic!("no GPU adapter — do not run this gate on a machine without one");
    };

    // The control that separates `LessEqual` from `Always`: the second draw is a
    // block *behind* the first (camera is at z = -2, so larger z is farther), so
    // a consulted depth test must reject it. Under `Always` the last draw would
    // win regardless and this reads blue.
    let pixels = render(&gpu, &[(0.0, RED), (1.0, BLUE)]);
    assert_survivor(&pixels, 0, "near red, then far blue");
}

#[test]
#[ignore = "requires a GPU adapter"]
fn each_tint_rendered_alone_is_the_byte_the_survivor_equals() {
    let Some(gpu) = setup() else {
        panic!("no GPU adapter — do not run this gate on a machine without one");
    };

    // Anti-vacuity for the two tests above: the composite's survivor is
    // byte-identical to that tint drawn on its own, so a "survivor" that is
    // really a blend of red and blue (which a colour-blend bug could produce
    // while still passing a channel-dominance check) fails here.
    let blue_alone = render(&gpu, &[(0.0, BLUE)]);
    let red_alone = render(&gpu, &[(0.0, RED)]);
    assert_survivor(&blue_alone, 2, "blue alone");
    assert_survivor(&red_alone, 0, "red alone");

    let blue_last = render(&gpu, &[(0.0, RED), (0.0, BLUE)]);
    let red_last = render(&gpu, &[(0.0, BLUE), (0.0, RED)]);
    for (x, y) in interior_samples() {
        assert_eq!(
            px(&blue_last, x, y),
            px(&blue_alone, x, y),
            "at ({x}, {y}) the blue survivor is not byte-identical to blue alone; \
             bbox of differing pixels: {:?}",
            bbox(&blue_last, |p| p != px(&blue_alone, x, y) && p[2] > 32),
        );
        assert_eq!(
            px(&red_last, x, y),
            px(&red_alone, x, y),
            "at ({x}, {y}) the red survivor is not byte-identical to red alone; \
             bbox of differing pixels: {:?}",
            bbox(&red_last, |p| p != px(&red_alone, x, y) && p[0] > 32),
        );
    }
}
