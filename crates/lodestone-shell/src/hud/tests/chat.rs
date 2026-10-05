//! Chat input, caret, suggestions, wrapping and chat options.

use super::*;

/// The dropdown reaches pixels, and does so **inside its own rect** — the
/// island check for the whole widget.
///
/// Counting vertices alone would pass for a popup drawn off-screen or on top
/// of the hotbar, which is the failure this repo keeps hitting. So the
/// assertion is on *where*: every quad the popup adds must have its corners
/// inside the rect `suggestion_layout` resolved (plus the 1px scroll-hint
/// gutters, which are outside the rows by construction). Mismatches are
/// collected and asserted as a set, so one stray quad does not hide the
/// others.
///
/// The negative control is the same frame with `chat_suggestions: None`, run
/// and compared, not described.
#[test]
fn the_suggestion_popup_draws_inside_the_rect_the_layout_resolved() {
    let stats = DebugStats::default();
    let candidates = popup_candidates(12);
    let (w, h) = (640u32, 480u32);
    let base_frame = HudFrame {
        crosshair: false,
        show_debug: false,
        chat_input: Some("ca"),
        chat_caret_visible: false,
        ..HudFrame::new(&stats)
    };
    // The control: identical frame, no popup. Run, not asserted about.
    let control = HudGeometry::build(&base_frame, w, h);

    let popup = SuggestionPopup {
        line: "ca",
        start: 0,
        candidates: &candidates,
        selected: 0,
        offset: 0,
        cursor: None,
    };
    let with = HudGeometry::build(
        &HudFrame {
            chat_suggestions: Some(popup),
            ..base_frame
        },
        w,
        h,
    );
    assert!(
        with.vertex_count() > control.vertex_count(),
        "the popup must add geometry — {} vs {}",
        with.vertex_count(),
        control.vertex_count()
    );

    // Re-derive the rect from the same function the draw called, with the
    // same measure: no font attached here, so `item_icon::text_w`.
    let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let layout = suggestion_layout(cw, ch, pose, &popup, |s| measure_text(None, s, pose));
    assert_eq!(
        layout.rows,
        crate::chat::SUGGESTION_LINE_LIMIT,
        "12 candidates must be windowed to 10 rows — otherwise the cap is untested"
    );

    // Every vertex the popup added, in canvas pixels. `verts` is NDC over the
    // logical canvas, so undo that rather than restating a pixel formula.
    let px = |x: f32| (x + 1.0) * 0.5 * cw;
    let py = |y: f32| (1.0 - y) * 0.5 * ch;
    let gutter = pose.max(1.0);
    let mut outside = Vec::new();
    for chunk in with.verts[control.verts.len()..].chunks(FLOATS_PER_VERTEX) {
        let (x, y) = (px(chunk[0]), py(chunk[1]));
        let inside_x = x >= layout.x - 0.5 && x <= layout.x + layout.w + 0.5;
        let inside_y =
            y >= layout.y - gutter - 0.5 && y <= layout.y + layout.h + gutter + 0.5;
        if !(inside_x && inside_y) {
            outside.push((x, y));
        }
    }
    assert!(
        outside.is_empty(),
        "{} of the popup's own vertices landed outside its rect \
         (x {}..{}, y {}..{}): {:?}",
        outside.len(),
        layout.x,
        layout.x + layout.w,
        layout.y - gutter,
        layout.y + layout.h + gutter,
        &outside[..outside.len().min(8)]
    );

    // And the rect really is above the input line rather than over it — the
    // `anchorToBottom` placement, which a sign error would invert.
    assert!(
        layout.y + layout.h <= chat_input_top(ch, pose),
        "the popup's bottom ({}) must sit at or above the input line's top ({})",
        layout.y + layout.h,
        chat_input_top(ch, pose)
    );
}

/// The regression this chat-scale fix can specifically introduce: the draw
/// (`HudGeometry::build_inner`) and the pointer hit-test
/// (`HudRenderer::suggestion_layout`, exercised headlessly here through the
/// same free functions it calls — a GPU-free `wgpu::Device` cannot be
/// constructed in this test, so this is the identical code path minus the
/// device handle) both resolve `chat_pose_scale`. Before this fix
/// `HudGeometry::build_inner` recomputed `HUD_TEXT_SCALE * opts.scale`
/// inline instead of calling [`chat_pose_scale`], so the two *could* have
/// drifted apart the moment either copy changed; now `build_inner` calls
/// the same function the hit-test does, structurally.
///
/// Run at **two** non-coincident chat scales — `1.0` (default) and `0.5`
/// — because a bug that only shows up away from the default (say, a stray
/// `HUD_TEXT_SCALE` reintroduced on one side only) would pass at `1.0` if
/// the two formulas happened to agree there by construction and diverge
/// everywhere else.
#[test]
fn the_hit_test_rect_and_the_drawn_popup_agree_at_two_different_chat_scales() {
    let stats = DebugStats::default();
    let candidates = popup_candidates(12);
    let (w, h) = (640u32, 480u32);

    for chat_scale in [1.0_f32, 0.5] {
        let opts = ChatDisplayOptions {
            scale: chat_scale,
            ..ChatDisplayOptions::default()
        };
        let base_frame = HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some("ca"),
            chat_caret_visible: false,
            chat_options: opts,
            ..HudFrame::new(&stats)
        };
        let control = HudGeometry::build(&base_frame, w, h);

        let popup = SuggestionPopup {
            line: "ca",
            start: 0,
            candidates: &candidates,
            selected: 0,
            offset: 0,
            cursor: None,
        };
        let with = HudGeometry::build(
            &HudFrame {
                chat_suggestions: Some(popup),
                ..base_frame
            },
            w,
            h,
        );

        // The "hit-test region": exactly what `HudRenderer::suggestion_layout`
        // computes (`logical_canvas` → `chat_pose_scale(opts)` →
        // `suggestion_layout`), not a restatement.
        let (cw, ch) =
            crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
        let pose = chat_pose_scale(opts);
        let layout =
            suggestion_layout(cw, ch, pose, &popup, |s| measure_text(None, s, pose));

        // The "drawn region": every vertex the popup actually added.
        let px = |x: f32| (x + 1.0) * 0.5 * cw;
        let py = |y: f32| (1.0 - y) * 0.5 * ch;
        let gutter = pose.max(1.0);
        let mut outside = Vec::new();
        for chunk in with.verts[control.verts.len()..].chunks(FLOATS_PER_VERTEX) {
            let (x, y) = (px(chunk[0]), py(chunk[1]));
            let inside_x = x >= layout.x - 0.5 && x <= layout.x + layout.w + 0.5;
            let inside_y =
                y >= layout.y - gutter - 0.5 && y <= layout.y + layout.h + gutter + 0.5;
            if !(inside_x && inside_y) {
                outside.push((x, y));
            }
        }
        assert!(
            outside.is_empty(),
            "chat_scale {chat_scale}: {} of the popup's own vertices landed \
             outside the hit-test rect (x {}..{}, y {}..{}): {:?}",
            outside.len(),
            layout.x,
            layout.x + layout.w,
            layout.y - gutter,
            layout.y + layout.h + gutter,
            &outside[..outside.len().min(8)]
        );
    }
}

/// `row_at` maps a pointer to the candidate the player is looking at.
///
/// The inputs are chosen so the two plausible readings disagree: with
/// `offset == 2` a hit on the **first visible row** must report candidate
/// `2`, not `0`, and the last visible row must report `11` rather than `9`.
/// An implementation that forgot `+ offset` agrees with the truth only at
/// `offset == 0`, which is why the scrolled case is the one asserted.
#[test]
fn a_pointer_resolves_to_the_candidate_under_it_including_when_scrolled() {
    let candidates = popup_candidates(12);
    let popup = SuggestionPopup {
        line: "ca",
        start: 0,
        candidates: &candidates,
        selected: 0,
        offset: 2,
        cursor: None,
    };
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let layout = suggestion_layout(640.0, 480.0, pose, &popup, |s| measure_text(None, s, pose));
    let mid_x = layout.x + layout.w * 0.5;

    assert_eq!(
        layout.row_at(mid_x, layout.y + layout.row_h * 0.5, 2, 12),
        Some(2),
        "the first visible row is candidate 2 once the window has scrolled"
    );
    assert_eq!(
        layout.row_at(mid_x, layout.y + layout.row_h * 1.5, 2, 12),
        Some(3)
    );
    assert_eq!(
        layout.row_at(mid_x, layout.y + layout.row_h * 9.5, 2, 12),
        Some(11),
        "and the last visible row is the last candidate"
    );
    // Outside, on each of the four edges.
    assert_eq!(layout.row_at(mid_x, layout.y - 1.0, 2, 12), None);
    assert_eq!(
        layout.row_at(mid_x, layout.y + layout.h + 1.0, 2, 12),
        None
    );
    assert_eq!(
        layout.row_at(layout.x - 1.0, layout.y + layout.row_h * 0.5, 2, 12),
        None
    );
    assert_eq!(
        layout.row_at(
            layout.x + layout.w + 1.0,
            layout.y + layout.row_h * 0.5,
            2,
            12
        ),
        None
    );
}

#[test]
fn chat_input_and_log_add_geometry() {
    let stats = DebugStats::default();
    let base = HudGeometry::build(&HudFrame::new(&stats), 640, 480).vertex_count();
    let chat = [("<a> hi", 0.0_f32), ("<b> yo", 0.0)];
    let frame = HudFrame {
        chat: &chat,
        chat_input: Some("hello"),
        ..HudFrame::new(&stats)
    };
    let with_chat = HudGeometry::build(&frame, 640, 480).vertex_count();
    assert!(with_chat > base, "chat log + input line must add geometry");
}

/// A chat selection is modelled in character positions, but its rectangle
/// must follow the exact rendered glyph advances (including UTF-8 input),
/// land behind the glyphs, and stay inside the input strip.
#[test]
fn chat_input_selection_draws_a_clipped_glyph_aligned_rect() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let input = "aébc";
    let base = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some(input),
            chat_caret_visible: false,
            ..HudFrame::new(&stats)
        },
        w,
        h,
    );
    let selected = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some(input),
            chat_selection: Some((1, 3)),
            chat_caret_visible: false,
            ..HudFrame::new(&stats)
        },
        w,
        h,
    );

    let (logical_w, logical_h) =
        crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let to_px_x = |ndc_x: f32| (ndc_x + 1.0) * 0.5 * logical_w;
    let to_px_y = |ndc_y: f32| (1.0 - ndc_y) * 0.5 * logical_h;

    let selection = selected
        .verts
        .chunks(FLOATS_PER_VERTEX)
        .filter(|vertex| vertex[2..6] == [0.0, 0.0, 1.0, 1.0])
        .map(|vertex| (to_px_x(vertex[0]), to_px_y(vertex[1])))
        .collect::<Vec<_>>();
    assert!(
        !selection.is_empty(),
        "the selected character range must add a blue selection rectangle"
    );

    let (min_x, max_x, min_y, max_y) = selection.iter().fold(
        (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY),
        |(min_x, max_x, min_y, max_y), &(x, y)| {
            (min_x.min(x), max_x.max(x), min_y.min(y), max_y.max(y))
        },
    );
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let left = CHAT_TEXT_INSET * pose + measure_text(None, "a", pose);
    let right = CHAT_TEXT_INSET * pose + measure_text(None, "aéb", pose);
    assert!((min_x - left).abs() < 0.01, "selection begins at its first glyph");
    assert!((max_x - right).abs() < 0.01, "selection ends at its last glyph");
    assert!(
        min_y >= chat_input_top(logical_h, pose) - 0.01
            && max_y <= chat_input_top(logical_h, pose) + font::GLYPH_H as f32 * pose + 0.01,
        "selection stays in the input glyph row"
    );
    assert!(
        selected.vertex_count() > base.vertex_count(),
        "the selection must be a render-model addition, not editor-only state"
    );
}

/// Regression gate for the player report's third defect: `hud.rs` used to
/// draw `format!("> {input}_")` unconditionally, so a `>` prompt appeared
/// that vanilla's own chat-screen input widget never draws. An empty input
/// with the caret off must therefore draw *nothing* beyond its background
/// strip, and turning the caret on must add exactly one `_` glyph — a
/// negative control (caret off) plus a positive one (caret on) rather than
/// eyeballing a vertex-count increase.
#[test]
fn no_stray_prompt_prefix_and_caret_blinks() {
    let stats = DebugStats::default();
    let caret_off = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some(""),
            chat_caret_visible: false,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    // Only the input row's own translucent background rect (one quad =
    // 6 vertices) may be here — no `>` , no space, nothing.
    assert_eq!(
        caret_off.vertex_count(),
        6,
        "an empty input with the caret off must draw only its background strip"
    );

    let caret_on = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some(""),
            chat_caret_visible: true,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    // `_`'s bitmap (`font::glyph_rows('_')`) lights only its bottom row's
    // 5 bits — exactly 5 quads = 30 vertices, not a guess.
    assert_eq!(
        caret_on.vertex_count(),
        caret_off.vertex_count() + 30,
        "chat_caret_visible must toggle exactly one `_` glyph (5 lit pixels)"
    );
}

/// Owner report: "the actual insertion point moves with Left/Right but the
/// flashing underscore stays at the end." Two separate defects behind one
/// symptom, and this gate has to see both — the caret's **x** was measured
/// from the width of the whole line rather than of the text before the
/// caret, and its **shape** was the appended `_` unconditionally where
/// vanilla switches to a 1 px insert bar the moment the caret is not at the
/// end (`EditBox.extractWidgetRenderState`'s `insert` predicate choosing
/// between `TextCursorUtils.extractInsertCursor` and `extractAppendCursor`).
///
/// The input and cursor are chosen so the two hypotheses disagree on both
/// axes: `"abcd"` with the caret at 2 is neither position 0 (where an empty
/// line's append and insert x coincide) nor the end (where the *whole*
/// bug is invisible, since "width of the line" and "width of the text
/// before the caret" are the same number). Each assertion below therefore
/// carries the value the buggy code would have produced as well as the
/// right one, so it fails rather than merely being satisfiable.
#[test]
fn chat_caret_follows_the_cursor_and_becomes_a_bar_mid_string() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let input = "abcd";
    let pose = chat_pose_scale(ChatDisplayOptions::default());

    let build = |cursor: Option<usize>, caret: bool| {
        HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat_input: Some(input),
                chat_cursor: cursor,
                chat_caret_visible: caret,
                ..HudFrame::new(&stats)
            },
            w,
            h,
        )
    };

    // The caret is drawn last, and the text and background before it are
    // identical across all three builds, so everything past the caret-off
    // build's vertex count *is* the caret. Nothing else in this frame draws
    // after the input row — the sibling gate above pins that by asserting an
    // empty input with the caret off emits exactly its 6 background
    // vertices.
    let off = build(Some(2), false);
    let mid = build(Some(2), true);
    let end = build(None, true);

    // Shape, as a count rather than an eyeball. `_`'s bitmap lights 5 pixels
    // (5 quads, 30 vertices); the insert bar is a single rect (1 quad, 6).
    // These are the two hypotheses, and they cannot coincide.
    assert_eq!(
        mid.verts.len(),
        off.verts.len() + 6 * FLOATS_PER_VERTEX,
        "a caret inside the line must be the 1 px insert bar (one quad), not \
         the 5-quad `_` glyph"
    );
    assert_eq!(
        end.verts.len(),
        off.verts.len() + 30 * FLOATS_PER_VERTEX,
        "a caret at the end of the line must still be the appended `_`"
    );

    let (logical_w, logical_h) =
        crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let to_px_x = |ndc_x: f32| (ndc_x + 1.0) * 0.5 * logical_w;
    let to_px_y = |ndc_y: f32| (1.0 - ndc_y) * 0.5 * logical_h;
    let caret_box = |g: &HudGeometry| {
        g.verts[off.verts.len()..].chunks(FLOATS_PER_VERTEX).fold(
            (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY),
            |(min_x, max_x, min_y, max_y), v| {
                let (x, y) = (to_px_x(v[0]), to_px_y(v[1]));
                (min_x.min(x), max_x.max(x), min_y.min(y), max_y.max(y))
            },
        )
    };

    // Position. `cursorX = textX + width(before) + 1`, then `cursorX--` in
    // insert mode, so the bar's left edge is `textX + width("ab")` exactly.
    // The old code's answer — `textX + width("abcd") + 1`, the append form
    // measured against the whole line — is the `wrong` value below, and the
    // assertion has to land on one of them.
    let inset = CHAT_TEXT_INSET * pose;
    let (mid_left, ..) = caret_box(&mid);
    let right = inset + measure_text(None, "ab", pose);
    let wrong = inset + measure_text(None, input, pose) + pose;
    assert!(
        (wrong - right) > 1.0,
        "the fixture must separate the two hypotheses: correct {right}, buggy \
         {wrong}"
    );
    assert!(
        (mid_left - right).abs() < 0.01,
        "the insert bar sits after the text before the caret ({right}), not at \
         the end of the line ({wrong}); measured {mid_left}"
    );

    // And the append arm still lands where it always did, so the fix moved
    // the mid-string caret rather than shifting every caret left.
    let (end_left, ..) = caret_box(&end);
    assert!(
        (end_left - wrong).abs() < 0.01,
        "the appended `_` keeps vanilla's `textX + width(value) + 1`; expected \
         {wrong}, measured {end_left}"
    );

    // The bar's rect: vanilla's `fill(x, y - 1, x + 1, y + lineHeight, …)`
    // spans one pixel above the glyph box to one below it, and is one pixel
    // wide. Both in this surface's scaled pixels.
    let (bar_l, bar_r, bar_t, bar_b) = caret_box(&mid);
    let top = chat_input_top(logical_h, pose);
    assert!(
        (bar_r - bar_l - pose).abs() < 0.01,
        "the insert bar is one (scaled) pixel wide, measured {}",
        bar_r - bar_l
    );
    assert!(
        (bar_t - (top - pose)).abs() < 0.01
            && (bar_b - (top + (font::GLYPH_H as f32 + 1.0) * pose)).abs() < 0.01,
        "the insert bar spans the glyph row plus one pixel either side; got \
         {bar_t}..{bar_b} against a glyph row starting at {top}"
    );
}

/// Owner report: "the inline autocomplete suggestion gets offset by the
/// ticking underscore, when it should not move." The ghost's pen used to
/// be measured from `{input}{caret}`'s live width, and `caret` is `""`
/// half of every blink cycle — so the ghost's x shifted by the caret
/// glyph's own advance every ~300ms. Fixed by always measuring against
/// `{input}_` regardless of the actual blink state.
///
/// This predicts the *old* (buggy) pens from first principles — via the
/// same jar-less `measure_text` the popup gate above uses — and asserts
/// they really would have differed, which is what makes the "now equal"
/// assertion below a discriminating regression rather than a vacuous one
/// (a font where `_` measured zero-width could satisfy equality by
/// accident either way).
#[test]
fn suggestion_ghost_pen_does_not_move_with_the_caret_blink() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let underscore_w = measure_text(None, "_", pose);
    assert!(
        underscore_w > 0.0,
        "the fallback font must give `_` a real width, or this test cannot \
         discriminate the fix from the bug it replaces"
    );

    let ghost_min_x = |g: &HudGeometry| -> f32 {
        let mut min_x = f32::INFINITY;
        for chunk in g.verts.chunks(FLOATS_PER_VERTEX) {
            if chunk[2..6] == SUGGESTION_GHOST {
                min_x = min_x.min(chunk[0]);
            }
        }
        assert!(min_x.is_finite(), "no SUGGESTION_GHOST-coloured vertex found");
        min_x
    };

    let frame = |caret_visible: bool| HudFrame {
        crosshair: false,
        show_debug: false,
        chat_input: Some("he"),
        chat_caret_visible: caret_visible,
        chat_suggestion_ghost: Some("llo"),
        ..HudFrame::new(&stats)
    };
    let on = HudGeometry::build(&frame(true), w, h);
    let off = HudGeometry::build(&frame(false), w, h);

    // What the pre-fix formula (`margin + text_width({input}{caret})`)
    // would have produced: the two pens differ by exactly `_`'s own
    // advance, since that is the only difference between the two
    // measured strings.
    let old_pen_on = measure_text(None, "he_", pose);
    let old_pen_off = measure_text(None, "he", pose);
    assert!(
        (old_pen_on - old_pen_off - underscore_w).abs() < 1e-4,
        "sanity check on the reproduction itself: the old pens must differ \
         by exactly the caret glyph's width"
    );

    assert_eq!(
        ghost_min_x(&on),
        ghost_min_x(&off),
        "the suggestion ghost's x must not move when the caret blinks"
    );
}

/// The landed blink-invariance fix above made the ghost's pen *stable*,
/// but stable at the wrong x: one whole underscore-width too far right,
/// permanently. **The discriminating assertion is the absolute x, not
/// stability** — a gate that only re-runs the blink-invariance check
/// above would pass on the regression this predicts and rejects.
///
/// `HudGeometry`'s `verts` are in **NDC** (`ColourStream::rect`'s own doc:
/// "positions in NDC"), not pixels, so the pixel-space prediction below is
/// converted through the same `to_ndc` vanilla-canvas math the draw uses
/// — via [`crate::menu::render::logical_canvas`], the one function that
/// resolves a framebuffer size to the logical canvas every layout site
/// (including this draw) measures against.
#[test]
fn suggestion_ghost_sits_at_cursor_x_minus_one_not_after_the_caret() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let (logical_w, _) =
        crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let to_ndc_x = |px: f32| 2.0 * px / logical_w - 1.0;

    let ghost_min_x = |g: &HudGeometry| -> f32 {
        let mut min_x = f32::INFINITY;
        for chunk in g.verts.chunks(FLOATS_PER_VERTEX) {
            if chunk[2..6] == SUGGESTION_GHOST {
                min_x = min_x.min(chunk[0]);
            }
        }
        assert!(min_x.is_finite(), "no SUGGESTION_GHOST-coloured vertex found");
        min_x
    };

    // The ghost text starts with `A`, not `llo` as the other gates in this
    // module use — deliberately: this fallback font's `l` glyph has a
    // **blank leading column** (`font::glyph_rows('l')`'s column 0 is
    // unlit in all seven rows), so "leftmost lit pixel" would measure one
    // glyph-column right of the real pen for any string starting with
    // `l`. `A` lights column 0 on at least one row, so its own leftmost
    // lit pixel *is* the pen position — which is what this test needs to
    // assert an exact x. The other gates in this module only compare
    // *relative* ghost positions, where that per-glyph offset cancels out
    // and does not matter.
    let frame = HudFrame {
        crosshair: false,
        show_debug: false,
        chat_input: Some("he"),
        chat_caret_visible: true,
        chat_suggestion_ghost: Some("Allo"),
        ..HudFrame::new(&stats)
    };
    let geo = HudGeometry::build(&frame, w, h);

    // Vanilla's `cursorX - 1`, `cursorX` being `EditBox
    // .extractWidgetRenderState`'s `drawX` *after* `drawX +=
    // font.width(charSequence) + 1;` — the typed text's width, no caret
    // glyph folded in (unlike the older, already-fixed `font.width("he_")`
    // bug), **plus vanilla's own reserved pixel**, which the `- 1` then
    // exactly cancels: `(font.width("he") + 1) - 1 == font.width("he")`.
    // So the *correct* ghost position is flush with the raw text width —
    // no further arithmetic on top of it, which is exactly the
    // discriminating case: a formula that forgets vanilla's `+ 1` (this
    // draw site's own bug until now) computes `font.width("he") - 1`
    // instead, landing the ghost one pixel *short*, overlapping into the
    // text's last glyph rather than sitting flush against it.
    let expected = to_ndc_x(CHAT_TEXT_INSET * pose + measure_text(None, "he", pose));
    let missing_caret_width_hypothesis =
        to_ndc_x(CHAT_TEXT_INSET * pose + measure_text(None, "he_", pose) - pose);
    let missing_plus_one_hypothesis =
        to_ndc_x(CHAT_TEXT_INSET * pose + measure_text(None, "he", pose) - pose);
    for (name, hypothesis) in [
        ("the caret-width bug", missing_caret_width_hypothesis),
        ("the missing `+ 1` bug", missing_plus_one_hypothesis),
    ] {
        assert!(
            (expected - hypothesis).abs() > 1e-3,
            "sanity check on the reproduction: {name}'s hypothesis must be \
             discriminably far from the correct one, or a coincidence could \
             pass either way"
        );
    }
    assert!(
        (ghost_min_x(&geo) - expected).abs() < 1e-4,
        "ghost x (NDC) = {}, expected cursorX - 1 = {expected} (the \
         caret-width bug would have placed it at \
         {missing_caret_width_hypothesis}, the missing-`+ 1` bug at \
         {missing_plus_one_hypothesis})",
        ghost_min_x(&geo)
    );
}

/// **The owner's report**: "the inline completion (grey text) is missing
/// the pixel gap after the last character... it touches the regular text
/// which is wrong." Established by direct comparison against
/// `crates/lodestone-shell/src/menu/edit_box.rs`'s `draw_state_with`,
/// which already carries vanilla's `+ 1.0`
/// (`EditBox.extractWidgetRenderState`'s `drawX += font.width
/// (charSequence) + 1;`) — this draw site did not.
///
/// **Why the assertion is against the text's own right edge, not the
/// caret.** A first attempt at this gate measured `caret_x - ghost_x` and
/// found it passed under a deliberate re-neuter of the `+ 1` fix —
/// because both the ghost (`cursor_x - pose`) and the caret (`cursor_x`)
/// move together with `cursor_x`, so the gap *between them* is `pose`
/// regardless of whether `cursor_x` itself carries vanilla's `+ 1`. The
/// bug is a shift of the whole `{ghost, caret}` pair relative to the
/// *text*, not a change in their separation from each other — so only a
/// measurement against the text's own (independently computed) right edge
/// can see it. Before the fix, `ghost_x` sat a full `pose` *before* the
/// text's right edge (overlapping the last glyph, `font.width(value) - 1`
/// instead of vanilla's `(font.width(value) + 1) - 1 ==
/// font.width(value)`); after it, `ghost_x` sits flush with the text's
/// right edge, matching vanilla's own edit-box widget's own cancellation exactly — not a
/// visible pixel of daylight, but no longer overlapping into the glyph
/// either, which is the actual "touches" the report named.
#[test]
fn the_ghost_sits_flush_with_the_text_not_overlapping_its_last_glyph() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let (logical_w, _) =
        crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let to_px_x = |ndc_x: f32| (ndc_x + 1.0) * 0.5 * logical_w;

    let ghost_min_px = |g: &HudGeometry| -> f32 {
        let mut min_x = f32::INFINITY;
        for chunk in g.verts.chunks(FLOATS_PER_VERTEX) {
            if chunk[2..6] == SUGGESTION_GHOST {
                min_x = min_x.min(to_px_x(chunk[0]));
            }
        }
        assert!(min_x.is_finite(), "no SUGGESTION_GHOST-coloured vertex found");
        min_x
    };

    // Two pairwise-distinct inputs, different lengths, so a formula that
    // (incorrectly) makes the offset depend on the text's own width
    // cannot pass by coincidence at a single length.
    for input in ["he", "cats"] {
        let frame = HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some(input),
            chat_caret_visible: true,
            chat_suggestion_ghost: Some("Allo"),
            ..HudFrame::new(&stats)
        };
        let geo = HudGeometry::build(&frame, w, h);
        let ghost_x = ghost_min_px(&geo);

        // The text's own right edge, computed from the font's advance
        // metric alone (`measure_text`) — not through `cursor_x`, the
        // value the draw itself derives the ghost from, so this cannot
        // pass by restating the code under test.
        let text_right_edge = CHAT_TEXT_INSET * pose + measure_text(None, input, pose);
        let offset = ghost_x - text_right_edge;
        // The bug this replaces: `font.width(value) - 1` (the missing
        // `+ 1` never reserved, so the ghost lands one pixel *inside* the
        // text's last glyph instead of flush with its advance edge). A
        // constant, not a second measurement, so the two hypotheses can
        // never coincide by construction (`0.0 - (-pose)` is always
        // `pose`, well past the tolerance below).
        let overlap_hypothesis = -pose;

        assert!(
            offset.abs() < pose * 0.25,
            "input {input:?}: the ghost must sit flush with the text's own \
             right edge (vanilla's `(font.width(value) + 1) - 1 == \
             font.width(value)`), not offset from it: measured {offset:.3}px"
        );
        assert!(
            (offset - overlap_hypothesis).abs() > pose * 0.5,
            "input {input:?}: measured offset {offset:.3}px is too close to \
             the missing-`+ 1` bug's prediction of overlapping the text's \
             last glyph by {overlap_hypothesis:.3}px to discriminate the \
             fix from the bug it replaces"
        );
    }
}

/// The other half of the fix: the caret must draw **after** (on top of)
/// the suggestion, not before — vanilla's own edit-box widget's render order is text →
/// hint → suggestion → highlight → cursor. `HudGeometry::build` appends
/// vertices in draw order, so "after" is observable as "later in `verts`".
#[test]
fn caret_draws_after_the_suggestion_so_it_composites_on_top() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);

    let frame = HudFrame {
        crosshair: false,
        show_debug: false,
        chat_input: Some("he"),
        chat_caret_visible: true,
        chat_suggestion_ghost: Some("llo"),
        ..HudFrame::new(&stats)
    };
    let geo = HudGeometry::build(&frame, w, h);

    let last_index_with_color = |target: [f32; 4]| -> Option<usize> {
        geo.verts
            .chunks(FLOATS_PER_VERTEX)
            .enumerate()
            .filter(|(_, chunk)| chunk[2..6] == target)
            .map(|(i, _)| i)
            .max()
    };
    let ghost_last = last_index_with_color(SUGGESTION_GHOST)
        .expect("the ghost must draw when chat_suggestion_ghost is Some");
    // The caret shares the input text's own white and the same input
    // row, so identify it as a white quad **inside that row's glyph box**
    // appearing after the ghost — restricted to the row so an unrelated
    // white element elsewhere in the frame (this test does not disable
    // every HUD element) cannot produce a false pass. The box spans the
    // full glyph height, not just `input_y` exactly: `_`'s own bitmap
    // (`font::glyph_rows('_')`) only lights the bottom row, so its quad's
    // y sits `6 * pose` px below `input_y`, not at it. `input_y` and the
    // span are converted to NDC the same way
    // [`suggestion_ghost_sits_at_cursor_x_minus_one_not_after_the_caret`]
    // converts x, using the logical (not raw framebuffer) canvas height
    // `chat_input_top` itself is measured against.
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let (_, logical_h) =
        crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let input_y_ndc = 1.0 - 2.0 * chat_input_top(logical_h, pose) / logical_h;
    let glyph_h_ndc = 2.0 * (font::GLYPH_H as f32 * pose) / logical_h;
    let white = [1.0_f32, 1.0, 1.0, 1.0];
    let caret_after_ghost = geo.verts.chunks(FLOATS_PER_VERTEX).enumerate().skip(ghost_last + 1).any(
        |(_, chunk)| {
            chunk[2..6] == white
                && chunk[1] <= input_y_ndc + 1e-3
                && chunk[1] >= input_y_ndc - glyph_h_ndc - 1e-3
        },
    );
    assert!(
        caret_after_ghost,
        "the caret's white quad, on the input's own row, must appear \
         after the ghost's grey quad in draw order, so it composites on \
         top"
    );
}

/// Vanilla's `!insert` gate: a full line (256 chars, `ChatInput::push_char`'s
/// own cap) suppresses the suggestion entirely, matching
/// `EditBox`'s own `insert = cursorPos < value.length() || value.length()
/// >= maxLength` — this shell's chat caret is always at the end (see the
/// draw's own comment), so only the length half of that disjunction can
/// ever apply here.
#[test]
fn suggestion_is_suppressed_once_the_chat_line_is_full() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let full_line: String = "x".repeat(256);

    let frame = HudFrame {
        crosshair: false,
        show_debug: false,
        chat_input: Some(full_line.as_str()),
        chat_caret_visible: true,
        chat_suggestion_ghost: Some("llo"),
        ..HudFrame::new(&stats)
    };
    let geo = HudGeometry::build(&frame, w, h);
    let has_ghost = geo
        .verts
        .chunks(FLOATS_PER_VERTEX)
        .any(|chunk| chunk[2..6] == SUGGESTION_GHOST);
    assert!(!has_ghost, "a full line must draw no suggestion ghost");
}

/// Predicts the exact geometry of a hard-wrapped chat line from first
/// principles (box width, the fixed fallback font's per-char advance, and
/// `a`'s own lit-pixel count), rather than merely asserting "it wrapped" —
/// CLAUDE.md's *magnitude* species of vacuous test is a predicate that
/// would pass for any wrap width; this one would fail for a wrong one.
#[test]
fn a_long_line_with_no_spaces_hard_wraps_at_the_predicted_row_count() {
    let stats = DebugStats::default();
    let line = "a".repeat(70);
    let chat = [(line.as_str(), 0.0_f32)];
    let geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    // The default chat box is `chat_width_px(1.0) == 320`px wide (capped
    // at `b.w == 640`, so uncapped here). With no `VanillaFont` attached,
    // `Builder::legacy_width` falls back to `item_icon::text_w`:
    // `(GLYPH_W + 1) * scale` per char, and the chat pose scale is
    // vanilla's own chat-scale option alone (`chat_pose_scale`,
    // vanilla's own scale getter), `1.0` at the default, so each `a` costs
    // `6 * 1.0 == 6`px. `floor(320 / 6) == 53` fit the first row; the
    // remaining `70 - 53 == 17` spill to a second — two rows.
    //
    // 70 chars, not 30: at this HUD's now-deleted ad-hoc 2× pitch each
    // `a` would have cost `12`px (`floor(320 / 12) == 26` per row), which
    // wraps 70 chars into **three** rows (26 + 26 + 18), not two — a
    // whole extra row, not a rounding-sized difference, so this input
    // cannot coincide between the two hypotheses the way a shorter line
    // could.
    //
    // `a`'s bitmap (`font::glyph_rows('a')`) lights `0+0+3+1+4+2+4 == 14`
    // pixels; each lit pixel is one quad (`ColourStream::glyph`)
    // of 6 vertices, so all 70 `a`s cost
    // `70 * 14 * 6 == 5880` vertices regardless of how they are split
    // across rows — the row *count* shows up only in the background
    // strips, one 6-vertex rect each.
    assert_eq!(
        geo.vertex_count(),
        5880 + 2 * 6,
        "expected exactly two wrapped rows' worth of geometry at vanilla's \
         chat-scale-only pose (one row would be 5880 + 6, three — the \
         deleted ad-hoc 2× pitch's prediction — would be 5880 + 18)"
    );
}

/// Direct, GPU-free gate on [`wrap_legacy_with`]'s wrap *decision*, using a
/// hand-specified width table rather than the fixed 5×7 fallback — the
/// fallback is itself fixed-advance, so it cannot exercise the
/// variable-width case the real vanilla font (attached only when a jar is
/// present) actually draws with. `i`/`W`'s widths below are vanilla's own,
/// documented in `crate::hud::vanilla_font`'s module doc ("`i` is 2 px
/// wide … `W` and `M` are 6"); the competing "flat character count"
/// hypothesis uses this shell's own real fixed-advance constant
/// (`(font::GLYPH_W + 1) * 1.0 == 6`) rather than an invented
/// number, so both sides of the comparison are real, citable code.
#[test]
fn wrap_uses_real_per_glyph_widths_not_a_flat_character_count() {
    let real_width = |s: &str| -> f32 {
        s.chars()
            .map(|c| match c {
                'i' => 2.0,
                'W' => 6.0,
                _ => 0.0,
            })
            .sum()
    };
    let flat_count_width = |s: &str| -> f32 { s.chars().count() as f32 * 6.0 };

    // Five narrow glyphs then five wide ones, no spaces, so the wrap is a
    // pure hard-break character-index decision with no word-boundary
    // logic muddying which hypothesis "wins".
    let s = "iiiiiWWWWW";
    let max_width_px = 20.0;

    // Real cumulative widths: 2,4,6,8,10 (the five `i`s), then 16, 22 …
    // for the `W`s — the largest prefix at or under 20px is "iiiiiW"
    // (16px); the next `W` would make 22px.
    let real_rows = wrap_legacy_with(real_width, s, max_width_px);
    assert_eq!(
        real_rows.first().map(String::as_str),
        Some("iiiiiW"),
        "real per-glyph widths must break after the 6th character: {real_rows:?}"
    );

    // The flat hypothesis charges every character 6px regardless of
    // glyph, so only `floor(20 / 6) == 3` fit before the 4th overflows —
    // three characters, not six.
    let flat_rows = wrap_legacy_with(flat_count_width, s, max_width_px);
    assert_eq!(
        flat_rows.first().map(String::as_str),
        Some("iii"),
        "a flat character-count hypothesis must break after the 3rd character: {flat_rows:?}"
    );

    let real_break = real_rows[0].chars().count();
    let flat_break = flat_rows[0].chars().count();
    assert_eq!(real_break, 6, "predicted real-width break index");
    assert_eq!(flat_break, 3, "predicted flat character-count break index");
    assert_eq!(
        real_break - flat_break,
        3,
        "the two hypotheses must diverge by a real, non-zero margin, or this test \
         cannot tell a real-width wrap from a character-count one"
    );
}

/// **The bug report**: a chat message carrying a literal `\n` (a
/// multi-line system message, or a pasted multi-line player message)
/// rendered the control character as a missing-glyph box instead of
/// breaking the line, because [`wrap_legacy_with`] only ever split on
/// `' '`. A width table that charges `\n` a large, easy-to-notice cost
/// makes the point unambiguously: if the character survived into a row
/// unbroken, the row's measured width would blow way past `max_width_px`
/// and this test's own `real_width` closure would report it — the
/// control is built into the fixture rather than bolted on afterward.
#[test]
fn wrap_legacy_with_splits_on_a_literal_newline() {
    let width = |s: &str| -> f32 {
        s.chars()
            .map(|c| if c == '\n' { 1000.0 } else { 1.0 })
            .sum()
    };
    let rows = wrap_legacy_with(width, "first line\nsecond line", 200.0);
    assert_eq!(
        rows,
        vec!["first line".to_string(), "second line".to_string()],
        "a literal \\n must start a new row, not survive as a character in the \
         middle of one: {rows:?}"
    );
    assert!(
        rows.iter().all(|r| !r.contains('\n')),
        "no returned row may still carry the control character: {rows:?}"
    );
}

/// [`wrap_measured`]'s own precedent (`menu::render::draw`): a blank line
/// in the source is a line, not something that collapses when its
/// neighbours are pulled together. `wrap_legacy_paragraph("")`'s
/// documented "never empty" guarantee is what makes this fall out for
/// free from the `\n` split alone — this test is what proves that
/// guarantee actually reaches the outer function rather than being an
/// unused promise on the inner one.
#[test]
fn wrap_legacy_with_treats_a_blank_paragraph_as_a_line() {
    let width = |s: &str| -> f32 { s.chars().count() as f32 };
    let rows = wrap_legacy_with(width, "a\n\nb", 200.0);
    assert_eq!(
        rows,
        vec!["a".to_string(), String::new(), "b".to_string()],
        "a blank line between two real ones must survive as its own empty row: {rows:?}"
    );
}

/// The [`TextSpan`] sibling of [`wrap_uses_real_per_glyph_widths_not_a_flat_character_count`]:
/// same real-per-glyph-width table over the same `"iiiiiWWWWW"` input, so
/// [`wrap_spans_with`] is proven against a real width hypothesis rather
/// than the fixed-advance fallback. Sharpened past that test in the one
/// way a span list can be: the ten glyphs are split across two
/// *differently-styled* runs, so the wrap point falls **inside** the
/// styled boundary — the case `wrap_legacy_with` cannot even express,
/// since a `§` code and the run it colours are just characters to it.
#[test]
fn wrap_spans_breaks_by_real_width_and_keeps_style_across_the_break() {
    let real_width = |spans: &[TextSpan]| -> f32 {
        spans
            .iter()
            .flat_map(|s| s.text.chars())
            .map(|c| match c {
                'i' => 2.0,
                'W' => 6.0,
                _ => 0.0,
            })
            .sum()
    };
    let max_width_px = 20.0;
    let red = TextSpan {
        text: "iiiii".to_string(),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Red),
            ..TextStyle::default()
        },
    };
    let blue = TextSpan {
        text: "WWWWW".to_string(),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Blue),
            ..TextStyle::default()
        },
    };
    let spans = [red, blue];

    let rows = wrap_spans_with(real_width, &spans, max_width_px);
    let first_row = &rows[0];
    let joined: String = first_row.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        joined, "iiiiiW",
        "must break after the 6th glyph (16px), same as the legacy gate's real-width \
         hypothesis: {rows:?}"
    );
    assert_eq!(
        first_row.len(),
        2,
        "the wrap point falls mid-style, so the row must keep two separate runs \
         rather than merging across the boundary: {first_row:?}"
    );
    assert_eq!(
        first_row[0].style.color,
        Some(TextColor::Red),
        "the `i`s before the break must keep their colour"
    );
    assert_eq!(
        first_row[1].style.color,
        Some(TextColor::Blue),
        "the lone `W` carried onto this row must keep *its own* colour, not the \
         red run's — a single-pending-style model (one colour per row) would get \
         this wrong on a row that starts one style and ends in another"
    );
}

/// The [`TextSpan`] sibling of [`wrap_legacy_with_splits_on_a_literal_newline`],
/// sharpened past it the one way a span list can be: the `\n` sits
/// **inside** a single styled run rather than between two plain `&str`s,
/// which is exactly the case [`split_span_paragraphs`] exists for — a
/// plain `&str::split('\n')` has no style to preserve, but a `TextSpan`'s
/// run does, on both sides of the break.
#[test]
fn wrap_spans_with_splits_on_a_literal_newline_mid_span() {
    let width = |spans: &[TextSpan]| -> f32 {
        spans
            .iter()
            .flat_map(|s| s.text.chars())
            .map(|c| if c == '\n' { 1000.0 } else { 1.0 })
            .sum()
    };
    let styled = TextSpan {
        text: "before\nafter".to_string(),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Red),
            ..TextStyle::default()
        },
    };
    let rows = wrap_spans_with(width, std::slice::from_ref(&styled), 200.0);
    let joined: Vec<String> = rows
        .iter()
        .map(|row| row.iter().map(|s| s.text.as_str()).collect())
        .collect();
    assert_eq!(
        joined,
        vec!["before".to_string(), "after".to_string()],
        "a \\n sitting inside one styled run must still split into two rows: {joined:?}"
    );
    assert!(
        rows.iter().all(|row| row.iter().all(|s| s.style.color == Some(TextColor::Red))),
        "both sides of the split must keep the run's original colour: {rows:?}"
    );
}

/// Proves `chat_options.colors` is read, not merely stored. `§c` is
/// zero-width whether it recolours or is stripped, so the two frames'
/// vertex *counts* are equal by construction — the option's whole effect
/// is on colour, so the control that actually matters is `verts`
/// (positions **and** colours) differing.
#[test]
fn chat_colors_option_strips_legacy_codes_when_off() {
    let stats = DebugStats::default();
    let coded = [("\u{00a7}chi", 0.0_f32)];
    let frame = |colors: bool| HudFrame {
        crosshair: false,
        show_debug: false,
        chat: &coded,
        chat_options: ChatDisplayOptions {
            colors,
            ..ChatDisplayOptions::default()
        },
        ..HudFrame::new(&stats)
    };
    let with_colors = HudGeometry::build(&frame(true), 640, 480);
    let without_colors = HudGeometry::build(&frame(false), 640, 480);
    assert_eq!(
        with_colors.vertex_count(),
        without_colors.vertex_count(),
        "the code is zero-width either way, so geometry *count* must match"
    );
    assert_ne!(
        with_colors.verts, without_colors.verts,
        "chat_colors=false must actually strip the colour, not just round-trip the option"
    );
}

/// Proves `chat_options.background_opacity` is read with the right
/// *magnitude*, not merely that changing it changes something — the
/// species of vacuous test CLAUDE.md calls out (a hurt-overlay gate once
/// passed 3440/3440 while only checking the *sign* of a change, not how
/// much). Row 0's background rect is emitted before any of its text
/// glyphs, so its first vertex's alpha channel is `verts[5]` — no
/// filtering, no averaging, the exact float the draw call passed in.
#[test]
fn chat_background_opacity_sets_the_exact_row_alpha() {
    let stats = DebugStats::default();
    let chat = [("hi", 0.0_f32)];
    for bg in [0.1_f32, 0.5, 1.0] {
        let geo = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &chat,
                chat_options: ChatDisplayOptions {
                    background_opacity: bg,
                    ..ChatDisplayOptions::default()
                },
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        let alpha = geo.verts[5];
        assert!(
            (alpha - bg).abs() < 1e-5,
            "row background alpha must equal chat_background_opacity ({bg}), got {alpha}"
        );
    }
}

/// As [`chat_background_opacity_sets_the_exact_row_alpha`], for
/// `chat_options.text_opacity`: `hi` carries no `§` code, so its colour
/// stays `base` throughout `Builder::text_legacy`'s fallback path and
/// every glyph pixel's alpha is exactly the `alpha` parameter passed in —
/// here, `text_opacity * 0.9 + 0.1` at a fresh
/// line's fade of `1.0`.
#[test]
fn chat_text_opacity_sets_the_exact_glyph_alpha() {
    let stats = DebugStats::default();
    let chat = [("hi", 0.0_f32)];
    for op in [0.0_f32, 0.5, 1.0] {
        let geo = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &chat,
                chat_options: ChatDisplayOptions {
                    text_opacity: op,
                    ..ChatDisplayOptions::default()
                },
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        let expected = op.mul_add(0.9, 0.1);
        // `verts[0..36)` is row 0's background rect (6 vertices); `h`'s
        // bitmap (`font::glyph_rows('h')`) lights bit 0 of its very top
        // row, so the next quad emitted is that pixel — its alpha is
        // `verts[41]` (the 6th float of the 2nd vertex block).
        let alpha = geo.verts[41];
        assert!(
            (alpha - expected).abs() < 1e-5,
            "text_opacity {op}: expected glyph alpha {expected}, got {alpha}"
        );
    }
}

/// As the two magnitude gates above, for `chat_options.width_pct`, via
/// vanilla's own `ChatComponent.getWidth` algebra
/// (`pct * 280.0 + 40.0`, floored) computed independently here rather
/// than by calling [`chat_width_px`] — so a bug shared between the two
/// could not cancel out.
#[test]
fn chat_width_option_sizes_the_box_to_the_predicted_pixel_width() {
    let stats = DebugStats::default();
    let chat = [("hi", 0.0_f32)];
    // `b.w == 320` at this canvas size:
    // `logical_canvas(AUTO_GUI_SCALE, 640, 480) == (320, 240)` (height
    // binds at `calculate_gui_scale(0, 640, 480) == 2`).
    const CANVAS_W: f32 = 320.0;
    for (pct, expected_px) in [(1.0_f32, 320.0_f32), (0.5, 180.0), (0.0, 40.0)] {
        let geo = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &chat,
                chat_options: ChatDisplayOptions {
                    width_pct: pct,
                    ..ChatDisplayOptions::default()
                },
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        // Row 0's background rect starts at `x == 0`, so its second
        // vertex `(x + w, y)` (`ColourStream::rect`) converted to NDC is
        // `2 * w / b.w - 1` — `verts[6]`.
        //
        // That rect is the *plate*, which is deliberately wider than the
        // text column it sits behind — see [`CHAT_PLATE_PAD_PX`], and note
        // the pad is the only shared term here: the discriminating part,
        // the `pct * 280 + 40` slope, is still derived independently of
        // `chat_width_px`, so a wrong slope cannot cancel out.
        let pad = CHAT_PLATE_PAD_PX * chat_pose_scale(ChatDisplayOptions::default());
        let x1_ndc = geo.verts[6];
        let expected_ndc = 2.0 * (expected_px + pad) / CANVAS_W - 1.0;
        assert!(
            (x1_ndc - expected_ndc).abs() < 1e-4,
            "pct {pct}: expected box width {expected_px}px plus {pad}px of plate \
             padding (x1 {expected_ndc}), got x1 {x1_ndc}"
        );
    }
}

/// As the width gate above, for `chat_options.scale`: it must exactly
/// double the on-screen row height when set to `2.0`, not merely change
/// it by some amount.
#[test]
fn chat_scale_option_doubles_the_row_height_exactly() {
    let stats = DebugStats::default();
    let chat = [("hi", 0.0_f32)];
    let frame = |scale: f32| HudFrame {
        crosshair: false,
        show_debug: false,
        chat: &chat,
        chat_options: ChatDisplayOptions {
            scale,
            ..ChatDisplayOptions::default()
        },
        ..HudFrame::new(&stats)
    };
    let default_geo = HudGeometry::build(&frame(1.0), 640, 480);
    let doubled_geo = HudGeometry::build(&frame(2.0), 640, 480);
    // Row 0's rect vertex 0 (`y0`) and vertex 2 (`y1`, the 3rd vertex —
    // floats 12..18) give its height in NDC: `verts[1] - verts[13]`.
    let height = |g: &HudGeometry| g.verts[1] - g.verts[13];
    let default_h = height(&default_geo);
    let doubled_h = height(&doubled_geo);
    assert!(default_h > 0.0, "sanity: the rect must have positive height");
    assert!(
        (doubled_h - 2.0 * default_h).abs() < 1e-5,
        "chat_scale=2.0 must exactly double the row height: default {default_h}, doubled {doubled_h}"
    );
}

/// Proves `chat_options.height_pct_unfocused` is read as a genuine *cap*
/// on visible rows, not just stored: at `0.0` (`chat_height_px(0.0) ==
/// 20`px against a `9`px vanilla-metrics default row — vanilla's own
/// `messageHeight`, vanilla's own chat-component rendering) exactly two rows fit
/// (`floor(20 / 9) == 2`), so a five-line log must render identically to
/// a two-line log, not five.
#[test]
fn chat_height_option_caps_the_number_of_visible_rows() {
    let stats = DebugStats::default();
    let chat = [
        ("a", 0.0_f32),
        ("b", 0.0),
        ("c", 0.0),
        ("d", 0.0),
        ("e", 0.0),
    ];
    let capped = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat,
            chat_options: ChatDisplayOptions {
                height_pct_unfocused: 0.0,
                ..ChatDisplayOptions::default()
            },
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    let two_lines_uncapped = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat[3..],
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert_eq!(
        capped.vertex_count(),
        two_lines_uncapped.vertex_count(),
        "height_pct_unfocused == 0.0 must cap the scrollback to exactly two rows \
         at vanilla's 9px row height"
    );
    let uncapped = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert!(
        uncapped.vertex_count() > capped.vertex_count(),
        "the default (uncapped-enough-for-5-lines) height must show more than the capped one"
    );
}

#[test]
fn chat_colour_codes_are_zero_width_and_recolour_runs() {
    let stats = DebugStats::default();
    // A `§c` prefix must not add glyph geometry (codes are 2 chars / 0 width):
    // "§chi" and "hi" draw the same number of lit pixels.
    let plain = [("hi", 0.0_f32)];
    let coded = [("\u{00a7}chi", 0.0_f32)];
    let plain_geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &plain,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    let coded_geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &coded,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert_eq!(
        plain_geo.vertex_count(),
        coded_geo.vertex_count(),
        "a colour code must draw no glyphs of its own"
    );
    // …but the pixels must be a different colour, so the code isn't ignored.
    assert_ne!(
        plain_geo.verts, coded_geo.verts,
        "a colour code must recolour the run, not merely be stripped"
    );
}

#[test]
fn chat_lines_fade_out_with_age_when_closed() {
    let stats = DebugStats::default();
    // A fresh line draws; a line older than the visible window draws nothing.
    let fresh = [("hello", 0.0_f32)];
    let stale = [("hello", 30.0_f32)];
    let fresh_n = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &fresh,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    )
    .vertex_count();
    let stale_n = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &stale,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    )
    .vertex_count();
    assert!(fresh_n > 0, "a fresh chat line must be visible");
    assert_eq!(
        stale_n, 0,
        "a line past its lifetime must vanish when closed"
    );

    // Opening the box (a chat_input present) resurrects the stale line.
    let opened = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &stale,
            chat_input: Some(""),
            ..HudFrame::new(&stats)
        },
        640,
        480,
    )
    .vertex_count();
    assert!(
        opened > 0,
        "an open chat box shows history regardless of age"
    );
}

/// The discriminating fixture for the whole chat-hex bug: one line
/// carrying all three colour conventions a real server mixes — a modern
/// `TextColor::Rgb` component style, a legacy-named component style, and
/// a `§` code embedded *inside* a literal (the shape the owner's report
/// actually showed: `"§f§lG§r§f §r§8| §rLumberjack..."` is entirely this
/// third convention). A fixture using only named colours cannot tell
/// "hex survives" from "everything survives", because a named colour
/// gets through the lossy `chat: &[(&str, f32)]` path too — see the
/// control below, which proves that path really does lose it.
#[test]
fn chat_spans_carry_hex_named_and_inline_legacy_colour_to_distinct_vertices() {
    use lodestone_model::text::Text;

    let hex = Text {
        content: lodestone_model::text::TextContent::Literal("Hex".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Rgb(0x1a_2b3c)),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    // The inline convention: no component-level colour at all, the `§c`
    // lives inside the literal text itself, exactly as a plugin server
    // (or the owner's server) embeds one.
    let inline_legacy = Text::literal("\u{00a7}cRed");
    let named = Text {
        content: lodestone_model::text::TextContent::Literal("Gray".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Gray),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    let root = Text {
        extra: vec![hex, inline_legacy, named],
        ..Text::default()
    };
    let spans = root.resolve(&|_| None).to_spans();
    assert_eq!(
        spans.len(),
        3,
        "sanity: three runs in, three runs out — {spans:?}"
    );

    let stats = DebugStats::default();
    let chat_spans = [(spans.as_slice(), 0.0_f32)];
    let geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat_spans: &chat_spans,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert!(
        geo.vertex_count() > 0,
        "sanity: the line must draw something at all"
    );

    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let has_colour = |rgb: (u8, u8, u8)| {
        geo.verts
            .chunks_exact(6)
            .any(|v| (byte(v[2]), byte(v[3]), byte(v[4])) == rgb)
    };
    // Pairwise-distinct RGB triples, per this repo's own fixture rule —
    // a transposition or a fallback-to-base cannot hide behind a shared
    // value. Vanilla's own named `red` and `gray` chat colours are
    // hand-transcribed from their packed RGB integers (16733525 and
    // 11184810 respectively) rather than read back through
    // `TextColor::rgb()`, which would make this `decode(encode(x)) == x`.
    let expected = [
        ("hex", (0x1a_u8, 0x2b_u8, 0x3c_u8)),
        ("inline §c", (0xff_u8, 0x55_u8, 0x55_u8)),
        ("named gray", (0xaa_u8, 0xaa_u8, 0xaa_u8)),
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter(|(_, rgb)| !has_colour(*rgb))
        .map(|(name, _)| *name)
        .collect();
    assert!(
        missing.is_empty(),
        "these colours never reached a vertex: {missing:?} (full expected set: {expected:?})"
    );

    // Control: the same three-way message, but through the lossy
    // `chat: &[(&str, f32)]` path (`Text::to_legacy_string`, which has
    // no representation for `TextColor::Rgb`). This must show the loss —
    // if it did not, the assertion above would be proving nothing about
    // which path actually carries the colour.
    let flattened = root.resolve(&|_| None).to_legacy_string();
    let chat_legacy = [(flattened.as_str(), 0.0_f32)];
    let legacy_geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat_legacy,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    let legacy_byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let legacy_has_colour = |rgb: (u8, u8, u8)| {
        legacy_geo
            .verts
            .chunks_exact(6)
            .any(|v| (legacy_byte(v[2]), legacy_byte(v[3]), legacy_byte(v[4])) == rgb)
    };
    assert!(
        !legacy_has_colour((0x1a, 0x2b, 0x3c)),
        "control failed: the legacy-string path was expected to lose the hex colour \
         (that is the bug), but it drew it anyway — this test's premise is wrong"
    );
}

/// The held-item name highlight's own version of the chat gate just
/// above: `lodestone_game::item::styled_hover_name_spans` feeding
/// `HudFrame::held_item_spans` must reach three pairwise-distinct vertex
/// RGBs, and the legacy `HudFrame::held_item` path built from the same
/// tree via `styled_hover_name`/`to_legacy_string` must lose the hex —
/// the control that proves the assertion above is measuring the right
/// path, not a coincidence.
#[test]
fn held_item_spans_carry_hex_named_and_inline_legacy_colour_to_distinct_vertices() {
    use lodestone_model::text::Text;

    let hex = Text {
        content: lodestone_model::text::TextContent::Literal("Hex".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Rgb(0x1a_2b3c)),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    // The inline convention: a server-authored item name whose colour
    // lives inside the literal text as a `§c` code rather than as a
    // component-level style — the second clause `Text::to_spans` handles
    // and `styled_hover_name_spans` inherits for free.
    let inline_legacy = Text::literal("\u{00a7}cRed");
    let named = Text {
        content: lodestone_model::text::TextContent::Literal("Gray".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Gray),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    let root = Text {
        extra: vec![hex, inline_legacy, named],
        ..Text::default()
    };
    let spans = root.resolve(&|_| None).to_spans();
    assert_eq!(
        spans.len(),
        3,
        "sanity: three runs in, three runs out — {spans:?}"
    );

    let stats = DebugStats::default();
    let geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            held_item_spans: Some((spans.clone(), 1.0)),
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert!(
        geo.vertex_count() > 0,
        "sanity: the label must draw something at all"
    );

    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let has_colour = |rgb: (u8, u8, u8)| {
        geo.verts
            .chunks_exact(6)
            .any(|v| (byte(v[2]), byte(v[3]), byte(v[4])) == rgb)
    };
    let expected = [
        ("hex", (0x1a_u8, 0x2b_u8, 0x3c_u8)),
        ("inline §c", (0xff_u8, 0x55_u8, 0x55_u8)),
        ("named gray", (0xaa_u8, 0xaa_u8, 0xaa_u8)),
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter(|(_, rgb)| !has_colour(*rgb))
        .map(|(name, _)| *name)
        .collect();
    assert!(
        missing.is_empty(),
        "these colours never reached a vertex: {missing:?} (full expected set: {expected:?})"
    );

    // Control: the same three-way name, but through the lossy
    // `held_item: Option<(String, f32)>` path — `styled_hover_name`'s
    // `Text::to_legacy_string`, which has no representation for
    // `TextColor::Rgb`. This must show the loss, or the assertion above
    // proves nothing about which field actually carries the colour.
    let flattened = root.resolve(&|_| None).to_legacy_string();
    let legacy_geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            held_item: Some((flattened, 1.0)),
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    let legacy_byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let legacy_has_colour = |rgb: (u8, u8, u8)| {
        legacy_geo
            .verts
            .chunks_exact(6)
            .any(|v| (legacy_byte(v[2]), legacy_byte(v[3]), legacy_byte(v[4])) == rgb)
    };
    assert!(
        !legacy_has_colour((0x1a, 0x2b, 0x3c)),
        "control failed: the legacy-string path was expected to lose the hex colour \
         (that is the bug), but it drew it anyway — this test's premise is wrong"
    );
}
