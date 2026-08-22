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
//! | title | `scale(4.0F, 4.0F)` | `Hud.java` |
//! | subtitle | `scale(2.0F, 2.0F)` | `Hud.java` |
//! | action bar / overlay message | **no scale call at all** → 1.0 | `Hud.java` |
//! | held-item name (this gate's reference) | **no scale call at all** → 1.0 | `Hud.java` |
//!
//! # Why the assertions are ratios, not absolute pixel heights
//!
//! An absolute height would have to restate a font metric, and this repo's
//! `CLAUDE.md` requires a gate to derive its expectation from the same
//! expression the draw uses rather than from a copied constant. So every
//! assertion here is measured **against the held-item name**, which is the one
//! text surface in `hud.rs` already known to be vanilla-correct at scale 1.0:
//! it was fixed to exactly that under that fix, and its draw site carries a
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
// (see the `[lib]` section of `Cargo.toml`) — the same `use lodestone::...`
// import every other test in this directory uses.
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
/// carries a `debug_assert_eq!(FLOATS_PER_VERTEX, 6)`
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
/// (`x_ndc = 2*px/w - 1`, `y_ndc = 1 - 2*py/h`).
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
/// `Hud.java`), so the top edge is directly comparable to vanilla's own
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
/// defaults `show_debug` and `crosshair` to `true`.
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
    // draws it (`Hud.java`, no pose scale). Everything else is measured
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
        ("title", title, 4.0_f32, 8.0_f32, "Hud.java scale(4.0F, 4.0F)"),
        ("subtitle", subtitle, 2.0, 4.0, "Hud.java scale(2.0F, 2.0F)"),
        (
            "action bar",
            action_bar,
            1.0,
            2.0,
            "Hud.java, no pose scale",
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
/// **screen centre** (`Hud.java`), after which the title is drawn at
/// `y = -10` inside `scale(4.0)` (`Hud.java`) and the subtitle at
/// `y = 5` inside `scale(2.0)` (`Hud.java`). Multiplying through, the
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
        "the title's top edge must be `h/2 - 10*4` (`Hud.java`): \
         predicted {want_title:.2} logical px, measured {title_top:.2}"
    );
    assert!(
        (subtitle_top - want_subtitle).abs() < 1.0,
        "the subtitle's top edge must be `h/2 + 5*2` (`Hud.java`): \
         predicted {want_subtitle:.2} logical px, measured {subtitle_top:.2}"
    );
    assert!(
        subtitle_top > title_top,
        "the subtitle must sit below the title, got title={title_top:.2} \
         subtitle={subtitle_top:.2}"
    );
}

// ---------------------------------------------------------------------------
// Chat: the last consumer of the ambient `HUD_TEXT_SCALE` pitch this file's
// header describes.
//
// # Why this section exists
//
// `chat_pose_scale` (`crates/lodestone-shell/src/hud.rs`) used to be
// `HUD_TEXT_SCALE * opts.scale`, i.e. `2.0` at the vanilla-legal default
// `chatScale == 1.0` — the same double-apply the title/subtitle/action bar
// above already had fixed. Vanilla's own chat pose scale
// (`ChatComponent.getScale`, `.cache/mc/26.2/client-src/net/minecraft/client/
// gui/components/ChatComponent.java`) is `chatScale` **alone**:
// `extractRenderState`'s `pose.scale(scale, scale)` where `scale =
// (float)this.getScale()`. `chat_pose_scale` is now `opts.scale.max(0.0)`,
// and `HUD_TEXT_SCALE`/`hud_line_h` — no longer having any caller — are
// deleted outright.
//
// Three vanilla clauses, verified independently rather than assumed to share
// one derivation (`CLAUDE.md`'s "enumerate the clauses" rule — the first two
// look right in a screenshot on their own; only a wrapping message exposes a
// wrong width):
//
// | clause | vanilla | ours |
// |---|---|---|
// | line pitch | `messageHeight == 9`, scaled by `chatScale` (`ChatComponent.extractRenderState`'s `entryHeight` inside `pose.scale`) | `chat_line_h` |
// | panel width | `getWidth(pct) = floor(pct * 280 + 40)`, **not** scaled by `chatScale` (computed outside the pose transform, in real screen pixels) | `chat_width_px` |
// | panel height | `getHeight(pct) = floor(pct * 160 + 20)`, likewise unscaled | `chat_height_px` |
//
// The width/height clauses were already correct before this fix (they never
// read `chat_pose_scale` at all), which is exactly why the panel's *edges*
// never looked wrong in a screenshot while its *text* was 2x too big — the
// magnitude species of vacuous test would have missed that split entirely.
mod chat_scale {
    use super::{canvas, ink_height, quiet, FB_H, FB_W, STRIDE};
    use lodestone::chat::Candidate;
    use lodestone::hud::{
        chat_pose_scale, suggestion_layout, ChatDisplayOptions, DebugStats, HudFrame, HudGeometry,
        SuggestionPopup,
    };

    /// Every vertex in `verts` whose colour matches `color` within `eps`,
    /// divided by 6 (`ColourStream::rect`'s own quad size) — the number of
    /// *quads* painted that colour. Row backgrounds are the only geometry in
    /// a quiet chat frame painted flat black at a fractional alpha (glyph ink
    /// is `[0.92, 0.94, 1.0]`, never black), so this counts scrollback rows
    /// without needing to know their y positions ahead of time.
    fn quads_with_color(verts: &[f32], color: [f32; 4], eps: f32) -> usize {
        let hits = verts
            .chunks(STRIDE)
            .filter(|v| {
                (v[2] - color[0]).abs() < eps
                    && (v[3] - color[1]).abs() < eps
                    && (v[4] - color[2]).abs() < eps
                    && (v[5] - color[3]).abs() < eps
            })
            .count();
        assert_eq!(hits % 6, 0, "a partial quad matched — the colour is not quad-exclusive");
        hits / 6
    }

    /// **Clause 1: line pitch.** Predicts vanilla's `messageHeight == 9` at
    /// the default `chatScale == 1.0` from `ChatComponent`'s own literal, not
    /// from this crate's `chat_line_h` formula (which would make the
    /// assertion circular), and states the double-applied wrong hypothesis
    /// explicitly so the tolerance cannot straddle both.
    #[test]
    fn chat_row_pitch_matches_vanillas_message_height_at_default_scale() {
        let stats = DebugStats::default();
        let mut f = quiet(&stats);
        let chat = [("hi", 0.0_f32)];
        f.chat = &chat;
        let measured = ink_height("chat row", &f);

        let vanilla = 9.0_f32; // `ChatComponent`'s literal `messageHeight`.
        let doubled = 18.0_f32; // the deleted `HUD_TEXT_SCALE` baseline's prediction.
        assert!(
            (measured - vanilla).abs() < 2.0,
            "a single chat row must be vanilla's messageHeight-tall row \
             (9px at chatScale 1.0): measured {measured:.2}"
        );
        assert!(
            (measured - doubled).abs() > 4.0,
            "measured {measured:.2} is too close to the deleted ad-hoc \
             double-applied pitch ({doubled}px) to discriminate the fix from \
             the bug it replaces"
        );
    }

    /// **Clauses 2 and 3: panel width and height**, read directly out of the
    /// background rect's own NDC corners rather than through `chat_width_px`/
    /// `chat_height_px` — the same helpers the draw uses — so a shared bug in
    /// either could not cancel itself out (mirrors
    /// `chat_width_option_sizes_the_box_to_the_predicted_pixel_width` in
    /// `hud.rs`, from outside the crate). Chat is **open** (`chat_input`
    /// `Some`) so `height_pct_focused` (default `1.0`) governs the box, and a
    /// single short line is the only geometry, so the input strip's own rect
    /// is `verts[0..STRIDE*6]`.
    #[test]
    fn chat_panel_width_and_height_match_vanillas_getwidth_getheight_formulas() {
        let stats = DebugStats::default();
        let chat = [("hi", 0.0_f32)];
        let frame = HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat,
            chat_input: Some(""),
            chat_caret_visible: false,
            ..HudFrame::new(&stats)
        };
        let (cw, _ch) = canvas();
        let geo = HudGeometry::build(&frame, FB_W, FB_H);
        assert!(!geo.verts.is_empty(), "the input strip must paint");

        // Vanilla's own formulas, recomputed independently of `chat_width_px`/
        // `chat_height_px`.
        let want_w = (1.0_f32 * 280.0 + 40.0).floor(); // 320
        let want_h = (1.0_f32 * 160.0 + 20.0).floor(); // 180
        assert!(want_w <= cw, "premise: a 320px box must fit the {cw}px canvas");

        // **The plate is not the text column**, and the 12 is vanilla's, not
        // ours. Vanilla fills the chat background from local `-4` to
        // `maxWidth + 4 + 4` — screen `0` to `getWidth() + 12 * scale`, four
        // scaled pixels of padding left of the text and eight right of it, so
        // asymmetric by construction because the plate is anchored at screen
        // `x = 0` while the text starts inset. `docs/chat.md` carries that
        // derivation and the source it came from.
        //
        // This gate predates the plate pad and asserted the strip was exactly
        // `want_w`. That stopped being true the moment the pad landed: the
        // *premise* went stale, not the measurement, and the production draw
        // is the half that is right. Spelled as a literal here rather than
        // imported because `hud::CHAT_PLATE_PAD_PX` is `pub(crate)` and this
        // test lives outside the crate — which suits the rule anyway, since
        // both halves are then derived from vanilla's own geometry rather than
        // read back out of the code under test.
        const CHAT_PLATE_PAD_PX: f32 = 12.0;
        let want_plate_w = want_w + CHAT_PLATE_PAD_PX; // 332 at chatScale 1.0

        // The input strip's own rect: vertex 0 is `(x0, y0)`, vertex 1 is
        // `(x1, y0)` (`ColourStream::rect`'s first triangle) — its width in
        // NDC converts back with `(x1_ndc + 1) * cw / 2`.
        let x1_px = (geo.verts[STRIDE] + 1.0) * 0.5 * cw;
        assert!(
            (x1_px - want_plate_w).abs() < 1e-2,
            "the chat plate must be vanilla's `getWidth(1.0) == {want_w}`px \
             (unscaled by chatScale — computed outside `pose.scale` in \
             `ChatComponent.java`) plus the {CHAT_PLATE_PAD_PX}px plate pad, \
             i.e. {want_plate_w}px, got {x1_px:.2}"
        );
        // The pad is what makes the two numbers different, so a build that
        // dropped it would still satisfy an `x1_px == want_plate_w` written
        // against a pad of zero. Assert they are genuinely apart, so this gate
        // cannot go quietly vacuous the way its predecessor did.
        assert!(
            (want_plate_w - want_w).abs() > 1.0,
            "premise: the plate and the text column must be measurably \
             different widths for this assertion to distinguish them"
        );

        // Height is asserted through the row cap it produces, not through a
        // second raw-vertex offset: at vanilla's real 9px row pitch, a 180px
        // box holds `floor(180 / 9) == 20` rows, not `floor(180 / 18) == 10`
        // (the doubled hypothesis) and not `floor(180 / 9) - epsilon`. Fifteen
        // short lines therefore must all be visible uncapped; a
        // twenty-five-line log must be capped to exactly twenty.
        let want_rows = (want_h / 9.0_f32).floor() as usize;
        assert_eq!(want_rows, 20, "sanity: vanilla's own arithmetic");

        let make = |n: usize| -> HudGeometry {
            let lines: Vec<(&str, f32)> = (0..n).map(|_| ("x", 0.0_f32)).collect();
            HudGeometry::build(
                &HudFrame {
                    crosshair: false,
                    show_debug: false,
                    chat: &lines,
                    chat_input: Some(""),
                    chat_caret_visible: false,
                    ..HudFrame::new(&stats)
                },
                FB_W,
                FB_H,
            )
        };
        let bg = [0.0_f32, 0.0, 0.0, 0.5]; // `chat_bg_opacity` default 0.5, `alpha` 1.0.
        // 15 rows fit uncapped (< 20); the input strip itself is drawn with a
        // *different* rect covering the same colour family only if
        // `chat_bg_opacity` happens to equal the row alpha, which it does
        // here by construction — so it is counted too, and both sides of the
        // comparison below include it identically, cancelling out.
        let fifteen = quads_with_color(&make(15).verts, bg, 1e-4);
        let twenty_five = quads_with_color(&make(25).verts, bg, 1e-4);
        assert_eq!(
            fifteen, 16,
            "15 lines under the 20-row cap plus the input strip must all paint \
             (16 background quads), not silently truncated"
        );
        assert_eq!(
            twenty_five, 21,
            "25 lines must cap at vanilla's `floor(180 / 9) == 20` rows plus \
             the input strip (21 background quads), not `floor(180 / 18) == \
             10` (11 with the strip) — the doubled-pitch hypothesis"
        );
    }

    /// **The wrapping case.** A single-word (no spaces) line long enough that
    /// vanilla's real per-glyph advance and the deleted ad-hoc pitch's advance
    /// predict *different row counts*, not just different pixel widths within
    /// one row — CLAUDE.md's warning that porting only the line-height and
    /// panel-width clauses "looks right in a screenshot and is wrong the
    /// moment a message wraps". With no `VanillaFont` attached (this test
    /// harness is jar-less), `Builder::text_width` falls back to
    /// `item_icon::text_w`: `(GLYPH_W + 1) * scale == 6 * scale` per
    /// character.
    ///
    /// At the fixed `chat_pose_scale == 1.0`: `floor(320 / 6) == 53` chars
    /// fit a row, so 70 chars wrap into **2** rows (53 + 17). At the deleted
    /// double-applied pitch (`scale == 2.0`, advance `12`px):
    /// `floor(320 / 12) == 26` chars fit a row, so 70 chars would wrap into
    /// **3** (26 + 26 + 18) — a whole extra row, not a rounding-sized
    /// difference.
    #[test]
    fn a_long_wrapping_line_hard_wraps_at_vanillas_chatscale_alone_row_count() {
        let stats = DebugStats::default();
        let line = "a".repeat(70);
        let chat = [(line.as_str(), 0.0_f32)];
        let frame = HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat,
            ..HudFrame::new(&stats)
        };
        let geo = HudGeometry::build(&frame, FB_W, FB_H);
        let bg = [0.0_f32, 0.0, 0.0, 0.5];
        let rows = quads_with_color(&geo.verts, bg, 1e-4);
        assert_eq!(
            rows, 2,
            "70 'a's at vanilla's chatScale-alone advance (6px/char) must wrap \
             into exactly 2 rows (53 + 17); 3 would be the deleted ad-hoc \
             2x-pitch hypothesis's prediction (26 + 26 + 18)"
        );
    }

    /// **The hit-test/drawn-region regression this fix can specifically
    /// introduce.** `suggestion_layout` is called both from the draw
    /// (`HudGeometry::build_inner`) and from `HudRenderer::suggestion_layout`
    /// (the pointer hit-test, `crates/lodestone-shell/src/app/menus.rs`'s
    /// `WindowApp::suggestion_row_under_cursor`) — exercised here through the
    /// identical free function a GPU-free test can reach, at **two**
    /// non-coincident chat scales so a bug that only shows away from the
    /// default cannot hide behind an accidental agreement at `1.0`.
    #[test]
    fn hit_test_rect_and_drawn_popup_agree_at_two_chat_scales() {
        let stats = DebugStats::default();
        let candidates: Vec<Candidate> = (0..12)
            .map(|i| Candidate { text: format!("cand{i:02}"), tooltip: None })
            .collect();

        for chat_scale in [1.0_f32, 0.5] {
            let opts = ChatDisplayOptions { scale: chat_scale, ..ChatDisplayOptions::default() };
            let base = HudFrame {
                crosshair: false,
                show_debug: false,
                chat_input: Some("ca"),
                chat_caret_visible: false,
                chat_options: opts,
                ..HudFrame::new(&stats)
            };
            let control = HudGeometry::build(&base, FB_W, FB_H);
            let popup = SuggestionPopup {
                line: "ca",
                start: 0,
                candidates: &candidates,
                selected: 0,
                offset: 0,
                cursor: None,
            };
            let with = HudGeometry::build(
                &HudFrame { chat_suggestions: Some(popup), ..base },
                FB_W,
                FB_H,
            );
            assert!(with.verts.len() > control.verts.len(), "the popup must add geometry");

            let (cw, ch) = canvas();
            let pose = chat_pose_scale(opts);
            // No font attached here (jar-less harness), so the same fallback
            // measure the draw itself falls back to.
            let layout = suggestion_layout(cw, ch, pose, &popup, |s| {
                s.chars().count() as f32 * 6.0 * pose
            });

            let px = |x: f32| (x + 1.0) * 0.5 * cw;
            let py = |y: f32| (1.0 - y) * 0.5 * ch;
            let gutter = pose.max(1.0);
            let mut outside = Vec::new();
            for chunk in with.verts[control.verts.len()..].chunks(STRIDE) {
                let (x, y) = (px(chunk[0]), py(chunk[1]));
                let inside = x >= layout.x - 0.5
                    && x <= layout.x + layout.w + 0.5
                    && y >= layout.y - gutter - 0.5
                    && y <= layout.y + layout.h + gutter + 0.5;
                if !inside {
                    outside.push((x, y));
                }
            }
            assert!(
                outside.is_empty(),
                "chat_scale {chat_scale}: {} popup vertices landed outside the \
                 hit-test rect: {:?}",
                outside.len(),
                &outside[..outside.len().min(8)]
            );
        }
    }
}
