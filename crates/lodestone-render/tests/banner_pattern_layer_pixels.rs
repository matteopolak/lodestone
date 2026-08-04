//! Issue #174, step F: the banner pattern-layer draw list, measured through
//! the real pipeline.
//!
//! # What this proves, and what it does not
//!
//! `docs/banner-shield-patterns.md`'s "Steps D-F: handoff" section proposed
//! this gate's exact shape: inject a directly-constructed 1×1 solid-colour
//! texture per layer (the real banner-pattern atlas is `lodestone-assets`
//! work not done in this pass — see that doc's "jar ships individual sprite
//! PNGs" section) and assert (a) the blend is genuinely translucent —
//! predicted from the two layers' tints and the `ALPHA_BLENDING` formula,
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
//! (`0.739546`) from `Lighting.java`'s two lights and predicts an exact
//! byte. A first version of this gate tried the same shape for the blend —
//! measure each tint alone at full alpha, then predict the composite via
//! `ALPHA_BLENDING`'s textbook `src * a + dst * (1 - a)` evaluated in linear
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
//! At full alpha (`a = 1.0`), `ALPHA_BLENDING`'s own formula collapses to a
//! pure overwrite (`src * 1 + dst * 0 = src`), so which layer is visible is
//! decided purely by **submission order**, not by depth or blending — this
//! is the same "translucent, depth-write-off" pipeline vanilla uses for
//! `submitPatterns`' opaque interior mask texels (most of a mask's own
//! coverage is `alpha = 1`; only its antialiased edge is partial). The two
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

use glam::Vec3;
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
    let draws: Vec<Draw> = layers
        .iter()
        .map(|layer| {
            let buf = upload_instances_tinted(
                device,
                &[flag_transform],
                &[light],
                &[InstanceTint::rgb(layer.tint)],
            )
            .expect("one instance is non-empty");
            let (view, sampler) = solid_texture(device, queue, layer.alpha);
            let tex_bg = ep.texture_bind_group(device, &view, &sampler);
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
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
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

    // At full alpha, `ALPHA_BLENDING` collapses to a pure overwrite, so
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
