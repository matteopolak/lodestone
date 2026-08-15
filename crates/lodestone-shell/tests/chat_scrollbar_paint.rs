//! Coverage gate for `HudFrame::chat_scrollbar`: the scroll-position state in
//! `crate::chat::ChatScroll` is thoroughly unit-tested in `chat.rs` itself,
//! but that is a closed loop — this asserts the geometry it drives actually
//! reaches the rasterised frame, and that a negative control (nothing to
//! scroll into) paints nothing.
//!
//! Per `CLAUDE.md`'s "nothing is done until something on screen changes":
//! `ChatScroll`'s own tests could be green while `HudFrame::chat_scrollbar`
//! stayed `None` forever and nothing here would know.

use lodestone::hud::{ChatDisplayOptions, ChatScrollbar, DebugStats, HudFrame, HudGeometry};
use lodestone::menu::render::logical_canvas;

const FB_W: u32 = 1280;
const FB_H: u32 = 720;
const STRIDE: usize = 6;

fn canvas() -> (f32, f32) {
    logical_canvas(0, FB_W, FB_H)
}

/// Every rectangle in the colour stream, as `(x0, x1, y0, y1, rgba)` — see
/// `chat_input_gap.rs`'s identical helper for why six vertices is one rect.
fn rects(frame: &HudFrame<'_>) -> Vec<(f32, f32, f32, f32, [f32; 4])> {
    let (w, h) = canvas();
    let geo = HudGeometry::build(frame, FB_W, FB_H);
    assert_eq!(geo.verts.len() % STRIDE, 0);
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

/// The scrollbar's own thumb: narrow (a couple of pixels wide), sat just past
/// the right edge of the chat box, non-black (either the amber or the
/// blue-grey vanilla colour) — distinct enough from every other black
/// background band and every glyph rect that filtering on "narrow and not
/// black and not white" isolates it without assuming a draw-call count.
fn scrollbar_thumbs(frame: &HudFrame<'_>) -> Vec<(f32, f32, f32, f32, [f32; 4])> {
    rects(frame)
        .into_iter()
        .filter(|(x0, x1, _, _, c)| {
            let w = x1 - x0;
            (0.5..6.0).contains(&w) && !(c[0] < 0.02 && c[1] < 0.02 && c[2] < 0.02)
        })
        .collect()
}

fn open_frame_with<'a>(
    stats: &'a DebugStats,
    chat: &'static [(&'static str, f32)],
    bar: Option<ChatScrollbar>,
) -> HudFrame<'a> {
    let mut frame = HudFrame::new(stats);
    frame.show_debug = false;
    frame.crosshair = false;
    frame.chat = chat;
    frame.chat_input = Some("typing");
    frame.chat_caret_visible = false;
    frame.chat_options = ChatDisplayOptions::default();
    frame.chat_scrollbar = bar;
    frame
}

/// With real scrolled history (more total entries than fit on screen), the
/// scrollbar must paint at least one non-black rectangle.
#[test]
fn a_scrolled_chat_paints_a_scrollbar_thumb() {
    let stats = DebugStats::default();
    let frame = open_frame_with(
        &stats,
        &[("hello world", 0.0)],
        Some(ChatScrollbar {
            scrolled: 5,
            total: 40,
            new_message_since_scroll: false,
        }),
    );
    let thumbs = scrollbar_thumbs(&frame);
    assert!(
        !thumbs.is_empty(),
        "a chat_scrollbar with total > rows_per_page must paint a visible thumb"
    );
}

/// Negative control: with nothing to scroll into (`total` fits on one page),
/// vanilla's own gate suppresses the scrollbar entirely. This is what proves
/// [`scrollbar_thumbs`]'s filter is not just matching *some* incidental
/// geometry — it must find nothing here.
#[test]
fn nothing_to_scroll_into_paints_no_scrollbar_thumb() {
    let stats = DebugStats::default();
    let frame = open_frame_with(
        &stats,
        &[("hello world", 0.0)],
        Some(ChatScrollbar {
            scrolled: 0,
            total: 2,
            new_message_since_scroll: false,
        }),
    );
    let thumbs = scrollbar_thumbs(&frame);
    assert!(
        thumbs.is_empty(),
        "total fits on one page: vanilla's own virtualHeight != chatHeight gate must suppress the bar, found {thumbs:?}"
    );
}

/// `chat_scrollbar: None` (the default, and every pre-existing caller before
/// this feature) must paint nothing extra — the additive-field guarantee
/// `HudFrame`'s own doc promises for every optional field.
#[test]
fn no_scrollbar_field_paints_nothing_extra() {
    let stats = DebugStats::default();
    let frame = open_frame_with(&stats, &[("hello world", 0.0)], None);
    let thumbs = scrollbar_thumbs(&frame);
    assert!(thumbs.is_empty(), "found {thumbs:?}");
}
