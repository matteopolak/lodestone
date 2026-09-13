use super::*;


/// A pixel gate using a real `EditBox` on a real screen, measured **inside
/// its own rect**, with the caret at two different positions.
///
/// Every bound here is derived from the widget rather than restated: the rect
/// comes from [`field_rect`] (the same function the draw calls) and the text
/// band from a clone of the live box repositioned into it, so the gate cannot
/// pass by agreeing with a constant that the draw does not use.
#[test]
fn the_edit_box_draws_its_text_and_its_caret_inside_its_own_rect() {
    const W: f32 = 854.0;
    const H: f32 = 480.0;
    let mut nav = test_nav("editbox-pixels");
    let mut ui = UiState::new();
    ui.open_server_list();
    nav.key(&mut ui, MenuKey::Char('a'));
    assert_eq!(ui.screen(), Screen::ServerEdit, "premise: the form is open");
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();

    // The widget as the draw sees it: a clone of the live box moved into this
    // frame's rect, exactly as `draw_edit_box` does it.
    let probe_of = |frame: &MenuFrame<'_>| -> EditBox {
        let rect = field_rect(&frame.rows, 0, W, H).expect("row 0 is the name field");
        let mut probe = frame.rows[0]
            .edit
            .clone()
            .expect("the name row must carry its EditBox, or nothing draws");
        probe.widget.x = rect.0;
        probe.widget.y = rect.1;
        probe.widget.width = rect.2;
        probe.widget.height = rect.3;
        probe
    };
    let band_of = |probe: &EditBox| -> (f32, f32, f32, f32) {
        (
            probe.text_x(),
            probe.text_y(),
            probe.inner_width(),
            // `draw_edit_box` draws at `EDIT_TEXT_SCALE`, not the
            // ordinary-row `TEXT_SCALE` — see that constant's doc for the
            // player report this measurement band would otherwise have
            // missed by 2×.
            GLYPH_H as f32 * EDIT_TEXT_SCALE,
        )
    };

    // The control, executed rather than described: an empty focused field
    // paints its caret and nothing else. If this were zero the band would be
    // pointing somewhere nothing draws and every measurement below would be of
    // the wrong rectangle.
    let empty = frame_for(&ui, &nav, &statuses, &mut fav).expect("the form owns its frame");
    let band = band_of(&probe_of(&empty));
    let blank = band_coverage(&geometry(&empty, W, H), W, H, band);
    assert!(
        blank.count > 0,
        "premise: a focused empty field paints a caret inside {band:?}, found \
         nothing — the band is in the wrong place"
    );
    let (_, blank_y0, _, blank_y1) = blank.bounds.unwrap();
    assert!(
        blank_y1 - blank_y0 < 4.0,
        "premise: with no value the band holds only the caret, so its vertical \
         extent is a bar and not a line of glyphs; got {}",
        blank_y1 - blank_y0
    );

    for c in "mc.example.com".chars() {
        nav.key(&mut ui, MenuKey::Char(c));
    }
    let typed = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let probe = probe_of(&typed);
    assert_eq!(
        band_of(&probe),
        band,
        "the field must not move between frames, or the two measurements are \
         of different rectangles"
    );
    let full = band_coverage(&geometry(&typed, W, H), W, H, band);
    assert!(
        full.count > blank.count * 8,
        "typing must paint glyphs inside the field: empty {blank:?}, typed {full:?}"
    );
    let (x0, y0, x1, y1) = full.bounds.expect("checked non-empty above");
    // The band is the *counting window*, so "it is inside the band" would be
    // vacuous. The claim is that what was painted matches the widget's **own**
    // arithmetic: the leftmost pixel is the box's `text_x` and the rightmost is
    // its caret's right edge. Both are read off the widget, never restated —
    // a draw that used the row's `PAD` (6) instead of `BORDER_INSET` (4) would
    // land two pixels out and fail here.
    let state = probe.draw_state(None);
    assert!(
        (x0 - probe.text_x()).abs() <= 0.5,
        "the value must start at the box's own text_x {}, painted from {x0}",
        probe.text_x()
    );
    assert!(
        (x1 - (state.cursor_x + probe.advance)).abs() <= 0.5,
        "the rightmost pixel must be the caret's right edge {}, painted to {x1} \
         (bounds ({x0}, {y0})..({x1}, {y1}))",
        state.cursor_x + probe.advance
    );
    assert!(
        // Margin kept proportional after the `EDIT_TEXT_SCALE` fix: the
        // blank-caret premise above requires under 4 px, so this bound
        // (5 px, i.e. `7 - 2`) still separates "just the caret bar" from
        // "a full line of glyphs" at the new, smaller scale.
        y1 - y0 >= GLYPH_H as f32 * EDIT_TEXT_SCALE - 2.0,
        "a full line of glyphs must be present, not just the caret bar: the \
         band's vertical extent is only {}",
        y1 - y0
    );

    // The caret at two positions: one Backspace and the rightmost painted
    // pixel in the band must retreat by about one character — not by nothing
    // (a frozen caret) and not by the whole field (a re-laid-out one).
    nav.key(&mut ui, MenuKey::Backspace);
    let shorter = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let after = band_coverage(&geometry(&shorter, W, H), W, H, band);
    let (_, _, x1_after, _) = after.bounds.expect("still drawing");
    let advance = probe.advance;
    assert!(
        x1_after < x1 - 1.0,
        "the caret must move left with the text: {x1} -> {x1_after}"
    );
    assert!(
        x1 - x1_after <= advance * 1.5,
        "one Backspace moved the right edge by {}, which is more than one \
         character ({advance} px)",
        x1 - x1_after
    );
    // And it landed on the shorter value's own caret, not just somewhere left.
    let shorter_probe = probe_of(&shorter);
    let shorter_state = shorter_probe.draw_state(None);
    assert!(
        (x1_after - (shorter_state.cursor_x + shorter_probe.advance)).abs() <= 0.5,
        "expected the caret's right edge at {}, painted to {x1_after}",
        shorter_state.cursor_x + shorter_probe.advance
    );
}
#[test]
fn the_edit_form_shows_both_fields_and_marks_the_focused_one() {
    use crate::menu::nav::{ADDRESS_FIELD, CANCEL_ROW, DONE_ROW, NAME_FIELD, RESOURCE_PACK_ROW};
    let mut nav = test_nav("form");
    let mut ui = UiState::new();
    ui.open_server_list();
    nav.key(&mut ui, MenuKey::Char('a'));
    for c in "abc".chars() {
        nav.key(&mut ui, MenuKey::Char(c));
    }
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    // Two text fields plus the framework conversion's three button rows.
    assert_eq!(f.rows.len(), 5);
    assert!(f.vanilla, "the framework conversion sets `vanilla`");
    assert!(f.rows[NAME_FIELD].field, "row 0 is a text field");
    assert!(f.rows[ADDRESS_FIELD].field, "row 1 is a text field");
    assert!(!f.rows[RESOURCE_PACK_ROW].field, "row 2 is a button, not text");
    assert_eq!(f.rows[NAME_FIELD].label, "abc");
    assert_eq!(f.selected, NAME_FIELD, "the name field has focus");
    // Vanilla disables Done rather than printing a message
    // — see `error_frame`'s sibling note
    // on why a `vanilla` frame's `message` is unused, and this screen's own
    // arm on why no extra label duplicates the disabled sprite.
    assert!(f.message.is_none(), "a vanilla frame draws no `message`");
    assert!(
        !f.rows[DONE_ROW].enabled,
        "an addressless form must not offer a working Done button"
    );
    assert!(f.rows[CANCEL_ROW].enabled, "Cancel always works");
    assert!(
        f.rows[RESOURCE_PACK_ROW].enabled,
        "the resource-pack row is live now that `EditForm::pack_status` \
         has somewhere real to go"
    );
    assert_eq!(
        f.rows[RESOURCE_PACK_ROW].label, "Server Resource Packs: Prompt",
        "Prompt is the default for a new entry"
    );
    for row in [NAME_FIELD, ADDRESS_FIELD, RESOURCE_PACK_ROW, DONE_ROW, CANCEL_ROW] {
        assert!(f.rows[row].slot.is_some(), "row {row} must be vanilla-placed");
    }

    nav.key(&mut ui, MenuKey::Tab);
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    assert_eq!(f.selected, ADDRESS_FIELD, "Tab moves focus to the address");
}

#[test]
fn every_vertex_lands_inside_the_viewport() {
    // The island's favourite disguise: geometry that exists and is drawn
    // entirely off-screen.
    let f = frame_with(
        vec![button("SINGLEPLAYER"), button("MULTIPLAYER"), button("QUIT")],
        1,
    );
    let v = geometry(&f, 1280.0, 720.0);
    assert!(!v.is_empty(), "a menu with rows must emit geometry");
    assert_eq!(v.len() % STRIDE, 0);
    for vert in v.chunks_exact(STRIDE) {
        assert!(
            (-1.001..=1.001).contains(&vert[0]) && (-1.001..=1.001).contains(&vert[1]),
            "vertex outside NDC: {:?}",
            &vert[..2]
        );
    }
}

#[test]
fn the_selected_row_is_visibly_different_from_its_neighbours() {
    // Reading only the vertex count cannot tell a highlight from a no-op.
    // This compares the *border colour actually painted at the row's own
    // rect*, not merely whether anything is there — the row's own fill
    // (`ROW_BG`/`ROW_SEL`) already covers those pixels regardless of
    // selection, so a colour-blind `coverage` check cannot tell
    // "outlined" from "an ordinary row" (see `coverage_of`'s docs).
    let rows = vec![button("ONE"), button("TWO"), button("THREE")];
    let (w, h) = (1280.0, 720.0);
    let sel = geometry(&frame_with(rows.clone(), 1), w, h);
    let unsel = geometry(&frame_with(rows.clone(), 99), w, h);
    assert_ne!(
        sel, unsel,
        "selecting a row must change the emitted geometry"
    );

    let rect = row_rect(&rows, 1, w, h).expect("row 1 exists");
    // The selection border is 2 px inside the row; sample its top edge.
    let border = (rect.0 + 4.0, rect.1, rect.2 - 8.0, 2.0);
    assert!(
        coverage_of(&sel, w, h, border, FG) > 0.9,
        "the highlighted row should be outlined in FG: {:?}",
        coverage_of(&sel, w, h, border, FG)
    );
    assert!(
        coverage_of(&unsel, w, h, border, FG) < 0.05,
        "an unhighlighted row must not be outlined: {:?}",
        coverage_of(&unsel, w, h, border, FG)
    );
}

#[test]
fn a_rows_text_lands_inside_that_rows_rect() {
    // Negative control included: a row's glyphs must be inside *its* rect
    // and absent from the rect below it, or the layout is off by a row.
    let rows = vec![button("AAAA"), button("BBBB")];
    let (w, h) = (1280.0, 720.0);
    let v = geometry(&frame_with(rows.clone(), 99), w, h);
    let (x, y, rw, rh) = row_rect(&rows, 0, w, h).unwrap();
    // Sample a band where the glyphs are, just right of the padding.
    let band = (x + PAD, y + rh * 0.35, text_px("AAAA", TEXT_SCALE), rh * 0.3);
    assert!(
        coverage(&v, w, h, band) > 0.25,
        "row 0's label is not in row 0's rect: {}",
        coverage(&v, w, h, band)
    );
    // And the gap between rows must be background only.
    let gap = (x, y + rh + 1.0, rw, ROW_GAP - 2.0);
    assert!(
        coverage(&v, w, h, gap) < 0.05,
        "something is drawn in the inter-row gap: {}",
        coverage(&v, w, h, gap)
    );
}

#[test]
fn row_rects_are_ordered_non_overlapping_and_on_screen() {
    let rows: Vec<MenuRow> = (0..6).map(|i| button(&format!("ROW{i}"))).collect();
    let (w, h) = (1280.0, 720.0);
    let mut prev_bottom = 0.0f32;
    for i in 0..rows.len() {
        let (x, y, rw, rh) = row_rect(&rows, i, w, h).expect("row exists");
        assert!(y >= prev_bottom, "row {i} overlaps the one above");
        assert!(x >= 0.0 && x + rw <= w, "row {i} is off-screen: {x}+{rw}");
        assert!(y + rh <= h, "row {i} runs off the bottom");
        prev_bottom = y + rh;
    }
    assert!(row_rect(&rows, 99, w, h).is_none());
}

#[test]
fn a_slotted_row_sharing_a_frame_does_not_perturb_the_centred_stacks_math() {
    // The bug this guards: `row_rect`'s centred-stack total used to sum
    // *every* row's height, including slotted ones, because no screen had
    // ever mixed the two kinds before the account screen (a scrollable
    // unslotted list plus slotted nine-slice action buttons). Build one
    // unslotted-only frame and one with an extra slotted row spliced in
    // between two unslotted rows, and require the *unslotted* rows to land
    // at identical rects in both — the slotted row must be invisible to
    // their stack.
    let (w, h) = (1280.0, 720.0);
    let plain: Vec<MenuRow> = vec![button("A"), button("B")];
    let plain_rects: Vec<_> = (0..plain.len())
        .map(|i| row_rect(&plain, i, w, h).unwrap())
        .collect();

    let mut mixed = vec![button("A")];
    mixed.push(MenuRow {
        label: "SLOTTED".to_string(),
        enabled: true,
        slot: Some(Slot {
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: 0.0,
            w: 50.0,
            h: 20.0,
        }),
        ..Default::default()
    });
    mixed.push(button("B"));

    let a_rect = row_rect(&mixed, 0, w, h).unwrap();
    let b_rect = row_rect(&mixed, 2, w, h).unwrap();
    assert_eq!(a_rect, plain_rects[0], "row A must not shift because a slotted row shares the frame");
    assert_eq!(b_rect, plain_rects[1], "row B must not shift either");

    // The slotted row itself is unaffected too — it always resolves via
    // its own `Slot`, never the stack.
    let slotted_rect = row_rect(&mixed, 1, w, h).unwrap();
    assert_eq!(slotted_rect, (1280.0 * 0.5, 0.0, 50.0, 20.0));
}

#[test]
fn default_head_icon_is_a_real_mosaic_not_a_blank_or_transparent_one() {
    // The account screen's placeholder head must actually reach pixels —
    // an all-transparent or all-zero mosaic would draw nothing and look
    // exactly like a missing icon, which is indistinguishable from this
    // function being wired to nothing.
    let m = default_head_icon();
    assert_eq!(m.size, MOSAIC);
    assert_eq!(m.cells.len(), MOSAIC * MOSAIC);
    assert!(m.cells.iter().any(|c| c[3] > 0.0), "every cell was transparent");
    // Not a flat single colour either — the hairline row and eye pixels
    // must show up as *some* variation, or `head_mosaic`'s box filter
    // could be silently discarding the source detail.
    let first = m.cells[0];
    assert!(
        m.cells.iter().any(|c| c != &first),
        "the mosaic is a single flat colour; the hand-authored detail did not survive the filter"
    );
}

#[test]
fn head_mosaic_is_the_same_drawable_favicon_mosaic_is() {
    // `head_mosaic` takes raw RGBA + dimensions (what a decoded skin's
    // face region would already be), unlike `favicon_mosaic`'s PNG bytes
    // — this pins that the two still produce the same shape of output
    // (same box filter) given equivalent solid-colour input.
    let rgba = vec![10u8, 200, 30, 255].repeat(4 * 4);
    let m = head_mosaic(&rgba, 4, 4).expect("a valid RGBA buffer must decode");
    assert_eq!(m.size, MOSAIC);
    for c in &m.cells {
        assert!((c[0] - 10.0 / 255.0).abs() < 0.01);
        assert!((c[1] - 200.0 / 255.0).abs() < 0.01);
        assert!((c[2] - 30.0 / 255.0).abs() < 0.01);
    }
}

#[test]
fn a_favicon_mosaic_reaches_the_rows_icon_square() {
    // The whole point of the favicon path: real PNG bytes → pixels in the
    // row. A solid red 8x8 PNG must fill the icon square with red.
    let png = solid_png(8, [220, 20, 20, 255]);
    let m = favicon_mosaic(&png).expect("a solid PNG must decode");
    assert_eq!(m.size, MOSAIC);
    assert_eq!(m.cells.len(), MOSAIC * MOSAIC);
    for c in &m.cells {
        assert!(c[0] > 0.8 && c[1] < 0.2 && c[2] < 0.2, "cell was {c:?}");
        assert!(c[3] > 0.9, "opaque source must stay opaque: {c:?}");
    }

    let rows = vec![MenuRow {
        label: "SERVER".into(),
        detail: "a motd".into(),
        favicon: Some(m),
        enabled: true,
        ..Default::default()
    }];
    let (w, h) = (1280.0, 720.0);
    let v = geometry(&frame_with(rows.clone(), 0), w, h);
    let (x, y, _, rh) = row_rect(&rows, 0, w, h).unwrap();
    let icon = (x + PAD, y + (rh - ICON) * 0.5, ICON, ICON);
    assert!(
        coverage(&v, w, h, icon) > 0.95,
        "the favicon square is not covered: {}",
        coverage(&v, w, h, icon)
    );

    // Negative control: the same row with no favicon leaves that square to
    // the row fill, so the assertion above is measuring the icon and not
    // the row background.
    let mut bare = rows.clone();
    bare[0].favicon = None;
    let v2 = geometry(&frame_with(bare, 0), w, h);
    assert_ne!(
        v.len(),
        v2.len(),
        "dropping the favicon must remove its quads"
    );
}

#[test]
fn a_broken_favicon_is_skipped_rather_than_panicking() {
    assert!(favicon_mosaic(b"not a png").is_none());
    assert!(favicon_mosaic(&[]).is_none());
    // A valid PNG header with a truncated body.
    assert!(favicon_mosaic(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).is_none());
}

#[test]
fn a_favicon_smaller_than_the_mosaic_still_fills_every_cell() {
    // The bug this guards: integer division leaving empty source rects and
    // therefore transparent (invisible) cells for a 4x4 icon.
    let m = favicon_mosaic(&solid_png(4, [10, 200, 40, 255])).expect("decodes");
    assert!(
        m.cells.iter().all(|c| c[3] > 0.9),
        "a {MOSAIC}-cell mosaic of a 4x4 source left transparent cells"
    );
}

#[test]
fn long_labels_are_clipped_instead_of_overrunning_the_row() {
    let rows = vec![MenuRow {
        label: "X".repeat(400),
        detail: "Y".repeat(400),
        trailing: "999/999".into(),
        enabled: true,
        ..Default::default()
    }];
    let (w, h) = (1280.0, 720.0);
    let v = geometry(&frame_with(rows.clone(), 0), w, h);
    let (x, y, rw, rh) = row_rect(&rows, 0, w, h).unwrap();
    // Nothing may be drawn to the right of the row.
    let outside = (x + rw + 2.0, y, 200.0, rh);
    assert_eq!(
        coverage(&v, w, h, outside),
        0.0,
        "text overran the row's right edge"
    );
}
