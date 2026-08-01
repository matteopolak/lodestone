//! GPU pixel gates for the three things issue #96 added to the sky pass: the
//! horizon-to-zenith **gradient** (and its freedom from banding), the
//! **sunrise/sunset band**, and **void fog**.
//!
//! `#[ignore]`d like every GPU gate in this crate:
//!
//! ```text
//! cargo test -p lodestone-render --test sky_gradient_pixels -- --ignored --nocapture
//! ```
//!
//! # Every measurement here is by *location*, never a frame average
//!
//! `CLAUDE.md`: a frame average once produced a confident wrong conclusion that
//! clustering by location immediately overturned, and a sunrise band is a
//! *localised* horizon feature that a whole-frame statistic is structurally
//! incapable of proving. So:
//!
//! * the gradient gate compares **each pixel** against the fog value derived
//!   from that pixel's own ray, and reports a bounding box plus the worst pixel
//!   on failure;
//! * the sunrise gate measures inside a rect obtained by **projecting the fan's
//!   own vertices** through the same `Camera::sky_view_projection` the draw
//!   uses — no restated screen constants — and its second control turns the
//!   camera *around* rather than changing the clock, which is the only thing
//!   that can distinguish a localised band from a global warm tint;
//! * the void-fog gate samples three eye heights on one curve.
//!
//! # What else paints here
//!
//! Before believing any control, `CLAUDE.md` asks what *else* already paints in
//! the measured region — a question this repo has got wrong four times, twice on
//! this exact sky work. In this pass the answer is the sun, the moon, the stars
//! and the cloud plane. Rather than reason about whether each happens to fall
//! outside a rect, [`bare_sky_manager`] feeds the pass **fully transparent**
//! sun/moon/cloud textures, so the disc and the sunrise band are the only two
//! draws that can produce a pixel at all. That is what makes "this pixel is
//! warm" or "this pixel is black" an unambiguous statement about the feature
//! under test.

use lodestone_render::fog::{VoidFog, linear_to_srgb_f32, srgb_to_linear_f32};
use lodestone_render::sky::{SKY_DISC_RADIUS, SKY_FOG_END_DISTANCE};
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, RenderTarget, SkyFrame, SkyRenderer, sunrise_fan_positions,
    sunrise_fan_transform, sunrise_sunset_color_for_time_of_day,
};

const W: u32 = 256;
const H: u32 = 256;

/// A non-sRGB target, so a shader's linear output lands in the readback bytes
/// unencoded and an expected colour can be computed in the same space the pass
/// writes in — the same choice `lodestone-shell`'s `sky_pixels.rs` makes.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The renderer's own bring-up sky colour (`lodestone_shell::gpu::SKY_COLOR`),
/// duplicated here because this crate cannot depend on the shell. Only ever
/// used as *an* input colour, never as an expected output, so a drift between
/// the two cannot make this gate wrong — only less representative.
const DAY_SKY: [f32; 3] = [0.242_867, 0.462_361, 0.827_571];

/// A distinctly different fog colour, so "the horizon end of the gradient is
/// the fog colour" is a falsifiable claim rather than something a single shared
/// constant would satisfy either way. A warm pale haze.
const DAY_FOG: [f32; 3] = [0.700, 0.600, 0.450];

/// The fog colour the **band** gates use instead of [`DAY_FOG`]: cool, so red
/// never dominates blue anywhere on the disc.
///
/// This is not a cosmetic difference, it is the fix for a control that failed
/// for a real reason. The band gates discriminate on "red clearly beats blue",
/// and with [`DAY_FOG`] the disc's own horizon end is a *warm* haze — so the
/// noon control (band alpha `0x00`, nothing drawn) still found 244 warm pixels
/// in a thin line at rows 119..121: the fogged rim of the disc. Exactly
/// `CLAUDE.md`'s "what else already paints here", caught by running the control
/// rather than by reasoning about it. Each gate now picks the fog colour that
/// makes the *other* draw in frame unable to satisfy its discriminator: warm fog
/// for the gradient (so it is clearly distinct from the blue sky), cool fog for
/// the band (so only the band can be warm).
const BAND_FOG: [f32; 3] = [0.300, 0.420, 0.580];

fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&rgba);
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(&buf).unwrap();
    }
    out
}

/// A pack whose sun, moon phases and cloud texture are **fully transparent**.
///
/// This is a deliberate choice, not a shortcut. `CelestialPipeline` blends
/// additively (`SrcAlpha`, `One`), so an alpha-0 texel adds exactly nothing, and
/// `CLOUD_WGSL` discards below `alpha < 0.04`. The frame therefore contains only
/// the sky disc and the sunrise band — which is precisely what lets these gates
/// say "the warm pixels here are the band" instead of "the warm pixels here are
/// the band or possibly the sun, which at dusk sits in the same place".
fn bare_sky_manager() -> lodestone_assets::ResourceManager {
    let mut src = lodestone_assets::MemorySource::new("sky-gradient-gate");
    src.insert(
        "assets/minecraft/textures/environment/celestial/sun.png".to_string(),
        png(8, 8, [0, 0, 0, 0]),
    );
    for name in lodestone_assets::MOON_PHASE_NAMES {
        src.insert(
            format!("assets/minecraft/textures/environment/celestial/moon/{name}.png"),
            png(8, 8, [0, 0, 0, 0]),
        );
    }
    src.insert(
        "assets/minecraft/textures/environment/clouds.png".to_string(),
        png(4, 4, [0, 0, 0, 0]),
    );
    lodestone_assets::ResourceManager::new(vec![Box::new(src)])
}

fn ctx() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    )
}

fn render(ctx: &GpuContext, camera: &Camera, frame: &SkyFrame) -> Vec<u8> {
    let (device, queue) = (ctx.device(), ctx.queue());
    let sky = SkyRenderer::new(device, queue, FORMAT, &bare_sky_manager())
        .expect("build the sky renderer over the transparent-art pack");
    let mut target = HeadlessTarget::new(device, W, H, FORMAT);
    let view = target.acquire().expect("headless acquire");
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("sky-gradient-gate"),
    });
    sky.render(device, queue, &mut encoder, view.view(), camera, frame);
    queue.submit(std::iter::once(encoder.finish()));
    target.read_texels(device, queue)
}

fn px(pixels: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2]]
}

// ---------------------------------------------------------------------------
// Ray/disc geometry, derived from the camera the draw uses
// ---------------------------------------------------------------------------

/// The camera-relative ray direction through the centre of pixel `(x, y)`,
/// obtained by **inverting the very matrix the sky pass uploads**
/// (`Camera::sky_view_projection`) rather than re-deriving a projection here.
///
/// `CLAUDE.md`: derive layout from the same expression the draw uses, never
/// restate a constant. A hand-rolled `tan(fov/2)` ray would agree with the
/// shipped matrix only as long as nothing about the projection changed — and
/// this project's depth range and handedness are exactly the sort of thing that
/// does change. The sky matrix is translation-free, so the eye is the origin of
/// this space and any unprojected clip point gives the direction outright.
fn ray_through_pixel(camera: &Camera, x: u32, y: u32) -> glam::Vec3 {
    let inv = camera.sky_view_projection().inverse();
    let u = 2.0 * (x as f32 + 0.5) / W as f32 - 1.0;
    let v = 1.0 - 2.0 * (y as f32 + 0.5) / H as f32;
    let clip = glam::Vec4::new(u, v, 0.5, 1.0);
    let world = inv * clip;
    (world.truncate() / world.w).normalize()
}

/// The exact fog blend factor the shipped fragment shader must produce for a
/// pixel, or `None` if that pixel's ray misses the sky disc.
///
/// Reproduces the geometry the pass actually draws, from the pass's own
/// constants: the disc is a plane at camera-relative `y = +16`
/// (`sky_disc_positions(16.0)` in `SkyRenderer::render`), and the fragment's fog
/// factor is `clamp(|hit| / SKY_FOG_END_DISTANCE, 0, 1)`.
///
/// The radius test is deliberately conservative. `sky_disc_positions` builds a
/// nine-gon *inscribed* in [`SKY_DISC_RADIUS`], so its boundary dips to
/// `R·cos(22.5°)` at the chord midpoints; a pixel between the inscribed polygon
/// and the circumscribed circle is legitimately unpainted, and asserting on it
/// would be a gate failing on its own bad premise. Everything inside 90% of the
/// inradius is unambiguously on the disc.
fn expected_fog_value(camera: &Camera, x: u32, y: u32) -> Option<f32> {
    const DISC_Y: f32 = 16.0;
    let inradius = SKY_DISC_RADIUS * (22.5f32.to_radians()).cos() * 0.9;
    let dir = ray_through_pixel(camera, x, y);
    if dir.y <= 1e-4 {
        return None; // ray goes level or downward: never meets the overhead disc
    }
    let hit = dir * (DISC_Y / dir.y);
    if hit.x.hypot(hit.z) > inradius {
        return None;
    }
    Some((hit.length() / SKY_FOG_END_DISTANCE).clamp(0.0, 1.0))
}

/// `mix(sky, fog, t)` in linear RGB, then quantised the way an `Rgba8Unorm`
/// target does — the same expression `SKY_DISC_WGSL`'s fragment stage computes.
fn expected_pixel(sky: [f32; 3], fog: [f32; 3], t: f32) -> [u8; 3] {
    let mut out = [0u8; 3];
    for i in 0..3 {
        let c = sky[i] + (fog[i] - sky[i]) * t;
        out[i] = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    out
}

/// The largest per-channel error between the frame and the per-pixel expected
/// gradient, with **where** it happened and how many pixels were checked.
///
/// Returns `(worst_error, worst_pixel_report, checked_count, bad_count,
/// bbox_of_bad)`.
///
/// **The `bad_count` is the discriminator, not the worst error** — measured, not
/// assumed. The per-vertex banding control's worst per-channel error is only
/// `8/255`, because the visible screen region tops out around fog value `0.83`
/// and the azimuthal ripple there is a few percent; an assertion on the worst
/// error alone read as "no banding". Its *count* is 16062 pixels against the
/// shipped path's **0**, which is the signal. A gate that reported only a scalar
/// could not tell a uniform-but-wrong frame from a localised ripple, which is
/// exactly the distinction between "the gradient is missing" and "the gradient
/// bands".
fn gradient_error(
    pixels: &[u8],
    camera: &Camera,
    sky: [f32; 3],
    fog: [f32; 3],
    tolerance: u8,
) -> (u8, String, usize, usize, String) {
    let mut worst = 0u8;
    let mut worst_report = "no pixels checked".to_string();
    let mut checked = 0usize;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut bad = 0usize;
    for y in 0..H {
        for x in 0..W {
            let Some(t) = expected_fog_value(camera, x, y) else {
                continue;
            };
            checked += 1;
            let want = expected_pixel(sky, fog, t);
            let got = px(pixels, x, y);
            let err = (0..3)
                .map(|i| got[i].abs_diff(want[i]))
                .max()
                .unwrap_or(0);
            if err > tolerance {
                bad += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
            if err > worst {
                worst = err;
                worst_report =
                    format!("({x},{y}) t={t:.3} want {want:?} got {got:?} err {err}");
            }
        }
    }
    let bbox = if bad == 0 {
        "no pixels outside tolerance".to_string()
    } else {
        format!("{bad} px outside tolerance, bbox x{x0}..{x1} y{y0}..{y1}")
    };
    (worst, worst_report, checked, bad, bbox)
}

/// A camera looking level at a narrow-ish vertical FOV, which is where the sky
/// gradient actually lives.
///
/// The gradient is compressed into a few degrees above the horizon and that is
/// not a bug: the disc is only 16 blocks above the eye and 512 across, so a ray
/// reaches the fully-fogged rim at `atan(16/512) = 1.79°` of elevation and is
/// already at half fog by `3.6°`. A 90°-FOV camera would spend 95% of its
/// pixels in the near-zenith regime where the gradient is flat — measurably
/// "correct" while exercising almost none of it. 30° is wide enough to include
/// the flat zenith end *and* dense enough through the ramp.
fn horizon_camera(yaw: f32) -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 70.0, 0.0),
        yaw,
        pitch: 0.0,
        fov_y_degrees: 30.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 1024.0,
    }
}

// ---------------------------------------------------------------------------
// 1. The gradient
// ---------------------------------------------------------------------------

/// The shipped disc must match, **per pixel**, the fog blend derived from that
/// pixel's own ray — which is simultaneously the assertion that the gradient
/// exists, that it runs the right way round (sky at the zenith, fog at the
/// horizon), and that it does not band.
#[test]
#[ignore = "requires a GPU adapter"]
fn the_horizon_gradient_matches_the_per_pixel_fog_value() {
    let ctx = ctx();
    let camera = horizon_camera(0.0);
    // Noon: the `SKY_COLOR`/`FOG_COLOR` multipliers are both `#ffffff`, so the
    // resolved colours are exactly the frame's inputs and the expected value
    // needs no timeline arithmetic of its own.
    let frame = SkyFrame::new(6_000, DAY_SKY).with_fog_color(DAY_FOG);
    let pixels = render(&ctx, &camera, &frame);

    let (worst, report, checked, bad, bbox) =
        gradient_error(&pixels, &camera, DAY_SKY, DAY_FOG, 3);
    eprintln!("=== #96 gradient gate: subject (shipped, per-fragment) ===");
    eprintln!("checked {checked} px on the disc; worst per-channel error {worst}; {bad} outside tolerance");
    eprintln!("worst pixel: {report}");
    eprintln!("{bbox}");

    assert!(
        checked > 4_000,
        "only {checked} pixels landed on the disc — the camera or the disc geometry \
         moved and this gate is now measuring almost nothing"
    );
    assert_eq!(
        bad, 0,
        "the disc does not match the per-pixel fog gradient. worst {worst}: {report}\n{bbox}"
    );

    // Sanity: the gradient must actually *span* a range in this frame, or a
    // pass that painted a flat fog-coloured disc would also match "the expected
    // value" everywhere the expected value happened to be constant.
    let mut min_t = 1.0f32;
    let mut max_t = 0.0f32;
    for y in 0..H {
        if let Some(t) = expected_fog_value(&camera, W / 2, y) {
            min_t = min_t.min(t);
            max_t = max_t.max(t);
        }
    }
    assert!(
        max_t - min_t > 0.5,
        "this camera only spans fog values {min_t:.3}..{max_t:.3}; a gradient gate needs \
         most of the range in frame"
    );
    eprintln!("centre column spans fog value {min_t:.3}..{max_t:.3}");
}

/// **Control, EXECUTED.** Hand the shipped pass a fog colour *equal* to the sky
/// colour — the pass's own pre-#96 state, where the disc was a single flat
/// colour. The frame must then be uniform, and the gradient detector above must
/// report a large error against the two-colour expectation.
///
/// This is what proves the gradient is driven by the fog colour rather than by
/// something incidental in the shader: it runs the shipped code, changes one
/// input, and the measurement collapses.
#[test]
#[ignore = "requires a GPU adapter"]
fn control_a_fog_colour_equal_to_the_sky_colour_produces_no_gradient() {
    let ctx = ctx();
    let camera = horizon_camera(0.0);
    let flat = SkyFrame::new(6_000, DAY_SKY); // `new` leaves fog == sky
    let pixels = render(&ctx, &camera, &flat);

    // Against the *flat* expectation (t irrelevant, both endpoints equal) it
    // must match everywhere...
    let (flat_worst, flat_report, checked, flat_bad, _) =
        gradient_error(&pixels, &camera, DAY_SKY, DAY_SKY, 3);
    // ...and against the two-colour expectation it must fail badly.
    let (two_worst, two_report, _, two_bad, two_bbox) =
        gradient_error(&pixels, &camera, DAY_SKY, DAY_FOG, 3);

    eprintln!("=== #96 gradient gate: flat-colour control ===");
    eprintln!("checked {checked} px");
    eprintln!("vs flat expectation:       worst {flat_worst}, {flat_bad} bad ({flat_report})");
    eprintln!("vs two-colour expectation: worst {two_worst}, {two_bad} bad ({two_report})");
    eprintln!("{two_bbox}");

    assert_eq!(
        flat_bad, 0,
        "with fog == sky the disc must be uniform: {flat_report}"
    );
    assert!(
        two_worst > 40,
        "control failed to fail: a flat disc should be dramatically wrong against the \
         gradient expectation, but the worst error was only {two_worst} — which would mean \
         the affirmative gate above is not actually sensitive to the gradient"
    );
}

/// **Control, EXECUTED.** Vanilla computes the fog factor per *vertex*
/// (`sky.vsh`) over ten vertices, so its gradient is barycentric across eight
/// triangles hundreds of blocks wide. That produces an eight-fold azimuthal
/// ripple: interpolated across a triangle, the factor depends only on the
/// centre vertex's barycentric weight, which reaches zero at a chord *midpoint*
/// (radius `512·cos(22.5°) = 473`, true fog value `0.924`) while the shader
/// there reports `1.0`.
///
/// This builds that per-vertex pipeline over the *same* disc geometry and checks
/// the detector sees it. Without this, "zero pixels outside tolerance" could
/// just mean the detector is blind to banding.
///
/// The assertion is on the *count*, and that choice was measured rather than
/// chosen: the per-vertex frame's worst per-channel error is only `8/255`, so an
/// assertion on the worst error would have read as "no banding here". The count
/// is 16062 pixels against the shipped path's 0.
#[test]
#[ignore = "requires a GPU adapter"]
fn control_a_per_vertex_gradient_bands_and_the_detector_sees_it() {
    let ctx = ctx();
    let camera = horizon_camera(0.0);
    let pixels = render_disc_per_vertex(&ctx, &camera, DAY_SKY, DAY_FOG);
    let (worst, report, checked, bad, bbox) =
        gradient_error(&pixels, &camera, DAY_SKY, DAY_FOG, 3);

    eprintln!("=== #96 gradient gate: per-vertex (vanilla-shaped) banding control ===");
    eprintln!("checked {checked} px; worst per-channel error {worst}; {bad} outside tolerance");
    eprintln!("worst pixel: {report}");
    eprintln!("{bbox}");

    assert!(checked > 4_000, "control measured almost nothing: {checked} px");
    assert!(
        bad * 4 > checked,
        "control failed to fail: a per-vertex fog factor over an eight-triangle fan must \
         deviate from the true radial gradient across a large share of the disc, but only \
         {bad} of {checked} pixels did (worst error {worst}). If this ever passes, the \
         detector cannot distinguish per-vertex from per-fragment and the affirmative gate \
         proves nothing about banding"
    );
}

/// Vanilla's per-vertex sky-disc shading, over the same ten-vertex fan the
/// shipped pass draws. Exists only as the executed control above.
fn render_disc_per_vertex(
    ctx: &GpuContext,
    camera: &Camera,
    sky: [f32; 3],
    fog: [f32; 3],
) -> Vec<u8> {
    // `sky.vsh` + `sky.fsh`, faithfully: `length(Position)` in the *vertex*
    // stage, so the rasteriser interpolates the resulting factor.
    const PER_VERTEX_WGSL: &str = r"
struct Camera { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> camera: Camera;
const SKY_FOG_END: f32 = 512.0;
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) fog_color: vec4<f32>,
    @location(2) fog_value: f32,
};
@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) fog_color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(position, 1.0);
    out.color = color;
    out.fog_color = fog_color;
    out.fog_value = clamp(length(position) / SKY_FOG_END, 0.0, 1.0);
    return out;
}
@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(mix(in.color.rgb, in.fog_color.rgb, in.fog_value * in.fog_color.a), 1.0);
}
";
    use wgpu::util::DeviceExt;
    let (device, queue) = (ctx.device(), ctx.queue());

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct V {
        position: [f32; 3],
        color: [f32; 4],
        fog_color: [f32; 4],
    }

    let verts: Vec<V> = lodestone_render::sky_disc_positions(16.0)
        .into_iter()
        .map(|position| V {
            position,
            color: [sky[0], sky[1], sky[2], 1.0],
            fog_color: [fog[0], fog[1], fog[2], 1.0],
        })
        .collect();
    let indices = lodestone_render::sky_disc_indices();

    let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("per-vertex-disc-vbuf"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("per-vertex-disc-ibuf"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    let cam_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("per-vertex-disc-cam"),
        contents: bytemuck::cast_slice(&camera.sky_view_projection().to_cols_array()),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("per-vertex-disc-bgl"),
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
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("per-vertex-disc-bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: cam_buf.as_entire_binding(),
        }],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("per-vertex-disc"),
        source: wgpu::ShaderSource::Wgsl(PER_VERTEX_WGSL.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("per-vertex-disc-layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    const ATTRS: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4, 2 => Float32x4];
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("per-vertex-disc"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<V>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &ATTRS,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    let mut target = HeadlessTarget::new(device, W, H, FORMAT);
    let view = target.acquire().expect("headless acquire");
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("per-vertex-disc-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("per-vertex-disc-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: view.view(),
                resolve_target: None,
                depth_slice: None,
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
        pass.set_bind_group(0, &bg, &[]);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..indices.len() as u32, 0, 0..1);
    }
    queue.submit(std::iter::once(encoder.finish()));
    target.read_texels(device, queue)
}

// ---------------------------------------------------------------------------
// 2. The sunrise/sunset band
// ---------------------------------------------------------------------------

/// Peak sunset (`day.json`'s tick-12732 keyframe, `#feda6333`).
const PEAK_SUNSET_TICK: i64 = 12_732;

/// A pixel is "warm" when red clearly dominates blue. At dusk the sky disc is
/// `SKY_COLOR` scaled by the `#848484` multiplier — still blue-dominant — while
/// the band is `#da6333`, so this separates the two cleanly without needing an
/// exact expected colour (which the linear-vs-gamma blend-space divergence
/// documented on `SUNRISE_BLEND` would make brittle).
fn warm(p: [u8; 3]) -> bool {
    i32::from(p[0]) > i32::from(p[2]) + 24
}

/// Warm-pixel statistics restricted to pixels **the sky disc paints**, plus a
/// bounding box and the mean elevation of the warm pixels versus of the disc
/// region as a whole.
///
/// # Why "restricted to disc pixels", and why not a projected rect
///
/// The first version of this gate projected the fan's own vertices to a screen
/// rect and asserted the warm pixels landed inside it. That rect came out as
/// (0, 0, 255, 128) — the entire upper frame — and the reason is worth
/// recording, because it is a fact about vanilla's geometry that is easy to get
/// wrong from the source: `buildSunriseFan`'s perimeter vertices are **not**
/// offsets from the bright centre vertex. The centre is `(0, 100, 0)` and the
/// perimeter is `(sin·120, cos·120, -cos·40)`, so after
/// `sunrise_fan_transform` the perimeter is a ring of radius 120 centred on the
/// **eye** with the bright apex 100 blocks off toward the sunset. The fan
/// therefore wraps the whole sky and no rect localises it; what makes it read as
/// a *band* is the vertical squash (±40·alpha tall against 120 wide) plus the
/// centre-to-rim alpha ramp.
///
/// Below the horizon the destination is the pass's black clear, so even a
/// near-transparent band fragment trivially satisfies "red beats blue" there —
/// which is why the measurement is confined to pixels where
/// [`expected_fog_value`] says the disc is painting. Against a blue disc, "warm"
/// means the band actually asserted itself.
struct BandReport {
    warm: usize,
    disc: usize,
    bbox: String,
    warm_mean_elevation_deg: f64,
    disc_mean_elevation_deg: f64,
}

fn band_report(pixels: &[u8], camera: &Camera) -> BandReport {
    let mut warm_n = 0usize;
    let mut disc_n = 0usize;
    let mut warm_elev = 0.0f64;
    let mut disc_elev = 0.0f64;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..H {
        for x in 0..W {
            if expected_fog_value(camera, x, y).is_none() {
                continue;
            }
            let elev = f64::from(ray_through_pixel(camera, x, y).y.asin().to_degrees());
            disc_n += 1;
            disc_elev += elev;
            if warm(px(pixels, x, y)) {
                warm_n += 1;
                warm_elev += elev;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    BandReport {
        warm: warm_n,
        disc: disc_n,
        bbox: if warm_n == 0 {
            "no warm pixels on the disc".to_string()
        } else {
            format!("warm bbox x{x0}..{x1} y{y0}..{y1}")
        },
        warm_mean_elevation_deg: warm_elev / warm_n.max(1) as f64,
        disc_mean_elevation_deg: disc_elev / disc_n.max(1) as f64,
    }
}

/// A level camera aimed at the sunset, with the yaw **derived** from where the
/// band's own transform puts it rather than typed in.
fn camera_facing_band(time_of_day: i64) -> Camera {
    let alpha = f32::from(sunrise_sunset_color_for_time_of_day(time_of_day)[3]) / 255.0;
    let angle =
        lodestone_render::celestial_angle_for_time_of_day(time_of_day) * std::f32::consts::TAU;
    let centre = sunrise_fan_transform(angle, alpha.max(1e-3))
        .transform_point3(glam::Vec3::from(sunrise_fan_positions()[0]));
    // `Camera::forward` is `(-sin(yaw), …, cos(yaw))` for pitch 0, so pointing
    // at `(dx, _, dz)` means `yaw = atan2(-dx, dz)`.
    let yaw = (-centre.x).atan2(centre.z).to_degrees();
    Camera {
        fov_y_degrees: 70.0,
        ..horizon_camera(yaw)
    }
}

/// The band must overwrite a substantial part of the blue sky disc with warm
/// pixels, and those pixels must **hug the horizon** rather than wash the whole
/// dome — the difference between a band and a global tint.
#[test]
#[ignore = "requires a GPU adapter"]
fn the_sunset_band_paints_a_warm_stripe_hugging_the_horizon() {
    let ctx = ctx();
    let camera = camera_facing_band(PEAK_SUNSET_TICK);
    let frame = SkyFrame::new(PEAK_SUNSET_TICK, DAY_SKY).with_fog_color(BAND_FOG);
    let r = band_report(&render(&ctx, &camera, &frame), &camera);

    eprintln!("=== #96 sunrise band gate: subject (peak sunset, facing the band) ===");
    eprintln!("camera yaw {:.1}deg", camera.yaw);
    eprintln!(
        "{} of {} disc px are warm; {}",
        r.warm, r.disc, r.bbox
    );
    eprintln!(
        "mean elevation: warm px {:.2}deg, whole disc region {:.2}deg",
        r.warm_mean_elevation_deg, r.disc_mean_elevation_deg
    );
    eprintln!(
        "band colour at this tick: {:?}",
        sunrise_sunset_color_for_time_of_day(PEAK_SUNSET_TICK)
    );

    assert!(r.disc > 4_000, "premise: the disc should fill much of the frame");
    assert!(
        r.warm > 1_000,
        "expected the band to turn a large part of the blue disc warm, got {} of {} px. {}",
        r.warm,
        r.disc,
        r.bbox
    );
    assert!(
        r.warm_mean_elevation_deg < r.disc_mean_elevation_deg - 1.0,
        "the warm pixels sit at mean elevation {:.2}deg against the disc region's \
         {:.2}deg — a band must be *lower* than the dome it sits in, so this reads as a \
         whole-sky wash rather than a horizon band. {}",
        r.warm_mean_elevation_deg,
        r.disc_mean_elevation_deg,
        r.bbox
    );
}

/// **Control, EXECUTED.** Same camera, same everything, clock moved to noon —
/// where the `SUNRISE_SUNSET_COLOR` track's alpha is `0x00` and
/// `SkyRenderer::render` skips the draw. Nothing warm on the disc.
#[test]
#[ignore = "requires a GPU adapter"]
fn control_noon_paints_no_band_from_the_same_camera() {
    let ctx = ctx();
    let camera = camera_facing_band(PEAK_SUNSET_TICK);
    let frame = SkyFrame::new(6_000, DAY_SKY).with_fog_color(BAND_FOG);
    let r = band_report(&render(&ctx, &camera, &frame), &camera);

    eprintln!("=== #96 sunrise band gate: noon control ===");
    eprintln!("{} of {} disc px are warm; {}", r.warm, r.disc, r.bbox);
    assert_eq!(
        sunrise_sunset_color_for_time_of_day(6_000)[3],
        0,
        "premise of this control: noon's band alpha is zero"
    );
    assert!(r.disc > 4_000, "premise: the disc should fill much of the frame");
    assert_eq!(
        r.warm, 0,
        "control failed to fail: noon must draw no band at all, but {} warm pixels \
         appeared. {} — either the alpha skip is not working, or something other than \
         the band is painting warm here and the affirmative gate is measuring that instead",
        r.warm, r.bbox
    );
}

/// **Control, EXECUTED, and the one that matters.** Same clock, same peak
/// sunset — camera turned to face *away* from the band.
///
/// A whole-frame warm-pixel count cannot tell a horizon band from a global warm
/// tint on the sky; only turning around can. If the band were (say) folded into
/// the disc's own colour instead of drawn as localised geometry, the affirmative
/// gate would pass and this one would fail.
///
/// Note this is a *ratio* assertion, not "zero warm pixels". The fan genuinely
/// wraps the sky (see [`BandReport`]), so vanilla's own band does tint the
/// opposite horizon slightly; the claim under test is that the tint is
/// concentrated toward the sun, which a ratio states and an absolute zero would
/// misstate.
#[test]
#[ignore = "requires a GPU adapter"]
fn control_facing_away_from_the_sunset_sees_far_less_band() {
    let ctx = ctx();
    let facing = camera_facing_band(PEAK_SUNSET_TICK);
    let away = Camera {
        yaw: facing.yaw + 180.0,
        ..facing
    };
    let frame = SkyFrame::new(PEAK_SUNSET_TICK, DAY_SKY).with_fog_color(BAND_FOG);

    let toward = band_report(&render(&ctx, &facing, &frame), &facing);
    let behind = band_report(&render(&ctx, &away, &frame), &away);

    eprintln!("=== #96 sunrise band gate: facing-away control ===");
    eprintln!(
        "facing the sunset: {} of {} disc px warm; {}",
        toward.warm, toward.disc, toward.bbox
    );
    eprintln!(
        "facing away:       {} of {} disc px warm; {}",
        behind.warm, behind.disc, behind.bbox
    );

    assert!(toward.warm > 1_000, "premise: facing the band should see it");
    assert!(
        behind.warm * 5 < toward.warm,
        "control failed to fail: looking away from the sunset still shows {} warm disc \
         pixels against {} facing it, so what this gate measures is not localised toward \
         the sun. {}",
        behind.warm,
        toward.warm,
        behind.bbox
    );
}

// ---------------------------------------------------------------------------
// 3. Void fog
// ---------------------------------------------------------------------------

/// Mean linear brightness inside the disc region, measured only over pixels a
/// ray actually reaches the disc through — not the whole frame, which is mostly
/// the pass's black clear at this camera and would drag any average toward the
/// answer the gate wants.
fn disc_brightness(pixels: &[u8], camera: &Camera) -> (f64, usize) {
    let mut sum = 0.0f64;
    let mut n = 0usize;
    for y in 0..H {
        for x in 0..W {
            if expected_fog_value(camera, x, y).is_none() {
                continue;
            }
            let p = px(pixels, x, y);
            sum += f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2]);
            n += 1;
        }
    }
    (sum / (3.0 * n.max(1) as f64), n)
}

/// Three heights on one curve, through the shipped GPU path: well above the
/// onset range (undarkened), halfway down it (quarter brightness, because the
/// falloff is quadratic), and at the world bottom (black).
///
/// The expected ratio is not a guess — it is [`VoidFog::brightness`] applied in
/// gamma space, which is what `FogRenderer.computeFogColor` does to
/// `ARGB.redFloat(color)`. Doing it in linear space instead would predict
/// roughly `0.53` at the midpoint where the correct answer is `0.25`, so this
/// gate also pins the colour space.
#[test]
#[ignore = "requires a GPU adapter"]
fn void_fog_darkens_the_sky_quadratically_as_the_eye_reaches_the_world_bottom() {
    let ctx = ctx();
    let void = VoidFog::OVERWORLD;
    let at = |eye_y: f32| {
        let camera = Camera {
            position: glam::Vec3::new(0.0, eye_y, 0.0),
            ..horizon_camera(0.0)
        };
        let frame = SkyFrame::new(6_000, DAY_SKY)
            .with_fog_color(DAY_FOG)
            .with_void_fog(void);
        let pixels = render(&ctx, &camera, &frame);
        let (mean, n) = disc_brightness(&pixels, &camera);
        (mean, n)
    };

    // Derived from `VoidFog::OVERWORLD` itself, never typed in: the top of the
    // onset range, its midpoint, and the world bottom.
    let above = void.min_y + void.onset_range * 3.0;
    let mid = void.min_y + void.onset_range * 0.5;
    let bottom = void.min_y;

    let (bright, n_bright) = at(above);
    let (half, n_half) = at(mid);
    let (dark, n_dark) = at(bottom);

    eprintln!("=== #96 void fog gate ===");
    eprintln!("eye {above}: mean disc byte {bright:.1} over {n_bright} px (brightness {})", void.brightness(above));
    eprintln!("eye {mid}: mean disc byte {half:.1} over {n_half} px (brightness {})", void.brightness(mid));
    eprintln!("eye {bottom}: mean disc byte {dark:.1} over {n_dark} px (brightness {})", void.brightness(bottom));

    assert!(n_bright > 4_000 && n_half > 4_000 && n_dark > 4_000);
    assert!(
        bright > 40.0,
        "premise: above the onset range the sky must be undarkened, got {bright:.1}"
    );
    assert!(
        dark < 2.0,
        "at the world bottom the sky must be black, got mean byte {dark:.1}"
    );

    // The midpoint must sit where a *gamma-space* quarter-brightness puts it.
    // Predicted from the same expression `resolve_colors` uses, applied to this
    // frame's own inputs, rather than from a measured ratio.
    let predict = |eye_y: f32| {
        let b = void.brightness(eye_y);
        let scaled: Vec<f64> = DAY_SKY
            .iter()
            .chain(DAY_FOG.iter())
            .map(|c| f64::from(srgb_to_linear_f32(linear_to_srgb_f32(*c) * b)))
            .collect();
        scaled.iter().sum::<f64>() / scaled.len() as f64
    };
    let ratio_measured = half / bright;
    let ratio_predicted = predict(mid) / predict(above);
    eprintln!(
        "midpoint/undarkened ratio: measured {ratio_measured:.4}, gamma-space prediction {ratio_predicted:.4}"
    );
    assert!(
        (ratio_measured - ratio_predicted).abs() < 0.06,
        "the midpoint darkening ({ratio_measured:.4}) does not match the gamma-space \
         prediction ({ratio_predicted:.4}) — most likely the scale is being applied in \
         linear space, which would come out substantially brighter"
    );
}

/// **Control, EXECUTED.** [`VoidFog::DISABLED`] at the very same eye height
/// that reads black above. If this frame were also black, the darkening measured
/// above would be something other than void fog (a camera below the disc, say,
/// or a black clear leaking into the mean).
#[test]
#[ignore = "requires a GPU adapter"]
fn control_disabled_void_fog_leaves_the_world_bottom_bright() {
    let ctx = ctx();
    let camera = Camera {
        position: glam::Vec3::new(0.0, VoidFog::OVERWORLD.min_y, 0.0),
        ..horizon_camera(0.0)
    };
    let on = SkyFrame::new(6_000, DAY_SKY)
        .with_fog_color(DAY_FOG)
        .with_void_fog(VoidFog::OVERWORLD);
    let off = SkyFrame::new(6_000, DAY_SKY)
        .with_fog_color(DAY_FOG)
        .with_void_fog(VoidFog::DISABLED);

    let (dark, n) = disc_brightness(&render(&ctx, &camera, &on), &camera);
    let (bright, _) = disc_brightness(&render(&ctx, &camera, &off), &camera);

    eprintln!("=== #96 void fog gate: disabled control ===");
    eprintln!("same eye height {}: void fog on {dark:.1}, off {bright:.1}, over {n} px", camera.position.y);
    assert!(
        bright > 40.0,
        "control failed to fail: with void fog disabled the same frame must be bright, got \
         {bright:.1} — so the darkness measured with it enabled is not attributable to void fog"
    );
    assert!(dark < 2.0, "premise: void fog on is black here, got {dark:.1}");
}
