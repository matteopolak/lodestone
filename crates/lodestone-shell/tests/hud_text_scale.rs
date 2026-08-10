//! Pins the **pose scale** of the three server-driven HUD text surfaces —
//! title, subtitle and action bar (overlay message) — against vanilla 26.2.
//!
//! # Why this gate exists
//!
//! A player reported all three drawing "too big". They are: each is exactly
//! **2× vanilla**, because `HudGeometry::build_inner` multiplies vanilla's own
//! pose factor by a hardcoded `let scale = 2.0;` that predates the logical
//! canvas. `logical_canvas` (added later, for the "HUD draws half-size on
//! Retina" fix) already divides the framebuffer down by the GUI scale, so the
//! canvas these constants are laid into is *already* vanilla's GUI pixel unit
//! — and the legacy 2× is applied on top of it. That is a double-apply.
//!
//! Vanilla, all in `.cache/mc/26.2/client-src/net/minecraft/client/gui/Hud.java`:
//!
//! | surface | vanilla pose scale | cite |
//! |---|---|---|
//! | title | `scale(4.0F, 4.0F)` | `Hud.java:378` |
//! | subtitle | `scale(2.0F, 2.0F)` | `Hud.java:385` |
//! | action bar / overlay message | **no scale call at all** → 1.0 | `Hud.java:327-355` |
//! | held-item name (this gate's reference) | **no scale call at all** → 1.0 | `Hud.java:626-645` |
//!
//! # Why the assertions are ratios, not absolute pixel heights
//!
//! An absolute height would have to restate a font metric, and this repo's
//! `CLAUDE.md` requires a gate to derive its expectation from the same
//! expression the draw uses rather than from a copied constant. So every
//! assertion here is measured **against the held-item name**, which is the one
//! text surface in `hud.rs` already known to be vanilla-correct at scale 1.0:
//! it was fixed to exactly that under issue #126, and its draw site carries a
//! comment warning the next reader *not* to reintroduce `scale` there. Vanilla
//! draws the held-item name and the action bar through the identical unscaled
//! path, so their rendered heights must be **equal**, and the title must be
//! exactly 4× and the subtitle exactly 2× that same reference.
//!
//! This also makes the gate immune to a font change: swapping the fixed 5×7
//! debug font for the real `ascii.png` raster moves every measurement here by
//! the same factor and leaves all three ratios untouched.
//!
//! # Both hypotheses
//!
//! Each assertion states the vanilla-correct ratio *and* the double-applied
//! one, and requires the measurement to land on the first while sitting a wide
//! margin away from the second. They differ by exactly 2×, so no font-padding
//! slop can produce a false pass in either direction:
//!
//! | surface | correct | double-applied (today) |
//! |---|---|---|
//! | title / reference | **4.0** | 8.0 |
//! | subtitle / reference | **2.0** | 4.0 |
//! | action bar / reference | **1.0** | 2.0 |
//!
//! This file is expected to FAIL until `hud.rs`'s three draw sites stop
//! multiplying by `scale`. It is the handover artefact for that patch.

// The crate is `lodestone-shell` but its `[lib] name` is `lodestone`
// (`Cargo.toml:10-12`) — the same import every other test in this directory
// uses, e.g. `held_item_name_pixels.rs:46`.
use lodestone::hud::{DebugStats, HudFrame, HudGeometry};
use lodestone::menu::render::logical_canvas;
use lodestone::overlay::plain_spans;

/// A framebuffer big enough that none of the three surfaces lands off-canvas
/// even at today's doubled sizes (which would truncate a bounding box and
/// could fake a pass).
const FB_W: u32 = 1280;
const FB_H: u32 = 720;

/// `hud.rs`'s `FLOATS_PER_VERTEX` — position `(x, y)` in NDC plus RGBA. The
/// constant itself is private to the crate; `ColourStream::rect`
/// (`hud/item_icon.rs:677-691`) carries a `debug_assert_eq!(FLOATS_PER_VERTEX, 6)`
/// pinning this layout, and `vertex_layout_is_still_six_floats` below fails if
/// the stride ever stops dividing the buffer.
const STRIDE: usize = 6;

/// `HudGeometry::build` renders at `AUTO_GUI_SCALE`, so the logical canvas the
/// draw lays its pixel constants into is `logical_canvas(0, ..)` — the same
/// call `build_inner` makes, not a restatement of it.
fn canvas() -> (f32, f32) {
    logical_canvas(0, FB_W, FB_H)
}

/// The ink bounding box of a frame's colour geometry, in **logical canvas
/// pixels**, inverting `ColourStream::rect`'s NDC mapping
/// (`hud/item_icon.rs:679`: `x_ndc = 2*px/w - 1`, `y_ndc = 1 - 2*py/h`).
///
/// Returns `None` for a frame that paints nothing.
fn ink_bbox(frame: &HudFrame<'_>) -> Option<(f32, f32, f32, f32)> {
    let (w, h) = canvas();
    let geo = HudGeometry::build(frame, FB_W, FB_H);
    assert_eq!(
        geo.verts.len() % STRIDE,
        0,
        "colour vertex buffer is not a whole number of {STRIDE}-float vertices \
         ({} floats) — the layout this gate inverts has changed",
        geo.verts.len()
    );
    if geo.verts.is_empty() {
        return None;
    }
    let (mut x0, mut x1) = (f32::MAX, f32::MIN);
    let (mut y0, mut y1) = (f32::MAX, f32::MIN);
    for v in geo.verts.chunks_exact(STRIDE) {
        let px = (v[0] + 1.0) * 0.5 * w;
        let py = (1.0 - v[1]) * 0.5 * h;
        x0 = x0.min(px);
        x1 = x1.max(px);
        y0 = y0.min(py);
        y1 = y1.max(py);
    }
    Some((x0, x1, y0, y1))
}

/// Ink **height and top edge** in logical pixels. Height is what a pose scale
/// multiplies; the top edge is what vanilla's pose translate fixes. The fixed
/// debug font starts a glyph's ink exactly at the `y` handed to the draw (the
/// reference surface measures `y0 == b.h - 59.0` to the digit, matching
/// `Hud.java:634`), so the top edge is directly comparable to vanilla's own
/// expression with no baseline correction.
fn ink_top_and_height(label: &str, frame: &HudFrame<'_>) -> (f32, f32) {
    let bbox = ink_bbox(frame)
        .unwrap_or_else(|| panic!("{label} painted no geometry at all — this gate cannot measure a surface that does not draw"));
    let (x0, x1, y0, y1) = bbox;
    eprintln!(
        "{label:>12}: bbox x[{x0:.2}..{x1:.2}] y[{y0:.2}..{y1:.2}]  top={y0:.2} h={:.2}",
        y1 - y0
    );
    (y0, y1 - y0)
}

/// Ink **height** in logical pixels, which is what a pose scale multiplies.
fn ink_height(label: &str, frame: &HudFrame<'_>) -> f32 {
    let bbox = ink_bbox(frame)
        .unwrap_or_else(|| panic!("{label} painted no geometry at all — this gate cannot measure a surface that does not draw"));
    let (x0, x1, y0, y1) = bbox;
    eprintln!("{label:>12}: bbox x[{x0:.2}..{x1:.2}] y[{y0:.2}..{y1:.2}]  h={:.2}", y1 - y0);
    y1 - y0
}

/// A frame with the two always-on surfaces switched off, so the only geometry
/// in the buffer belongs to whichever text surface a test sets. `HudFrame::new`
/// defaults `show_debug` and `crosshair` to `true` (`hud.rs:484-485`).
fn quiet(stats: &DebugStats) -> HudFrame<'_> {
    let mut f = HudFrame::new(stats);
    f.show_debug = false;
    f.crosshair = false;
    f
}

/// A single glyph whose ink spans all rows of the fixed debug font's cell, so
/// its bounding box measures the full glyph box rather than a partial one.
/// `T` is a full top bar plus a centre stem: every row carries ink.
const GLYPH: &str = "T";

/// **The control this whole file rests on.** Every measurement below assumes
/// the only ink in the buffer is the surface under test — so a quiet frame must
/// paint *nothing*. If anything else draws unconditionally, every ratio here is
/// measuring that instead, and the gate would be the "premise false before the
/// feature existed" species of vacuous test that `CLAUDE.md` documents.
#[test]
fn a_quiet_hud_frame_paints_nothing() {
    let stats = DebugStats::default();
    let bbox = ink_bbox(&quiet(&stats));
    assert!(
        bbox.is_none(),
        "a frame with no debug overlay, no crosshair and no text must paint \
         nothing, but geometry appeared at {bbox:?} — every ratio in this file \
         is measuring that geometry rather than the text under test"
    );
}

/// The second control: the ratios for the subtitle are taken with a
/// **blank title**, because title and subtitle share one `Option` and one draw
/// block. That only isolates the subtitle if a space really is inkless.
#[test]
fn a_blank_title_paints_nothing() {
    let stats = DebugStats::default();
    let mut f = quiet(&stats);
    f.title = Some((plain_spans(" "), None, 1.0));
    let bbox = ink_bbox(&f);
    assert!(
        bbox.is_none(),
        "a single-space title must paint no ink, but geometry appeared at \
         {bbox:?} — the subtitle isolation trick below is invalid"
    );
}

/// Guards the stride this file inverts, independently of any scale question.
#[test]
fn vertex_layout_is_still_six_floats() {
    let stats = DebugStats::default();
    let mut f = quiet(&stats);
    f.held_item = Some((GLYPH.into(), 1.0));
    let geo = HudGeometry::build(&f, FB_W, FB_H);
    assert!(!geo.verts.is_empty(), "the reference surface must paint");
    assert_eq!(geo.verts.len() % STRIDE, 0);
}

#[test]
fn title_subtitle_and_action_bar_match_vanillas_pose_scales() {
    let stats = DebugStats::default();

    // The reference: the held-item name, drawn at scale 1.0 exactly as vanilla
    // draws it (`Hud.java:626-645`, no pose scale). Everything else is measured
    // as a multiple of this.
    let reference = {
        let mut f = quiet(&stats);
        f.held_item = Some((GLYPH.into(), 1.0));
        ink_height("reference", &f)
    };
    assert!(
        reference > 0.0,
        "the scale-1.0 reference must have a positive height"
    );

    let title = {
        let mut f = quiet(&stats);
        f.title = Some((plain_spans(GLYPH), None, 1.0));
        ink_height("title", &f)
    };

    let subtitle = {
        let mut f = quiet(&stats);
        // Blank title so the only ink is the subtitle — see
        // `a_blank_title_paints_nothing`.
        f.title = Some((plain_spans(" "), Some(plain_spans(GLYPH)), 1.0));
        ink_height("subtitle", &f)
    };

    let action_bar = {
        let mut f = quiet(&stats);
        f.action_bar = Some((plain_spans(GLYPH), 1.0));
        ink_height("action_bar", &f)
    };

    let (cw, ch) = canvas();
    eprintln!("=== HUD text pose-scale gate ===");
    eprintln!("framebuffer {FB_W}x{FB_H} -> logical canvas {cw:.2}x{ch:.2}");
    eprintln!(
        "ratios vs reference: title={:.3} subtitle={:.3} action_bar={:.3}",
        title / reference,
        subtitle / reference,
        action_bar / reference
    );

    // `(surface, measured, vanilla-correct ratio, double-applied ratio, cite)`
    let cases = [
        ("title", title, 4.0_f32, 8.0_f32, "Hud.java:378 scale(4.0F, 4.0F)"),
        ("subtitle", subtitle, 2.0, 4.0, "Hud.java:385 scale(2.0F, 2.0F)"),
        (
            "action bar",
            action_bar,
            1.0,
            2.0,
            "Hud.java:327-355, no pose scale",
        ),
    ];

    for (name, measured, correct, doubled, cite) in cases {
        let ratio = measured / reference;
        let want = correct * reference;
        let wrong = doubled * reference;

        // The predicted value, not a direction. Tolerance is a quarter of the
        // reference glyph box — far tighter than the 1× gap between the two
        // hypotheses, so it cannot straddle them.
        assert!(
            (measured - want).abs() < reference * 0.25,
            "{name} must render at {correct}x the unscaled reference \
             ({cite}): predicted {want:.2} logical px, measured {measured:.2} \
             (ratio {ratio:.3}, reference {reference:.2})"
        );

        // And explicitly not the double-applied hypothesis, so this fails
        // loudly rather than drifting if `scale` is only partly removed.
        assert!(
            (measured - wrong).abs() > reference * 0.5,
            "{name} is rendering at {doubled}x the unscaled reference — the \
             hardcoded `let scale = 2.0;` in `HudGeometry::build_inner` is \
             still multiplying vanilla's own pose factor ({cite}). Measured \
             {measured:.2} logical px, double-applied hypothesis predicts \
             {wrong:.2}, vanilla predicts {want:.2}"
        );
    }
}

/// Vanilla's title block is positioned entirely by one pose translate to the
/// **screen centre** (`Hud.java:376`), after which the title is drawn at
/// `y = -10` inside `scale(4.0)` (`Hud.java:378,381`) and the subtitle at
/// `y = 5` inside `scale(2.0)` (`Hud.java:385,387`). Multiplying through, the
/// two top edges are fixed offsets from the vertical centre:
///
/// * title    `h/2 + (-10 * 4.0)` = `h/2 - 40`
/// * subtitle `h/2 + (  5 * 2.0)` = `h/2 + 10`
///
/// Our draw instead anchors the title at `b.h * 0.40` and stacks the subtitle a
/// scale-dependent `ts * 9.0` below it, so once the pose scale is corrected the
/// subtitle *moves* — which is why the fix has to replace the whole block's
/// geometry rather than only the two scale factors. This gate pins the result.
///
/// Kept separate from the scale gate above so a failure names which axis is
/// wrong: a pure size regression fails that test, a pure position regression
/// fails this one.
#[test]
fn the_title_block_sits_where_vanillas_pose_translate_puts_it() {
    let stats = DebugStats::default();
    let (_, ch) = canvas();
    let cy = ch * 0.5;

    let (title_top, _) = {
        let mut f = quiet(&stats);
        f.title = Some((plain_spans(GLYPH), None, 1.0));
        ink_top_and_height("title", &f)
    };
    let (subtitle_top, _) = {
        let mut f = quiet(&stats);
        f.title = Some((plain_spans(" "), Some(plain_spans(GLYPH)), 1.0));
        ink_top_and_height("subtitle", &f)
    };

    // Vanilla's offsets, multiplied out from the pose the draw uses rather than
    // restated as bare pixel numbers.
    let want_title = cy - 10.0 * 4.0;
    let want_subtitle = cy + 5.0 * 2.0;

    eprintln!("=== HUD title block anchor gate ===");
    eprintln!(
        "canvas centre y = {cy:.2}; title want {want_title:.2} got {title_top:.2}; \
         subtitle want {want_subtitle:.2} got {subtitle_top:.2}"
    );

    assert!(
        (title_top - want_title).abs() < 1.0,
        "the title's top edge must be `h/2 - 10*4` (`Hud.java:376,378,381`): \
         predicted {want_title:.2} logical px, measured {title_top:.2}"
    );
    assert!(
        (subtitle_top - want_subtitle).abs() < 1.0,
        "the subtitle's top edge must be `h/2 + 5*2` (`Hud.java:376,385,387`): \
         predicted {want_subtitle:.2} logical px, measured {subtitle_top:.2}"
    );
    assert!(
        subtitle_top > title_top,
        "the subtitle must sit below the title, got title={title_top:.2} \
         subtitle={subtitle_top:.2}"
    );
}
