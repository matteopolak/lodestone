//! Pixel gate: the attack-strength crosshair indicator reaches
//! the screen through the **shell's real HUD path**, not just
//! `Sim::attack_strength_scale`'s pure model.
//!
//! `sim.rs`'s own unit tests (`attack_strength_scale_ramps_to_full_over_five_
//! ticks_unarmed`, `attacking_an_entity_resets_the_strength_ticker_
//! immediately`) prove the cooldown fraction is computed and reset
//! correctly — that is the model working, and it is a closed loop. It says
//! nothing about whether `HudFrame::attack_cooldown` ever reaches
//! `hud.rs`'s draw site or whether that site ever asks the atlas for the
//! indicator sprites. Every link in that chain (`Sim::attack_strength_scale`
//! -> `app.rs`'s `hud_frame.attack_cooldown` -> `HudGeometry::build_inner`'s
//! crosshair block) was added at once, so a green model test alone is
//! exactly the "individually built, reaches zero pixels" island shape
//! `CLAUDE.md` names.
//!
//! # The metric
//!
//! The indicator is a 16x4 native-pixel bar anchored at
//! `(guiWidth/2 - 8, guiHeight/2 - 7 + 16)` — vanilla's own offset
//! (`Hud.java`, `.cache/mc/26.2/client-src`) — which `hud.rs`'s
//! crosshair block computes as `(cx - 8, cy + 9)` against this canvas's own
//! centre. [`indicator_rect`] below derives from that *same* expression
//! rather than a hand-copied constant — `air_bubble_pixels.rs` in this same
//! directory documents the cost of getting that wrong (a hardcoded rect
//! silently assumed a different anchor and reported 0 px for a row that was
//! drawing perfectly, ~20 logical pixels away).
//!
//! # The controls, all executed
//!
//! **This region already has a crosshair in it** — `CLAUDE.md`'s warning
//! about a control's premise being false before the feature under test
//! existed applies directly here, more than to most HUD gates. The
//! crosshair's own vertical arm (`hud.rs`: `rect_px(cx - thick*0.5, cy -
//! arm, thick, arm*2.0, …)`, `arm = 8.0`) ends at exactly `cy + 8`, one whole
//! pixel above the indicator's `cy + 9` top edge — so [`indicator_rect`]
//! deliberately adds **no** slack on that edge (see its own comment), rather
//! than padding generously the way `air_bubble_pixels.rs` does in open space.
//!
//! Three disjuncts, each isolated:
//!
//! 1. **Partial cooldown, crosshair on** — the subject: `attack_cooldown =
//!    Some(0.5)`, `crosshair = true`. Must paint inside the rect.
//! 2. **Full cooldown (`1.0`), crosshair on** — vanilla's actual rule
//!    (`Hud.java`, `else if (attackStrengthScale < 1.0F)`) hides the
//!    indicator entirely once it reaches full strength, unless a slow
//!    weapon's "ready" icon takes over — a variant this shell does not
//!    implement (`docs/combat.md` names the cut). Getting this control wrong
//!    — asserting the indicator stays visible at `1.0` — is exactly the
//!    "wrong vanilla rule outright" mistake `CLAUDE.md` warns
//!    `air_bubble_pixels.rs`'s own history made once already.
//! 3. **`attack_cooldown = None`, crosshair on** — the pre-fix behaviour.
//! 4. **`Some(0.5)`, crosshair off** — proves the indicator is nested inside
//!    `frame.crosshair`'s own gate (that fix's lesson: two questions must
//!    not share one boolean, and here the indicator answers only one of
//!    them).
//! 5. **`Some(0.5)`, crosshair on, `AttackIndicator::Off`** — the settings
//!    row's own control. Zero px in the crosshair rect.
//! 6. **`Some(0.5)`, crosshair on, `AttackIndicator::Hotbar`** — zero px in
//!    the crosshair rect *and* non-zero in the hotbar rect, which is the
//!    only pair that distinguishes "the option moved the draw" from "the
//!    option turned the draw off". Measured separately by
//!    [`hotbar_indicator_rect`], derived from `hud.rs`'s own hotbar block the
//!    way [`indicator_rect`] is derived from its crosshair one.
//!
//! Arms 5 and 6 exist because `AttackIndicator::Hotbar` is a **new draw**,
//! not the crosshair bar re-anchored: different sprites, an 18x18 box instead
//! of 16x4, and a fill that runs bottom-up rather than left-to-right. Nothing
//! in the model layer can tell whether it reaches the atlas.
//!
//! Fail-closed like its siblings: a missing GPU or a missing `client.jar` is
//! a failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test attack_indicator_pixels -- --ignored --nocapture
//! ```

use lodestone::config::AttackIndicator;
use lodestone::hud::{DebugStats, HudFrame, HudRenderer};
use lodestone_render::{HeadlessTarget, RenderTarget};
use std::sync::Arc;

const W: u32 = 640;
const H: u32 = 480;

fn clear_view(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView, rgb: [u8; 3]) {
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("attack-indicator-gate-clear"),
    });
    {
        let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("attack-indicator-gate-clear-pass"),
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

/// The indicator's screen rect, derived from the **same** layout arithmetic
/// `hud.rs`'s crosshair block uses rather than eyeballed: native 16x4,
/// anchored at `(cx - 8, cy + 9)` where `cx`/`cy` are the logical canvas's
/// own centre — identical to how the crosshair plus itself is centred.
///
/// No slack on the top edge deliberately: the crosshair's vertical arm
/// occupies `cy - 8 .. cy + 8`, and the indicator starts at `cy + 9` — a
/// real one-pixel gap at the same float precision both draws use. Padding
/// upward here would let the crosshair's own always-on pixels leak into
/// this rect and silently inflate every count below, defeating the point of
/// a rect-scoped measurement.
fn indicator_rect(width: u32, height: u32, scale: f32) -> (u32, u32, u32, u32) {
    let lw = width as f32 / scale;
    let lh = height as f32 / scale;
    let cx = lw * 0.5;
    let cy = lh * 0.5;
    let iw = 16.0;
    let ih = 4.0;
    let ix = cx - iw * 0.5;
    let iy = cy + 9.0;
    let slack = 2.0;
    let x0 = ((ix - slack) * scale).max(0.0) as u32;
    let y0 = (iy * scale) as u32;
    let x1 = (((ix + iw) + slack) * scale).min(width as f32) as u32;
    let y1 = (((iy + ih) + slack) * scale).min(height as f32) as u32;
    (x0, y0, x1, y1)
}

/// The **hotbar** variant's screen rect, derived from the same expression
/// `hud.rs`'s hotbar block uses: an 18x18 box at
/// `(cx + 91 + 6, h - 20)` in logical pixels, which is vanilla's own
/// `(guiWidth / 2 + 91 + 6, guiHeight - 20)`.
///
/// The slack is deliberately asymmetric and small. The hotbar itself ends at
/// `cx + 91`, so the six-pixel gap vanilla leaves is the only separation there
/// is; padding leftward would pull the hotbar's own frame art into the count
/// and make every measurement below unattributable — the identical hazard
/// [`indicator_rect`]'s top edge documents for the crosshair's arm.
fn hotbar_indicator_rect(width: u32, height: u32, scale: f32) -> (u32, u32, u32, u32) {
    let lw = width as f32 / scale;
    let lh = height as f32 / scale;
    let ix = lw * 0.5 + 91.0 + 6.0;
    let iy = lh - 20.0;
    let size = 18.0;
    let x0 = (ix * scale).max(0.0) as u32;
    let y0 = (iy * scale).max(0.0) as u32;
    let x1 = ((ix + size) * scale).min(width as f32) as u32;
    let y1 = ((iy + size) * scale).min(height as f32) as u32;
    (x0, y0, x1, y1)
}

/// Bounding box of every painted pixel in the whole frame, so a rect
/// mismatch is diagnosable instead of mysterious (`air_bubble_pixels.rs`'s
/// own history: a first draft reported 0 px everywhere and looked like a
/// dead wiring chain, when the row was in fact drawing ~20 logical pixels
/// away from the rect being measured).
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
fn the_attack_indicator_reaches_the_screen_through_the_real_hud_path() {
    // `load_gui_atlas` is the *production* loader — the same call `app.rs`
    // makes — rather than a test-only manager, for the same reason
    // `air_bubble_pixels.rs` insists on it: this gate exists to prove the
    // shell's real path reaches pixels.
    let atlas = lodestone::resources::load_gui_atlas().expect(
        "GPU gate opted in via --ignored but the vanilla GUI atlas did not load; set \
         LODESTONE_ASSETS to a pack root containing client.jar, or populate \
         .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
    );

    // The indicator is unreachable if the sprites are not in the atlas at
    // all, and that failure would otherwise read as "the wiring does not
    // work". Assert the precondition separately so the two cannot be
    // confused — the earlier air-bubble/hotbar work already established
    // `GuiAtlas` globs `gui/sprites/**`, so these should already be stitched.
    for id in [
        "hud/crosshair_attack_indicator_background",
        "hud/crosshair_attack_indicator_progress",
        "hud/hotbar_attack_indicator_background",
        "hud/hotbar_attack_indicator_progress",
    ] {
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
    let rect = indicator_rect(W, H, scale as f32);

    let stats = DebugStats::default();
    // Everything else that could paint is off (no debug text, no hotbar/
    // vitals, no chat), so the indicator — when it draws at all — is the
    // only thing in this fixture that paints inside `rect`. Built per-call
    // rather than cloned from one template, matching `air_bubble_pixels.rs`:
    // `HudFrame` owns non-`Copy` text fields, and the four frames must be
    // identical in every respect except `crosshair`/`attack_cooldown` for
    // the controls to isolate them.
    let frame_with = |crosshair: bool, attack_cooldown: Option<f32>| HudFrame {
        show_debug: false,
        crosshair,
        hotbar: None,
        hotbar_items: None,
        attack_cooldown,
        ..HudFrame::new(&stats)
    };
    // The hotbar variant needs `hotbar` set, because `hud.rs` draws its gauge
    // inside the block that draws the hotbar — vanilla's own nesting. That
    // means this frame paints the hotbar too, which is exactly why the hotbar
    // rect below is measured **against a matching hotbar-on control** rather
    // than against the crosshair-only frames above: the hotbar's own art would
    // otherwise be counted as the indicator.
    let hotbar_frame_with = |indicator: AttackIndicator, attack_cooldown: Option<f32>| HudFrame {
        show_debug: false,
        crosshair: true,
        hotbar: Some(0),
        hotbar_items: None,
        attack_cooldown,
        attack_indicator: indicator,
        ..HudFrame::new(&stats)
    };
    let indicator_frame_with = |indicator: AttackIndicator| HudFrame {
        show_debug: false,
        crosshair: true,
        hotbar: None,
        hotbar_items: None,
        attack_cooldown: Some(0.5),
        attack_indicator: indicator,
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

    // Subject: half-charged cooldown, crosshair on.
    let subject_frame = shoot(&frame_with(true, Some(0.5)));
    let subject_where = painted_bbox(&subject_frame, W, backdrop);
    let subject_px = painted_in_rect(&subject_frame, W, rect, backdrop);

    // Control 1, EXECUTED: full charge (`1.0`) — vanilla's real rule hides
    // the indicator entirely here (no "ready"-icon variant implemented).
    let full_px = painted_in_rect(&shoot(&frame_with(true, Some(1.0))), W, rect, backdrop);

    // Control 2, EXECUTED: no cooldown data at all — the pre-fix behaviour.
    let none_px = painted_in_rect(&shoot(&frame_with(true, None)), W, rect, backdrop);

    // Control 3, EXECUTED: half-charged cooldown, crosshair OFF — proves the
    // indicator is nested inside `frame.crosshair`'s own gate rather than
    // answering a question of its own.
    let hidden_px = painted_in_rect(&shoot(&frame_with(false, Some(0.5))), W, rect, backdrop);

    // Control 4, EXECUTED: the settings row set to OFF. Same frame as the
    // subject in every other respect.
    let off_px = painted_in_rect(
        &shoot(&indicator_frame_with(AttackIndicator::Off)),
        W,
        rect,
        backdrop,
    );

    // Control 5, EXECUTED: the settings row set to HOTBAR. The crosshair rect
    // must go empty — proving the option *moved* the draw rather than merely
    // being read.
    let moved_px = painted_in_rect(
        &shoot(&indicator_frame_with(AttackIndicator::Hotbar)),
        W,
        rect,
        backdrop,
    );

    // And the other half of that pair, in the hotbar rect. Both arms carry a
    // hotbar, so the difference between them is the gauge and nothing else:
    // measuring HOTBAR against a crosshair-only frame would credit the hotbar's
    // own art to the indicator.
    let hotbar_rect = hotbar_indicator_rect(W, H, scale as f32);
    let gauge_frame = shoot(&hotbar_frame_with(AttackIndicator::Hotbar, Some(0.5)));
    let gauge_px = painted_in_rect(&gauge_frame, W, hotbar_rect, backdrop);
    let gauge_where = painted_bbox(&gauge_frame, W, backdrop);
    let gauge_control_px = painted_in_rect(
        &shoot(&hotbar_frame_with(AttackIndicator::Crosshair, Some(0.5))),
        W,
        hotbar_rect,
        backdrop,
    );

    eprintln!("=== attack indicator pixel gate (through HudRenderer::render) ===");
    eprintln!("gui scale {scale}, indicator rect {rect:?}");
    eprintln!("subject, cooldown 0.5, crosshair on: {subject_px} px in rect ({subject_where})");
    eprintln!("control, cooldown 1.0, crosshair on: {full_px} px (vanilla hides at full charge)");
    eprintln!("control, cooldown None, crosshair on: {none_px} px");
    eprintln!("control, cooldown 0.5, crosshair off: {hidden_px} px (nested-gate check)");

    // The bar is 16x4 native pixels; even at the smallest supported GUI
    // scale that clears a stray antialiased-edge false positive comfortably.
    assert!(
        subject_px > 20,
        "expected the half-charged indicator to paint inside its own rect, got {subject_px} px \
         — the chain (Sim::attack_strength_scale -> HudFrame::attack_cooldown -> hud.rs's \
         crosshair block) is not reaching the atlas"
    );
    assert_eq!(
        full_px, 0,
        "control failed to fail: the indicator drew {full_px} px at full charge (1.0), where \
         vanilla's own rule (`Hud.java`) hides it entirely absent the unimplemented \
         'ready' icon variant"
    );
    assert_eq!(
        none_px, 0,
        "control failed to fail: {none_px} px drew with attack_cooldown = None, so something \
         other than the indicator paints in this rect and the subject's count is not \
         attributable to it"
    );
    assert_eq!(
        hidden_px, 0,
        "control failed to fail: {hidden_px} px drew with crosshair = false, so the indicator \
         is not actually nested inside the crosshair's own visibility gate"
    );

    eprintln!("--- options.attackIndicator ---");
    eprintln!("control, AttackIndicator::Off:     {off_px} px in the crosshair rect");
    eprintln!("control, AttackIndicator::Hotbar:  {moved_px} px in the crosshair rect");
    eprintln!("hotbar rect {hotbar_rect:?}");
    eprintln!("subject, Hotbar + hotbar on:       {gauge_px} px in the hotbar rect ({gauge_where})");
    eprintln!("control, Crosshair + hotbar on:    {gauge_control_px} px in the hotbar rect");

    assert_eq!(
        off_px, 0,
        "control failed to fail: {off_px} px drew with AttackIndicator::Off, so the settings \
         row does not reach the crosshair draw site at all"
    );
    assert_eq!(
        moved_px, 0,
        "control failed to fail: {moved_px} px drew in the CROSSHAIR rect with \
         AttackIndicator::Hotbar — the two placements are mutually exclusive in vanilla and \
         each draw site must test for its own variant, not for 'not off'"
    );
    // The gauge is 18x18 native with a partial fill; the background sprite alone
    // clears this comfortably at any supported GUI scale.
    assert!(
        gauge_px > 20,
        "expected AttackIndicator::Hotbar to paint its gauge beside the hotbar, got \
         {gauge_px} px — the option reaches `HudFrame` but the hotbar draw site is an \
         island (whole-frame bbox: {gauge_where})"
    );
    assert_eq!(
        gauge_control_px, 0,
        "control failed to fail: {gauge_control_px} px drew in the hotbar-gauge rect with \
         AttackIndicator::Crosshair. Either the hotbar's own art reaches into this rect — in \
         which case the subject's count above is not attributable to the gauge — or the \
         gauge ignores the option"
    );
}
