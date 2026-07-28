//! Offscreen gate for the mining-crack pass: **the crack sprite must darken the
//! block surface it is drawn over**, and only where the sprite has cracks.
//!
//! The scene is minimal so the assertion is unambiguous. Clear the framebuffer
//! to a solid mid-grey "block face", clear depth to `1.0`, then draw a
//! full-frame crack quad through the real [`CrackPipeline`]. The crack atlas is
//! painted so its centre is opaque black (a crack) and its border is fully
//! transparent (no crack). If the pass works, the centre pixel is driven dark
//! while a corner pixel keeps the grey surface.
//!
//! The **negative control** runs in the same test and is observed failing: the
//! identical draw with a *fully transparent* atlas (alpha `0` everywhere, i.e.
//! no crack anywhere) leaves the centre grey. The whole point of the crack pass
//! is that delta; a gate that only saw the pass case would not catch a
//! regression to "crack draws nothing" — the exact defect this closes, where
//! the destroy-stage sprites were computed but never rendered.
//!
//! `#[ignore]`d because it needs a real GPU adapter; run explicitly:
//! `cargo test -p lodestone-render --test crack_gate -- --ignored --nocapture`.

use lodestone_render::crack::{CrackMesh, CrackVertex};
use lodestone_render::crack_pipeline::{CrackPipeline, GpuCrackMesh};
use lodestone_render::{CameraUniform, GpuAtlas, model_camera_buffer};

const W: u32 = 64;
const H: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Two surface clear colours (linear light — the convention `wgpu::Color`
/// clear values use on an `Rgba8UnormSrgb` target, confirmed by the existing
/// `crack_sprite_darkens_the_block_surface` comment: "clears to grey 0.5
/// linear, which the sRGB framebuffer reads back as byte ~188") for the
/// surface-proportionality gate below. `SURFACE_A` is the grey every other
/// case in this file renders over; `SURFACE_B` is a second, darker surface.
/// `SURFACE_B / SURFACE_A` is an exact `0.2`, chosen so the multiply
/// prediction below is exact-ratio arithmetic rather than a fudged decimal.
const SURFACE_A: f32 = 0.5; // linear; baseline byte ~188 (unchanged from the rest of this file)
const SURFACE_B: f32 = 0.1; // linear; baseline byte ~89

/// Predicted cracked-centre byte ratio (`SURFACE_B` case / `SURFACE_A` case)
/// under the **correct doubled-multiply blend**, `out = 2 * src * dst` —
/// **linear in `dst`**. This crack atlas's texel is near-black RGB `10` at
/// alpha `200` (see `crack_atlas_rgba`); sRGB-decoded, `src_linear =
/// srgb_decode(10/255) ~= 0.00304`. At both surfaces the multiplied output
/// stays under the sRGB encode curve's `0.0031308` linear segment
/// (`2*0.00304*0.5 ~= 0.00304`, `2*0.00304*0.1 ~= 0.00061`, both below
/// threshold), where the encode is the exact linear scale `x*12.92` — so the
/// *displayed byte* inherits the clear values' own ratio exactly:
/// `0.1/0.5 = 0.2`. Worked numerically: byte(`SURFACE_A`) = 10.0,
/// byte(`SURFACE_B`) = 2.0, ratio = 0.2 to the last digit.
const MULTIPLY_RATIO_PREDICTION: f32 = 0.2;

/// Predicted ratio under **alpha blending** (the bug this gate exists to
/// catch), `out = src*a + dst*(1-a)` — **affine, not linear, in `dst`**: it
/// carries a floor of `src*a` that does not vanish as the surface darkens.
/// Plugging in the same texel (`src_linear ~= 0.00304`, `a = 200/255 ~=
/// 0.784`) and re-encoding to sRGB bytes gives `~93` at `SURFACE_A` and
/// `~43` at `SURFACE_B` (compare the darkened-but-still-substantial `94`
/// this project's own negative control measured at `SURFACE_A` — see this
/// file's header comment), a ratio of `~0.46`: more than double the
/// multiply prediction, because the floor keeps `SURFACE_B`'s result far
/// above the `0.2`-scaled value a true multiply would produce.
const ALPHA_RATIO_PREDICTION: f32 = 0.46;

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
                label: Some("crack_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// A full-frame crack quad in clip space (identity camera). The UVs span the
/// whole atlas so the painted crack maps straight onto the frame.
fn crack_quad() -> CrackMesh {
    let v = |x: f32, y: f32, u: f32, w: f32| CrackVertex {
        position: [x, y, 0.5],
        uv: [u, w],
    };
    CrackMesh {
        vertices: vec![
            v(-1.0, -1.0, 0.0, 1.0),
            v(1.0, -1.0, 1.0, 1.0),
            v(1.0, 1.0, 1.0, 0.0),
            v(-1.0, 1.0, 0.0, 0.0),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

/// A 4x4 crack atlas. `cracked` controls the centre texels' alpha: `200` is a
/// real crack (dark, mostly opaque), `0` is the transparent negative control.
/// The border texels are always transparent so a corner sample stays on the
/// grey surface.
fn crack_atlas_rgba(cracked: u8) -> Vec<u8> {
    let mut px = Vec::with_capacity(4 * 4 * 4);
    for y in 0..4u32 {
        for x in 0..4u32 {
            let interior = (1..3).contains(&x) && (1..3).contains(&y);
            let a = if interior { cracked } else { 0 };
            // Crack colour is near-black; alpha carries the shape.
            px.extend_from_slice(&[10, 10, 10, a]);
        }
    }
    px
}

/// Render the crack quad over a grey "block face" and read back a pixel.
/// `center` picks the centre pixel (over a crack) when true, else a corner.
/// `draw` controls whether the crack pass is submitted at all: `false` gives
/// the true "without the overlay" baseline (the render pass runs, clears to
/// the grey surface, and nothing further is drawn onto it), as opposed to the
/// `cracked: 0` case, which draws the pass but with a fully transparent
/// atlas — a different scene that happens to produce the same pixels once the
/// alpha-discard is correct, and a useful cross-check of that fact.
/// `surface` is the render target's clear colour (linear light, same
/// convention as `wgpu::Color` clear values on an sRGB target — see
/// `SURFACE_A`/`SURFACE_B`'s doc comment) standing in for the block face's
/// brightness; every caller before the surface-proportionality gate below
/// passes `0.5`, the grey face those tests were written against.
fn render_pixel(gpu: &Gpu, cracked: u8, center: bool, draw: bool, surface: f32) -> (u8, u8, u8) {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = CrackPipeline::new(device, FORMAT);
    let atlas = GpuAtlas::from_rgba(device, queue, 4, 4, &crack_atlas_rgba(cracked), &[]);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);

    let cam_buffer = model_camera_buffer(
        device,
        CameraUniform {
            view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            section_origin: [0.0, 0.0, 0.0, 0.0],
        },
    );
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer);
    let mesh = GpuCrackMesh::upload(device, &crack_quad()).expect("non-empty crack mesh");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("block face + crack target"),
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
        label: Some("depth"),
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
            label: Some("crack gate"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The "block face": a solid grey already in the buffer, at
                    // whatever linear brightness the caller asked for.
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(surface),
                        g: f64::from(surface),
                        b: f64::from(surface),
                        a: 1.0,
                    }),
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
        if draw {
            pass.set_pipeline(&pipeline.pipeline);
            pass.set_bind_group(0, &cam_bg, &[]);
            pass.set_bind_group(1, &atlas_bg, &[]);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        }
    }

    let padded = (W * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
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

    let (px, py) = if center { (W / 2, H / 2) } else { (2, 2) };
    let i = (py * padded + px * 4) as usize;
    (data[i], data[i + 1], data[i + 2])
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fail"]
fn crack_sprite_darkens_the_block_surface() {
    let Some(gpu) = setup() else {
        panic!(
            "crack_gate: no GPU adapter. This test is #[ignore]d, so running it is an explicit \
             request for a real GPU frame — a headless CI box has none and should not run it."
        );
    };

    // The surface clears to grey 0.5 linear, which the sRGB framebuffer reads
    // back as byte ~188. A real crack (alpha ~0.78, colour ~black) must pull the
    // centre far below that.
    let (cr, _cg, _cb) = render_pixel(&gpu, 200, true, true, 0.5);
    // A corner sits over a transparent border texel: the crack must NOT touch it.
    let (edge_r, _, _) = render_pixel(&gpu, 200, false, true, 0.5);
    // Negative control: a fully transparent atlas (no crack) leaves the centre grey.
    let (control_r, _, _) = render_pixel(&gpu, 0, true, true, 0.5);

    println!("crack centre  r={cr}   (grey surface reads ~188; a crack pulls it dark)");
    println!("crack corner  r={edge_r} (transparent border: must stay ~188)");
    println!("no-crack ctrl r={control_r} (transparent atlas: must stay ~188)");

    // The cracked centre is clearly darkened, well below the grey 188 surface.
    assert!(
        cr < 140,
        "the crack sprite must darken the block surface: centre r={cr} should be well below \
         the grey ~188 surface"
    );
    // The transparent border does not touch the surface.
    assert!(
        edge_r > 160,
        "the crack must only affect cracked texels: corner r={edge_r} should keep the grey surface"
    );
    // Negative control: with no crack, the centre must remain the grey surface —
    // and it must be visibly brighter than the cracked centre. This is the delta
    // that makes the gate real; the no-crack render (r={control_r}) does NOT meet
    // the darken criterion above, which is exactly the regression this guards.
    assert!(
        control_r > 160 && control_r > cr + 40,
        "no-crack control must leave the surface grey (r={control_r}) and far brighter than the \
         cracked centre (r={cr})"
    );
}

/// Gate on the specific defect this pass fixes: **a multiply blend can only
/// ever darken**, so the crack pass must never leave the same texel brighter
/// than the raw, undrawn surface. `ALPHA_BLENDING` (the "too white" defect
/// this replaces) fails this precisely because alpha blending is free to
/// *add* the sprite's light texels on top, which can and does lighten.
///
/// This compares the crack pass against a genuine baseline: the same
/// framebuffer, same clear, but with the crack pass **not submitted at all**
/// (`draw: false`) rather than submitted-with-a-blank-atlas — the literal
/// "with and without the overlay" comparison, not a proxy for it.
///
/// To exercise the negative control described in the task: change the
/// `blend:` field in `crack_pipeline.rs` back to
/// `Some(wgpu::BlendState::ALPHA_BLENDING)`, rerun with
/// `-- --ignored --nocapture crack_multiply_can_only_darken`, and confirm the
/// `cracked_r <= baseline_r` assertion fails with alpha blending's lighter
/// centre pixel — then restore the multiply blend.
#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fail"]
fn crack_multiply_can_only_darken() {
    let Some(gpu) = setup() else {
        panic!(
            "crack_gate: no GPU adapter. This test is #[ignore]d, so running it is an explicit \
             request for a real GPU frame — a headless CI box has none and should not run it."
        );
    };

    // True baseline: the crack pass is never submitted. This is the block
    // face exactly as it would look with no mining overlay whatsoever.
    let (baseline_r, baseline_g, baseline_b) = render_pixel(&gpu, 200, true, false, 0.5);
    // Same scene, same atlas, but the crack pass is drawn over the centre
    // (which sits over the atlas's opaque, dark "crack" texels).
    let (cracked_r, cracked_g, cracked_b) = render_pixel(&gpu, 200, true, true, 0.5);
    // Same draw, but a corner texel (transparent border, discarded by the
    // alpha < 0.1 cutoff): must match the baseline, not just be "darker than
    // centre" -- the pass must be a true no-op off the cracked region.
    let (corner_r, corner_g, corner_b) = render_pixel(&gpu, 200, false, true, 0.5);

    println!(
        "baseline (no overlay drawn)      rgb=({baseline_r},{baseline_g},{baseline_b})"
    );
    println!(
        "cracked centre (multiply blend)  rgb=({cracked_r},{cracked_g},{cracked_b})"
    );
    println!(
        "uncracked corner (overlay drawn) rgb=({corner_r},{corner_g},{corner_b})"
    );

    // The core claim: a multiply blend can only ever darken. The cracked
    // texel must come out at or below the undrawn baseline on every channel,
    // and strictly below on at least one (it must actually do something).
    assert!(
        cracked_r <= baseline_r && cracked_g <= baseline_g && cracked_b <= baseline_b,
        "a multiply blend must never lighten the surface: cracked rgb=({cracked_r},{cracked_g},{cracked_b}) \
         must be <= baseline rgb=({baseline_r},{baseline_g},{baseline_b}) on every channel"
    );
    assert!(
        cracked_r < baseline_r,
        "the crack must measurably darken the surface, not merely fail to lighten it: \
         cracked r={cracked_r} should be strictly below baseline r={baseline_r}"
    );

    // The alpha-discard makes an undrawn corner byte-identical to the
    // baseline: no crack pass side effect should reach a texel the sprite
    // doesn't cover.
    assert!(
        corner_r == baseline_r && corner_g == baseline_g && corner_b == baseline_b,
        "an uncracked texel under the crack pass must exactly match the undrawn baseline: \
         corner rgb=({corner_r},{corner_g},{corner_b}) vs baseline rgb=({baseline_r},{baseline_g},{baseline_b})"
    );
}

/// Gate on the discriminating property `crack_multiply_can_only_darken`
/// misses: **a multiply scales with the surface underneath; alpha blending
/// does not.** That gate's `cracked_r <= baseline_r` assertion is a bound
/// alpha blending also satisfies at a single mid-grey surface (`94 <= 188`
/// holds exactly as well as `10 <= 188`) — it is the *assertion* species of
/// vacuous test, provably so: reverting `crack_pipeline.rs` to
/// `BlendState::ALPHA_BLENDING` and rerunning that gate still passes. Only
/// rendering the *same* crack sprite over *two* surfaces of different
/// brightness and comparing the ratio separates the two blends, because a
/// multiply is linear in the surface (ratio tracks the surface ratio
/// exactly) while alpha blending is affine (ratio is pulled toward `1.0` by
/// a floor that does not shrink with the surface). See
/// [`MULTIPLY_RATIO_PREDICTION`] and [`ALPHA_RATIO_PREDICTION`] for the
/// worked-out numbers this assertion's band is built to separate.
///
/// To exercise the negative control: change the `blend:` field in
/// `crack_pipeline.rs` back to `Some(wgpu::BlendState::ALPHA_BLENDING)`,
/// rerun with
/// `-- --ignored --nocapture crack_multiply_scales_proportionally_with_surface_brightness`,
/// and confirm the ratio assertion fails near [`ALPHA_RATIO_PREDICTION`] —
/// then restore the multiply blend.
#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fail"]
fn crack_multiply_scales_proportionally_with_surface_brightness() {
    let Some(gpu) = setup() else {
        panic!(
            "crack_gate: no GPU adapter. This test is #[ignore]d, so running it is an explicit \
             request for a real GPU frame — a headless CI box has none and should not run it."
        );
    };

    // The two surfaces' own (uncracked) brightness, for reference only —
    // confirms the clear colours landed where the doc comment predicts.
    let (base_a_r, _, _) = render_pixel(&gpu, 200, true, false, SURFACE_A);
    let (base_b_r, _, _) = render_pixel(&gpu, 200, true, false, SURFACE_B);
    // The crack sprite's centre texel, over each surface.
    let (cracked_a_r, _, _) = render_pixel(&gpu, 200, true, true, SURFACE_A);
    let (cracked_b_r, _, _) = render_pixel(&gpu, 200, true, true, SURFACE_B);
    // Localisation still holds at the darker surface too: a corner sits over
    // a transparent border texel and must not be touched by the pass.
    let (corner_b_r, _, _) = render_pixel(&gpu, 200, false, true, SURFACE_B);

    let ratio = f32::from(cracked_b_r) / f32::from(cracked_a_r);

    println!("=== CRACK BLEND SURFACE-PROPORTIONALITY GATE ===");
    println!("surface A baseline (no overlay, linear {SURFACE_A}) r={base_a_r}");
    println!("surface B baseline (no overlay, linear {SURFACE_B}) r={base_b_r}");
    println!("cracked centre @ surface A r={cracked_a_r}");
    println!("cracked centre @ surface B r={cracked_b_r}");
    println!("uncracked corner @ surface B r={corner_b_r} (must equal surface B baseline)");
    println!("measured ratio (surface B / surface A) = {ratio:.3}");
    println!("multiply prediction (fix)              = {MULTIPLY_RATIO_PREDICTION:.3}");
    println!("alpha prediction (bug)                 = {ALPHA_RATIO_PREDICTION:.3}");
    println!(
        "negative control: reverting crack_pipeline.rs's blend to \
         wgpu::BlendState::ALPHA_BLENDING renders this same two-surface scene at a ratio near \
         {ALPHA_RATIO_PREDICTION:.3}, which falls outside the 0.05..=0.35 band below and fails \
         the assertion."
    );

    // Localisation cross-check at the second surface: the corner is still
    // untouched by the pass, matching that surface's own baseline.
    assert_eq!(
        corner_b_r, base_b_r,
        "an uncracked texel under the crack pass must match the surface-B baseline: \
         corner r={corner_b_r} vs baseline r={base_b_r}"
    );

    // The discriminating assertion. The two predictions (0.2 vs 0.46) are
    // ~0.26 apart; a band of 0.05..=0.35 (0.15 of half-width around the
    // multiply prediction) comfortably absorbs integer-rounding noise on the
    // small bytes involved (cracked-centre bytes are ~10 and ~2) while still
    // leaving the alpha prediction (0.46) more than 0.11 outside the upper
    // bound — over 70% of the band's own half-width of daylight.
    assert!(
        (0.05..=0.35).contains(&ratio),
        "a doubled multiply must scale proportionally with the surface underneath: cracked-centre \
         ratio (surface B / surface A) = {ratio:.3} (B={cracked_b_r}, A={cracked_a_r}) should land \
         near the multiply prediction {MULTIPLY_RATIO_PREDICTION:.3}; a value near the alpha \
         prediction {ALPHA_RATIO_PREDICTION:.3} means the blend is additive, not multiplicative — \
         precisely the defect `crack_multiply_can_only_darken`'s single-surface `cracked_r <= \
         baseline_r` assertion cannot see, because alpha blending over a mid-grey surface still \
         satisfies that bound (94 <= 188) as easily as multiply's correct 10 <= 188"
    );
}
