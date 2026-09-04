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

    let start = crate::platform::Instant::now();
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
/// source unset and with a real structure-block outline, and confirm the
/// second frame lit pixels the first did not. The structure geometry is what
/// the live source supplies after its permission and world-data gates.
#[test]
#[ignore = "requires a GPU adapter"]
fn structure_block_outline_draws_visible_pixels() {
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

    let structure = lodestone_core::Nbt::Compound(vec![
        ("mode".to_owned(), lodestone_core::Nbt::String("SAVE".to_owned())),
        ("posY".to_owned(), lodestone_core::Nbt::Int(0)),
        ("sizeX".to_owned(), lodestone_core::Nbt::Int(6)),
        ("sizeY".to_owned(), lodestone_core::Nbt::Int(4)),
        ("sizeZ".to_owned(), lodestone_core::Nbt::Int(6)),
    ]);
    state.set_debug_lines_source(move |_| {
        crate::block_entities::structure_block_outline_vertices([0, 64, 4], &structure)
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

    eprintln!("=== structure-block outline pixel readback ===");
    eprintln!("pixels changed by the structure outline = {changed}");

    assert!(
        changed > 20,
        "installing a structure-block outline should visibly change the frame, \
         only {changed} px moved"
    );
}

/// Headless pixel differential for the plugin-billboard pass. Render the same
/// open-sky scene with [`RenderState::set_plugin_billboards_source`] unset and
/// with it returning one bright, untextured billboard in view; the second
/// frame must contain pixels the first does not. Keeping the scene and camera
/// identical makes the changed-pixel count attributable to billboard
/// submission rather than pipeline construction.
#[test]
#[ignore = "requires a GPU adapter"]
fn plugin_billboards_source_draws_visible_pixels() {
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

    // Open sky, no terrain at all: nothing else in the scene could account
    // for a pixel changing between the two frames below.
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
    let without_billboards = target.read_texels(device, queue);

    // A large, bright, untextured (`Solid`) billboard squarely in view —
    // `Solid` rather than `Named`, so this gate proves the pipeline itself
    // paints pixels without depending on a vanilla atlas being loaded (this
    // scene passes `None` for `vanilla`, so `plugin_billboard_atlas_sprites`
    // is empty and any `Named` id would fall back to the same untextured
    // path anyway — see `gpu/plugin_billboards.rs`'s module doc).
    state.set_plugin_billboards_source(|| {
        vec![PluginBillboardInstance {
            position: [0.0, 64.0, 4.0, 0.0],
            size_textured: [4.0, 4.0, 0.0, 0.0],
            uv: [0.0, 0.0, 1.0, 1.0],
            color: [1.0, 0.0, 0.0, 1.0],
        }]
    });

    let frame = target.acquire().expect("acquire");
    state.render(device, queue, frame.view(), &camera, None, &[]);
    let with_billboards = target.read_texels(device, queue);

    let mut changed = 0usize;
    for (a, b) in without_billboards
        .chunks_exact(4)
        .zip(with_billboards.chunks_exact(4))
    {
        let d = (i32::from(a[0]) - i32::from(b[0])).abs()
            + (i32::from(a[1]) - i32::from(b[1])).abs()
            + (i32::from(a[2]) - i32::from(b[2])).abs();
        if d > 20 {
            changed += 1;
        }
    }

    eprintln!("=== plugin-billboard pixel readback ===");
    eprintln!("pixels changed by plugin billboard = {changed}");

    assert!(
        changed > 20,
        "installing a plugin-billboards source should visibly change the frame, \
         only {changed} px moved"
    );
}

/// Negative control for the test above: with no source installed (the
/// default state of a fresh [`RenderState`]), two renders of the same scene
/// must be pixel-identical. Without this, the assertion above could be
/// satisfied by a pass that draws unconditionally regardless of whether a
/// source was ever installed.
#[test]
#[ignore = "requires a GPU adapter"]
fn no_plugin_billboards_source_installed_draws_nothing() {
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
        "an unset plugin-billboards source must draw nothing"
    );
}

/// A real committed [`lodestone_autopilot`] plan pushes **several**
/// `PluginBillboard`s spread along a route — `extract_plan_billboards`
/// (`crates/plugins/lodestone-autopilot/src/lib.rs`) pushes one per
/// remaining edge, not the single billboard the precedent gate above uses.
/// `lodestone-shell` must not depend on `lodestone-autopilot` (an
/// LGPL-3.0-or-later external plugin — see that crate's `Cargo.toml`
/// comment), so this gate cannot drive the real plugin through a real
/// `Sim`; it instead feeds the render pipeline the same *shape* that
/// producer emits — several distinct world-space markers along a line —
/// and checks each one reaches its own region of the frame.
///
/// This is the `CLAUDE.md` "measure by location, never by frame average"
/// case directly: an aggregate diff count (the precedent gate's own
/// assertion) cannot tell three markers apart from one dominant billboard
/// eclipsing the other two, so this gate partitions the diff into three
/// horizontal bands and requires each to register real coverage on its own.
#[test]
#[ignore = "requires a GPU adapter"]
fn plugin_billboards_source_draws_a_multi_waypoint_path() {
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
    let empty = target.read_texels(device, queue);

    // Three waypoint markers spread horizontally in view — `Solid`, exactly
    // as `extract_plan_billboards` draws them (no vanilla atlas is loaded in
    // this scene either), at world x = -4, 0, 4 so their projections land in
    // roughly the left, middle and right thirds of the frame.
    state.set_plugin_billboards_source(|| {
        [-4.0f32, 0.0, 4.0]
            .into_iter()
            .map(|x| PluginBillboardInstance {
                position: [x, 64.0, 4.0, 0.0],
                size_textured: [1.5, 1.5, 0.0, 0.0],
                uv: [0.0, 0.0, 1.0, 1.0],
                color: [0.1, 0.9, 1.0, 1.0],
            })
            .collect()
    });

    let frame = target.acquire().expect("acquire");
    state.render(device, queue, frame.view(), &camera, None, &[]);
    let with_path = target.read_texels(device, queue);

    let diff = diff_mask(&with_path, &empty);
    let third = (w as usize / 3).max(1);
    let mut per_third = [0usize; 3];
    for (i, &changed) in diff.iter().enumerate() {
        if !changed {
            continue;
        }
        let x = i % w as usize;
        let band = (x / third).min(2);
        per_third[band] += 1;
    }

    eprintln!("=== plugin-billboard path pixel readback ===");
    eprintln!("changed pixels per horizontal third = {per_third:?}");

    for (band, &count) in per_third.iter().enumerate() {
        assert!(
            count > 10,
            "waypoint marker {band} of 3 did not register visible pixels in its own \
             third of the frame ({count} px, full split {per_third:?}) — a real plan's \
             remaining edges must each reach the screen, not just the first or the \
             largest"
        );
    }
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

/// The discriminator for the owner's report that F3+B (entity hitboxes) and
/// F3+G (chunk borders) draw nothing. Every other debug-line gate in this
/// file installs a *synthetic* closure (`structure_block_outline_vertices`,
/// a bare billboard) — never [`entity_hitbox_vertices`] or
/// [`chunk_border_vertices`], the exact two functions
/// `app::session::WindowApp::install_debug_lines_source`'s closure calls when
/// `debug_hitboxes`/`debug_chunk_borders` is set. So a break specific to
/// either one was invisible to the rest of this corpus — the
/// shared-construction-path blindness this repo's evidence standard already
/// names for a render frame reached through one factory: every existing gate
/// reached the debug-line *pipeline*, none reached these two *producers*.
///
/// Same differential idiom as the two gates above: render the same open-sky
/// scene with no source, then with each producer's real output, and require
/// pixels to move. If this fails, the bug is inside `entity_hitbox_vertices`/
/// `chunk_border_vertices` (or the census/dimensions data they depend on);
/// if it passes, the two producers and the pipeline are innocent and the
/// break is upstream, in `install_debug_lines_source`'s own closure wiring
/// (the `ecs`/`local`/`Arc<AtomicBool>` capture) or in the toggle path that
/// feeds it.
#[test]
#[ignore = "requires a GPU adapter"]
fn entity_hitbox_and_chunk_border_vertices_draw_visible_pixels() {
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

    let camera = Camera {
        position: glam::Vec3::new(0.5, 64.5, -2.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let mut state = RenderState::new(device, queue, format, w, h, None);
    let frame = target.acquire().expect("acquire");
    state.render(device, queue, frame.view(), &camera, None, &[]);
    let baseline = target.read_texels(device, queue);

    // F3+B: one zombie standing squarely in frame — same placement idiom as
    // `structure_block_outline_draws_visible_pixels`'s structure. `"zombie"`
    // is a real entry in the jar-derived dimension census, so this exercises
    // the same `entity_type_id_parts` → `base_dimensions` lookup production
    // makes, not a stubbed box.
    let zombie = EntityDraw {
        id: 1,
        type_path: std::sync::Arc::from("zombie"),
        variant_sheet: None,
        item: None,
        item_model: None,
        item_skin: None,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_skin: Vec::new(),
        equipment_trim: Vec::new(),
        wool: None,
        block_state: None,
        item_frame_rotation: 0,
        count: 1,
        foil: false,
        item_dyed_color: None,
        item_potion_color: None,
        feet: glam::Vec3::new(0.5, 64.0, 3.0),
        yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::default(),
        name_tag: None,
        hurt: false,
        item_use: None,
        main_arm_left: false,
        creeper_swelling: 0.0,
        swim_amount: 0.0,
        death_time: 0.0,
        on_fire: false,
        invisible: false,
        armor_stand: None,
        player_skin: None,
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
        firework: None,
        projectile_owner: None,
    };
    state.set_debug_lines_source(move |_| entity_hitbox_vertices(&[zombie.clone()]));
    let frame = target.acquire().expect("acquire");
    state.render(device, queue, frame.view(), &camera, None, &[]);
    let with_hitbox = target.read_texels(device, queue);

    // F3+G: the chunk-border box around the camera's own position — the
    // real `(player, min_y, height)` shape `install_debug_lines_source`
    // passes, not a mock. The near edge of chunk (0, -1) sits 2 blocks in
    // front of the camera, squarely in frame.
    state.set_debug_lines_source(move |_| chunk_border_vertices([0.5, 64.5, -2.0], -64, 384));
    let frame = target.acquire().expect("acquire");
    state.render(device, queue, frame.view(), &camera, None, &[]);
    let with_borders = target.read_texels(device, queue);

    let count_changed = |a: &[u8], b: &[u8]| -> usize {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .filter(|(p, q)| {
                let d = (i32::from(p[0]) - i32::from(q[0])).abs()
                    + (i32::from(p[1]) - i32::from(q[1])).abs()
                    + (i32::from(p[2]) - i32::from(q[2])).abs();
                d > 20
            })
            .count()
    };

    let hitbox_changed = count_changed(&baseline, &with_hitbox);
    let border_changed = count_changed(&baseline, &with_borders);

    eprintln!("=== entity hitbox / chunk border pixel readback ===");
    eprintln!("pixels changed by entity_hitbox_vertices (F3+B) = {hitbox_changed}");
    eprintln!("pixels changed by chunk_border_vertices (F3+G)  = {border_changed}");

    assert!(
        hitbox_changed > 20,
        "F3+B: entity_hitbox_vertices should visibly draw a wireframe box, \
         only {hitbox_changed} px moved"
    );
    assert!(
        border_changed > 20,
        "F3+G: chunk_border_vertices should visibly draw the column outline, \
         only {border_changed} px moved"
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
        hud.render(device, queue, ht_frame.view(), ht_frame.view(), frame, w, h);
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

/// The F3+Shift profiler pie chart must actually reach pixels — this repo's
/// dominant defect class (`CLAUDE.md`'s island rule) is a subsystem that is
/// individually correct and reaches zero pixels because nothing calls the
/// draw branch, and a gate that only asserted on `ProfilerChart`'s fields
/// (or on `hud::draw_profiler_chart` in isolation) could not see that. Same
/// two-sided shape as [`hud_chat_text_rasterizes_to_pixels`]: an empty HUD
/// (`profiler_chart: None`) stays background; a HUD carrying a real chart
/// (eight pairwise-distinct wedge means, two GPU rows) lights a substantial
/// run of pixels — both the wedge fan and the legend text.
#[test]
#[ignore = "requires a GPU adapter"]
fn profiler_chart_draws_visible_pixels() {
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
        hud.render(device, queue, ht_frame.view(), ht_frame.view(), frame, w, h);
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

    let empty_stats = crate::hud::DebugStats::default();
    let empty_frame = crate::hud::HudFrame {
        crosshair: false,
        show_debug: false,
        ..crate::hud::HudFrame::new(&empty_stats)
    };
    let empty_lit = lit_pixels(&empty_frame);

    // Pairwise-distinct means (`CLAUDE.md`'s evidence standard: two adjacent
    // same-typed fields must differ, or a transposition survives unnoticed) —
    // real `FramePhase` names, in `FramePhase::ALL` order, so this exercises
    // the same eight-wedge shape `app::redraw` actually builds.
    let names = [
        "setup",
        "sim_tick",
        "mesh_upload",
        "acquire",
        "prepare",
        "world_encode_submit",
        "hud_ui_encode_submit",
        "present",
    ];
    let slices: Vec<crate::hud::ProfilerChartSlice> = names
        .iter()
        .enumerate()
        .map(|(i, &name)| crate::hud::ProfilerChartSlice {
            name,
            mean_ms: 0.4 + i as f32 * 0.31,
            p95_ms: 0.6 + i as f32 * 0.31,
            p99_ms: 0.8 + i as f32 * 0.31,
            samples: 240,
            window: 240,
            skipped: 0,
        })
        .collect();
    let chart_stats = crate::hud::DebugStats {
        profiler_chart: Some(crate::hud::ProfilerChart {
            slices,
            selected: None,
            gpu: vec![("world", Some(2.3)), ("first_person", None)],
            gpu_unavailable: false,
            gpu_stalled_frames: 0,
        }),
        ..crate::hud::DebugStats::default()
    };
    let chart_frame = crate::hud::HudFrame {
        crosshair: false,
        show_debug: false,
        ..crate::hud::HudFrame::new(&chart_stats)
    };
    let chart_lit = lit_pixels(&chart_frame);

    eprintln!("=== profiler pie chart rasterization ===");
    eprintln!("empty HUD lit px = {empty_lit}");
    eprintln!("chart HUD lit px = {chart_lit}");

    assert!(
        empty_lit < 20,
        "an empty HUD should read as background, but {empty_lit} px were lit — \
         a stray clear or wrong LoadOp is drawing something"
    );
    assert!(
        chart_lit > 500,
        "the profiler chart should rasterize a substantial run of wedge + legend \
         pixels, only {chart_lit} lit — the draw branch may not be reached"
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
        hud.render(device, queue, ht_frame.view(), ht_frame.view(), frame, w, h);
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
            type_path: std::sync::Arc::from("pig"),
            item: None,
            item_model: None,
            item_skin: None,
            feet: pig_feet,
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            equipment: Vec::new(),
            // No equipment above, so nothing here could carry a dye.
            equipment_dye: Vec::new(),
            equipment_skin: Vec::new(),
            equipment_trim: Vec::new(),
            wool: None,
            block_state: None,
            item_frame_rotation: 0,
            count: 1,
            foil: false,
            item_dyed_color: None,
            item_potion_color: None,
            name_tag: None,
            item_use: None,
            // Right-handed: not relevant to this gate.
            main_arm_left: false,
            // Not a creeper: only a creeper ever swells.
            creeper_swelling: 0.0,
            // A pig, not a player.
            swim_amount: 0.0,
            death_time: 0.0,
        // No flame overlay from this construction site.
        on_fire: false,
        // Not invisible and not an armour stand.
        invisible: false,
        armor_stand: None,
        // Not a player, so no skin can apply.
        player_skin: None,
        variant_sheet: None,
        // Not an experience orb: `None` keeps this subject out of the orb
        // billboard pass entirely.
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
        firework: None,
        projectile_owner: None,
        },
        // A second pig behind the camera so frustum culling has something
        // real to remove — the anti-vacuity guard on the cull path.
        EntityDraw {
            hurt: false,
            id: 2,
            type_path: std::sync::Arc::from("pig"),
            item: None,
            item_model: None,
            item_skin: None,
            main_arm_left: false,
            feet: glam::Vec3::new(0.0, 0.0, -12.0),
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            equipment: Vec::new(),
            // No equipment above, so nothing here could carry a dye.
            equipment_dye: Vec::new(),
            equipment_skin: Vec::new(),
            equipment_trim: Vec::new(),
            wool: None,
            block_state: None,
            item_frame_rotation: 0,
            count: 1,
            foil: false,
            item_dyed_color: None,
            item_potion_color: None,
            name_tag: None,
            item_use: None,
            // Not a creeper: only a creeper ever swells.
            creeper_swelling: 0.0,
            // A pig, not a player.
            swim_amount: 0.0,
            death_time: 0.0,
        // No flame overlay from this construction site.
        on_fire: false,
        // Not invisible and not an armour stand.
        invisible: false,
        armor_stand: None,
        // Not a player, so no skin can apply.
        player_skin: None,
        variant_sheet: None,
        // Not an experience orb: `None` keeps this subject out of the orb
        // billboard pass entirely.
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
        firework: None,
        projectile_owner: None,
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
        type_path: std::sync::Arc::from("zombie"),
        item: None,
        item_model: None,
        item_skin: None,
        main_arm_left: false,
        feet: glam::Vec3::new(0.0, 0.0, 3.0),
        yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        anim: lodestone_render::AnimInput::REST,
        equipment: Vec::new(),
        // No equipment above, so nothing here could carry a dye.
        equipment_dye: Vec::new(),
        equipment_skin: Vec::new(),
            equipment_trim: Vec::new(),
        wool: None,
        block_state: None,
        item_frame_rotation: 0,
        count: 1,
        foil: false,
        item_dyed_color: None,
        item_potion_color: None,
        name_tag: None,
        item_use: None,
        // Not a creeper: only a creeper ever swells.
        creeper_swelling: 0.0,
        // A zombie, not a player.
        swim_amount: 0.0,
        death_time: 0.0,
        // No flame overlay from this construction site.
        on_fire: false,
        // Not invisible and not an armour stand.
        invisible: false,
        armor_stand: None,
        // Not a player, so no skin can apply.
        player_skin: None,
        variant_sheet: None,
        // Not an experience orb: `None` keeps this subject out of the orb
        // billboard pass entirely.
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
        firework: None,
        projectile_owner: None,
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

// ---------------------------------------------------------------------------
// An unloaded column must stop reaching pixels
// ---------------------------------------------------------------------------

/// Which pixels of `pixels` differ from the empty-scene render `reference`.
///
/// A `Vec<bool>` in row-major order, one entry per pixel — a *mask*, not a
/// bounding box, and measured against a **rendered** reference rather than
/// against [`sky_clear_bytes`]. Two premise-check rejections produced that
/// signature, and both are worth keeping:
///
/// * *bounding box → mask.* A distant column at a shallow angle has a
///   176 × 112 box that is 99.9% painted by the columns in front of it, so "the
///   box went to sky" would have measured the neighbours. A silhouette mask is
///   the only form of "the subject's screen rect" that survives perspective.
/// * *constant → rendered reference.* The flat sky clear is **not** what the sky
///   actually renders as: `RenderState` draws a gradient sky disc whose end
///   distance moves with the fog setting, so a band near the horizon sits more
///   than the classifier's threshold away from `SKY_COLOR` and reads as terrain.
///   That misclassification contaminated 46.4% of the subject's silhouette, and
///   the tell was that the count was **byte-identical (4291 px, same bounding
///   box) after doubling the camera distance** — a contaminant that does not move
///   with the camera is not in the world. Differencing against a frame rendered
///   with nothing uploaded makes the reference per-pixel and immune to the
///   gradient, the fog setting and the sky disc alike.
fn diff_mask(pixels: &[u8], reference: &[u8]) -> Vec<bool> {
    pixels
        .chunks_exact(4)
        .zip(reference.chunks_exact(4))
        .map(|(px, rf)| {
            let d = (i32::from(px[0]) - i32::from(rf[0])).abs()
                + (i32::from(px[1]) - i32::from(rf[1])).abs()
                + (i32::from(px[2]) - i32::from(rf[2])).abs();
            d > 60
        })
        .collect()
}

/// How much of `subject` is terrain in `frame`, plus the bounding box of the
/// pixels that are.
///
/// The box is returned because a fraction cannot distinguish a uniform-but-wrong
/// frame from a localised blob — a failure here has to be able to say *where*.
fn coverage_within(
    frame: &[bool],
    subject: &[bool],
    w: u32,
) -> (usize, usize, Option<(u32, u32, u32, u32)>) {
    let mut total = 0usize;
    let mut hit = 0usize;
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for (i, &in_subject) in subject.iter().enumerate() {
        if !in_subject {
            continue;
        }
        total += 1;
        if frame[i] {
            hit += 1;
            let (x, y) = ((i as u32) % w, (i as u32) / w);
            bbox = Some(match bbox {
                None => (x, y, x, y),
                Some((bx0, by0, bx1, by1)) => (bx0.min(x), by0.min(y), bx1.max(x), by1.max(y)),
            });
        }
    }
    (hit, total, bbox)
}

/// **That fix's pixel gate: a column the client no longer has must stop
/// painting.**
///
/// A count of resident sections cannot see this — a chunk can be meshed and not
/// drawn, and (the actual that fix failure) *drawn and not meshed*. So this asserts
/// coverage inside one column's own screen rect, and the rect is **measured from
/// the real draw** rather than restated as a constant: a first render with only
/// the subject column uploaded gives its exact pixel footprint through the same
/// projection, pipeline and atlas the gate then re-renders with. The camera's aim
/// is likewise solved from the yaw convention in `lodestone_render::camera`'s
/// module doc rather than hand-tuned.
///
/// Three premise checks come first, in the order this repo's doctrine asks for:
///
/// 1. *isolated* — the subject alone. Establishes the rect and that it is a
///    substantial one.
/// 2. *without the subject* — every other column uploaded, subject absent. This
///    is "what else already paints here", and it must be near zero or the rect
///    is unusable and every later assertion is satisfied by a neighbour. Five
///    premise-false controls were found in this repo by *not* asking this.
/// 3. *full* — every column including the subject. Coverage must return, or the
///    gate would pass on a client that draws nothing at all.
///
/// Then the subject: drive the **production** eviction path
/// ([`crate::mesher::TerrainMesh::forget_column`] → `drain_removals` →
/// `remove_section`, in `app/redraw.rs`'s order) and require the rect to go back
/// to sky. Its negative control is
/// [`control_unevicted_column_keeps_painting_after_the_client_drops_it`], which
/// runs the identical sequence with the one call omitted and observes this
/// assertion fail.
#[test]
#[ignore = "requires a GPU adapter"]
fn unloaded_column_stops_painting_its_screen_rect() {
    run_eviction_gate(true);
}

/// **The negative control for the gate above, and it fails that gate's
/// assertion.**
///
/// Identical sequence with `forget_column` omitted — i.e. the pre-fix client,
/// whose store loses the column while the GPU keeps it. The subject's rect stays
/// covered, which is the state that grows into an exhausted section-origin arena
/// and the reported "collide with terrain you cannot see".
///
/// Asserting the *buggy* coverage rather than describing it is the point: if a
/// future change makes the gate above pass for an unrelated reason (a blanked
/// frame, a broken clear, a camera that sees nothing), this test goes red too
/// and names it.
#[test]
#[ignore = "requires a GPU adapter"]
fn control_unevicted_column_keeps_painting_after_the_client_drops_it() {
    run_eviction_gate(false);
}

fn run_eviction_gate(evict: bool) {
    use crate::mesher::{ColumnSource, MeshScheduler, TerrainMesh};

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

    // One isolated column, seen side-on against void, with the other columns
    // **laterally** offset so they cannot share a pixel with it.
    //
    // Premise check 2 rejected four earlier versions of this gate, on measurement
    // rather than on taste. Each number is what it reported for the subject's
    // silhouette **with the subject genuinely absent** — i.e. the share of the
    // gate that would have been measuring something other than its subject:
    //
    // | version                                            | contaminated | real cause |
    // |----------------------------------------------------|--------------|------------|
    // | bounding box, across a contiguous 5 × 5 world       |       99.9% | occlusion |
    // | silhouette mask, straight down at that world        |       71.8% | culled shared faces |
    // | straight down, columns two apart                    |       55.4% | 70-block-tall side sprawl |
    // | side-on, isolated column, flat sky reference        |       46.4% | **the sky gradient** |
    //
    // The first three are geometry, and separating the columns and viewing side-on
    // fixes them: the silhouette becomes a tall slab, what lies behind it is void,
    // and the other columns sit beside it on screen rather than behind it.
    //
    // The fourth was not geometry at all, and it is the one worth remembering. The
    // contaminant did not move when the camera distance was **doubled** — the count
    // stayed byte-identical at 4291 px in the same bounding box — which is only
    // possible if it is not in the world. It was the gradient sky disc being
    // classified as terrain by a threshold against the flat `sky_clear_bytes()`.
    // Three rounds of moving the camera were spent on a contaminant that a camera
    // could never have moved; the fix was to make the reference a *rendered* frame
    // (see `diff_mask`). "What else already paints here" has an answer that is not
    // in the scene, and a premise check that only ever considers geometry cannot
    // reach it.
    //
    // Adjacency and spacing are irrelevant to everything actually under test here
    // (eviction bookkeeping, `remove_section`, and the uncalled draw loop), so
    // buying attributability with them is a fair trade. The distractors are not
    // decoration either: they are what makes "the frame was not simply blanked" a
    // measurement at the end of this test.
    let subject = (0i32, 0i32);
    let distractors: Vec<(i32, i32)> = vec![(2, 0), (2, 1), (-2, 0), (-2, 1)];
    let surface = crate::worldgen::surface_height(subject.0 * 16 + 8, subject.1 * 16 + 8);
    // Aim at the slab's mid-height so the whole silhouette is in frame with sky
    // above it and void below.
    let subject_centre = glam::Vec3::new(
        (subject.0 * 16 + 8) as f32,
        surface as f32 / 2.0,
        (subject.1 * 16 + 8) as f32,
    );
    // Square on, from `-z`. The distance is not load-bearing now that fog is
    // pushed out and the reference is a rendered frame (see `new_state` and
    // `diff_mask`); it was chosen while chasing a contaminant that turned out not
    // to be in the world at all.
    let position = subject_centre - glam::Vec3::new(0.0, 0.0, 90.0);
    // Solved, not tuned. `lodestone_render::camera`'s module doc gives forward as
    // `(-sin(yaw)cos(pitch), -sin(pitch), cos(yaw)cos(pitch))`, so aiming at a
    // horizontal offset `(dx, dz)` means `yaw = atan2(-dx, dz)` — and yaw `0`
    // facing `+Z` with yaw `90` facing `-X` is exactly what that inverts to.
    let delta = subject_centre - position;
    let yaw = (-delta.x).atan2(delta.z).to_degrees();
    let pitch = (-delta.y)
        .atan2((delta.x * delta.x + delta.z * delta.z).sqrt())
        .to_degrees();
    let camera = Camera {
        position,
        yaw,
        pitch,
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    // Mesh every column once, keyed by column, so the renders below differ only
    // in which columns are uploaded.
    let mut world = lodestone_world::World::new();
    for &(cx, cz) in std::iter::once(&subject).chain(distractors.iter()) {
        world.load(
            lodestone_world::ChunkPos::new(cx, cz),
            crate::worldgen::generate_column(cx, cz),
        );
    }
    // The test edits the store (unload below), so it holds the write
    // handle and hands the paired read handle to the mesher.
    let write = lodestone_ecs::ChunkWorldWrite::new(world);
    let store = write.read_handle();
    let mut terrain = TerrainMesh::new(MeshScheduler::new(
        2,
        crate::blocks::ShellClassifier::Demo(crate::blocks::DemoClassifier),
    ));
    // The demo classifier yields `Complete`; the live path is `Streaming`. The
    // eviction path under test is id-space and column-source agnostic, and
    // `Complete` keeps every column drawable so the rect is not confounded by a
    // deferred frontier.
    terrain.column_source = ColumnSource::Complete;

    let mut by_column: HashMap<(i32, i32), Vec<crate::mesher::Meshed>> = HashMap::new();
    for &(cx, cz) in std::iter::once(&subject).chain(distractors.iter()) {
        terrain.mesh_column(&store, cx, cz);
        let meshes = terrain.drain_all_meshes();
        let _ = terrain.drain_removals();
        by_column.insert((cx, cz), meshes);
    }
    assert!(
        by_column.get(&subject).is_some_and(|m| !m.is_empty()),
        "the subject column must mesh to something, else every rect below is empty"
    );

    // Fog pushed out past the far plane on every one of the four renders below.
    // Not a convenience: the terrain/sky classifier is a distance from the sky
    // clear, and default fog (which starts near `8 * 16` blocks) fades distant
    // terrain *toward that clear*, so at this camera distance a fogged slab would
    // read as sky and the gate would report an eviction that never happened —
    // passing for the one reason that would make it worthless. Fog is orthogonal
    // to eviction, so removing it narrows the gate to its subject rather than
    // weakening it.
    let new_state = || {
        let mut s = RenderState::new(device, queue, format, w, h, None);
        s.set_fog(
            lodestone_render::fog::FogSettings::for_render_distance(SKY_COLOR, 32),
            32,
        );
        s
    };
    let render_once = |state: &RenderState, target: &mut HeadlessTarget| -> Vec<u8> {
        let frame = target.acquire().expect("headless acquire");
        let _ = state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };
    let upload_all = |state: &mut RenderState, skip: Option<(i32, i32)>| {
        for (col, meshes) in &by_column {
            if Some(*col) == skip {
                continue;
            }
            for m in meshes {
                state.upload_section(device, queue, m.key, &m.mesh);
            }
        }
    };

    eprintln!("=== #479 eviction pixel gate (evict={evict}) ===");
    eprintln!("subject column    = {subject:?}");
    eprintln!("camera            = pos {position:?} yaw {yaw:.1} pitch {pitch:.1}");

    // --- the reference: this camera's sky, with nothing uploaded ------------
    // Every mask below is a difference against *this*, not against the flat
    // `sky_clear_bytes()`. See `diff_mask` for the measurement that forced it.
    let sky_state = new_state();
    let sky_frame = render_once(&sky_state, &mut target);

    // --- premise check 1: the subject alone, to measure its silhouette ------
    let mut isolated = new_state();
    for m in &by_column[&subject] {
        isolated.upload_section(device, queue, m.key, &m.mesh);
    }
    let subject_mask = diff_mask(&render_once(&isolated, &mut target), &sky_frame);
    let mask_px = subject_mask.iter().filter(|&&b| b).count();
    let frame_px = (w * h) as usize;
    eprintln!(
        "subject silhouette= {mask_px} px ({:.1}% of frame)",
        mask_px as f64 / frame_px as f64 * 100.0
    );
    assert!(
        mask_px > 2_000,
        "the subject's silhouette must be a substantial share of the frame for a \
         coverage fraction over it to mean anything: {mask_px} px of {frame_px}"
    );

    // --- premise check 2: what else already paints here ---------------------
    let mut without = new_state();
    upload_all(&mut without, Some(subject));
    let bg = diff_mask(&render_once(&without, &mut target), &sky_frame);
    let (bg_hit, bg_total, bg_box) = coverage_within(&bg, &subject_mask, w);
    let bg_frac = bg_hit as f64 / bg_total as f64;
    eprintln!(
        "silhouette w/o it = {:.1}% ({bg_hit}/{bg_total}, box {bg_box:?})",
        bg_frac * 100.0
    );
    assert!(
        bg_frac < 0.15,
        "premise check: with the subject column absent, {:.1}% of its silhouette \
         is still terrain (box {bg_box:?}) — something else paints here, so 'the \
         silhouette went to sky' could not be attributed to the subject and this \
         gate would be unusable as written",
        bg_frac * 100.0
    );

    // --- premise check 3: full frame, coverage returns ----------------------
    let mut state = new_state();
    upload_all(&mut state, None);
    let full = diff_mask(&render_once(&state, &mut target), &sky_frame);
    let (full_hit, _, full_box) = coverage_within(&full, &subject_mask, w);
    let full_frac = full_hit as f64 / bg_total as f64;
    eprintln!(
        "silhouette full   = {:.1}% (box {full_box:?})",
        full_frac * 100.0
    );
    assert!(
        full_frac > 0.90,
        "the subject must paint its own silhouette in a full frame, got {:.1}% \
         (box {full_box:?}) — if this is low the detector is broken and every \
         later assertion passes for free",
        full_frac * 100.0
    );

    // --- the subject: the client drops the column ---------------------------
    // Production order: the adapter unloads the column from the one store
    // *before* it emits, and the shell then evicts what the GPU still holds.
    write
        .write()
        .unload(lodestone_world::ChunkPos::new(subject.0, subject.1));
    if evict {
        terrain.forget_column(subject.0, subject.1);
    }
    // `app/redraw.rs`'s drain, in its order (removals before uploads).
    let removals = terrain.drain_removals();
    for key in &removals {
        state.remove_section(key);
    }
    for m in terrain.drain_meshes() {
        state.upload_section(device, queue, m.key, &m.mesh);
    }
    let after = diff_mask(&render_once(&state, &mut target), &sky_frame);
    let (after_hit, _, after_box) = coverage_within(&after, &subject_mask, w);
    let after_frac = after_hit as f64 / bg_total as f64;
    // Everything *outside* the subject's silhouette — the distractor columns.
    // Without this a frame that lost all its terrain would sail through the
    // assertion below, which is the "correctly rendered nothing" failure the
    // older gates in this file guard with a two-sided sky check.
    let elsewhere = |mask: &[bool]| -> usize {
        mask.iter()
            .zip(subject_mask.iter())
            .filter(|(m, s)| **m && !**s)
            .count()
    };
    let (rest_full, rest_after) = (elsewhere(&full), elsewhere(&after));
    eprintln!("removals drained  = {}", removals.len());
    eprintln!(
        "silhouette after  = {:.1}% (box {after_box:?})",
        after_frac * 100.0
    );
    eprintln!("terrain elsewhere = {rest_full} -> {rest_after}");
    assert!(
        rest_after * 100 >= rest_full * 95 && rest_full > 2_000,
        "the columns that did NOT unload must be unaffected: {rest_full} px of \
         terrain outside the subject's silhouette became {rest_after}. An \
         eviction that blanks the frame would otherwise pass the assertion below \
         for entirely the wrong reason."
    );

    if evict {
        assert!(
            !removals.is_empty(),
            "the eviction path produced no removals at all — `forget_column` \
             derived nothing from `uploaded_sections`"
        );
        // Back to the floor premise check 2 measured for this exact rect, rather
        // than to a restated constant.
        assert!(
            after_frac < bg_frac.max(0.15),
            "a column the client no longer has is still painting {:.1}% of its own \
             on-screen silhouette (terrain bounding box {after_box:?}); with the \
             column genuinely absent that same silhouette measures {:.1}% (premise \
             check 2). The renderer is drawing blocks the store does not have, and \
             its section-origin arena never gets those slots back — walk far \
             enough and `upload_section` drops new geometry instead.",
            after_frac * 100.0,
            bg_frac * 100.0
        );
    } else {
        assert!(
            removals.is_empty(),
            "the control must not evict, but {} removals were drained",
            removals.len()
        );
        assert!(
            after_frac > 0.60,
            "control premise: without `forget_column` the stale column must keep \
             painting, got {:.1}% (box {after_box:?}) — if this is low then \
             something *else* already evicts it, and the gate above proves \
             nothing about `forget_column`",
            after_frac * 100.0
        );
        eprintln!(
            "CONTROL: the gate's assertion fails here as required — {:.1}% \
             coverage remains against a {:.1}% floor",
            after_frac * 100.0,
            bg_frac * 100.0
        );
    }
}

// ---------------------------------------------------------------------------
// Experience orbs: the sprite has to reach pixels, and the *bucket* has to
// reach the sprite
// ---------------------------------------------------------------------------

/// One `EntityDraw` for an experience orb worth `value`, hung above the
/// horizon so nothing else in the frame can paint over it.
///
/// Every field but `experience_orb_value` is held constant across the arms of
/// the gates below — the age in particular, because the orb's pulsing tint is
/// derived from it and a differing age would make two frames differ for a
/// reason that has nothing to do with the sprite cell.
#[cfg(test)]
fn orb_draw(value: i32) -> EntityDraw {
    EntityDraw {
        id: 1,
        type_path: std::sync::Arc::from(crate::entities::EXPERIENCE_ORB_TYPE_PATH),
        item: None,
        item_model: None,
        item_skin: None,
        // Above the eye and dead ahead of a yaw-0 camera, so the sprite lands in
        // the upper middle of the frame — clear of the first-person arm, which is
        // drawn unconditionally into the bottom right.
        feet: glam::Vec3::new(0.0, 2.0, 2.0),
        yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        anim: lodestone_render::AnimInput::REST,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_skin: Vec::new(),
        equipment_trim: Vec::new(),
        wool: None,
        block_state: None,
        item_frame_rotation: 0,
        count: 1,
        foil: false,
        item_dyed_color: None,
        item_potion_color: None,
        name_tag: None,
        hurt: false,
        item_use: None,
        main_arm_left: false,
        creeper_swelling: 0.0,
        swim_amount: 0.0,
        death_time: 0.0,
        on_fire: false,
        invisible: false,
        armor_stand: None,
        player_skin: None,
        variant_sheet: None,
        experience_orb_value: Some(value),
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
        firework: None,
        projectile_owner: None,
    }
}

/// The camera the orb gates render from — small, so the readback is cheap, and
/// pitched flat so the orb sits above the horizon line.
#[cfg(test)]
fn orb_camera(w: u32, h: u32) -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 1.4, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

/// The orb's own screen rect, as the **upper half** of the frame: the sprite is
/// at world `y ≈ 2.1` seen from an eye at `1.4` with no pitch, so it is above
/// the horizon, and the only other thing that can paint up there is the sky.
///
/// A rect rather than the whole frame, and the upper half rather than a tight
/// box, because the first-person arm draws unconditionally into the bottom right
/// and would otherwise be counted as orb coverage — the premise-false failure
/// mode `zombie_wears_its_real_skin_not_the_flat_placeholder` records for the
/// same harness.
#[cfg(test)]
fn orb_rect_pixels(pixels: &[u8], w: u32, h: u32) -> Vec<[u8; 4]> {
    let mut out = Vec::new();
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let i = i as u32;
        if i / w >= h / 2 {
            continue;
        }
        out.push([px[0], px[1], px[2], px[3]]);
    }
    out
}

/// An experience orb must put **green** pixels on screen, and a scene with no
/// orb in it must not.
///
/// This is the island detector for the whole five-hop chain: the decode, the
/// component, the bridge and the pass are each individually green in hermetic
/// tests while the orb reaches zero pixels, which is exactly what shipped before
/// this landed.
///
/// The classifier is **green-dominant and away from the sky**, not merely
/// "different from the sky": vanilla's orb pins green at 255 and modulates red to
/// at most half and blue to at most a tenth, so `g > r` and `g > b` is a
/// signature only the orb tint can produce up there. The negative control is the
/// same frame with the orb removed and must measure ~zero — if it does not, the
/// upper half of the frame already has something green in it and this gate is
/// measuring that instead.
#[test]
#[ignore = "requires a GPU adapter and .cache/mc/26.2/client.jar"]
fn an_experience_orb_paints_green_pixels_and_an_empty_scene_does_not() {
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
    let camera = orb_camera(w, h);

    let green_count = |pixels: &[u8]| -> usize {
        let sky = sky_clear_bytes();
        orb_rect_pixels(pixels, w, h)
            .into_iter()
            .filter(|px| {
                let far_from_sky = (i32::from(px[0]) - i32::from(sky[0])).abs()
                    + (i32::from(px[1]) - i32::from(sky[1])).abs()
                    + (i32::from(px[2]) - i32::from(sky[2])).abs()
                    > 60;
                far_from_sky && px[1] > px[0] && px[1] > px[2]
            })
            .count()
    };

    // The subject: one orb worth 617, which buckets to sprite cell 8.
    let draws = vec![orb_draw(617)];
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), &camera, None, &draws);
    let with_orb = target.read_texels(device, queue);
    let painted = green_count(&with_orb);

    // The control: the identical frame with nothing in it.
    let frame = target.acquire().expect("headless acquire");
    let empty_stats = state.render(device, queue, frame.view(), &camera, None, &[]);
    let without_orb = target.read_texels(device, queue);
    let background = green_count(&without_orb);

    eprintln!("=== experience orb pixel gate ===");
    eprintln!(
        "with orb: orbs_drawn={} green_px={painted}",
        stats.experience_orbs_drawn
    );
    eprintln!(
        "control:  orbs_drawn={} green_px={background}",
        empty_stats.experience_orbs_drawn
    );

    // Collected, so a counter that is right and a sprite that is missing are
    // reported together rather than the first one aborting.
    let mut wrong: Vec<String> = Vec::new();
    if stats.experience_orbs_drawn != 1 {
        wrong.push(format!(
            "expected exactly one orb billboard, got {}",
            stats.experience_orbs_drawn
        ));
    }
    if empty_stats.experience_orbs_drawn != 0 {
        wrong.push(format!(
            "the empty control drew {} orb billboards",
            empty_stats.experience_orbs_drawn
        ));
    }
    // The control has to fire *first*: if the background is already green, the
    // subject's count means nothing.
    if background > 20 {
        wrong.push(format!(
            "control premise is false — the orb-less upper half already has \
             {background} green pixels, so this gate would pass without an orb"
        ));
    }
    // 0.3 blocks wide at 2 blocks' distance through a 60-degree vertical FOV over
    // 240 rows is roughly 30 px across, so a real sprite covers hundreds of
    // pixels even after its transparent corners are discarded. A floor of 100
    // separates "the sprite drew" from "a stray antialiased edge".
    if painted < 100 {
        wrong.push(format!(
            "the orb painted only {painted} green pixels; a 0.3-block sprite two \
             blocks away should cover hundreds — the billboard is not reaching the \
             screen even though the counter says it drew"
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The **bucket** has to reach the sprite: two orbs whose values fall in
/// *different* buckets must draw different pixels, and two whose values fall in
/// the *same* bucket must draw byte-identical ones.
///
/// This is the pixel-level half of
/// `orb_icon_is_bucketed_and_constant_inside_a_bucket`, and it is what the unit
/// test structurally cannot see: `experience_orb_icon` can be perfectly correct
/// while the mesh always samples cell 0, in which case every orb in the game
/// draws a plausible sprite and nothing is ever wrong-looking enough to report.
///
/// The same-bucket arm is the control, and it is a **stronger** one than "the
/// frames differ": it is byte-identical or the gate is measuring frame noise
/// rather than the cell. 7 and 16 are both cell 2; 7 and 617 are cells 2 and 8.
/// Every other input is held fixed, the age included, so the tint cycle cannot
/// contribute a difference of its own.
#[test]
#[ignore = "requires a GPU adapter and .cache/mc/26.2/client.jar"]
fn orbs_in_different_buckets_draw_different_sprites_and_same_bucket_orbs_do_not() {
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
    let camera = orb_camera(w, h);

    let mut rect_for = |value: i32| -> Vec<[u8; 4]> {
        let frame = target.acquire().expect("headless acquire");
        let draws = vec![orb_draw(value)];
        state.render(device, queue, frame.view(), &camera, None, &draws);
        let pixels = target.read_texels(device, queue);
        orb_rect_pixels(&pixels, w, h)
    };

    // Cell 2 twice, then cell 8. `experience_orb_icon` is re-asserted here rather
    // than assumed, so a future change to the ladder makes this gate say what went
    // wrong instead of failing mysteriously.
    assert_eq!(lodestone_render::experience_orb_icon(7), 2);
    assert_eq!(lodestone_render::experience_orb_icon(16), 2);
    assert_eq!(lodestone_render::experience_orb_icon(617), 8);

    let low = rect_for(7);
    let same_bucket = rect_for(16);
    let other_bucket = rect_for(617);

    let differing = |a: &[[u8; 4]], b: &[[u8; 4]]| -> usize {
        a.iter().zip(b).filter(|(x, y)| x != y).count()
    };
    let same = differing(&low, &same_bucket);
    let across = differing(&low, &other_bucket);
    eprintln!("=== orb sprite-cell gate ===");
    eprintln!("value 7 vs 16 (both cell 2): {same} differing pixels");
    eprintln!("value 7 vs 617 (cell 2 vs 8): {across} differing pixels");

    let mut wrong: Vec<String> = Vec::new();
    if same != 0 {
        wrong.push(format!(
            "two values inside one bucket drew {same} differing pixels; the cell is \
             not the only thing the value decides, so the assertion below cannot be \
             attributed to the bucket"
        ));
    }
    if across == 0 {
        wrong.push(
            "two values in different buckets drew byte-identical frames — the sprite \
             cell is not reaching the mesh, so every orb in the game draws cell 0"
                .to_owned(),
        );
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// **The end-to-end wiring half of the menu-blur fix** — `checkerboard_loses_edge_contrast_but_keeps_its_mean`
/// in `menu/render/blur.rs` proves the `MenuBlur` pipeline itself blurs; this
/// proves `MenuFrame::blur` actually reaches it through the real
/// `MenuRenderer::begin_frame` → `render_overlay` path, not just a unit
/// calling `MenuBlur::run` directly. Without this, `MenuBlur` could be
/// correct, tested, and still be the "built, tested, reaches no pixels"
/// island shape this repo has shipped nine times before.
///
/// Two draws of the *same* seeded checkerboard through the *same*
/// `MenuRenderer`, differing only in `MenuFrame::blur` — a paired
/// comparison, so the `Dim` wash quad both frames also draw (translucent,
/// so it shifts every pixel toward black by a constant factor) cancels out
/// of the *contrast* measurement instead of needing its own tolerance.
#[test]
#[ignore = "requires a GPU adapter"]
fn menu_frame_blur_flag_reaches_the_real_render_overlay_path() {
    use crate::menu::render::{MenuBackdrop, MenuFrame, MenuRenderer};

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter), don't 'skip' — a silent pass \
         here would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h): (u32, u32) = (64, 64);
    const CELL: u32 = 8;

    let mut checkerboard = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let v = if ((x / CELL) + (y / CELL)) % 2 == 0 { 255 } else { 0 };
            let i = ((y * w + x) * 4) as usize;
            checkerboard[i] = v;
            checkerboard[i + 1] = v;
            checkerboard[i + 2] = v;
            checkerboard[i + 3] = 255;
        }
    }

    let draw = |blur: bool| -> Vec<u8> {
        let mut target = HeadlessTarget::new(device, w, h, format);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: target.texture(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &checkerboard,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        let frame = RenderTarget::acquire(&mut target).expect("headless acquire");
        let mut menu = MenuRenderer::new(device, format);
        menu.begin_frame(frame.colour_texture().clone());
        let menu_frame = MenuFrame {
            backdrop: MenuBackdrop::Dim,
            blur,
            ..Default::default()
        };
        menu.render_overlay(device, queue, frame.view(), &menu_frame, w, h);
        target.read_texels(device, queue)
    };

    let unblurred = draw(false);
    let blurred = draw(true);

    let px = |buf: &[u8], x: u32, y: u32| -> i32 { i32::from(buf[((y * w + x) * 4) as usize]) };
    let mut worse: Vec<(u32, u32, i32, i32)> = Vec::new();
    for x in (CELL..w).step_by(CELL as usize) {
        for y in [CELL / 2, CELL * 5 / 2, CELL * 9 / 2, CELL * 13 / 2] {
            let plain = (px(&unblurred, x - 1, y) - px(&unblurred, x, y)).abs();
            let with_blur = (px(&blurred, x - 1, y) - px(&blurred, x, y)).abs();
            // Each location must lose contrast once the flag is set — a
            // per-location comparison, collected rather than asserted
            // inside the loop, so a failure names every offending edge.
            if with_blur >= plain {
                worse.push((x, y, plain, with_blur));
            }
        }
    }
    assert!(
        worse.is_empty(),
        "{} of 28 edges did not lose contrast when MenuFrame::blur was set, through \
         the real render_overlay path -- offending (x, y, unblurred_contrast, \
         blurred_contrast): {worse:?}",
        worse.len()
    );
}

/// Standard sRGB electro-optical transfer function, `[0,1] -> [0,1]`.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Its inverse (opto-electronic transfer function).
fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// `TAB_ROW_FILL`'s alpha, `0x20 / 255`.
const TAB_ROW_FILL_ALPHA: f32 = 32.0 / 255.0;

/// Reference gamma-byte blend: composited directly on raw gamma bytes, no colour
/// management at all. A white (`0xFFFFFF`) foreground at [`TAB_ROW_FILL_ALPHA`]
/// over a grey background byte `bg` — every channel is symmetric for a grey
/// background, so this is a scalar. Exact, not a bracket: raw-byte alpha
/// compositing is plain linear interpolation in 8-bit space.
fn predicted_vanilla_gamma_byte(bg: u8) -> f32 {
    let bg_f = f32::from(bg);
    bg_f + TAB_ROW_FILL_ALPHA * (255.0 - bg_f)
}

/// sRGB-target control hypothesis: the GPU treats the shader's raw-byte-derived
/// output as *linear* light, blends in linear space, then re-encodes on write.
/// White's linear value is `1.0` (sRGB's own fixed point), so only the
/// background side needs converting.
fn predicted_linear_hypothesis_gamma_byte(bg: u8) -> f32 {
    let bg_lin = srgb_to_linear(f32::from(bg) / 255.0);
    let blended_lin = TAB_ROW_FILL_ALPHA + bg_lin * (1.0 - TAB_ROW_FILL_ALPHA);
    linear_to_srgb(blended_lin) * 255.0
}

/// Build the *exact* `hud.wgsl` flat-colour pipeline `HudRenderer::new`
/// builds in `hud.rs` (same vertex layout, same `ALPHA_BLENDING` state), but
/// standalone against a caller-chosen `color_format`. The raw-target result
/// represents production's `RenderTarget::raw_view_format()` plus
/// `HudRenderer::flat_colour_view()` pairing; the sRGB target is an explicit
/// control for the colour-space comparison. Sourced from the same
/// `shaders/hud.wgsl` file production uses, not a reimplementation.
fn build_hud_flat_pipeline(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::Buffer) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("blend-gate-hud-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/hud.wgsl").into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("blend-gate-hud-layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("blend-gate-hud-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: (6 * 4) as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 8,
                        shader_location: 1,
                    },
                ],
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("blend-gate-hud-verts"),
        size: (6 * 6 * 4) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    (pipeline, buffer)
}

/// Render one full-viewport quad of `fg_rgba` (`hud.wgsl`'s flat-colour
/// pipeline, `ALPHA_BLENDING`) over a target seeded with a known raw grey
/// background byte `bg`, and read the resulting stored byte back.
///
/// The background is seeded with [`wgpu::Queue::write_texture`], a literal
/// byte copy with **no** colour-space translation on either format — unlike a
/// `LoadOp::Clear`, whose `wgpu::Color` is specified in linear space and
/// re-encoded on write for an sRGB target, which would silently launder the
/// exact bug this gate exists to measure into the very fixture that sets it
/// up. This is why the background is written, not cleared.
fn render_flat_blend_over_grey(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    bg: u8,
    fg_rgba: [f32; 4],
) -> [u8; 4] {
    let mut target = HeadlessTarget::new(device, 4, 4, format);
    let bg_bytes = [bg, bg, bg, 255];
    let mut row = Vec::with_capacity(16);
    for _ in 0..4 {
        row.extend_from_slice(&bg_bytes);
    }
    let mut full = Vec::with_capacity(64);
    for _ in 0..4 {
        full.extend_from_slice(&row);
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: target.texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &full,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(16), rows_per_image: Some(4) },
        wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
    );

    let (pipeline, buffer) = build_hud_flat_pipeline(device, format);
    // Two triangles covering the whole `[-1,1]` NDC square, every vertex
    // carrying the same flat colour (position x2, colour x4 per vertex,
    // matching `hud.wgsl`'s vertex layout).
    let [r, g, b, a] = fg_rgba;
    #[rustfmt::skip]
    let verts: [f32; 36] = [
        -1.0, -1.0, r, g, b, a,
         1.0, -1.0, r, g, b, a,
        -1.0,  1.0, r, g, b, a,
        -1.0,  1.0, r, g, b, a,
         1.0, -1.0, r, g, b, a,
         1.0,  1.0, r, g, b, a,
    ];
    queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&verts));

    let frame = target.acquire().expect("headless acquire");
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("blend-gate-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blend-gate-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: frame.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.draw(0..6, 0..1);
    }
    queue.submit(std::iter::once(encoder.finish()));
    let pixels = target.read_texels(device, queue);
    [pixels[0], pixels[1], pixels[2], pixels[3]]
}

/// Gamma-byte HUD blend control for the tab-list row fill
/// (`TAB_ROW_FILL = 0x20FFFFFF`). The reference composition blends
/// translucent GUI colour directly on raw gamma bytes, while this HUD's
/// flat-colour pipeline (`hud.wgsl`) is paired in production with
/// `RenderTarget::raw_view_format()` and `HudRenderer::flat_colour_view()` so
/// the hardware blends directly on gamma bytes. Comparing that raw target
/// with an sRGB-decoding control, which blends in **linear** light, isolates
/// the colour-space difference over the same sweep.
///
/// Sweeps the background from black to white and renders the real
/// `hud.wgsl`/`ALPHA_BLENDING` pipeline (built by [`build_hud_flat_pipeline`],
/// sourced from the same `shaders/hud.wgsl` production uses) against two
/// targets:
///
/// - **raw** (`Rgba8Unorm`) — the raw view that production's
///   `HudRenderer::flat_colour_view()` draws into (from
///   [`lodestone_render::target::RenderTarget::raw_view_format`]);
/// - **sRGB control** (`Rgba8UnormSrgb`) — an explicit comparison target whose
///   view decodes to linear light before blending.
///
/// # What is predicted exactly vs. bracketed
///
/// Raw-byte alpha compositing in 8-bit space is plain linear interpolation,
/// so the **raw** target's result is predicted to the byte
/// ([`predicted_vanilla_gamma_byte`], tolerance ±2 for rounding). The
/// **sRGB control** target's result is *not* asserted to the byte — this
/// codebase has already measured that `ALPHA_BLENDING` on an sRGB Metal
/// target is "a real, repeatable, non-trivial function of the raw fragment
/// alpha byte" that resists a textbook closed form — but it *is* bracketed.
///
/// `TAB_ROW_FILL`'s foreground is white — `1.0` in both gamma and linear
/// representations — so the only source of divergence is the background's
/// sRGB decode. The independently evaluated transfer function predicts a
/// monotonically decreasing difference from black to white: about `67/255` at
/// `bg=0`, shrinking smoothly to exactly `0` at `bg=255`, where foreground
/// and background coincide. The assertions below check that measured sweep
/// against those independently derived expectations.
#[test]
#[ignore = "requires a GPU adapter"]
fn hud_flat_colour_blend_matches_vanilla_gamma_on_a_raw_target() {
    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();

    const RAW: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
    const CORRECTED: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
    // `RAW` really is `CORRECTED`'s raw counterpart, tying this measurement
    // to the exact pair `RenderTarget::raw_view_format`/`RenderTarget::format`
    // would hand a real caller for a target built with `CORRECTED`.
    assert_eq!(CORRECTED.remove_srgb_suffix(), RAW);

    let fg = [1.0_f32, 1.0, 1.0, TAB_ROW_FILL_ALPHA];
    // Black to white, nine points, endpoints included — both target formats
    // receive the same background sweep for a direct comparison.
    let sweep: [u8; 9] = [0, 32, 64, 96, 128, 160, 192, 224, 255];

    struct Row {
        bg: u8,
        vanilla: f32,
        linear_hyp: f32,
        raw_actual: u8,
        corrected_actual: u8,
    }
    let mut rows = Vec::new();
    for &bg in &sweep {
        let raw_px = render_flat_blend_over_grey(device, queue, RAW, bg, fg);
        let corrected_px = render_flat_blend_over_grey(device, queue, CORRECTED, bg, fg);
        rows.push(Row {
            bg,
            vanilla: predicted_vanilla_gamma_byte(bg),
            linear_hyp: predicted_linear_hypothesis_gamma_byte(bg),
            raw_actual: raw_px[0],
            corrected_actual: corrected_px[0],
        });
    }

    eprintln!("=== hud flat-colour blend: black-to-white sweep ===");
    eprintln!(
        "{:>4} {:>10} {:>10} {:>10} {:>12} {:>10} {:>12}",
        "bg", "vanilla*", "linear_hyp", "raw_got", "raw_err", "corr_got", "corr_err_van"
    );
    for r in &rows {
        eprintln!(
            "{:>4} {:>10.2} {:>10.2} {:>10} {:>12.2} {:>10} {:>12.2}",
            r.bg,
            r.vanilla,
            r.linear_hyp,
            r.raw_actual,
            f32::from(r.raw_actual) - r.vanilla,
            r.corrected_actual,
            f32::from(r.corrected_actual) - r.vanilla,
        );
    }

    // 1) The raw target must match the reference gamma-byte blend to the byte.
    // Replacing `RAW` with `CORRECTED` makes these comparisons fail, so the
    // format-sensitive differential is what gives this assertion its control.
    let mut raw_mismatches: Vec<(u8, f32, u8)> = Vec::new();
    for r in &rows {
        if (f32::from(r.raw_actual) - r.vanilla).abs() > 2.0 {
            raw_mismatches.push((r.bg, r.vanilla, r.raw_actual));
        }
    }
    assert!(
        raw_mismatches.is_empty(),
        "the RAW (non-sRGB) target must reproduce vanilla's own raw-gamma blend to within \
         ±2/255 at every background level -- (bg, predicted_vanilla, actual) mismatches: \
         {raw_mismatches:?}"
    );

    // 2) The sRGB control's error against the reference must be large against
    // a *dark* background. This is measured live rather than asserted from
    // prose, and it is what makes assertion 1 meaningful
    // rather than coincidental (a pipeline indifferent to format would pass
    // assertion 1 by luck if this one also passed). Per the module doc's
    // re-derivation, the divergence is largest near black and falls off
    // toward white -- not a hump peaking mid-sweep -- so this checks the dark
    // end specifically (bg <= 64) rather than "somewhere in the middle".
    let dark: Vec<&Row> = rows.iter().filter(|r| r.bg <= 64).collect();
    let mut small_dark_errors: Vec<(u8, f32, u8)> = Vec::new();
    for r in &dark {
        let err = (f32::from(r.corrected_actual) - r.vanilla).abs();
        if err <= 15.0 {
            small_dark_errors.push((r.bg, r.vanilla, r.corrected_actual));
        }
    }
    assert!(
        small_dark_errors.is_empty(),
        "expected the CORRECTED (sRGB) target to diverge from vanilla's blend by >15/255 \
         against a dark background (bg <= 64; the bug this gate exists to reproduce) -- but \
         these dark-sweep levels stayed close: {small_dark_errors:?}. Either the bug is gone \
         (re-check docs/tab-list.md) or this gate's fixture is broken."
    );

    // 3) And it must collapse close to the reference at the *white* end only —
    // white is the one point in this sweep where foreground and background
    // coincide (both `0xFFFFFF`), so every consistent blend model, gamma or
    // linear, must agree there. This is deliberately **not** asserted at
    // black too — black is where this sweep's divergence is largest, not where
    // it collapses.
    let mut white_end_errors: Vec<(u8, f32, u8)> = Vec::new();
    for r in rows.iter().filter(|r| r.bg == 255) {
        let err = (f32::from(r.corrected_actual) - r.vanilla).abs();
        if err > 4.0 {
            white_end_errors.push((r.bg, r.vanilla, r.corrected_actual));
        }
    }
    assert!(
        white_end_errors.is_empty(),
        "expected the CORRECTED (sRGB) target's divergence from vanilla to collapse to ~0 at \
         white (foreground and background coincide there, `0xFFFFFF` over `0xFFFFFF`) -- (bg, \
         predicted_vanilla, actual) that did not collapse: {white_end_errors:?}"
    );

    // 4) Diagnostic only, not a hard assertion: does the corrected target's
    // actual result track the *linear*-blend hypothesis at all, tying the
    // mechanism to the symptom? Not asserted tightly because this codebase
    // has already measured real sRGB `ALPHA_BLENDING` hardware behaviour as a
    // "non-trivial function" that resists a textbook closed form on at least
    // one backend (Metal) -- a tight per-point tolerance here would risk
    // being exactly the kind of unverified precision CLAUDE.md warns against
    // asserting. Printed for the record; large, systematic disagreement here
    // would be worth a follow-up even though nothing above requires it.
    let mut max_linear_gap: f32 = 0.0;
    for r in &rows {
        max_linear_gap = max_linear_gap.max((f32::from(r.corrected_actual) - r.linear_hyp).abs());
    }
    eprintln!(
        "corrected-target vs. linear-hypothesis: max |actual - predicted_linear| = \
         {max_linear_gap:.2}/255 across the sweep (diagnostic only, see row table above)"
    );
}

/// Dropping the presentation-side GPU state must **measurably**
/// release real `wgpu` resources — not merely stop submitting draw calls,
/// which buys nothing for a session left running headless in the background.
///
/// # How this is measured
///
/// `wgpu::Instance::generate_report()` returns live per-resource-type
/// allocation counts straight out of `wgpu-core`'s own bookkeeping
/// (`RegistryReport::num_allocated`) — not a count this crate derives, and
/// not the absence of a draw call. This builds the same [`RenderState`] and
/// [`crate::hud::HudRenderer`] `WindowApp::finish_bring_up` builds on a real
/// attach (offscreen here — no window needed to allocate and drop `wgpu`
/// handles), takes a report before and after, drops everything, polls the
/// device so any deferred `wgpu-core` cleanup actually runs, and takes a
/// third report.
///
/// The **control** this test needs (an absence assertion needs a detector
/// proven to work): the *attach* report is asserted to have **more**
/// textures, buffers and render pipelines than the baseline first. A
/// `generate_report()` call that always read zero, or that this test misused
/// so it never actually counted anything, would make the *detach* assertion
/// beneath it vacuous — "went from 0 to 0" proves nothing was measured, not
/// that presentation was released.
#[test]
#[ignore = "requires a GPU adapter"]
fn detach_presentation_releases_wgpu_resources() {
    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;

    let report = || {
        ctx.instance()
            .generate_report()
            .expect("native wgpu-core backend must support generate_report")
    };

    let baseline = report();

    // The same construction `WindowApp::finish_bring_up` does on a real
    // attach: `RenderState` (every terrain/entity/HUD pipeline plus the
    // block/particle atlases) and `HudRenderer` (its own flat-colour
    // pipeline). Small offscreen size — the GPU resource *count* this test
    // reads does not depend on framebuffer dimensions.
    let render = crate::gpu::RenderState::new(device, queue, format, 64, 64, None);
    let hud = crate::hud::HudRenderer::new(device, format);

    let attached = report();
    eprintln!(
        "baseline: textures={} buffers={} render_pipelines={} bind_groups={}",
        baseline.hub.textures.num_allocated,
        baseline.hub.buffers.num_allocated,
        baseline.hub.render_pipelines.num_allocated,
        baseline.hub.bind_groups.num_allocated,
    );
    eprintln!(
        "attached: textures={} buffers={} render_pipelines={} bind_groups={}",
        attached.hub.textures.num_allocated,
        attached.hub.buffers.num_allocated,
        attached.hub.render_pipelines.num_allocated,
        attached.hub.bind_groups.num_allocated,
    );
    assert!(
        attached.hub.textures.num_allocated > baseline.hub.textures.num_allocated,
        "RenderState/HudRenderer must allocate real textures (atlases, the depth \
         buffer) — {} -> {} shows none were created, which would make the detach \
         assertion below meaningless",
        baseline.hub.textures.num_allocated,
        attached.hub.textures.num_allocated
    );
    assert!(
        attached.hub.render_pipelines.num_allocated > baseline.hub.render_pipelines.num_allocated,
        "RenderState/HudRenderer must allocate real render pipelines — {} -> {} shows \
         none were created",
        baseline.hub.render_pipelines.num_allocated,
        attached.hub.render_pipelines.num_allocated
    );
    assert!(attached.hub.buffers.num_allocated > baseline.hub.buffers.num_allocated);
    assert!(attached.hub.bind_groups.num_allocated > baseline.hub.bind_groups.num_allocated);

    // The actual release: `WindowApp::detach_presentation` (`app::session`)
    // does exactly this — set `self.render`/`self.hud` (and every other
    // GPU-owning field) to `None`, dropping the last strong reference to
    // each `wgpu` handle these two objects hold.
    drop(render);
    drop(hud);
    // `wgpu-core` frees some resources lazily on the next device poll rather
    // than synchronously on drop (deferred destruction); without this a
    // measurement taken immediately after `drop` can under-report the
    // release, not because nothing was released but because cleanup had not
    // run yet.
    let _ = device.poll(wgpu::PollType::wait_indefinitely());

    let detached = report();
    eprintln!(
        "detached: textures={} buffers={} render_pipelines={} bind_groups={}",
        detached.hub.textures.num_allocated,
        detached.hub.buffers.num_allocated,
        detached.hub.render_pipelines.num_allocated,
        detached.hub.bind_groups.num_allocated,
    );
    assert!(
        detached.hub.textures.num_allocated < attached.hub.textures.num_allocated,
        "dropping RenderState/HudRenderer must release textures — {} -> {} shows \
         nothing was freed (a detach that only stops drawing does not release \
         anything)",
        attached.hub.textures.num_allocated,
        detached.hub.textures.num_allocated
    );
    assert!(
        detached.hub.render_pipelines.num_allocated < attached.hub.render_pipelines.num_allocated,
        "dropping RenderState/HudRenderer must release render pipelines — {} -> {} \
         shows nothing was freed",
        attached.hub.render_pipelines.num_allocated,
        detached.hub.render_pipelines.num_allocated
    );
    assert!(detached.hub.buffers.num_allocated < attached.hub.buffers.num_allocated);
    assert!(detached.hub.bind_groups.num_allocated < attached.hub.bind_groups.num_allocated);
    // Back to (at most) the baseline — not merely "some improvement". A real
    // leak would still show `detached < attached` while leaving `detached`
    // permanently above `baseline`, so this is the sharper claim.
    assert!(detached.hub.textures.num_allocated <= baseline.hub.textures.num_allocated);
    assert!(detached.hub.render_pipelines.num_allocated <= baseline.hub.render_pipelines.num_allocated);
}
