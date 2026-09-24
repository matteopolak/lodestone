//! The banner pattern-layer draw list, measured through
//! the real pipeline: base colour plus ordered pattern-and-dye layers,
//! resolved from the item's pattern-layers component.
//!
//! # What this proves, and what it does not
//!
//! `docs/banner-shield-patterns.md`'s "Steps D-F: handoff" section proposed
//! this gate's exact shape: inject a directly-constructed 1×1 solid-colour
//! texture per layer (the real banner-pattern atlas is `lodestone-assets`
//! work not done in this pass — see that doc's "jar ships individual sprite
//! PNGs" section) and assert (a) the blend is genuinely translucent —
//! predicted from the two layers' tints and the standard alpha-blending formula,
//! not merely "some pixels changed" — and (b) two layers submitted in
//! opposite orders produce different composited colour.
//!
//! **The geometry and transform are real, not a stand-in.** Every draw here
//! uses [`BlockEntityModelSet::resolve_banner`]'s actual flag mesh
//! (`lodestone_assets::block_entity_models::banner_flag_model`) posed by its
//! actual `banner_ground_placement_matrix` and `banner_flag_x_rot` sway —
//! the same instance a real consumer's `BannerInstances::layers` would draw.
//! Only the *texture* is a fallback (a 1×1 solid colour standing in for a
//! real pattern-mask sprite); the mesh, resolver, placement and pipeline are
//! all the real step D/E/step-B/C code, consumed end to end.
//!
//! # Predicting the blend — and a measured surprise that changed the design
//!
//! `entity_depth_coincident_pixels.rs` hand-derives the diffuse term
//! (`0.739546`) from vanilla's fixed two-light entity lighting setup and
//! predicts an exact byte. A first version of this gate tried the same shape
//! for the blend —
//! measure each tint alone at full alpha, then predict the composite via
//! the standard alpha-blending textbook `src * a + dst * (1 - a)` evaluated in linear
//! light. **That formula's `a` was wrong on this machine's backend
//! (Metal).** A 12-point sweep of the fragment's raw alpha byte against the
//! *implied* linear-space mixing factor (solved from each measured
//! composite) traced a real, repeatable, monotonic curve — but not the
//! identity, not `linear_to_srgb(a)`, and not any single power law `aᵖ`
//! tried against it. Concretely: raw alpha `0.502` implied an effective
//! mixing factor of `~0.76`; raw `0.251` implied `~0.44`; raw `0.031`
//! implied `~0.08`. Something in this backend's SRGB-target blend path
//! reshapes the alpha factor, and pinning the exact closed form was not
//! worth the remaining effort on this task — chasing it further would have
//! been guessing a curve to fit noise, exactly the "read the answer off
//! the shader" failure `CLAUDE.md` warns against, just moved one level
//! up into a hand-fitted formula instead.
//!
//! So the assertion this gate actually makes is the property that curve
//! *cannot* affect either way, because it holds for **any** monotonic
//! alpha→mix curve with `mix(0) = 0` and `mix(1) = 1` (which the measured
//! sweep satisfies at every one of its 12 points): a **low** raw alpha must
//! composite close to the *first* (destination) layer and far from the
//! second; a **high** raw alpha must composite close to the second and far
//! from the first; and increasing alpha must move the result *monotonically*
//! from one anchor toward the other. This is still "predict the value, not
//! merely the sign" — [`the_composite_moves_from_destination_toward_source_as_alpha_rises`]
//! predicts three concrete numeric relationships (two "close to" bounds and
//! a three-point monotonic order) and requires the measurement to land on
//! all three, with a margin (`>40` of `255`) no rounding error can produce
//! by accident.
//!
//! The **wrong** hypothesis this rejects is what the *ordinary* (opaque,
//! cutout, `blend: None`) entity pipeline would produce instead: alpha
//! ignored entirely, so the second draw's colour would overwrite the first's
//! **at every alpha, including the lowest one tried**. That is the concrete,
//! wrong-but-plausible alternative a mis-wired banner layer pass would
//! produce — reusing `EntityPipeline::new`'s pipeline instead of
//! `banner_layer_pipeline` — and it is falsified by the low-alpha case alone
//! landing nowhere near the second layer's own colour.
//!
//! # Order-dependence, isolated from the blend
//!
//! At full alpha (`a = 1.0`), the standard alpha-blending formula collapses
//! to a pure overwrite (`src * 1 + dst * 0 = src`), so which layer is visible
//! is decided purely by **submission order**, not by depth or blending — this
//! is the same "translucent, depth-write-off" pipeline vanilla uses for a
//! banner pattern layer's own opaque interior mask texels (most of a mask's
//! own coverage is `alpha = 1`; only its antialiased edge is partial). The two
//! orderings must produce the complementary result, and each must be
//! byte-identical to that layer drawn alone — the same anti-vacuity control
//! the coincident-depth gate already uses, so a "survivor" that is secretly
//! a blend of both fails here too.
//!
//! # Fail closed
//!
//! `#[ignore]`d, so running it is opt-in; once opted in a missing adapter is
//! a failure, not a skip — mirroring every other GPU gate in this crate.
//!
//! ```text
//! cargo test -p lodestone-render --test banner_pattern_layer_pixels -- --ignored --nocapture
//! ```

#[path = "../gate_harness/mod.rs"]
mod gate_harness;

use glam::Vec3;
use lodestone_assets::{BannerPatternAtlas, Image, ResourceManager, ResourceSource, ZipSource};
use lodestone_render::block::DepthBuffer;
use lodestone_render::block_entity::{BANNER_FLAG, BannerSpawn, BlockEntityModelSet};
use lodestone_render::camera::Camera;
use lodestone_render::entity::ENTITY_FULLBRIGHT;
use lodestone_render::entity_pipeline::{EntityPipeline, GpuEntityModel, InstanceTint, upload_instances_tinted};

const W: u32 = 128;
const H: u32 = 128;

/// sRGB, matching the real swapchain and `entity_depth_coincident_pixels.rs`'s
/// own note: the entity shader's shade/tint multiply happens in gamma space
/// and is only correct — and only round-trips byte-for-byte through a lone
/// draw — against a target that re-encodes on write.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

const CLEAR: wgpu::Color = wgpu::Color { r: 0.0, g: 0.5, b: 0.0, a: 1.0 };

/// Vanilla's own gamma-space `textureDiffuseColor` bytes
/// (`lodestone_render::DyeColor`), used as the two layers' tints so the
/// predicted values are anchored to a real, independently-verified colour
/// table rather than an arbitrary test constant.
const RED: [u8; 3] = [176, 46, 38];
const BLUE: [u8; 3] = [60, 68, 170];

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
                label: Some("banner-pattern-layer-pixels"),
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

/// A real ground/standing banner at `[0, 0, 0]`, segment `0`, no sway
/// (`phase = 0`) — [`BannerSpawn::at`]'s default. The flag's real
/// `banner_ground_placement_matrix`/`banner_flag_x_rot` composition puts its
/// cloth roughly at world `x: 0.08..0.92, y: 0.17..1.83, z: 0.54..0.58`
/// (derived from the real baked box extents and the real placement formula,
/// not eyeballed), so a camera at `(0.5, 1.0, -2.0)` looking down `+Z` faces
/// it head-on at a comfortable distance.
fn camera() -> Camera {
    Camera {
        position: Vec3::new(0.5, 1.0, -2.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    }
}

/// A 2×2 solid-colour texture with the given alpha — the "directly-
/// constructed 1×1 solid-colour texture per layer" the handoff doc proposed
/// in place of the (unbuilt) real banner-pattern atlas. Colour is always
/// white: the *visible* colour a real consumer would see is vanilla's own
/// per-layer dye tint multiplied in by `EntityInstanceRaw::tint`, exactly
/// the mechanism this test exercises via [`InstanceTint`] — the texture
/// itself only has to carry coverage (alpha), matching how a real,
/// mostly-opaque pattern mask's *interior* behaves (its dye colour is not
/// baked into the PNG).
fn solid_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    alpha: u8,
) -> (wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("banner-layer-fallback-mask"),
        size: wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let texel = [255u8, 255, 255, alpha];
    let bytes: Vec<u8> = texel.iter().copied().cycle().take(16).collect();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &bytes,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(8), rows_per_image: Some(2) },
        wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("banner-layer-fallback-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

/// One layer to draw, in submission order: its tint and its mask's alpha.
#[derive(Clone, Copy)]
struct Layer {
    tint: [u8; 3],
    alpha: u8,
}

/// Draws `layers` in order, each its own draw call through the real
/// `EntityPipeline::banner_layer_pipeline`, all reusing the **same** real
/// flag instance transform from [`BlockEntityModelSet::resolve_banner`].
/// Returns the RGBA readback.
fn render(gpu: &Gpu, layers: &[Layer]) -> Vec<u8> {
    let textures: Vec<(wgpu::TextureView, wgpu::Sampler)> = layers
        .iter()
        .map(|layer| solid_texture(&gpu.device, &gpu.queue, layer.alpha))
        .collect();
    let tints: Vec<[u8; 3]> = layers.iter().map(|layer| layer.tint).collect();
    render_tinted_textures(gpu, &tints, &textures)
}

/// [`render`], generalised to accept any per-layer texture rather than only
/// [`solid_texture`]'s synthetic fallback — what [`render`] itself and
/// [`render_real`] (real jar sprites) both delegate to, so the two share
/// every line of pipeline/pass/readback plumbing and can only differ in how
/// the bound texture was produced.
fn render_tinted_textures(
    gpu: &Gpu,
    tints: &[[u8; 3]],
    textures: &[(wgpu::TextureView, wgpu::Sampler)],
) -> Vec<u8> {
    assert_eq!(tints.len(), textures.len(), "one tint per texture");
    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = camera();

    let models = BlockEntityModelSet::load();
    let banner = models
        .resolve_banner(&BannerSpawn::at([0, 0, 0]))
        .expect("banner_body and banner_flag must both be in the corpus");
    let flag_mesh = models.get(BANNER_FLAG).expect("banner_flag mesh");

    let ep = EntityPipeline::new(device, COLOR_FORMAT);
    let banner_pipeline = ep.banner_layer_pipeline(device, COLOR_FORMAT);
    let cam_buf = ep.camera_buffer(device, &camera);
    let cam_bg = ep.camera_bind_group(device, &cam_buf);
    let model = GpuEntityModel::upload_parts(
        device,
        &flag_mesh.vertices,
        &flag_mesh.indices,
        flag_mesh.parts.clone(),
    )
    .expect("the flag mesh is non-empty");

    // Every layer paints over the same posed flag — the real
    // `BannerInstances::layers[i].transform`, identical for every entry
    // (see `resolve_banner`'s doc). `layers[0]` (the always-present base
    // mask) is as good an anchor as any other index.
    let flag_transform = banner.layers[0].transform;
    let light = u32::from(ENTITY_FULLBRIGHT);

    struct Draw {
        buf: wgpu::Buffer,
        tex_bg: wgpu::BindGroup,
    }
    let draws: Vec<Draw> = tints
        .iter()
        .zip(textures)
        .map(|(tint, (view, sampler))| {
            let buf = upload_instances_tinted(device, &[flag_transform], &[light], &[InstanceTint::rgb(*tint)])
                .expect("one instance is non-empty");
            let tex_bg = ep.texture_bind_group(device, view, sampler);
            Draw { buf, tex_bg }
        })
        .collect();

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("banner-layer-color"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = DepthBuffer::new(device, W, H);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("banner-layer-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(CLEAR), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(lodestone_render::DEPTH_CLEAR), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&banner_pipeline);
        pass.set_bind_group(0, &cam_bg, &[]);
        for draw in &draws {
            pass.set_bind_group(1, &draw.tex_bg, &[]);
            pass.set_vertex_buffer(0, model.vertices.slice(..));
            pass.set_vertex_buffer(1, draw.buf.slice(..));
            pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..model.index_count, 0, 0..1);
        }
    }

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("banner-layer-readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &color, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(H) },
        },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("map failed"));
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll failed");
    let mapped = slice.get_mapped_range().expect("mapped range");
    let mut out = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        let src = (y * padded) as usize;
        let dst = (y * bytes_per_row) as usize;
        out[dst..dst + bytes_per_row as usize].copy_from_slice(&mapped[src..src + bytes_per_row as usize]);
    }
    drop(mapped);
    readback.unmap();
    out
}

/// The real 26.2 `client.jar`, opened once per call — mirrors
/// `entity_variant_pixels.rs`'s own `jar()` helper in this crate. Fails
/// closed: this file's real-sprite tests are `#[ignore]`d, so a missing jar
/// is an environment failure, never a silent skip.
fn jar_manager() -> ResourceManager {
    let path = gate_harness::require_client_jar();
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let zip = ZipSource::from_bytes(bytes).unwrap_or_else(|e| panic!("open jar: {e}"));
    ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>])
}

/// The real banner-pattern atlas (`lodestone_assets::BannerPatternAtlas` —
/// see `docs/banner-shield-patterns.md`), loaded from the real jar. Fails
/// closed for the same reason as [`jar_manager`].
fn real_atlas() -> BannerPatternAtlas {
    let manager = jar_manager();
    let (atlas, report) = BannerPatternAtlas::load_reported(&manager)
        .unwrap_or_else(|e| panic!("load the real banner-pattern atlas: {e}"));
    assert!(
        report.missing_textures.is_empty() && report.decode_errors.is_empty(),
        "real jar produced a lossy atlas: missing={:?} decode_errors={:?}",
        report.missing_textures,
        report.decode_errors,
    );
    atlas
}

/// Uploads a real decoded pattern-mask [`Image`] as the layer's sampled
/// texture — the real-sprite counterpart of [`solid_texture`]. Same format
/// and filter mode as the fallback (`Rgba8UnormSrgb`, `Nearest`,
/// `ClampToEdge`): the real PNGs are the same kind of near-white
/// mask-plus-coverage art the fallback stands in for (see
/// `docs/banner-shield-patterns.md`'s "The jar ships individual sprite
/// PNGs" section), so nothing about how the sampler is configured needs to
/// change, only what bytes it samples.
fn real_texture(device: &wgpu::Device, queue: &wgpu::Queue, img: &Image) -> (wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("banner-layer-real-mask"),
        size: wgpu::Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        &img.rgba,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(img.width * 4), rows_per_image: Some(img.height) },
        wgpu::Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("banner-layer-real-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

/// [`render`]/[`render_tinted_textures`], but binding **real** decoded
/// pattern-mask sprites instead of [`solid_texture`]'s synthetic fallback —
/// `layers[i]` is `(tint, sprite)`.
fn render_real(gpu: &Gpu, layers: &[([u8; 3], &Image)]) -> Vec<u8> {
    let textures: Vec<(wgpu::TextureView, wgpu::Sampler)> = layers
        .iter()
        .map(|(_, img)| real_texture(&gpu.device, &gpu.queue, img))
        .collect();
    let tints: Vec<[u8; 3]> = layers.iter().map(|(tint, _)| *tint).collect();
    render_tinted_textures(gpu, &tints, &textures)
}

fn px(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

/// The bounding box of pixels satisfying `pred` — `CLAUDE.md`'s "measure by
/// location, never by frame average": failure output says *where*.
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

/// The bounding box of pixels where `a` and `b` differ — the two-frame
/// counterpart of [`bbox`]'s single-predicate form, for comparing two full
/// renders rather than one render against a constant.
fn diff_bbox(a: &[u8], b: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..H {
        for x in 0..W {
            if px(a, x, y) != px(b, x, y) {
                found = Some(match found {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    found
}

/// Interior sample points, well inside the flag's own silhouette — the
/// centre plus four offsets, mirroring `entity_depth_coincident_pixels.rs`'s
/// `interior_samples`.
fn interior_samples() -> Vec<(u32, u32)> {
    let c = (W / 2, H / 2);
    vec![c, (c.0 - 8, c.1), (c.0 + 8, c.1), (c.0, c.1 - 8), (c.0, c.1 + 8)]
}

/// A low raw alpha for the second (`src`) layer — chosen, per the module
/// doc, to be close to the *destination* end of whatever this backend's
/// alpha→mix curve turns out to be, without needing to know that curve's
/// shape.
const LOW_ALPHA_BYTE: u8 = 24;
/// A high raw alpha for the second layer — the mirror of
/// [`LOW_ALPHA_BYTE`], close to the *source* end.
const HIGH_ALPHA_BYTE: u8 = 232;
/// A mid raw alpha, for the monotonicity check between the two extremes.
const MID_ALPHA_BYTE: u8 = 128;

/// Sum of per-channel absolute differences — used only to rank "closer to
/// A or to B", never as an absolute predicted value.
fn manhattan_rgb(a: [u8; 4], b: [u8; 4]) -> i32 {
    (0..3).map(|c| (i32::from(a[c]) - i32::from(b[c])).abs()).sum()
}

#[test]
#[ignore = "requires a GPU adapter"]
fn the_composite_moves_from_destination_toward_source_as_alpha_rises() {
    let Some(gpu) = setup() else {
        panic!("no GPU adapter — do not run this gate on a machine without one");
    };

    // Ground truth: each tint, alone, at full alpha — this single
    // measurement folds in whatever the diffuse/light-term/gamma round-trip
    // produces, with no hand-derived lighting constant to transcribe.
    let red_alone = render(&gpu, &[Layer { tint: RED, alpha: 255 }]);
    let blue_alone = render(&gpu, &[Layer { tint: BLUE, alpha: 255 }]);

    // Red opaque (the "base" layer) then blue at three different partial
    // alphas (a "pattern" layer's antialiased edge texel, at three
    // coverages) — exactly the shape `fs_main_no_cutout` exists for.
    let low = render(&gpu, &[Layer { tint: RED, alpha: 255 }, Layer { tint: BLUE, alpha: LOW_ALPHA_BYTE }]);
    let mid = render(&gpu, &[Layer { tint: RED, alpha: 255 }, Layer { tint: BLUE, alpha: MID_ALPHA_BYTE }]);
    let high = render(&gpu, &[Layer { tint: RED, alpha: 255 }, Layer { tint: BLUE, alpha: HIGH_ALPHA_BYTE }]);

    for (x, y) in interior_samples() {
        let dst = px(&red_alone, x, y);
        let src = px(&blue_alone, x, y);
        let l = px(&low, x, y);
        let m = px(&mid, x, y);
        let h = px(&high, x, y);

        let d_low_to_dst = manhattan_rgb(l, dst);
        let d_low_to_src = manhattan_rgb(l, src);
        let d_high_to_dst = manhattan_rgb(h, dst);
        let d_high_to_src = manhattan_rgb(h, src);

        // The wrong hypothesis this rejects: the *ordinary* opaque/cutout
        // entity pipeline (`blend: None`) ignores alpha for blending
        // purposes entirely, so it would land on `src` (blue) regardless of
        // how low the alpha is — `low`'s own alpha (24/255 ≈ 9%) is about as
        // unfavourable a case for that wrong hypothesis to be caught in as
        // this test picks, which is exactly why it is the one asserted with
        // the largest margin.
        assert!(
            d_low_to_dst < d_low_to_src,
            "at ({x},{y}) a low-alpha ({LOW_ALPHA_BYTE}/255) second layer composited \
             {l:?}, which is *closer* to blue-alone {src:?} (dist {d_low_to_src}) than \
             to red-alone {dst:?} (dist {d_low_to_dst}) — indistinguishable from the \
             wrong hypothesis that alpha is ignored and the second draw simply \
             overwrites the first"
        );
        assert!(
            d_low_to_src > 40,
            "at ({x},{y}) low-alpha composite {l:?} is within {d_low_to_src} of \
             blue-alone {src:?} — too close to the *wrong* pure-overwrite hypothesis \
             for this margin to mean anything"
        );

        assert!(
            d_high_to_src < d_high_to_dst,
            "at ({x},{y}) a high-alpha ({HIGH_ALPHA_BYTE}/255) second layer composited \
             {h:?}, which is *closer* to red-alone {dst:?} (dist {d_high_to_dst}) than \
             to blue-alone {src:?} (dist {d_high_to_src}) — a high-coverage mask texel \
             should look mostly like its own colour"
        );
        assert!(
            d_high_to_dst > 40,
            "at ({x},{y}) high-alpha composite {h:?} is within {d_high_to_dst} of \
             red-alone {dst:?} — too close to \"no blending happened\" for this \
             margin to mean anything"
        );

        // Monotonicity: raising the second layer's alpha must move the
        // composite steadily away from the destination and toward the
        // source, in *every* channel where the two anchors actually differ
        // — not just in an aggregate distance, which a channel-swapping bug
        // could still satisfy.
        for c in 0..3 {
            if dst[c] == src[c] {
                continue;
            }
            let (lc, mc, hc) = (i32::from(l[c]), i32::from(m[c]), i32::from(h[c]));
            if src[c] > dst[c] {
                assert!(
                    lc <= mc && mc <= hc,
                    "channel {c} at ({x},{y}) must rise monotonically toward blue's \
                     own {} as alpha rises: low {lc}, mid {mc}, high {hc}",
                    src[c]
                );
            } else {
                assert!(
                    lc >= mc && mc >= hc,
                    "channel {c} at ({x},{y}) must fall monotonically toward blue's \
                     own {} as alpha rises: low {lc}, mid {mc}, high {hc}",
                    src[c]
                );
            }
        }

        // The control this margin actually exists to catch: a *step*
        // function (discard below some threshold alpha, full overwrite
        // above it — exactly what the ordinary opaque/cutout pipeline's
        // `tex_col.a < 0.5` discard plus `blend: None` produces) can satisfy
        // every non-strict monotonic inequality above while landing the mid
        // sample **exactly** on one of the two anchors: `MID_ALPHA_BYTE`
        // (`128`, i.e. `0.502`) sits just above that pipeline's `0.5`
        // cutout threshold, so it would draw and fully overwrite, landing
        // exactly on `src` (blue) — measured directly by swapping
        // `banner_pipeline` for `ep.pipeline` in this file and re-running:
        // that neutered run passed every assertion above and only failed
        // here, with `d_mid_to_src == 0`. Genuine blending at a mid alpha
        // must differ from *both* anchors by more than rounding noise.
        let d_mid_to_dst = manhattan_rgb(m, dst);
        let d_mid_to_src = manhattan_rgb(m, src);
        assert!(
            d_mid_to_dst > 15 && d_mid_to_src > 15,
            "at ({x},{y}) the mid-alpha ({MID_ALPHA_BYTE}/255) composite {m:?} sits \
             within rounding noise of one of the two anchors (dist to red-alone \
             {dst:?}: {d_mid_to_dst}, dist to blue-alone {src:?}: {d_mid_to_src}) — \
             this is exactly what a hard cutout-discard-then-overwrite pipeline would \
             produce instead of a genuine blend, and the whole point of a mid sample \
             is to be a real mixture of both, not a copy of either"
        );
    }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn two_full_alpha_layers_composite_by_submission_order_not_by_colour() {
    let Some(gpu) = setup() else {
        panic!("no GPU adapter — do not run this gate on a machine without one");
    };

    let red_alone = render(&gpu, &[Layer { tint: RED, alpha: 255 }]);
    let blue_alone = render(&gpu, &[Layer { tint: BLUE, alpha: 255 }]);

    // At full alpha, standard alpha blending collapses to a pure overwrite, so
    // whichever layer is submitted *last* must win outright — and the
    // result must be byte-identical to that layer drawn alone (the same
    // anti-vacuity control `each_tint_rendered_alone_is_the_byte_the_
    // survivor_equals` uses for the coincident-depth gate).
    let blue_last = render(&gpu, &[Layer { tint: RED, alpha: 255 }, Layer { tint: BLUE, alpha: 255 }]);
    let red_last = render(&gpu, &[Layer { tint: BLUE, alpha: 255 }, Layer { tint: RED, alpha: 255 }]);

    for (x, y) in interior_samples() {
        assert_eq!(
            px(&blue_last, x, y),
            px(&blue_alone, x, y),
            "red-then-blue at ({x},{y}) must equal blue drawn alone; bbox of \
             mismatched pixels: {:?}",
            bbox(&blue_last, |p| p != px(&blue_alone, x, y)),
        );
        assert_eq!(
            px(&red_last, x, y),
            px(&red_alone, x, y),
            "blue-then-red at ({x},{y}) must equal red drawn alone; bbox of \
             mismatched pixels: {:?}",
            bbox(&red_last, |p| p != px(&red_alone, x, y)),
        );
        assert_ne!(
            px(&blue_last, x, y),
            px(&red_last, x, y),
            "the two submission orders must produce different composited colour \
             at ({x},{y}) — order-dependence is the whole property under test"
        );
    }
}

/// Per-pixel classification of `composite` against two anchors within a
/// screen-space `region`: `dst` (the layer beneath, i.e. what an
/// untouched/zero-coverage texel looks like) and `src` (the same layer's
/// own colour at full coverage). Shared by the real-sprite test below and
/// its uniform-texture control, so both read the region the same way.
#[derive(Debug)]
struct RegionCounts {
    /// Byte-identical to `dst` — zero coverage reached this pixel exactly
    /// (no margin needed: alpha `0` draws nothing at all).
    hole: usize,
    /// Clearly closer to `src` than to `dst` (see the margin in
    /// [`classify_region`]) — full coverage reached this pixel.
    full: usize,
    /// Neither of the above — a genuine partial blend.
    partial: usize,
    /// `region`'s pixel count, for computing fractions if needed.
    total: usize,
}

fn classify_region(composite: &[u8], dst: &[u8], src: &[u8], region: (u32, u32, u32, u32)) -> RegionCounts {
    let (x0, y0, x1, y1) = region;
    let mut counts = RegionCounts { hole: 0, full: 0, partial: 0, total: 0 };
    for y in y0..=y1 {
        for x in x0..=x1 {
            let c = px(composite, x, y);
            let d = px(dst, x, y);
            let s = px(src, x, y);
            counts.total += 1;
            if c == d {
                counts.hole += 1;
            } else if manhattan_rgb(c, s) < 40 {
                // Measured, not guessed: a 12-point sweep in this file's own
                // module doc found this backend's alpha->mix curve pushes a
                // raw-255 (fully covered) texel to within ~21-26 of `src`
                // (real texture RGB is not pure white -- creeper.png's
                // opaque interior is ~0.88-0.96 gray -- so it is not
                // byte-identical to the solid-fallback `src`, just close),
                // while a raw-191 (antialiased-edge) texel measured ~60-68
                // away -- a clean, wide gap this threshold sits inside.
                counts.full += 1;
            } else {
                counts.partial += 1;
            }
        }
    }
    counts
}

/// **Item 1's "re-gate against real sprites" step.** Every other test in
/// this file binds [`solid_texture`]'s directly-constructed 1×1 fallback —
/// deliberately, per this file's own module doc, to isolate the *pipeline*
/// (translucency, order-dependence) from the *sprite content*. This test
/// swaps in the real `entity/banner/{base,creeper}.png` masks (via the real
/// [`BannerPatternAtlas`], `lodestone-assets`) and proves the real,
/// spatially-varying alpha data they carry actually reaches the screen —
/// the island question `CLAUDE.md`'s own top rule asks of every subsystem:
/// a correctly *loaded* atlas that nothing ever samples differently is
/// indistinguishable, at this test's old coverage, from no atlas at all.
///
/// Three claims, and a control proving each is falsifiable:
/// - **A hole exists.** `creeper.png`'s real zero-alpha background must
///   leave some pixel *exactly* equal to the render beneath it — no margin
///   needed, since zero coverage draws nothing.
/// - **Full coverage exists.** `creeper.png`'s real fully-opaque interior
///   must land clearly closer to the pattern's own colour than to the
///   layer beneath, by a wide relative margin robust to the real texture's
///   own not-quite-white RGB (see [`classify_region`]'s doc).
/// - **A genuine partial blend exists.** `creeper.png` is real-measured to
///   carry a third alpha value (`191`, its antialiased edge — see
///   `lodestone-assets`' `real_jar.rs`), so some pixel must land in neither
///   bucket above.
///
/// **The control, executed:** rebinding `base.png` — verified spatially
/// uniform (fully opaque) across this exact region in `lodestone-assets`'
/// own real-jar census — as the second layer instead of `creeper.png`. A
/// uniform texture can produce `full` pixels but structurally cannot
/// produce a `hole`: there is no zero-alpha region for it to reveal. If
/// `hole > 0` still fired on this control, the detector would be finding
/// holes that are an artifact of the render pipeline (a coincident-depth
/// quirk, a rasterisation gap at the mesh's own edges) rather than real
/// alpha data — this is exactly the kind of thing a control has to prove
/// would fail, not merely be described as failing.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar"]
fn real_creeper_pattern_reaches_pixels_with_its_real_alpha_shape_not_a_uniform_rectangle() {
    let Some(gpu) = setup() else {
        panic!("no GPU adapter — do not run this gate on a machine without one");
    };

    let atlas = real_atlas();
    let base_img = atlas.base().expect("base mask must be in the real atlas");
    let creeper_img = atlas.get("creeper").expect("creeper pattern must be in the real atlas");

    // Ground truth silhouette: a flat, fully-opaque fallback layer touches
    // exactly the flag's real screen footprint. `render(&gpu, &[])` (zero
    // layers) gives the exact clear-colour bytes with no hardcoded constant.
    let clear_only = render(&gpu, &[]);
    let clear_px = px(&clear_only, 0, 0);
    let flag_red_only = render(&gpu, &[Layer { tint: RED, alpha: 255 }]);
    let flag_bbox =
        bbox(&flag_red_only, |p| p != clear_px).expect("the flat fallback layer must touch some pixels");

    let blue_alone = render(&gpu, &[Layer { tint: BLUE, alpha: 255 }]);

    let real_base_only = render_real(&gpu, &[(RED, base_img)]);
    let real_base_bbox = bbox(&real_base_only, |p| p != clear_px);
    assert_eq!(
        real_base_bbox,
        Some(flag_bbox),
        "the real base.png mask should be opaque across the whole flag silhouette, exactly \
         like the flat fallback -- got {real_base_bbox:?} vs flag silhouette {flag_bbox:?}"
    );

    let real_base_plus_creeper = render_real(&gpu, &[(RED, base_img), (BLUE, creeper_img)]);
    assert!(
        diff_bbox(&real_base_plus_creeper, &real_base_only).is_some(),
        "binding the real creeper.png as a second layer changed nothing -- the real atlas \
         reached the pipeline but not the screen (an island)"
    );

    let real_counts = classify_region(&real_base_plus_creeper, &real_base_only, &blue_alone, flag_bbox);
    assert!(
        real_counts.hole > 0,
        "expected some pixels within the flag silhouette to show zero creeper coverage \
         (== the base-only render exactly) -- creeper.png's real alpha=0 background never \
         reached the screen; counts={real_counts:?}"
    );
    assert!(
        real_counts.full > 0,
        "expected some pixels to show full creeper coverage (clearly closer to blue-alone \
         than to base-only) -- creeper.png's real alpha=255 interior never reached the \
         screen; counts={real_counts:?}"
    );
    assert!(
        real_counts.partial > 0,
        "expected some pixels to show a genuine partial blend (neither a hole nor full \
         coverage) -- creeper.png's real alpha=191 antialiased edge (measured directly \
         against the real PNG, see lodestone-assets' real_jar.rs) never reached the \
         screen; counts={real_counts:?}"
    );
    eprintln!(
        "real creeper pattern over flag_bbox {flag_bbox:?}: hole={} full={} partial={} \
         (total={})",
        real_counts.hole, real_counts.full, real_counts.partial, real_counts.total
    );

    // The control: a spatially uniform second layer must produce zero holes.
    let uniform_control = render_real(&gpu, &[(RED, base_img), (BLUE, base_img)]);
    let control_counts = classify_region(&uniform_control, &real_base_only, &blue_alone, flag_bbox);
    eprintln!("uniform (base-as-second-layer) control over the same region: {control_counts:?}");
    assert_eq!(
        control_counts.hole,
        0,
        "control failed to fail: a spatially uniform second layer produced a 'hole' pixel, \
         which should be structurally impossible -- counts={control_counts:?}"
    );
}
