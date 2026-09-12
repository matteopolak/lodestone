use super::*


#[test]
fn logical_canvas_shrinks_a_retina_style_framebuffer_back_to_visual_size() {
    // A 2x HiDPI display reports a framebuffer double an ordinary window's
    // physical size for the same visual window. Auto scale must pick up
    // roughly double the scale too, so the logical canvas (what `geometry`
    // actually lays fixed pixel constants into) lands close to the same
    // apparent size in both cases — this is the fix for the "menu draws
    // half-size on Retina" report.
    let lo_dpi = logical_canvas(0, 1280, 720);
    let hi_dpi = logical_canvas(0, 2560, 1440);
    // Not a no-op: the canvas must actually shrink relative to the raw
    // framebuffer, or this is the exact island the change was for.
    assert!(hi_dpi.0 < 2560.0 && hi_dpi.1 < 1440.0);
    // And the two logical canvases must be close in size, not 2x apart,
    // which is what "half size on Retina" looked like before this existed.
    assert!(
        (lo_dpi.0 - hi_dpi.0).abs() < lo_dpi.0 * 0.5,
        "logical canvases diverged: {lo_dpi:?} vs {hi_dpi:?}"
    );
}

#[test]
fn logical_canvas_is_the_identity_at_scale_one() {
    // A tiny framebuffer forces scale 1 (see `config`'s own tests), at
    // which point the logical canvas must equal the physical one exactly —
    // this is what keeps every fixed-size `geometry` test above valid.
    assert_eq!(logical_canvas(0, 200, 200), (200.0, 200.0));
}

#[test]
fn logical_canvas_never_divides_by_zero_for_a_degenerate_framebuffer() {
    let (w, h) = logical_canvas(0, 0, 0);
    assert!(w.is_finite() && h.is_finite());
}

#[test]
fn a_narrow_viewport_does_not_produce_out_of_range_geometry() {
    // Small windows are where layout arithmetic goes negative.
    for (w, h) in [(320.0f32, 240.0f32), (200.0, 900.0), (1.0, 1.0)] {
        let rows = vec![button("ONE"), button("TWO")];
        let v = geometry(&frame_with(rows, 0), w, h);
        for vert in v.chunks_exact(STRIDE) {
            assert!(
                vert[0].is_finite() && vert[1].is_finite(),
                "non-finite vertex at {w}x{h}"
            );
        }
    }
}

#[test]
fn an_empty_menu_still_clears_the_screen() {
    // Otherwise the last world frame stays on screen behind a blank menu.
    let f = frame_with(vec![], 0);
    let v = geometry(&f, 1280.0, 720.0);
    assert!(
        v.len() >= STRIDE * 6,
        "an empty menu must still emit the background"
    );
}

/// Vanilla's own rects for its own select-world screen, hand-derived from its own source at
/// 854×480 rather than read back out of the layout — `CLAUDE.md`'s rule that
/// an expected value must originate outside the code under test.
///
/// The derivation, which is what a future reader has to be able to check:
///
/// - The header column is `LinearLayout.vertical().spacing(4)` holding a 9 px
///   vanilla-own string-label widget and a nested 200×20 row, so it measures 200×33 and the
///   header `FrameLayout` (854×49, `align(0.5, 0.5)`) puts it at
///   `((854-200)/2, (49-33)/2)` = (327, 8). The search box is one spacing plus
///   the title below that: y = 8 + 9 + 4 = **21**, *not* the 22 written in
///   vanilla's own select-world screen rendering, because the layout overwrites it.
/// - The footer's four columns are all 71: Play's 150 px spanning two columns
///   with an 8 px gutter splits `Divisor(142, 2)` = 71/71, and the four 71 px
///   buttons can only match it. So the grid is `4*71 + 3*8` = **308** wide and
///   `20 + 4 + 20` = 44 tall, and the footer frame (854×60, pinned at y 420)
///   puts it at `((854-308)/2, 420 + (60-44)/2)` = (273, 428).
/// - Within it: row 1 cells start at 0 and 158 (`71+8+71+8`), row 2 cells at
///   0, 79, 158, 237, and row 2 is 24 px down.
/// - The content band's top is `min(headerHeight + 30, height - footerHeight -
///   contentHeight)` = `min(79, 480 - 60 - 371)` = **49**, i.e. flush under the
///   header, because vanilla sizes the list to its own content-height accessor exactly.
/// - The first list row is at vanilla's own y accessor plus 2 = 51, 270 wide
///   (its own row-width accessor), 36 tall, centred: `427 - 135` = 292.
#[test]
fn the_world_select_rects_are_vanillas_own() {
    use crate::menu::world_select::WorldSelectButton as B;
    let expected = [
        (B::Play, (273.0, 428.0, 150.0, 20.0)),
        (B::Create, (431.0, 428.0, 150.0, 20.0)),
        (B::Edit, (273.0, 452.0, 71.0, 20.0)),
        (B::Delete, (352.0, 452.0, 71.0, 20.0)),
        (B::ReCreate, (431.0, 452.0, 71.0, 20.0)),
        (B::Back, (510.0, 452.0, 71.0, 20.0)),
    ];
    for (button, want) in expected {
        assert_eq!(
            world_select_slot(button).resolve(V_W, V_H),
            want,
            "{button:?} is not where vanilla puts it"
        );
    }
    // The footer's 8 px gutter, which is the pause screen's and not the title
    // screen's 4 — the same conflation `the_title_screen_rects_are_vanillas_own`
    // pins from the other side.
    let (ex, _, ew, _) = world_select_slot(B::Edit).resolve(V_W, V_H);
    let (dx, ..) = world_select_slot(B::Delete).resolve(V_W, V_H);
    assert_eq!(dx - (ex + ew), 8.0, "footer column gutter");

    assert_eq!(
        world_select_search_slot().resolve(V_W, V_H),
        (327.0, 21.0, 200.0, 20.0),
        "the search box is placed by the layout, not by its own constructor"
    );
    let title = world_select_title_label();
    assert_eq!(
        (title.origin.anchor(V_W, V_H).0 + title.dx, title.dy),
        (427.0, 8.0),
        "the title is centred at the top of the header band"
    );
    assert_eq!(title.align, Align::Centre);

    assert_eq!(world_list_row_rect(0, V_W, 0.0), (292.0, 51.0, 270.0, 36.0));
    assert_eq!(
        world_list_row_rect(1, V_W, 0.0),
        (292.0, 87.0, 270.0, 36.0),
        "rows stack by itemHeight with no gap"
    );
    assert_eq!(
        world_list_row_content_rect(0, V_W, 0.0),
        (294.0, 53.0, 266.0, 32.0),
        "CONTENT_PADDING insets the entry by 2, and 36 - 4 is the icon's 32"
    );
}

/// The slots must be the same at every canvas, or the screen is right at one
/// size and wrong everywhere else.
///
/// This is the condition `WORLD_SELECT_REF_CANVAS` rests on, and the only
/// thing that makes arranging a *canvas-dependent* container once legitimate.
/// 320×240 is the real floor `config::calculate_gui_scale` can produce; the
/// widths are even, because an odd logical width truncates in vanilla's
/// integer centring where `Origin`'s anchor does not — the same half-pixel
/// `title_slot` has always had.
#[test]
fn the_world_select_slots_do_not_depend_on_the_reference_canvas() {
    for (w, h) in [(320.0f32, 240.0f32), (854.0, 480.0), (1920.0, 1080.0)] {
        let block = WorldSelectBlock::at(w, h);
        for i in 0..2 {
            assert_eq!(
                block.header_slot(i),
                world_select_block().header_slot(i),
                "header slot {i} moved at {w}x{h}"
            );
        }
        for i in 0..crate::menu::world_select::WORLD_SELECT_BUTTONS.len() {
            assert_eq!(
                block.footer_slot(i),
                world_select_block().footer_slot(i),
                "footer slot {i} moved at {w}x{h}"
            );
        }
        assert_eq!(
            block.content_top,
            world_select_block().content_top,
            "the content band moved at {w}x{h}"
        );
    }
}

/// The frame is the screen vanilla draws: seven widgets in vanilla's order,
/// five of them present-and-disabled, at the rects the layout placed them.
#[test]
fn the_world_select_frame_is_the_screen_vanilla_draws() {
    use crate::menu::world_select::{SEARCH_FIELD, WORLD_SELECT_BUTTONS, WorldSelectButton};
    let (nav, ui) = world_select_nav("ws-frame");
    let f = world_select_frame(&nav, &ui);

    assert!(f.vanilla, "it reproduces one of vanilla's own screens");
    assert!(!f.logo, "the logo is the title screen's");
    assert_eq!(f.rows.len(), 1 + WORLD_SELECT_BUTTONS.len());

    // Row 0 is the search field, and it carries a real `EditBox` — the row
    // indices are `world_select`'s focus ids, so this is also the guard that
    // `app.rs`'s hit-test and the focus layer agree about what row 0 is.
    assert!(
        f.rows[SEARCH_FIELD].field && f.rows[SEARCH_FIELD].edit.is_some(),
        "row 0 must be the search box"
    );
    assert_eq!(
        f.selected, SEARCH_FIELD,
        "setInitialFocus puts the keyboard in the search box"
    );
    assert_eq!(f.hovered, None, "nothing is hovered before the mouse moves");

    // The six footer buttons, in vanilla's order, with vanilla's labels.
    let labels: Vec<&str> = f.rows[1..].iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "Play Selected World",
            "Create New World",
            "Edit",
            "Delete",
            "Re-Create",
            "Back",
        ]
    );
    // **Four** disabled here, not three, and that is the empty-list state rather
    // than a regression: `test_nav` points this nav at a temp data directory whose
    // `saves/` does not exist, so there is nothing selected and
    // vanilla's own update-button-status call, given no selection, greys Play as well as Edit/Delete/Re-Create.
    // Create and Back stay live, which is what keeps a fresh install off a dead
    // end. `the_world_select_frame_with_worlds_lists_them_all` is the populated
    // arm, and between them they are also each other's control: the same frame
    // builder must answer differently for the two.
    let enabled: Vec<&str> = f.rows[1..]
        .iter()
        .filter(|r| r.enabled)
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(enabled, vec!["Create New World", "Back"]);
    assert!(
        !f.rows[WorldSelectButton::Edit.row()].enabled,
        "Edit must be present and disabled"
    );
    assert!(
        !f.rows[WorldSelectButton::Play.row()].enabled,
        "Play must be present and disabled with nothing to play"
    );

    // Every row's rect is the slot the layout placed it in, through the same
    // `row_rect` `app.rs` hit-tests with.
    assert_eq!(
        row_rect(&f.rows, SEARCH_FIELD, V_W, V_H),
        Some(world_select_search_slot().resolve(V_W, V_H))
    );
    for button in WORLD_SELECT_BUTTONS {
        assert_eq!(
            row_rect(&f.rows, button.row(), V_W, V_H),
            Some(world_select_slot(button).resolve(V_W, V_H)),
            "{button:?}'s row is not at its slot"
        );
    }

    // The two free-standing strings: the title, and vanilla's own
    // no-worlds-entry's notice —
    // which is what keeps "no worlds" apart from "the list failed to draw".
    let texts: Vec<&str> = f.labels.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            crate::menu::world_select::WORLD_SELECT_TITLE,
            crate::menu::world_select::NO_WORLDS_LABEL,
        ]
    );
}

/// **The populated arm of the frame gate**: N worlds on disk become N rows, at
/// the rects `app.rs` hit-tests, carrying the three text lines
/// vanilla's own world-list-entry draws.
///
/// This is the anti-island assertion for the save list: `crate::saves` can
/// enumerate perfectly and reach zero pixels if `frame_for` never emits a row for
/// a world. It also pins the **row order** — the ids run search → buttons →
/// worlds, which is *not* the on-screen order; getting that wrong produces a
/// one-control offset at list scale (every click one control off).
#[test]
fn the_world_select_frame_with_worlds_lists_them_all() {
    use crate::menu::world_select::{FIRST_WORLD_ROW, WORLD_SELECT_BUTTONS, WorldSelectButton};
    let (nav, ui) = world_select_nav_with_worlds("ws-frame-worlds", &["alpha", "bravo"]);
    let f = world_select_frame(&nav, &ui);

    assert_eq!(
        f.rows.len(),
        1 + WORLD_SELECT_BUTTONS.len() + 2,
        "search field + six footer buttons + one row per world"
    );
    // The footer band is untouched by the list: every button is still at its own
    // slot, which is what would break if the world rows had been inserted between
    // the search field and the buttons.
    for button in WORLD_SELECT_BUTTONS {
        assert_eq!(
            row_rect(&f.rows, button.row(), V_W, V_H),
            Some(world_select_slot(button).resolve(V_W, V_H)),
            "{button:?}'s row moved when the list gained rows"
        );
    }
    // Play and Delete are live, while Edit and Re-Create are still inactive.
    assert!(f.rows[WorldSelectButton::Play.row()].enabled);
    assert!(f.rows[WorldSelectButton::Delete.row()].enabled);
    assert!(!f.rows[WorldSelectButton::Edit.row()].enabled);
    assert!(!f.rows[WorldSelectButton::ReCreate.row()].enabled);

    // No vanilla-own no-worlds-entry notice — the list is not empty.
    let texts: Vec<&str> = f.labels.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, vec![crate::menu::world_select::WORLD_SELECT_TITLE]);

    for row in 0..2 {
        let entry = &f.rows[FIRST_WORLD_ROW + row];
        let view = entry
            .world
            .as_ref()
            .expect("a world row must carry its WorldEntryView, or nothing draws");
        assert_eq!(view.index, row);
        assert_eq!(
            view.selected,
            row == 0,
            "row 0 is the selection (most recently played); row 1 is not"
        );
        assert!(entry.slot.is_none(), "a list entry is not a slotted button");
        // The three lines: display name, `folder (last played)`, `mode, Version`.
        let world = nav.world_select().world_at(row).expect("row exists");
        assert_eq!(entry.label, world.display_name);
        assert_eq!(entry.detail, world.detail_line());
        assert_eq!(entry.trailing, world.info_line());
        assert!(
            entry.detail.starts_with(&world.dir_name),
            "line 2 opens with the folder name: {:?}",
            entry.detail
        );
        assert!(
            entry.trailing.contains("Version: 26.2"),
            "line 3 carries the world version: {:?}",
            entry.trailing
        );
        // The rect is the list geometry, not a slot — and it is the same one the
        // hit-test reads.
        assert_eq!(
            row_rect(&f.rows, FIRST_WORLD_ROW + row, V_W, V_H),
            Some(world_list_row_rect(row, V_W, 0.0)),
            "world row {row} is not at its list rect"
        );
    }
    // `alpha` was planted first and therefore has the later `LastPlayed`, so it
    // sorts to row 0. Asserted so the ordering is not merely "some order".
    assert_eq!(f.rows[FIRST_WORLD_ROW].label, "alpha");
    assert_eq!(f.rows[FIRST_WORLD_ROW + 1].label, "bravo");

    // -- control ---------------------------------------------------------
    // A row beyond the list must report no rect at all, or a click below the last
    // world would hit-test onto a row that is nowhere near the cursor.
    assert_eq!(
        row_rect(&f.rows, FIRST_WORLD_ROW + 2, V_W, V_H),
        None,
        "there are only two worlds"
    );
    // And the visibility gate really does reject: at a canvas too short for row
    // 1, row 1 has no rect while row 0 still does.
    let short = world_list_row_top(1, 0.0) + 40.0;
    assert!(
        world_list_row_visible(0, V_H, 0.0) && !world_list_row_visible(1, short, 0.0),
        "the visibility gate does not discriminate: rows visible at {V_H} = {}, \
         at {short} = {}",
        world_list_visible_rows(V_H),
        world_list_visible_rows(short)
    );
    assert_eq!(row_rect(&f.rows, FIRST_WORLD_ROW + 1, V_W, short), None);
}

/// Every world-select button draws the sprite the widget layer picks, at the
/// rect the layout placed it in.
///
/// The same gate `every_title_and_pause_widget_draws_the_sprite_the_widget_layer_picks`
/// makes for the other two screens, and for the same reason: without it
/// `world_select_slot` and `WorldSelectButton::enabled` could both be correct
/// and reach zero pixels. The `enabled` flags come from the **real frame**, so
/// this cannot drift from what the screen actually says.
#[test]
fn every_world_select_button_draws_the_sprite_the_widget_layer_picks() {
    use crate::menu::world_select::WORLD_SELECT_BUTTONS;
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    let (nav, ui) = world_select_nav("ws-sprites");
    let frame = world_select_frame(&nav, &ui);

    // The premise: the screen really does carry a mix, or "the disabled
    // sprite was chosen" is never exercised.
    assert!(
        frame.rows[1..].iter().any(|r| r.enabled) && frame.rows[1..].iter().any(|r| !r.enabled),
        "this screen no longer has both an enabled and a disabled button"
    );
    // And the rects are really distinct, or a widget stuck at one position
    // could still pass.
    let distinct: std::collections::BTreeSet<(i32, i32)> = WORLD_SELECT_BUTTONS
        .iter()
        .map(|b| {
            let (x, y, ..) = world_select_slot(*b).resolve(V_W, V_H);
            (x as i32, y as i32)
        })
        .collect();
    assert_eq!(distinct.len(), WORLD_SELECT_BUTTONS.len());

    for button in WORLD_SELECT_BUTTONS {
        let row = frame.rows[button.row()].clone();
        let enabled = row.enabled;
        for focused in [false, true] {
            let mut f = frame_with(vec![row.clone()], if focused { 0 } else { 99 });
            f.vanilla = true;
            let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;

            let expected = widget::BUTTON_SPRITES.get(enabled, focused);
            let (min, max) = sprite_uv_bounds(&atlas, expected);
            assert!(
                all_uvs_within(&sprite, min, max),
                "{button:?} (enabled={enabled}, focused={focused}) did not sample \
                 {expected}, which is what WidgetSprites::get selects"
            );
            // Per-case control: flipping `active` must move the sample off
            // this region. For the five disabled buttons this is the
            // assertion run in reverse — an enabled Create New World must
            // *not* sample `widget/button_disabled`.
            let flipped = widget::BUTTON_SPRITES.get(!enabled, focused);
            if flipped != expected {
                let (fmin, fmax) = sprite_uv_bounds(&atlas, flipped);
                assert!(
                    !all_uvs_within(&sprite, fmin, fmax),
                    "the detector cannot tell {expected} from {flipped}"
                );
            }

            let drawn = sprite_dest_bounds(&sprite, V_W, V_H);
            let want = world_select_slot(button).resolve(V_W, V_H);
            let same = [
                (drawn.0, want.0),
                (drawn.1, want.1),
                (drawn.2, want.2),
                (drawn.3, want.3),
            ]
            .iter()
            .all(|(a, b)| (a - b).abs() < 0.01);
            assert!(
                same,
                "{button:?} (enabled={enabled}, focused={focused}) drew at {drawn:?}, \
                 not at {want:?} where the layout placed it"
            );
        }
    }
}

/// A disabled world-select button's label is vanilla's grey, and it is that
/// exact value.
///
/// Predicted, not asserted as a direction — `CLAUDE.md`'s *magnitude*
/// species. The expectation comes from vanilla's own abstract-widget base's
/// `-6250336` unpacked by `widget::argb_to_rgba`, and the enabled button
/// beside it is the control that says the measurement can tell them apart.
#[test]
fn a_disabled_world_select_label_lands_on_vanillas_grey() {
    use crate::menu::world_select::WorldSelectButton as B;
    let (nav, ui) = world_select_nav("ws-grey");
    let frame = world_select_frame(&nav, &ui);
    let grey = widget::argb_to_rgba(widget::INACTIVE_MESSAGE_ARGB);
    assert_eq!(grey, widget::INACTIVE_LABEL);

    for (button, want, name) in [
        // Create is live; Edit remains present and disabled, so it provides the
        // disabled example here.
        (B::Edit, grey, "disabled"),
        (B::Back, widget::ACTIVE_LABEL, "enabled"),
    ] {
        let row = frame.rows[button.row()].clone();
        let rect = world_select_slot(button).resolve(V_W, V_H);
        let mut f = frame_with(vec![row], 99);
        f.vanilla = true;
        let colour = build(&f, None, None, V_W, V_H).colour;
        assert!(
            coverage_of(&colour, V_W, V_H, rect, want) > 0.0,
            "{button:?}'s {name} label did not reach {want:?} inside {rect:?}"
        );
        // The control: the *other* colour must not appear in the same rect,
        // or "the label is grey" is satisfied by a frame containing both.
        let other = if want == grey {
            widget::ACTIVE_LABEL
        } else {
            grey
        };
        assert_eq!(
            coverage_of(&colour, V_W, V_H, rect, other),
            0.0,
            "{button:?} drew {other:?} as well, so the colour is not a discriminator"
        );
    }
}

/// The list draws its one row, inside row 0's own content rect.
///
/// This is the assertion that keeps "the list has a world" distinguishable
/// from "the list failed to draw" — without it the two are the same picture,
/// which is exactly the absence-needs-a-control rule. It is also the pixel
/// pixel half of the world list: the button that launches is only honest if the
/// world it launches is on screen. The band is the row's content rect from
/// `world_list_row_content_rect`, the same expression the label's position is
/// derived from, and the failure output is a bounding box rather than a
/// fraction.
///
/// Two controls, both executed: the band *below* the row must be empty (so
/// this is not measuring a frame that paints everywhere), and the same band
/// on the **title screen** must be empty too (so it is not measuring
/// something every menu draws there).
#[test]
fn the_empty_world_list_draws_its_notice_inside_row_zeros_content_rect() {
    let (nav, ui) = world_select_nav("ws-row");
    assert_eq!(
        nav.world_select().shown_len(),
        0,
        "premise: this nav's temp saves root is empty, so the notice is what draws"
    );
    let frame = world_select_frame(&nav, &ui);
    let colour = geometry(&frame, V_W, V_H);

    let band = world_list_row_content_rect(0, V_W, 0.0);
    let inside = band_coverage(&colour, V_W, V_H, band);
    assert!(
        inside.count > 0,
        "the empty-list notice reached no pixels inside {band:?}"
    );
    let bounds = inside.bounds.expect("a non-empty band has bounds");
    // It is a line of text, not a full-height fill: the notice is 9 px of
    // glyphs centred in a 32 px box, so its vertical extent must be well
    // short of the band's.
    assert!(
        bounds.3 - bounds.1 < band.3 * 0.75,
        "what drew in {band:?} spans {:?} vertically — that is a fill, not a line of text",
        (bounds.1, bounds.3)
    );
    // And it is centred, so it must straddle the screen's own centre line.
    assert!(
        bounds.0 < V_W * 0.5 && bounds.2 > V_W * 0.5,
        "the notice is not centred: bounds {bounds:?}"
    );

    // -- control 1: the row below it is empty ----------------------------
    let empty_band = world_list_row_content_rect(1, V_W, 0.0);
    assert_eq!(
        band_coverage(&colour, V_W, V_H, empty_band).count,
        0,
        "something drew in row 1 as well, so the band is not a discriminator: {:?}",
        band_coverage(&colour, V_W, V_H, empty_band).bounds
    );

    // -- control 2: the same band on the title screen is empty -----------
    // What else already paints here? On the title screen, nothing: the logo
    // ends at y 94 and the button column starts at 168, and row 0's content
    // rect is y 53..85. If that ever stops being true this fires, which is
    // the point.
    let title_nav = test_nav("ws-empty-control");
    let title_ui = UiState::new();
    assert_eq!(title_ui.screen(), Screen::MainMenu, "the control is the title");
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let title = frame_for(&title_ui, &title_nav, &statuses, &mut fav).expect("title frame");
    let title_colour = geometry(&title, V_W, V_H);
    assert_eq!(
        band_coverage(&title_colour, V_W, V_H, band).count,
        0,
        "the title screen already paints in {band:?}, so control 1 measures nothing: {:?}",
        band_coverage(&title_colour, V_W, V_H, band).bounds
    );
}

/// **The save list reaches pixels, one band per world.**
///
/// This is the re-derivation of `the_world_list_draws_its_one_row_…` for N rows,
/// and it is a strictly stronger gate rather than the same one loosened: the
/// single-row version could not distinguish "the list drew" from "row 0 drew",
/// which is exactly the failure a hardcoded row has. Every row's band is measured
/// separately, so a list that draws only its first entry fails.
///
/// Three things are asserted per row, and the second is what a coverage *fraction*
/// could never see:
///
/// 1. ink inside that row's own content rect;
/// 2. the ink starts to the **right** of the reserved 32 px icon column
///    (vanilla's own text-x accessor equals content-x plus 32 plus 3) — measured by location, not by amount,
///    which is `CLAUDE.md`'s "ask where, not what";
/// 3. it is three short lines of text, not a fill: the vertical extent spans more
///    than one 9 px line and less than the band.
///
/// The controls: the band **after** the last world must be empty (so the bands
/// discriminate at all), and the same bands on the title screen must be empty (so
/// this is not measuring something every menu paints there — the question
/// `CLAUDE.md` says to ask before believing a control).
#[test]
fn every_world_in_the_list_draws_inside_its_own_row_band() {
    use crate::menu::render::WORLD_LIST_TEXT_DX;
    let names = ["alpha", "bravo", "charlie"];
    let (nav, ui) = world_select_nav_with_worlds("ws-rows-pixels", &names);
    let frame = world_select_frame(&nav, &ui);
    let colour = geometry(&frame, V_W, V_H);

    for row in 0..names.len() {
        let band = world_list_row_content_rect(row, V_W, 0.0);
        let inside = band_coverage(&colour, V_W, V_H, band);
        assert!(
            inside.count > 0,
            "world row {row} reached no pixels inside {band:?}"
        );
        let bounds = inside.bounds.expect("a non-empty band has bounds");
        // (2) The text column starts past the icon column, for **every** row
        // including the selected one: vanilla's own item-extraction's outline is
        // drawn at the *row*
        // rect's edge, which is vanilla's own content-padding constant outside this band, and its
        // interior fill is black — so the leftmost thing inside a content rect is
        // always vanilla's own text-x accessor. That equality is checked rather than an inequality
        // where it can be: vanilla's own text-x accessor is `contentX + 32 + 3` exactly.
        assert!(
            bounds.0 >= band.0 + WORLD_LIST_TEXT_DX,
            "row {row}'s text must start past the {WORLD_LIST_TEXT_DX} px icon \
             column: leftmost ink at {} for a band starting at {}",
            bounds.0,
            band.0
        );
        // (3) Three lines of 9 px text in a 32 px box: taller than one line,
        // shorter than the band.
        let ink_h = bounds.3 - bounds.1;
        assert!(
            ink_h > 9.0 && ink_h <= band.3,
            "row {row} drew {ink_h} px vertically in a {} px band — three text \
             lines measure more than one line and no more than the band",
            band.3
        );
    }

    // The selection outline — vanilla's own abstract-selection-list item
    // extraction — is drawn for the selected row **only**, at the row rect's own left edge.
    // Sampled by colour at a 1 px column there, which is a different measurement
    // from the vertex bands above and is the one that discriminates a selected row
    // from an unselected one. Its own control is the second assertion: row 1 is not
    // selected and must not have it.
    let selected_edge = |row: usize| {
        let (rx, ry, _, rh) = world_list_row_rect(row, V_W, 0.0);
        coverage_of(&colour, V_W, V_H, (rx, ry + 2.0, 1.0, rh - 4.0), [1.0, 1.0, 1.0, 1.0])
    };
    assert!(
        selected_edge(0) > 0.5,
        "row 0 is the selection and must draw `extractItem`'s white outline at the \
         row's left edge: coverage {}",
        selected_edge(0)
    );
    assert!(
        selected_edge(1) < 0.1,
        "row 1 is not selected and must have no outline: coverage {}",
        selected_edge(1)
    );

    // -- control 1: the band after the last world is empty ---------------
    let after = world_list_row_content_rect(names.len(), V_W, 0.0);
    let empty = band_coverage(&colour, V_W, V_H, after);
    assert_eq!(
        empty.count, 0,
        "something drew in the band after the last world, so the per-row bands are \
         not discriminators: {:?}",
        empty.bounds
    );

    // -- control 2: the same bands on the title screen are empty ---------
    // Asked of *every* band this test measures, not only row 0's: rows 1 and 2 sit
    // lower on screen, where the title screen's button column begins at y 168, so
    // "what else already paints here" has a different answer per row.
    let title_nav = test_nav("ws-rows-pixels-control");
    let title_ui = UiState::new();
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let title = frame_for(&title_ui, &title_nav, &statuses, &mut fav).expect("title frame");
    let title_colour = geometry(&title, V_W, V_H);
    for row in 0..names.len() {
        let band = world_list_row_content_rect(row, V_W, 0.0);
        let painted = band_coverage(&title_colour, V_W, V_H, band);
        assert_eq!(
            painted.count, 0,
            "the title screen already paints in row {row}'s band {band:?}, so this \
             test's assertion for that row measures nothing: {:?}",
            painted.bounds
        );
    }
}

/// The empty-list notice fits the row it is centred in.
///
/// Vanilla's own no-worlds-entry type gives its own string-label widget no max-width
/// clamp, so nothing clips it and a longer
/// string would overhang the row. Measured with [`text_px`], the same
/// fixed-advance measure the jar-less draw uses — the real vanilla font is
/// narrower, so this is the conservative direction.
///
/// `BUNDLED_WORLD.label` is measured too even though nothing draws it any more:
/// its length was the reason that constant was written the way it was, and the
/// measurement is the only place that fact survives.
#[test]
fn the_world_list_row_label_fits_the_row_it_is_centred_in() {
    let (.., content_w, _) = world_list_row_content_rect(0, V_W, 0.0);
    // **Both** empty-list strings, not just this target's. The label is
    // `cfg`-selected (a browser says the list is never saved, not "not yet"), and this
    // test runs on the host — so measuring `NO_WORLDS_LABEL` alone would leave the
    // browser string unmeasured on every machine that runs the suite. The first draft
    // of it was 53 characters against a 44-character row and nothing would have
    // caught it. A guard only covers what it names.
    for label in [
        crate::menu::world_select::NO_WORLDS_LABEL_NATIVE,
        crate::menu::world_select::NO_WORLDS_LABEL_BROWSER,
        crate::menu::world_select::BUNDLED_WORLD.label,
    ] {
        let measured = text_px(label, 1.0);
        assert!(
            measured <= content_w,
            "{label:?} measures {measured} px in a {content_w} px row"
        );
    }
}

/// A world row's three lines are clipped to vanilla's own world-list-entry's
/// own max-text-width, so a long name cannot overhang the row.
///
/// Unlike the notice above, these lines *are* clipped in vanilla
/// (its own string-label widget's max-width clamp, applied by its own
/// world-selection list rendering), so the assertion
/// is about the **draw** rather than about the string: a 200-character world name
/// must still paint nothing outside its row.
#[test]
fn a_long_world_name_is_clipped_to_its_row_rather_than_overhanging_it() {
    let long: String = std::iter::repeat_n('W', 200).collect();
    let (nav, ui) = world_select_nav_with_worlds("ws-long-name", &[long.as_str()]);
    let frame = world_select_frame(&nav, &ui);
    let colour = geometry(&frame, V_W, V_H);

    let (rx, ry, rw, rh) = world_list_row_rect(0, V_W, 0.0);
    // The band immediately to the right of the row must be untouched.
    let right_of = (rx + rw, ry, 80.0, rh);
    let spill = band_coverage(&colour, V_W, V_H, right_of);
    assert_eq!(
        spill.count, 0,
        "a long world name spilled out of its row into {right_of:?}: {:?}",
        spill.bounds
    );

    // -- control ---------------------------------------------------------
    // The band it must *not* spill into has to be one this frame could paint in,
    // or the assertion is vacuous. The row itself is inked, and the clip width is
    // less than the row width — so there is real text being cut.
    assert!(
        band_coverage(&colour, V_W, V_H, world_list_row_content_rect(0, V_W, 0.0)).count > 0,
        "the row drew nothing at all, so the no-spill assertion measures nothing"
    );
    assert!(
        text_px(&long, 1.0) > crate::menu::render::world_list_text_width(),
        "the name is not actually long enough to be clipped, so nothing was cut"
    );
}

