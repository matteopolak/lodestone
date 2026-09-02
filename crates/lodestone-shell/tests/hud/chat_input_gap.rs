//! Pins the vertical gap between the chat scrollback's newest line and the
//! chat input box's translucent background strip.
//!
//! # Why this gate exists
//!
//! Reported by the owner: "there's a gap between the bottom of the chat and
//! the bar where I type stuff." Tracing `hud.rs`'s chat draw site
//! (`HudGeometry::build_inner`) found the scrollback's own anchor,
//! `chat_bottom`, was computed as `input_y - INPUT_STRIP_PAD * chat_pose_scale`
//! while the box was open — literally *the input strip's own top edge* — so
//! the newest scrollback row was drawn flush against the input box, off by
//! one pixel of rounding. That is not what vanilla does.
//!
//! `ChatComponent.extractRenderState`
//! (vanilla's decompiled chat-component source, 26.2)
//! computes `final int chatBottom = Mth.floor((screenHeight - 40) / scale);`
//! as one expression, **before** it ever branches on `displayMode.foreground`
//! (open vs. closed) and with no reference anywhere to where the `EditBox`
//! sits (`this.height - 12`, a wholly separate literal in `ChatScreen.init`,
//! a different class). Vanilla's scrollback and its input box are two
//! independently-anchored things that happen to sit near each other, not one
//! derived from the other — and the un-derived distance between them, at the
//! vanilla-default `chatScale` of `1.0`, is a real ~26 canvas-pixel gap this
//! HUD used to erase entirely.
//!
//! # Both hypotheses
//!
//! At a 1280x720 framebuffer (auto GUI scale 3, per [`canvas`]) the logical
//! canvas is 426.667 x 240 (see `hud_text_scale.rs`'s own citation for why
//! `HudGeometry::build` resolves GUI scale 3 here). With the single-line chat
//! fixture below (`chat_line_h` = 9, `chat_pose_scale` = 1.0 at the default
//! `chatScale`):
//!
//! | quantity | wrong (pre-fix, coupled to the input strip) | vanilla-correct |
//! |---|---|---|
//! | `chat_bottom` | `input_y - INPUT_STRIP_PAD` = 225 | `floor((240 - 40) / 1.0)` = 200 |
//! | newest row's drawn bottom edge | 224 | 199 |
//! | gap to the input strip's top edge (225) | 1 | 26 |
//!
//! These are not round numbers reached for — 199 and 225 are exactly what
//! [`chat_row_and_input_bands`] measures by rasterising the real geometry
//! (`HudGeometry::build`), and 1 vs. 26 is a wide enough margin that no font
//! or padding slop could straddle them.

use lodestone::hud::{ChatDisplayOptions, DebugStats, HudFrame, HudGeometry};
use lodestone::menu::render::logical_canvas;

const FB_W: u32 = 1280;
const FB_H: u32 = 720;
/// `hud.rs`'s private `FLOATS_PER_VERTEX`: `(x, y)` NDC position + RGBA.
const STRIDE: usize = 6;

/// `HudGeometry::build` resolves GUI scale automatically (0 == auto), so the
/// canvas its pixel constants land in is `logical_canvas(0, ..)` — the same
/// call the draw site itself makes, not a restatement.
fn canvas() -> (f32, f32) {
    logical_canvas(0, FB_W, FB_H)
}

/// A single-message chat log with the box open and typing in progress — the
/// exact scenario the owner's report describes.
fn open_chat_frame<'a>(stats: &'a DebugStats, chat: &'static [(&'static str, f32)]) -> HudFrame<'a> {
    let mut frame = HudFrame::new(stats);
    frame.show_debug = false;
    frame.crosshair = false;
    frame.chat = chat;
    frame.chat_input = Some("typing");
    // Fixed (not blinking) so the caret's own quad — a separate, tiny,
    // non-black shape — never lands in the wide-black-band filter below.
    frame.chat_caret_visible = false;
    frame.chat_options = ChatDisplayOptions::default();
    frame
}

/// Every axis-aligned rectangle [`HudGeometry::build`]'s colour stream holds,
/// reconstructed from vertex sextets: [`lodestone::hud`]'s `Builder::rect_px`
/// (via `ColourStream::rect`) always emits exactly six vertices — two
/// triangles sharing an edge — per rectangle, so grouping the flat `[x, y, r,
/// g, b, a]` stream into chunks of `6 * STRIDE` floats recovers one rect per
/// chunk. Glyph quads go through the same stream, so this recovers text ink
/// too; callers filter for what they want.
fn rects(frame: &HudFrame<'_>) -> Vec<(f32, f32, f32, f32, [f32; 4])> {
    let (w, h) = canvas();
    let geo = HudGeometry::build(frame, FB_W, FB_H);
    assert_eq!(
        geo.verts.len() % STRIDE,
        0,
        "colour vertex buffer is not a whole number of {STRIDE}-float vertices"
    );
    geo.verts
        .chunks_exact(STRIDE * 6)
        .map(|chunk| {
            let (mut x0, mut x1) = (f32::MAX, f32::MIN);
            let (mut y0, mut y1) = (f32::MAX, f32::MIN);
            for v in chunk.chunks_exact(STRIDE) {
                let px = (v[0] + 1.0) * 0.5 * w;
                let py = (1.0 - v[1]) * 0.5 * h;
                x0 = x0.min(px);
                x1 = x1.max(px);
                y0 = y0.min(py);
                y1 = y1.max(py);
            }
            (x0, x1, y0, y1, [chunk[2], chunk[3], chunk[4], chunk[5]])
        })
        .collect()
}

/// The chat row background band (the newest scrollback line's translucent
/// black strip) and the input box's own background band, as `(top, bottom)`
/// pairs in logical-canvas pixels.
///
/// Distinguishes them from individual glyph-ink rectangles by width alone
/// (a background band spans the whole chat box; a glyph does not) and from
/// each other by position (the input strip is always the lower of the two,
/// closer to the canvas bottom) — never by an assumed count or order, so a
/// third wide black band (there should never be one here) fails loudly
/// instead of silently picking the wrong pair.
fn chat_row_and_input_bands(frame: &HudFrame<'_>) -> ((f32, f32), (f32, f32)) {
    let mut bands: Vec<(f32, f32)> = rects(frame)
        .into_iter()
        .filter(|(x0, x1, _, _, c)| x1 - x0 > 30.0 && c[0] < 0.01 && c[1] < 0.01 && c[2] < 0.01)
        .map(|(_, _, y0, y1, _)| (y0, y1))
        .collect();
    bands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    assert_eq!(
        bands.len(),
        2,
        "expected exactly one chat-row band and one input-strip band, found {}: {bands:?}",
        bands.len()
    );
    (bands[0], bands[1])
}

/// The chat row must sit strictly above the input strip with vanilla's real,
/// non-zero gap between them — not flush, and not the pre-fix single pixel.
#[test]
fn newest_chat_row_sits_vanillas_real_gap_above_the_input_strip() {
    let stats = DebugStats::default();
    let frame = open_chat_frame(&stats, &[("hello world", 0.0)]);
    let (chat_row, input_strip) = chat_row_and_input_bands(&frame);

    let (_, canvas_h) = canvas();
    assert_eq!(canvas_h, 240.0, "fixture assumption: GUI scale 3 at 1280x720");

    // `chat_bottom` predicted fresh from vanilla's own expression
    // (`Mth.floor((screenHeight - 40) / scale)`, scale == 1.0 by default),
    // not copied from the implementation under test.
    let predicted_chat_bottom = ((canvas_h - 40.0) / 1.0).floor();
    assert_eq!(predicted_chat_bottom, 200.0);

    // The row rect's own drawn bottom edge is one pixel above its anchor
    // (`hud.rs` draws each row's background at `y - 1.0`) — collected into
    // one named value rather than asserted inline so the mismatch case
    // below can report both numbers together.
    let measured_row_bottom = chat_row.1;
    let measured_strip_top = input_strip.0;

    let mut mismatches = Vec::new();
    if (measured_row_bottom - 199.0).abs() > f32::EPSILON {
        mismatches.push(format!(
            "chat row bottom edge: predicted 199.0 (chat_bottom {predicted_chat_bottom} - 1), measured {measured_row_bottom}"
        ));
    }
    let gap = measured_strip_top - measured_row_bottom;
    // Both hypotheses: the pre-fix coupled formula put the row flush against
    // the strip (a 1px gap, pure rounding slop); vanilla's independent
    // anchors put a real 26px of daylight between them. Assert the
    // measurement lands on the correct one and sits far from the wrong one.
    let wrong_hypothesis_gap = 1.0;
    let correct_hypothesis_gap = 26.0;
    if (gap - correct_hypothesis_gap).abs() > f32::EPSILON {
        mismatches.push(format!(
            "gap: predicted {correct_hypothesis_gap} (vanilla's independent chatBottom), \
             measured {gap} (pre-fix coupled hypothesis was {wrong_hypothesis_gap})"
        ));
    }
    assert!(
        (gap - wrong_hypothesis_gap).abs() > 5.0,
        "measured gap {gap} sits suspiciously close to the pre-fix flush hypothesis \
         ({wrong_hypothesis_gap}) rather than vanilla's real one ({correct_hypothesis_gap})"
    );
    assert!(
        mismatches.is_empty(),
        "chat/input gap mismatches:\n{}",
        mismatches.join("\n")
    );
}

/// A frame with no chat history and a closed box paints neither band — the
/// negative control proving [`chat_row_and_input_bands`]'s filter really
/// isolates these two rectangles rather than matching by accident.
#[test]
fn a_quiet_closed_chat_paints_no_background_bands() {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.show_debug = false;
    frame.crosshair = false;
    let bands: Vec<(f32, f32, f32, f32, [f32; 4])> = rects(&frame)
        .into_iter()
        .filter(|(x0, x1, _, _, c)| x1 - x0 > 30.0 && c[0] < 0.01 && c[1] < 0.01 && c[2] < 0.01)
        .collect();
    assert!(
        bands.is_empty(),
        "a quiet, closed HUD frame must paint no wide black bands, found {bands:?}"
    );
}

/// [`HudGeometry::build`]'s colour stream is still whole `rect()` sextets —
/// the layout [`rects`] inverts. Guards the assumption the other two tests
/// build on.
#[test]
fn vertex_layout_is_still_rect_sextets() {
    let stats = DebugStats::default();
    let frame = open_chat_frame(&stats, &[("hello world", 0.0)]);
    let geo = HudGeometry::build(&frame, FB_W, FB_H);
    assert_eq!(geo.verts.len() % (STRIDE * 6), 0);
}
