//! The `#[ignore]`d GPU gates: render a frame through the real shell path and
//! read the pixels back.
//!
//! All of these need a wgpu adapter, so they are `#[ignore]`d and run
//! explicitly (`cargo test -p lodestone-shell -- --ignored --nocapture`).
//! They exist because a crate's own hermetic suite is a closed loop: it can be
//! entirely green while the subsystem reaches zero pixels. Each gate here
//! asserts coverage inside the subject's own screen rect, and several carry a
//! negative control that must fail the same assertion — see
//! `no_debug_lines_source_installed_draws_nothing` for the paired shape.
//!
//! [`super::sky_clear_bytes`] is the shared sky reference every silhouette
//! test classifies against; its doc records why it is derived rather than
//! hardcoded.
use lodestone_render::{Camera, HeadlessTarget, RenderTarget};

use crate::entities::EntityDraw;

use super::*;

/// Headless GPU test: generate a world, mesh + upload every section, render
/// one frame, and read pixels back to prove terrain (not just sky) drew.
#[test]
#[ignore = "requires a GPU adapter"]
fn world_renders_terrain_with_pixel_readback() {
    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (320u32, 240u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    let world = crate::worldgen::generate(2);
    let classifier = crate::blocks::DemoClassifier;
    let mut state = RenderState::new(device, queue, format, w, h, None);

    let mut total_quads = 0usize;
    let mut sections = 0usize;
    let radius = 2;
    for cz in -radius..=radius {
        for cx in -radius..=radius {
            for si in 0..crate::worldgen::SECTION_COUNT {
                let key = SectionKey {
                    cx,
                    cz,
                    si,
                    min_y: crate::worldgen::MIN_Y,
                };
                if let Some(snap) = crate::mesher::snapshot_section(&world, key) {
                    let mesh = crate::mesher::mesh_snapshot(&snap, &classifier);
                    total_quads += mesh.quad_count();
                    sections += 1;
                    state.upload_section(
                        device,
                        queue,
                        key,
                        &crate::mesher::SectionGeometry::Packed(mesh),
                    );
                }
            }
        }
    }
    assert!(sections > 0, "some sections should have meshed");

    // Camera above the origin, backed off to the north, looking south and
    // angled down over the terrain.
    let feet = crate::worldgen::spawn_feet();
    let camera = Camera {
        position: glam::Vec3::new(feet[0] as f32, feet[1] as f32 + 6.0, feet[2] as f32 - 18.0),
        yaw: 0.0,
        pitch: 22.0,
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let start = std::time::Instant::now();
    let frame = target.acquire().expect("headless acquire");
    // Draw with a block outline enabled to exercise the outline pipeline.
    let stats = state.render(
        device,
        queue,
        frame.view(),
        &camera,
        Some([0, feet[1] as i32, 0]),
        &[],
    );
    let pixels = target.read_texels(device, queue);
    let frame_ms = start.elapsed().as_secs_f64() * 1000.0;

    // Count pixels that clearly differ from the sky clear: terrain sprites
    // are green/brown/grey, far from sky blue.
    let sky = sky_clear_bytes();
    let mut terrain_px = 0usize;
    for px in pixels.chunks_exact(4) {
        let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
            + (i32::from(px[1]) - i32::from(sky[1])).abs()
            + (i32::from(px[2]) - i32::from(sky[2])).abs();
        if d > 60 {
            terrain_px += 1;
        }
    }
    let coverage = terrain_px as f64 / (w * h) as f64;
    let sky_px = (w * h) as usize - terrain_px;
    let sky_coverage = sky_px as f64 / (w * h) as f64;

    eprintln!("=== shell world render (headless) ===");
    eprintln!("sections meshed   = {sections}");
    eprintln!("sections drawn    = {}", stats.sections_drawn);
    eprintln!("quads (meshed)    = {total_quads}");
    eprintln!("quads (drawn)     = {}", stats.total_quads);
    eprintln!("draw calls        = {}", stats.draw_calls);
    eprintln!("mesh VRAM (bytes) = {}", stats.vram_bytes);
    eprintln!("terrain coverage  = {:.1}%", coverage * 100.0);
    eprintln!("sky coverage      = {:.1}%", sky_coverage * 100.0);
    eprintln!("frame time (ms)   = {frame_ms:.3}");

    // Two-sided on purpose: a blank/all-sky frame fails the terrain guard,
    // and an all-terrain frame (camera stuck inside a block, full-screen
    // fog, a broken clear) fails the sky guard. "Correctly rendered nothing"
    // and "rendered one solid colour" must both be distinguishable from a
    // real horizon.
    assert!(
        coverage > 0.05,
        "expected visible terrain, only {:.1}% non-sky pixels",
        coverage * 100.0
    );
    assert!(
        sky_coverage > 0.05,
        "expected visible sky above the horizon, only {:.1}% sky pixels — \
         frame may be a solid fill rather than a rendered scene",
        sky_coverage * 100.0
    );
}

/// Headless proof that the block outline actually draws distinct pixels:
/// render the same scene twice — once without an outline, once with one
/// around a block squarely in view — and confirm the outline adds a modest
/// number of near-black pixels where terrain used to be. Pixel readback is
/// the project's evidence standard for "did it really render?".
#[test]
#[ignore = "requires a GPU adapter"]
fn block_outline_draws_visible_edges() {
    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (320u32, 240u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    let world = crate::worldgen::generate(2);
    let classifier = crate::blocks::DemoClassifier;
    let mut state = RenderState::new(device, queue, format, w, h, None);
    for cz in -2..=2 {
        for cx in -2..=2 {
            for si in 0..crate::worldgen::SECTION_COUNT {
                let key = SectionKey {
                    cx,
                    cz,
                    si,
                    min_y: crate::worldgen::MIN_Y,
                };
                if let Some(snap) = crate::mesher::snapshot_section(&world, key) {
                    let mesh = crate::mesher::mesh_snapshot(&snap, &classifier);
                    state.upload_section(
                        device,
                        queue,
                        key,
                        &crate::mesher::SectionGeometry::Packed(mesh),
                    );
                }
            }
        }
    }

    // Outline a cube floating in the air with open sky behind it, so its
    // edges are crisp black lines on blue and can't be confused with dark
    // terrain. The outline is a pure wireframe at world coords — it draws
    // whether or not a block occupies the cell.
    let target_block = [0i32, crate::worldgen::surface_height(0, 0) + 12, 6];
    let camera = Camera {
        position: glam::Vec3::new(0.5, target_block[1] as f32 + 0.5, -2.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let frame = target.acquire().expect("acquire");
    state.render(device, queue, frame.view(), &camera, None, &[]);
    let plain = target.read_texels(device, queue);

    let frame = target.acquire().expect("acquire");
    state.render(
        device,
        queue,
        frame.view(),
        &camera,
        Some(target_block),
        &[],
    );
    let outlined = target.read_texels(device, queue);

    // The only thing that changed between the two frames is the outline, so
    // count pixels whose colour moved. A blended 0.6-alpha black line darkens
    // whatever it covers; we detect the change directly rather than guessing
    // its final colour.
    let mut changed = 0usize;
    let mut darkened = 0usize;
    for (a, b) in plain.chunks_exact(4).zip(outlined.chunks_exact(4)) {
        let d = (i32::from(a[0]) - i32::from(b[0])).abs()
            + (i32::from(a[1]) - i32::from(b[1])).abs()
            + (i32::from(a[2]) - i32::from(b[2])).abs();
        if d > 20 {
            changed += 1;
            // The outline can only darken (black over colour).
            if i32::from(b[0]) + i32::from(b[1]) + i32::from(b[2])
                < i32::from(a[0]) + i32::from(a[1]) + i32::from(a[2])
            {
                darkened += 1;
            }
        }
    }

    eprintln!("=== outline pixel readback ===");
    eprintln!("pixels changed by outline = {changed}");
    eprintln!("of which darkened         = {darkened}");

    assert!(
        changed > 50,
        "outline should visibly change the frame, only {changed} px moved"
    );
    assert_eq!(
        changed, darkened,
        "an outline only darkens pixels it covers"
    );
}

/// Headless proof that the debug-line pass — the render half of
/// `ExtractSet::Debug` (`docs/plugin-api.md`) — actually draws pixels
/// through [`RenderState::set_debug_lines_source`], not merely that a
/// pipeline object exists. Same differential idiom as
/// `block_outline_draws_visible_edges`: render the same scene with the
/// source unset and with it returning a bright line across open sky, and
/// confirm the second frame lit pixels the first did not.
///
/// This is deliberately the *only* place that calls
/// `set_debug_lines_source` in this repo today — see that method's docs,
/// and [`DebugLinesSource`]'s, for why the ECS `DebugLines` resource is
/// not actually polled by anything yet. This test proves the pipeline
/// side works in isolation; it does not and cannot prove the ECS-to-here
/// wire exists, because that wire is unbuilt.
#[test]
#[ignore = "requires a GPU adapter"]
fn debug_lines_source_draws_visible_pixels() {
    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (320u32, 240u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    // Open sky, no terrain at all: nothing else in the scene could
    // account for a pixel changing between the two frames below.
    let mut state = RenderState::new(device, queue, format, w, h, None);

    let camera = Camera {
        position: glam::Vec3::new(0.5, 64.5, -2.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let frame = target.acquire().expect("acquire");
    state.render(device, queue, frame.view(), &camera, None, &[]);
    let without_lines = target.read_texels(device, queue);

    // A bright red line squarely in view, well inside the frustum near
    // and far planes, and thick enough (drawn as several parallel
    // segments) to survive the near-black outline's "only darkens" logic
    // not applying here — a bright line lightens sky-blue pixels.
    state.set_debug_lines_source(|| {
        let mut verts = Vec::new();
        for dy in [-0.5f32, 0.0, 0.5] {
            verts.push(DebugLineVertex {
                position: [-3.0, 64.0 + dy, 4.0],
                color: [1.0, 0.0, 0.0, 1.0],
            });
            verts.push(DebugLineVertex {
                position: [3.0, 64.0 + dy, 4.0],
                color: [1.0, 0.0, 0.0, 1.0],
            });
        }
        verts
    });

    let frame = target.acquire().expect("acquire");
    state.render(device, queue, frame.view(), &camera, None, &[]);
    let with_lines = target.read_texels(device, queue);

    let mut changed = 0usize;
    for (a, b) in without_lines
        .chunks_exact(4)
        .zip(with_lines.chunks_exact(4))
    {
        let d = (i32::from(a[0]) - i32::from(b[0])).abs()
            + (i32::from(a[1]) - i32::from(b[1])).abs()
            + (i32::from(a[2]) - i32::from(b[2])).abs();
        if d > 20 {
            changed += 1;
        }
    }

    eprintln!("=== debug-line pixel readback ===");
    eprintln!("pixels changed by debug lines = {changed}");

    assert!(
        changed > 20,
        "installing a debug-lines source should visibly change the frame, \
         only {changed} px moved"
    );
}

/// Negative control for the test above: with no source installed (the
/// default state of a fresh [`RenderState`]), two renders of the same
/// scene must be pixel-identical. Without this, the assertion above could
/// be satisfied by a pass that draws unconditionally regardless of
/// whether a source was ever installed.
#[test]
#[ignore = "requires a GPU adapter"]
fn no_debug_lines_source_installed_draws_nothing() {
    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (320u32, 240u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let state = RenderState::new(device, queue, format, w, h, None);
    let camera = Camera {
        position: glam::Vec3::new(0.5, 64.5, -2.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let frame = target.acquire().expect("acquire");
    state.render(device, queue, frame.view(), &camera, None, &[]);
    let first = target.read_texels(device, queue);

    let frame = target.acquire().expect("acquire");
    state.render(device, queue, frame.view(), &camera, None, &[]);
    let second = target.read_texels(device, queue);

    assert_eq!(
        first, second,
        "an unset debug-lines source must draw nothing"
    );
}

/// Headless proof that HUD **text actually rasterizes to pixels**, not just
/// that geometry is generated. Renders two frames over the same known clear
/// colour: an empty HUD (no crosshair/debug/chat) and one carrying chat
/// lines plus a prompt. The empty frame must stay essentially background;
/// the chat frame must light a substantial run of glyph pixels. Two-sided on
/// purpose — a stray clear or wrong `LoadOp` lights the empty frame, and a
/// no-op text path leaves the chat frame dark, so neither degenerate outcome
/// can pass.
#[test]
#[ignore = "requires a GPU adapter"]
fn hud_chat_text_rasterizes_to_pixels() {
    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (320u32, 240u32);
    let clear = wgpu::Color {
        r: 0.04,
        g: 0.04,
        b: 0.08,
        a: 1.0,
    };
    let bg = [10i32, 10, 20];

    // Clear a fresh target to `clear`, render one HUD frame over it (the HUD
    // draws with `LoadOp::Load`), and count pixels far from the background.
    let lit_pixels = |frame: &crate::hud::HudFrame| -> usize {
        let mut target = HeadlessTarget::new(device, w, h, format);
        let mut hud = crate::hud::HudRenderer::new(device, format);
        let ht_frame = target.acquire().expect("headless acquire");
        {
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("clear"),
            });
            {
                let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("hud-clear"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: ht_frame.view(),
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(clear),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            queue.submit(std::iter::once(enc.finish()));
        }
        hud.render(device, queue, ht_frame.view(), frame, w, h);
        let pixels = target.read_texels(device, queue);
        pixels
            .chunks_exact(4)
            .filter(|px| {
                let d = (i32::from(px[0]) - bg[0]).abs()
                    + (i32::from(px[1]) - bg[1]).abs()
                    + (i32::from(px[2]) - bg[2]).abs();
                d > 40
            })
            .count()
    };

    let stats = crate::hud::DebugStats::default();
    let empty_frame = crate::hud::HudFrame {
        crosshair: false,
        show_debug: false,
        ..crate::hud::HudFrame::new(&stats)
    };
    let empty_lit = lit_pixels(&empty_frame);

    let chat = [("<Steve> hello world", 0.0_f32), ("<Alex> hi there", 0.0)];
    let chat_frame = crate::hud::HudFrame {
        crosshair: false,
        show_debug: false,
        chat: &chat,
        chat_input: Some("typing a message"),
        ..crate::hud::HudFrame::new(&stats)
    };
    let chat_lit = lit_pixels(&chat_frame);

    eprintln!("=== hud chat rasterization ===");
    eprintln!("empty HUD lit px = {empty_lit}");
    eprintln!("chat  HUD lit px = {chat_lit}");

    assert!(
        empty_lit < 20,
        "an empty HUD should read as background, but {empty_lit} px were lit — \
         a stray clear or wrong LoadOp is drawing something"
    );
    assert!(
        chat_lit > 200,
        "chat text should rasterize a substantial run of glyph pixels, only {chat_lit} lit — \
         the text path may be a no-op"
    );
}

/// The scoreboard sidebar must actually reach pixels. Same two-sided shape as
/// the chat proof: an empty HUD stays background; a sidebar with two scored
/// rows lights a substantial run of glyph pixels. A no-op fold, a panel drawn
/// with no text, or a wrong `LoadOp` each fails one side.
#[test]
#[ignore = "requires a GPU adapter"]
fn hud_sidebar_rasterizes_to_pixels() {
    use crate::overlay::{Sidebar, SidebarLine};
    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (320u32, 240u32);
    let clear = wgpu::Color {
        r: 0.04,
        g: 0.04,
        b: 0.08,
        a: 1.0,
    };
    let bg = [10i32, 10, 20];

    let lit_pixels = |frame: &crate::hud::HudFrame| -> usize {
        let mut target = HeadlessTarget::new(device, w, h, format);
        let mut hud = crate::hud::HudRenderer::new(device, format);
        let ht_frame = target.acquire().expect("headless acquire");
        {
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("clear"),
            });
            {
                let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("hud-clear"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: ht_frame.view(),
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(clear),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            queue.submit(std::iter::once(enc.finish()));
        }
        hud.render(device, queue, ht_frame.view(), frame, w, h);
        let pixels = target.read_texels(device, queue);
        pixels
            .chunks_exact(4)
            .filter(|px| {
                let d = (i32::from(px[0]) - bg[0]).abs()
                    + (i32::from(px[1]) - bg[1]).abs()
                    + (i32::from(px[2]) - bg[2]).abs();
                d > 40
            })
            .count()
    };

    let stats = crate::hud::DebugStats::default();
    let empty_frame = crate::hud::HudFrame {
        crosshair: false,
        show_debug: false,
        ..crate::hud::HudFrame::new(&stats)
    };
    let empty_lit = lit_pixels(&empty_frame);

    let side = Sidebar {
        title: crate::overlay::plain_spans("Objectives"),
        lines: vec![
            SidebarLine {
                label: crate::overlay::plain_spans("Kills"),
                score: crate::overlay::plain_spans("7"),
            },
            SidebarLine {
                label: crate::overlay::plain_spans("Deaths"),
                score: crate::overlay::plain_spans("2"),
            },
        ],
    };
    let side_frame = crate::hud::HudFrame {
        crosshair: false,
        show_debug: false,
        sidebar: Some(&side),
        ..crate::hud::HudFrame::new(&stats)
    };
    let side_lit = lit_pixels(&side_frame);

    eprintln!("=== hud sidebar rasterization ===");
    eprintln!("empty   HUD lit px = {empty_lit}");
    eprintln!("sidebar HUD lit px = {side_lit}");

    assert!(
        empty_lit < 20,
        "an empty HUD should read as background, but {empty_lit} px were lit"
    );
    assert!(
        side_lit > 200,
        "the sidebar title, labels and scores should rasterize a substantial run \
         of glyph pixels, only {side_lit} lit — the fold or text path may be a no-op"
    );
}

/// Headless GPU test: render a single entity (no terrain) through the real
/// [`RenderState::render`] path — the same call the live frame loop uses —
/// and read pixels back to prove a mob reaches the screen. This is the
/// shell-level analogue of `lodestone-render`'s `entity_gate`, but it drives
/// the *shell's* wiring: `EntityDraw` → resolve → `plan_entities` → upload →
/// instanced draw, sharing the terrain depth buffer.
#[test]
#[ignore = "requires a GPU adapter"]
fn entity_renders_to_pixels_through_shell_path() {
    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter), don't 'skip' — a silent pass \
         here would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (320u32, 240u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    let state = RenderState::new(device, queue, format, w, h, None);

    // A pig standing just in front of the camera, which looks south (+Z,
    // yaw 0) at eye level with the pig's body — mirrors the render-crate
    // gate's geometry so a regression there shows up here too.
    let pig_feet = glam::Vec3::new(0.0, 0.0, 4.0);
    let camera = Camera {
        position: glam::Vec3::new(0.0, 0.9, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let draws = vec![
        EntityDraw {
            hurt: false,
            id: 1,
            type_path: "pig".to_owned(),
            item: None,
            feet: pig_feet,
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            equipment: Vec::new(),
            // No equipment above, so nothing here could carry a dye.
            equipment_dye: Vec::new(),
            wool: None,
            count: 1,
            name_tag: None,
            item_use: None,
            // Not a creeper: only a creeper ever swells.
            creeper_swelling: 0.0,
        // No flame overlay from this construction site (issue #434).
        on_fire: false,
        },
        // A second pig behind the camera so frustum culling has something
        // real to remove — the anti-vacuity guard on the cull path.
        EntityDraw {
            hurt: false,
            id: 2,
            type_path: "pig".to_owned(),
            item: None,
            feet: glam::Vec3::new(0.0, 0.0, -12.0),
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            equipment: Vec::new(),
            // No equipment above, so nothing here could carry a dye.
            equipment_dye: Vec::new(),
            wool: None,
            count: 1,
            name_tag: None,
            item_use: None,
            // Not a creeper: only a creeper ever swells.
            creeper_swelling: 0.0,
        // No flame overlay from this construction site (issue #434).
        on_fire: false,
        },
    ];

    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), &camera, None, &draws);
    let pixels = target.read_texels(device, queue);

    assert_eq!(
        stats.entities_drawn, 1,
        "exactly the front pig should draw; the one behind the camera must cull \
         (drawn={}, culled={})",
        stats.entities_drawn, stats.entities_culled
    );
    assert!(
        stats.entities_culled >= 1,
        "the pig behind the camera should have been frustum-culled, but culled={}",
        stats.entities_culled
    );

    // The synthetic pig texture is a solid tint; count pixels that clearly
    // differ from the sky clear colour, and confirm they cluster in the
    // centre (where the pig is) rather than smeared across the frame.
    let sky = sky_clear_bytes();
    let is_mob = |px: &[u8]| -> bool {
        let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
            + (i32::from(px[1]) - i32::from(sky[1])).abs()
            + (i32::from(px[2]) - i32::from(sky[2])).abs();
        d > 60
    };

    let mut mob_px = 0usize;
    let mut centre_px = 0usize;
    let mut corner_px = 0usize;
    let mut arm_px = 0usize;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let x = (i as u32) % w;
        let y = (i as u32) / w;
        if is_mob(px) {
            mob_px += 1;
        }
        let cx = x >= w / 4 && x < 3 * w / 4;
        let cy = y >= h / 4 && y < 3 * h / 4;
        if cx && cy && is_mob(px) {
            centre_px += 1;
        }
        // The **bottom-right** corner is excluded on purpose: that is where
        // the unconditional first-person arm lives (`prepare_first_person_arm`
        // → `first_person_arm_pose`, camera-space, roughly the right-hand 30%
        // and bottom 30% of frame). This assertion is about the *pig* being
        // centred, and folding the arm into it would turn a working feature
        // into a red gate. The other three corners still have to stay sky, so
        // the "mob smeared across the whole frame" defect is still caught.
        let bottom_right = x >= w / 2 && y >= h / 2;
        let corner = (x < w / 8 || x >= 7 * w / 8) && (y < h / 8 || y >= 7 * h / 8);
        if corner && !bottom_right && is_mob(px) {
            corner_px += 1;
        }
        if bottom_right && is_mob(px) {
            arm_px += 1;
        }
    }
    let coverage = mob_px as f64 / (w * h) as f64;

    eprintln!("=== shell entity render (headless) ===");
    eprintln!("entities drawn  = {}", stats.entities_drawn);
    eprintln!("entities culled = {}", stats.entities_culled);
    eprintln!("mob coverage    = {:.2}%", coverage * 100.0);
    eprintln!("centre mob px   = {centre_px}");
    eprintln!("corner mob px   = {corner_px}");
    eprintln!("arm px (bot-rt) = {arm_px}");
    eprintln!("arm drawn       = {}", stats.first_person_arm_drawn);

    // Two-sided: the pig must reach pixels (not a blank frame) but not fill
    // the screen (a broken clear or a mob glued to the camera), and it must
    // be centred (the corners stay sky).
    assert!(
        mob_px > 200,
        "expected the pig to reach pixels, only {mob_px} non-sky px ({:.2}%)",
        coverage * 100.0
    );
    assert!(
        coverage < 0.6,
        "the pig should not fill the frame ({:.1}% non-sky) — a mob glued to the \
         near plane or a broken clear",
        coverage * 100.0
    );
    assert!(
        centre_px > 100,
        "the pig should sit in the centre of the frame, only {centre_px} centre px"
    );
    assert_eq!(
        corner_px, 0,
        "the frame corners should stay sky, but {corner_px} corner px read as mob"
    );

    // The first-person arm, on the same frame and for free: it is drawn
    // unconditionally in its own pass, so it must reach pixels in the
    // bottom-right quadrant. `first_person_arm_drawn` distinguishes "the pass
    // never ran" (a missing mesh/texture/part — a plumbing defect) from "it
    // ran and rasterised nothing" (a wrong pose or a winding flip), which look
    // identical from the pixel count alone.
    assert!(
        stats.first_person_arm_drawn,
        "the first-person arm pass must run: player_wide's mesh, texture and \
         arm part are all expected to exist"
    );
    assert!(
        arm_px > 500,
        "the first-person arm should fill a chunk of the bottom-right quadrant, \
         only {arm_px} non-sky px there — a wrong camera-space pose parks it at \
         the world origin, and an inverted winding culls every face"
    );
}

/// Headless GPU texture-correctness gate. The placeholder
/// (`synthetic_entity_texture`) paints an entire mob a *single* flat hue
/// (`model_tint`), varying only in brightness under lighting. A real per-mob
/// sheet from `client.jar` carries several hues on one body — the zombie's
/// green skin, teal shirt and dark-blue legs. So "a meaningful share of one
/// mob's pixels sit at a hue far from any single flat tint" is a signal only
/// the real sheet can produce. This renders the *same* zombie twice — once
/// with the jar sheet, once forced back to the placeholder — and asserts the
/// real render is markedly more multi-hued. If texture loading regresses to
/// the fallback, the two renders converge and this reddens.
///
/// This is the screen-capture-free stand-in for "look at the screenshot":
/// screencapture needs Screen Recording permission the CI/agent host lacks,
/// so instead of eyeballing the window we read the drawn pixels back and
/// assert the mob's *colour* — not merely that something drew.
#[test]
#[ignore = "requires a GPU adapter and .cache/mc/26.2/client.jar"]
fn zombie_wears_its_real_skin_not_the_flat_placeholder() {
    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter), don't 'skip' — a silent pass \
         here would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (320u32, 240u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    let mut state = RenderState::new(device, queue, format, w, h, None);

    // One zombie centred in front of a south-looking camera, framed on its
    // torso and head where the shirt/skin hues live.
    let camera = Camera {
        position: glam::Vec3::new(0.0, 1.4, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };
    let draws = vec![EntityDraw {
        hurt: false,
        id: 1,
        type_path: "zombie".to_owned(),
        item: None,
        feet: glam::Vec3::new(0.0, 0.0, 3.0),
        yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        anim: lodestone_render::AnimInput::REST,
        equipment: Vec::new(),
        // No equipment above, so nothing here could carry a dye.
        equipment_dye: Vec::new(),
        wool: None,
        count: 1,
        name_tag: None,
        item_use: None,
        // Not a creeper: only a creeper ever swells.
        creeper_swelling: 0.0,
        // No flame overlay from this construction site (issue #434).
        on_fire: false,
    }];

    // Fraction of a mob's bright pixels whose *hue direction* is far from the
    // model's single flat placeholder tint. Brightness scaling (lighting)
    // leaves the direction unchanged, so under the placeholder this is ~0; a
    // real multi-hue sheet pushes it up.
    //
    // **Left half of the frame only.** The first-person arm is drawn
    // unconditionally into the bottom-right (see
    // `prepare_first_person_arm`), textured from `player_wide` — a *different*
    // model, so a different `model_tint` under the synthetic control. Its
    // pixels would land in `off` and blow the `off_syn < 0.05` control clean
    // open, making the gate red for a working feature. The zombie is centred
    // at `x = w/2` and vertically stratified (skin / shirt / legs), so its
    // left half carries every hue this gate is looking for, while the arm
    // starts around `x = 0.77·w` — a wide margin.
    let off_hue_fraction = |pixels: &[u8]| -> (usize, f64) {
        let sky = sky_clear_bytes().map(f32::from);
        let tint = model_tint("zombie");
        let tv = glam::Vec3::new(tint[0] as f32, tint[1] as f32, tint[2] as f32).normalize();
        let mut mob = 0usize;
        let mut off = 0usize;
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            if (i as u32) % w >= w / 2 {
                continue;
            }
            let c = glam::Vec3::new(px[0] as f32, px[1] as f32, px[2] as f32);
            let d = (c.x - sky[0]).abs() + (c.y - sky[1]).abs() + (c.z - sky[2]).abs();
            if d <= 60.0 {
                continue; // sky
            }
            mob += 1;
            // Skip near-black shadow pixels where a hue direction is noise.
            if c.x + c.y + c.z < 60.0 {
                continue;
            }
            let dir = c.normalize();
            if dir.dot(tv) < 0.95 {
                off += 1;
            }
        }
        let frac = if mob == 0 {
            0.0
        } else {
            off as f64 / mob as f64
        };
        (mob, frac)
    };

    // Real jar sheet first.
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), &camera, None, &draws);
    let real_px = target.read_texels(device, queue);
    let (mob_real, off_real) = off_hue_fraction(&real_px);
    assert_eq!(
        stats.entities_drawn, 1,
        "the zombie should draw exactly once (drawn={})",
        stats.entities_drawn
    );

    // Same mob, forced back to the flat placeholder — the built-in control.
    state.entities.force_synthetic_textures(device, queue);
    let frame = target.acquire().expect("headless acquire");
    state.render(device, queue, frame.view(), &camera, None, &draws);
    let syn_px = target.read_texels(device, queue);
    let (mob_syn, off_syn) = off_hue_fraction(&syn_px);

    eprintln!("=== zombie texture-correctness gate ===");
    eprintln!("real: mob_px={mob_real} off_hue={:.1}%", off_real * 100.0);
    eprintln!("synth: mob_px={mob_syn} off_hue={:.1}%", off_syn * 100.0);

    assert!(
        mob_real > 300 && mob_syn > 300,
        "both renders must actually put the zombie on screen (real={mob_real}, \
         synth={mob_syn}) — otherwise the comparison is vacuous"
    );
    assert!(
        off_syn < 0.05,
        "the flat placeholder is a single hue, so its off-hue fraction must be \
         ~0, got {:.1}% — the control isn't controlling",
        off_syn * 100.0
    );
    assert!(
        off_real > 0.20,
        "the real zombie sheet should paint a substantial share of the body at \
         hues away from any single tint (green skin / teal shirt / dark legs), \
         got only {:.1}% — textures likely fell back to the placeholder",
        off_real * 100.0
    );
    assert!(
        off_real > off_syn * 4.0,
        "the real sheet must be markedly more multi-hued than the placeholder \
         (real {:.1}% vs synth {:.1}%) — if they're close, the real path is a \
         no-op and mobs are still flat",
        off_real * 100.0,
        off_syn * 100.0
    );
}
