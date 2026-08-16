//! Pixel gate: the underwater air-bubble row reaches the screen through the
//! **shell's real HUD path**, not just `lodestone_render::air_bubbles`' pure
//! model.
//!
//! `air_bubbles.rs`' own unit tests prove `bubble_row` returns the right
//! `BubbleSlot` sequence for a given air supply — that is the model working, and
//! it is a closed loop. It says nothing about whether `airSupply` survives the
//! decode chain (`metadata.rs` -> `EntityMetadataUpdate` -> `Vitals::air` ->
//! `PlayerSnapshot` -> `Sim::air` -> `HudFrame::air`) or whether `sprite_vitals`
//! ever asks the atlas for a bubble sprite. Every link in that chain was added
//! at once, so a green model test is exactly the "individually built, reaches
//! zero pixels" island shape `CLAUDE.md` names.
//!
//! # The metric
//!
//! Bubbles are drawn into a row of their own, one icon-height **above** the
//! health/hunger line (`hud.rs`'s `air_row_y = row_y - icon - 1.0`, vanilla's
//! `yLineAir = yLineBase - 10`, `Hud.java`). Nothing else in this
//! fixture paints there: the debug overlay, crosshair, hotbar frame and hotbar
//! items are all switched off, and health/hunger sit *below* it. So the
//! measurement is simply "non-backdrop pixels inside the air row's rect".
//!
//! Deliberately **rect-scoped rather than whole-frame**: `sky_pixels.rs` learned
//! the hard way in this same session that a whole-frame "differs from the
//! backdrop" count silently picks up anything else that draws, and reports a
//! percentage that looks like evidence while measuring something unrelated.
//!
//! # The controls, all executed
//!
//! Vanilla's visibility rule is `isUnderWater || currentAirSupplyTicks <
//! maxAirSupplyTicks` (`Hud.java`) — an **or**. The first draft of this gate
//! got that wrong, asserting that leaving the water hides the row immediately;
//! it does not, and must not, because that is precisely what makes the gradual
//! refill visible after you surface. The controls below isolate the two
//! disjuncts instead of assuming one dominates:
//!
//! 1. **Full air, not underwater** — both disjuncts false, so the row is hidden.
//! 2. **Full air, underwater** — only `isUnderWater` is true, so the row still
//!    draws. This is what makes `eye_in_water` demonstrably load-bearing: an
//!    implementation that dropped it entirely would fail this one while passing
//!    every "air < max" assertion.
//! 3. **`frame.air = None`** — the pre-air-supply behaviour, i.e. what this row
//!    looked like before any of the wiring existed.
//!
//! Fail-closed like its siblings: a missing GPU or a missing `client.jar` is a
//! failure, never a skip. A silent pass here asserts nothing.
//!
//! ```text
//! cargo test -p lodestone-shell --test air_bubble_pixels -- --ignored --nocapture
//! ```

use std::sync::Arc;

use lodestone::hud::{DebugStats, HudFrame, HudRenderer};
use lodestone_render::{BUBBLE_COUNT, BUBBLE_SIZE, HeadlessTarget, RenderTarget};

const W: u32 = 640;
const H: u32 = 480;

/// Vanilla's full air supply — `Entity.TOTAL_AIR_SUPPLY`
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/Entity.java`), the same
/// constant `HudState::MAX_AIR` carries.
const MAX_AIR: i32 = 300;

/// Deliberately mid-drown rather than nearly-full: enough bubbles popped that
/// the row is unambiguously in its partial state, but plenty still drawn.
const HALF_AIR: i32 = 150;

fn clear_view(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView, rgb: [u8; 3]) {
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("air-bubble-gate-clear"),
    });
    {
        let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("air-bubble-gate-clear-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(rgb[0]) / 255.0,
                        g: f64::from(rgb[1]) / 255.0,
                        b: f64::from(rgb[2]) / 255.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    queue.submit([enc.finish()]);
}

/// The air row's screen rect, derived from the **same** layout arithmetic
/// `hud.rs::sprite_vitals` uses rather than eyeballed from a screenshot: the row
/// is anchored to the HUD's right edge and sits one icon-height plus a 1px gap
/// above the health/hunger line.
///
/// Returned generously (a couple of pixels of slack on each side) because the
/// point is to exclude *other* HUD elements, not to pin sub-pixel placement —
/// `air_bubbles.rs`'s own tests already pin `bubble_position`.
fn air_row_rect(width: u32, height: u32, scale: f32) -> (u32, u32, u32, u32) {
    // `sprite_vitals` lays out in logical (scaled) space, so convert once.
    let lw = width as f32 / scale;
    let lh = height as f32 / scale;
    // Mirrors `hud.rs`: the vitals block is anchored to the hotbar's right edge,
    // itself centred on the canvas.
    let hw = 182.0;
    let hx = (lw - hw) / 2.0;
    // **Now one call, not a re-derivation.** `sprite_vitals` used to stack upward
    // from a `cluster_top` that moved with the hotbar and the XP bar, so this
    // fixture had to reproduce that stack — and getting it wrong is how the first
    // version of this gate reported 0 px for a row that was drawing perfectly,
    // ~20 logical pixels below the rect being measured.
    //
    // `hud::vitals_line_base` is now the single expression the draw itself calls
    // (vanilla's `Hud.extractPlayerHealth`'s `yLineBase == guiHeight - 39`, which
    // takes no branch at all), so this rect cannot desync from it — including if
    // the hotbar or an XP bar is enabled in this fixture later, which is exactly
    // what used to break it.
    //
    // The air row is one [`VITALS_ROW_PITCH`] above that. **Not two**: the second
    // `yLineAir -= 10` in `extractPlayerHealth` is cancelled by
    // `getAirBubbleYLine`'s `rowOffset == -1` for an unmounted player — see the
    // table in `sprite_vitals`' air block, which was written after this gate's
    // arithmetic was checked against it.
    let row_y = lodestone::hud::vitals_line_base(lh);
    let air_y = row_y - 10.0;
    let row_w = BUBBLE_COUNT as f32 * (BUBBLE_SIZE + 1.0);
    let x0 = ((hx + hw - row_w - 2.0) * scale).max(0.0) as u32;
    let y0 = ((air_y - 2.0) * scale).max(0.0) as u32;
    let x1 = (((hx + hw) + 2.0) * scale).min(width as f32) as u32;
    let y1 = (((air_y + BUBBLE_SIZE) + 2.0) * scale).min(height as f32) as u32;
    (x0, y0, x1, y1)
}

/// Bounding box of every painted pixel in the whole frame, so a rect mismatch is
/// diagnosable instead of mysterious.
///
/// The first run of this gate reported 0 px in all three cases and looked like a
/// dead wiring chain; the row was in fact drawing ~20 logical pixels lower than
/// the rect being measured. `CLAUDE.md`'s rule applies to gates as much as to
/// bugs: ask *where*, not just *how much*.
fn painted_bbox(pixels: &[u8], w: u32, backdrop: [u8; 3]) -> String {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut n = 0usize;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let d = (i32::from(px[0]) - i32::from(backdrop[0])).abs()
            + (i32::from(px[1]) - i32::from(backdrop[1])).abs()
            + (i32::from(px[2]) - i32::from(backdrop[2])).abs();
        if d > 12 {
            let (x, y) = (i as u32 % w, i as u32 / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            n += 1;
        }
    }
    if n == 0 {
        return "nothing painted anywhere in frame".to_string();
    }
    format!("{n} px painted, whole-frame bbox x{x0}..{x1} y{y0}..{y1}")
}

/// Count pixels inside `rect` that differ from the backdrop.
fn painted_in_rect(pixels: &[u8], w: u32, rect: (u32, u32, u32, u32), backdrop: [u8; 3]) -> usize {
    let (x0, y0, x1, y1) = rect;
    let mut n = 0usize;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        if x < x0 || x >= x1 || y < y0 || y >= y1 {
            continue;
        }
        let d = (i32::from(px[0]) - i32::from(backdrop[0])).abs()
            + (i32::from(px[1]) - i32::from(backdrop[1])).abs()
            + (i32::from(px[2]) - i32::from(backdrop[2])).abs();
        if d > 12 {
            n += 1;
        }
    }
    n
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_air_bubble_row_reaches_the_screen_through_the_real_hud_path() {
    // `load_gui_atlas` is the *production* loader — the same call `app.rs` makes
    // — rather than a test-only manager. That matters here: this gate exists to
    // prove the shell's real path reaches pixels, so building the atlas a
    // different way than the shell does would weaken exactly the claim being
    // made.
    let atlas = lodestone::resources::load_gui_atlas().expect(
        "GPU gate opted in via --ignored but the vanilla GUI atlas did not load; set \
         LODESTONE_ASSETS to a pack root containing client.jar, or populate \
         .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
    );

    // The row is unreachable if the sprites are not in the atlas at all, and
    // that failure would otherwise read as "the wiring does not work". Assert
    // the precondition separately so the two cannot be confused.
    for id in ["hud/air", "hud/air_empty", "hud/air_bursting"] {
        assert!(
            atlas.contains(id),
            "{id} must be present in the GUI atlas; `GuiAtlas` globs gui/sprites/** so \
             this is a pack/jar problem, not a wiring problem"
        );
    }

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let backdrop = [0u8, 0, 0];
    let scale = lodestone::config::calculate_gui_scale(lodestone::config::AUTO_GUI_SCALE, W, H);
    let rect = air_row_rect(W, H, scale as f32);

    let stats = DebugStats::default();
    // Everything that could paint is off, so the air row is the only thing in
    // this fixture that draws at all. Built per-call rather than cloned from one
    // template: `HudFrame` owns non-`Copy` text fields, and the three frames must
    // be identical in every respect *except* `air` for the controls to isolate it.
    let frame_with = |air: Option<(i32, i32, bool)>| HudFrame {
        show_debug: false,
        crosshair: false,
        hotbar: None,
        hotbar_items: None,
        air,
        ..HudFrame::new(&stats)
    };

    let mut hud = HudRenderer::new(device, format);
    hud.attach_gui(device, queue, format, Arc::clone(&atlas));

    let mut shoot = |frame: &HudFrame| -> Vec<u8> {
        let acquired = target.acquire().expect("headless acquire");
        clear_view(device, queue, acquired.view(), backdrop);
        hud.render(device, queue, acquired.view(), acquired.view(), frame, W, H);
        target.read_texels(device, queue)
    };

    // Subject: mid-drown, underwater.
    let under_frame = shoot(&frame_with(Some((HALF_AIR, MAX_AIR, true))));
    let under_where = painted_bbox(&under_frame, W, backdrop);
    let under_px = painted_in_rect(&under_frame, W, rect, backdrop);

    // Control 1, EXECUTED: full air, out of water — both of vanilla's disjuncts
    // false, so nothing draws.
    let full_dry_px = painted_in_rect(
        &shoot(&frame_with(Some((MAX_AIR, MAX_AIR, false)))),
        W,
        rect,
        backdrop,
    );

    // Control 2, EXECUTED: full air, underwater — only `isUnderWater` is true.
    // This is the one that proves `eye_in_water` reaches the draw at all.
    let full_wet_px = painted_in_rect(
        &shoot(&frame_with(Some((MAX_AIR, MAX_AIR, true)))),
        W,
        rect,
        backdrop,
    );

    // Control 2, EXECUTED: no air data at all — the pre-wiring behaviour.
    let none_px = painted_in_rect(&shoot(&frame_with(None)), W, rect, backdrop);

    eprintln!("=== air bubble row pixel gate (through HudRenderer::render) ===");
    eprintln!("gui scale {scale}, air row rect {rect:?}");
    eprintln!("underwater, air {HALF_AIR}/{MAX_AIR}: {under_px} px in rect ({under_where})");
    eprintln!("control, full air + dry:  {full_dry_px} px (both disjuncts false)");
    eprintln!("control, full air + wet:  {full_wet_px} px (isUnderWater only)");
    eprintln!("control, frame.air = None: {none_px} px");

    // A single bubble is 9x9 with transparent corners, so even one drawn bubble
    // clears this comfortably while a stray antialiased edge would not.
    assert!(
        under_px > 200,
        "expected the underwater bubble row to paint inside its own rect, got {under_px} px \
         — the decode chain (metadata -> Vitals::air -> PlayerSnapshot -> HudFrame::air) or \
         `sprite_vitals`' bubble loop is not reaching the atlas"
    );
    assert_eq!(
        full_dry_px, 0,
        "control failed to fail: the row drew {full_dry_px} px on full air out of water, \
         where both of vanilla's disjuncts (`Hud.java`) are false"
    );
    assert!(
        full_wet_px > 200,
        "full air underwater drew only {full_wet_px} px — `eye_in_water` is not reaching \
         the draw, so the subject's pixels are attributable to `air < max` alone and this \
         gate would pass with the underwater half of vanilla's condition missing entirely"
    );
    assert_eq!(
        none_px, 0,
        "control failed to fail: {none_px} px drew in the air row with `frame.air = None`, \
         so something other than the bubble row paints there and the subject's count is \
         not attributable to bubbles"
    );
}
