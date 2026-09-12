use super::*


/// A resource-pack row draws vanilla's selection-list entry — thumbnail, name and
/// description — and **no button fill**, with or without a `pack.png`.
///
/// This is the assertion the reported bug walked straight past. `packs::frame` has
/// always carried the icon, the label and the description, and every test in
/// `menu::packs` asserts on that frame *data* — so the suite stayed green while the
/// row was dispatched to `draw_widget` and came out as a button with one centred
/// label. The discriminator here is therefore the button fill: `ROW_BG`/`ROW_SEL`
/// is exactly what the wrong draw painted over the whole row, and a pack row must
/// paint none of it.
///
/// The built-in row is checked as well as an iconed one because it is the row a
/// player with an empty `resourcepacks/` folder sees — it has no `pack.png` at all,
/// which is how the old draw's `MenuRow::favicon` gate missed it completely.
#[test]
fn a_resource_pack_row_draws_its_icon_and_description_not_a_centred_button_label() {
    use super::draw::{PACK_DESC_DY, PACK_ENTRY_DIM, PACK_ICON, PACK_ROW_PAD, PACK_TEXT_DX};
    use crate::menu::packs::{self, MOVE_BTN, PackList, PacksNav, PacksPlacement, ROW_H, ROW_W};

    // `alpha` ships the icon and is **selected**, so it is Selected row 0 with the
    // built-in row under it; `bravo.zip` stays Available and takes the cursor
    // (control 0), which keeps the iconed row clear of the selection's own hover
    // overlay — that overlay deliberately dims the icon, and dimming it is not
    // what this gate is about.
    let nav = PacksNav::rebuild(
        vec![
            pack_fixture("alpha", "green everywhere", true),
            pack_fixture("bravo.zip", "a zip pack", false),
        ],
        &["file/alpha".to_string()],
    );
    let v = geometry(&packs::frame(&nav), V_W, V_H);
    let anchor = |list, row| {
        packs::placement_anchor(PacksPlacement::Row { list, row, scroll: 0.0 }, V_W, V_H)
    };
    // The reorder buttons live inside a Selected row's right edge (that is their
    // own gate in `menu::packs`), and they *are* buttons — so a "no button fill"
    // assertion has to stop short of them.
    let content_w = ROW_W - MOVE_BTN - 2.0 * PACK_ROW_PAD;

    // -- the iconed row ------------------------------------------------------
    let (x, y) = anchor(PackList::Selected, 0);
    let (cx, cy) = (x + PACK_ROW_PAD, y + PACK_ROW_PAD);
    let icon = coverage_of(&v, V_W, V_H, (cx, cy, PACK_ICON, PACK_ICON), PACK_FIXTURE_ICON);
    assert!(
        icon > 0.9,
        "the pack.png mosaic should fill the 32x32 icon column, covered {icon}"
    );
    let desc = coverage_of(
        &v,
        V_W,
        V_H,
        (cx + PACK_TEXT_DX, cy + PACK_DESC_DY, 100.0, LINE_H),
        PACK_ENTRY_DIM,
    );
    assert!(
        desc > 0.0,
        "the description draws in vanilla's own -8355712 grey, on the second line \
         past the icon column"
    );
    for (fill, what) in [(ROW_BG, "ROW_BG"), (ROW_SEL, "ROW_SEL"), (ROW_OFF, "ROW_OFF")] {
        let painted = coverage_of(&v, V_W, V_H, (x, y, content_w, ROW_H), fill);
        assert_eq!(
            painted, 0.0,
            "a pack row must draw no {what} button fill; it is a list entry"
        );
    }

    // -- the built-in row, which has no `pack.png` at all --------------------
    let (bx, by) = anchor(PackList::Selected, 1);
    let (bcx, bcy) = (bx + PACK_ROW_PAD, by + PACK_ROW_PAD);
    let line = (bcx + PACK_TEXT_DX, bcy + PACK_DESC_DY, 100.0, LINE_H);
    assert!(
        coverage_of(&v, V_W, V_H, line, PACK_ENTRY_DIM) > 0.0,
        "the built-in row draws its description too — it has no icon at all, which \
         is precisely the case the old `favicon`-gated draw missed"
    );
    // And it draws it *past* the icon column rather than across the row, which the
    // empty icon column here can actually witness: nothing else paints there.
    assert_eq!(
        coverage_of(
            &v,
            V_W,
            V_H,
            (bcx, bcy + PACK_DESC_DY, PACK_ICON, LINE_H),
            PACK_ENTRY_DIM
        ),
        0.0,
        "the icon column is reserved even when there is no icon to put in it"
    );
    for (fill, what) in [(ROW_BG, "ROW_BG"), (ROW_SEL, "ROW_SEL"), (ROW_OFF, "ROW_OFF")] {
        let painted = coverage_of(&v, V_W, V_H, (bx, by, ROW_W, ROW_H), fill);
        assert_eq!(painted, 0.0, "the built-in pack row must draw no {what} either");
    }
}
/// The two reorder buttons carry a **triangle**, and it points the way the button
/// says it does.
///
/// Measured by where the ink is, not by whether any is: an up arrow's apex row is
/// its narrowest, so its top half must carry less ink than its bottom half, and a
/// down arrow the reverse. A letter, a square, or a pair of arrows drawn the same
/// way round all fail that.
#[test]
fn a_move_button_draws_a_triangle_pointing_its_own_way() {
    use crate::menu::packs::{self, MOVE_BTN, PacksNav, PacksPlacement};

    // Three packs, all selected, so **row 1**'s buttons are both live (it has a
    // neighbour above and a non-built-in one below) and neither is the cursor —
    // which is control 0, Selected row 0. Row 1's arrows are therefore the only
    // `ACTIVE_LABEL` ink inside their own rects.
    let nav = PacksNav::rebuild(
        vec![
            pack_fixture("alpha", "a folder pack", false),
            pack_fixture("bravo.zip", "a zip pack", false),
            pack_fixture("charlie", "a third pack", false),
        ],
        &[
            "file/alpha".to_string(),
            "file/bravo.zip".to_string(),
            "file/charlie".to_string(),
        ],
    );
    let v = geometry(&packs::frame(&nav), V_W, V_H);

    let halves = |up: bool| {
        let (bx, by) = packs::placement_anchor(
            PacksPlacement::MoveButton { row: 1, up, scroll: 0.0 },
            V_W,
            V_H,
        );
        let half = MOVE_BTN * 0.5;
        (
            coverage_of(&v, V_W, V_H, (bx, by, MOVE_BTN, half), widget::ACTIVE_LABEL),
            coverage_of(
                &v,
                V_W,
                V_H,
                (bx, by + half, MOVE_BTN, half),
                widget::ACTIVE_LABEL,
            ),
        )
    };

    let (up_top, up_bottom) = halves(true);
    assert!(up_top > 0.0 && up_bottom > 0.0, "the up button draws something");
    assert!(
        up_top < up_bottom,
        "an up arrow's apex is its narrowest row: top {up_top} should be under bottom {up_bottom}"
    );
    let (down_top, down_bottom) = halves(false);
    assert!(
        down_top > down_bottom,
        "a down arrow is the mirror image: top {down_top} should be over bottom {down_bottom}"
    );
}

/// A selected row's **fallback** icon is painted after the row's opaque black
/// selection fill, on both screens that have one.
///
/// ## The discriminating cell is (default icon × selected), and nothing had it
///
/// A real favicon passes under both hypotheses — it is a mosaic on the colour
/// stream, so emission order always protected it — and an *unselected* default
/// icon passes too, because no fill is drawn for it. Only the pair sees the bug.
/// No gate in this file had that pair: every default-icon assertion here runs
/// through `geometry`, which builds with **no atlas** and therefore emits no
/// sprite at all, and every icon-with-an-atlas gate asks which region was
/// sampled rather than in what order. The corpus was structurally blind to it.
///
/// Three cells and two controls per screen, collected rather than asserted in the
/// loop so a neuter cannot pass by aborting on the first one.
#[test]
fn a_selected_rows_fallback_icon_paints_over_its_selection_fill() {
    use super::draw::{PACK_ICON, PACK_ROW_PAD, PACK_SELECTION_FILL};
    use super::server_list::{SERVER_LIST_SELECTION_FILL, SERVER_UNKNOWN_ICON};
    use super::world_list::WORLD_LIST_SELECTION_FILL;
    use crate::menu::packs::{self, PackList, PacksNav, PacksPlacement};

    let mut bad: Vec<String> = Vec::new();

    // -- the multiplayer list ------------------------------------------------
    let atlas = server_list_atlas();
    let (nav, ui) = list_nav("paint-order", &[("A", "a.example"), ("B", "b.example")]);
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let row_of = |f: &MenuFrame<'_>, index: usize| {
        f.rows
            .iter()
            .position(|r| r.entry.as_ref().is_some_and(|e| e.index == index))
            .expect("the frame carries the server row")
    };
    // **Which row is selected is read, not assumed.** `list_nav` adds each server
    // through the real Add Server path, which leaves the row it just created
    // selected — so a two-entry list arrives on row 1, and a gate that assumed
    // row 0 fails its own premise instead of measuring anything.
    let sel = nav.server_index();
    assert!(sel < 2, "premise: the selection is one of the two rows, not {sel}");
    let unsel = 1 - sel;
    let (rs, ru) = (row_of(&f, sel), row_of(&f, unsel));
    assert!(
        f.rows[rs].entry.as_ref().unwrap().selected,
        "premise: server row {sel} is the selection"
    );
    assert!(
        !f.rows[ru].entry.as_ref().unwrap().selected,
        "premise: server row {unsel} is not"
    );
    assert!(
        f.rows[rs].favicon.is_none() && f.rows[ru].favicon.is_none(),
        "premise: an unreachable server sends no favicon, so both rows draw the fallback sprite"
    );
    assert!(f.cursor.is_none(), "premise: no hover — this is about selection");

    let centre_of = |rect: (f32, f32, f32, f32)| (rect.0 + rect.2 * 0.5, rect.1 + rect.3 * 0.5);
    let icon_rect = |index: usize| server_entry_icon_rect(index, V_W, 0.0);
    let geo = build(&f, Some(&atlas), None, V_W, V_H);
    let fallback = loose_uv_bounds(&atlas, SERVER_UNKNOWN_ICON);

    // The cell that matters.
    let at = centre_of(icon_rect(sel));
    let order = paint_at(&geo, V_W, V_H, at);
    match fill_then_icon(&order, SERVER_LIST_SELECTION_FILL, fallback) {
        (Some(fi), Some(si)) if si > fi => {}
        (fi, si) => bad.push(format!(
            "server row {sel} (default icon, selected): fill at {fi:?}, fallback icon at {si:?} — \
             the icon must be painted after the fill. icon rect {:?}, order {order:?}",
            icon_rect(sel)
        )),
    }

    // Control 1 — the same icon in an **unselected** row: no fill at all, which
    // is what makes the cell above about the fill rather than about the sprite.
    let at1 = centre_of(icon_rect(unsel));
    let order1 = paint_at(&geo, V_W, V_H, at1);
    let (fill1, icon1) = fill_then_icon(&order1, SERVER_LIST_SELECTION_FILL, fallback);
    if fill1.is_some() || icon1.is_none() {
        bad.push(format!(
            "server row {unsel} (default icon, unselected): expected no fill and an icon, got fill \
             {fill1:?} icon {icon1:?}. icon rect {:?}, order {order1:?}",
            icon_rect(unsel)
        ));
    }

    // Control 2 — the ordering this fix replaced must **bury** the icon at the
    // same cell. Run and observed failing, not described.
    let legacy = paint_at_legacy(&geo, V_W, V_H, at);
    match fill_then_icon(&legacy, SERVER_LIST_SELECTION_FILL, fallback) {
        (Some(fi), Some(si)) if si < fi => {}
        (fi, si) => bad.push(format!(
            "the control did not fire: under the pre-fix ordering the fallback icon ({si:?}) must \
             land *before* the fill ({fi:?}) and be buried. icon rect {:?}, order {legacy:?}",
            icon_rect(sel)
        )),
    }

    // Control 3 — a **real** icon survives even the pre-fix ordering, which is
    // the asymmetry the report described: a mosaic is flat quads on the colour
    // stream, emitted after the fill, so nothing ever buried it.
    let mut mosaic = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    const PROBE: [f32; 4] = [0.25, 0.5, 0.75, 1.0];
    mosaic.rows[rs].favicon = Some(FaviconMosaic {
        size: 1,
        cells: vec![PROBE],
    });
    let mgeo = build(&mosaic, Some(&atlas), None, V_W, V_H);
    for (what, order) in [
        ("now", paint_at(&mgeo, V_W, V_H, at)),
        ("pre-fix", paint_at_legacy(&mgeo, V_W, V_H, at)),
    ] {
        let fill = order
            .iter()
            .position(|p| *p == Paint::Colour(SERVER_LIST_SELECTION_FILL));
        let cell = order.iter().position(|p| *p == Paint::Colour(PROBE));
        if !matches!((fill, cell), (Some(fi), Some(ci)) if ci > fi) {
            bad.push(format!(
                "a real favicon must outlive the fill under the {what} ordering too: fill \
                 {fill:?}, mosaic cell {cell:?}. icon rect {:?}, order {order:?}",
                icon_rect(sel)
            ));
        }
    }

    // -- the Resource Packs screen -------------------------------------------
    let patlas = pack_screen_atlas();
    let pnav = PacksNav::rebuild(vec![pack_fixture("bravo.zip", "a zip pack", false)], &[]);
    let pf = packs::frame(&pnav);
    assert_eq!(
        pf.selected, 0,
        "premise: the cursor is on Available row 0, so that row is the selection"
    );
    assert!(
        pf.rows[0].pack.is_some() && pf.rows[0].favicon.is_none(),
        "premise: a pack with no pack.png draws the fallback sprite"
    );
    let (px, py) = packs::placement_anchor(
        PacksPlacement::Row {
            list: PackList::Available,
            row: 0,
            scroll: 0.0,
        },
        V_W,
        V_H,
    );
    let pack_icon_rect = (px + PACK_ROW_PAD, py + PACK_ROW_PAD, PACK_ICON, PACK_ICON);
    let pat = centre_of(pack_icon_rect);
    let pgeo = build(&pf, Some(&patlas), None, V_W, V_H);
    let pack_fallback = loose_uv_bounds(&patlas, super::draw::PACK_UNKNOWN_ICON);

    let porder = paint_at(&pgeo, V_W, V_H, pat);
    match fill_then_icon(&porder, PACK_SELECTION_FILL, pack_fallback) {
        (Some(fi), Some(si)) if si > fi => {}
        (fi, si) => bad.push(format!(
            "pack row 0 (default icon, focused): fill at {fi:?}, fallback icon at {si:?} — the \
             icon must be painted after the fill. icon rect {pack_icon_rect:?}, order {porder:?}"
        )),
    }
    let plegacy = paint_at_legacy(&pgeo, V_W, V_H, pat);
    match fill_then_icon(&plegacy, PACK_SELECTION_FILL, pack_fallback) {
        (Some(fi), Some(si)) if si < fi => {}
        (fi, si) => bad.push(format!(
            "the pack control did not fire: under the pre-fix ordering the fallback icon ({si:?}) \
             must land before the fill ({fi:?}). icon rect {pack_icon_rect:?}, order {plegacy:?}"
        )),
    }

    // -- the singleplayer world list -----------------------------------------
    //
    // This screen drew *nothing* in its 32 px icon column until the fallback
    // landed, so the cell is "the thumbnail exists at all" as well as "it survives
    // selection". Both halves matter and neither implies the other.
    let (wnav, wui) = world_select_nav_with_worlds("paint-order-worlds", &["alpha", "bravo"]);
    let wf = world_select_frame(&wnav, &wui);
    let wrow = |index: usize| {
        wf.rows
            .iter()
            .position(|r| r.world.as_ref().is_some_and(|v| v.index == index))
            .expect("the frame carries the world row")
    };
    assert!(
        wf.rows[wrow(0)].world.as_ref().unwrap().selected
            && !wf.rows[wrow(1)].world.as_ref().unwrap().selected,
        "premise: world row 0 is the selection (most recently played) and row 1 is not"
    );
    assert!(
        wf.rows[wrow(0)].favicon.is_none(),
        "premise: nothing writes a per-world image, so the row draws the fallback"
    );
    let wgeo = build(&wf, Some(&atlas), None, V_W, V_H);
    for (index, selected) in [(0usize, true), (1usize, false)] {
        let rect = world_list_icon_rect(index, V_W, 0.0);
        let order = paint_at(&wgeo, V_W, V_H, centre_of(rect));
        let (fill, icon) = fill_then_icon(&order, WORLD_LIST_SELECTION_FILL, fallback);
        let ok = match (selected, fill, icon) {
            (true, Some(fi), Some(si)) => si > fi,
            (false, None, Some(_)) => true,
            _ => false,
        };
        if !ok {
            bad.push(format!(
                "world row {index} (selected {selected}): fill {fill:?}, thumbnail {icon:?} — a \
                 selected row needs the icon after the fill, an unselected one needs no fill and \
                 still a thumbnail. icon rect {rect:?}, order {order:?}"
            ));
        }
    }

    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The hover scrim goes **under** the icon's overlay sprites, which is the same
/// cause one step further on: `SERVER_ICON_DARKEN` is a translucent grey rect and
/// vanilla fills it before blitting the join / move arrows over it
/// (`ServerSelectionList.OnlineServerEntry.extractContent`), so the pre-fix
/// ordering washed all three arrows out.
#[test]
fn the_hover_scrim_is_painted_under_the_icon_overlay_sprites() {
    let atlas = server_list_atlas();
    let (nav, ui) = list_nav("scrim-order", &[("A", "a.example"), ("B", "b.example")]);
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let mut f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

    // The icon's right half, so `over_right_half` picks the highlighted join
    // sprite — the discriminator is position, which the quadrant gate above owns;
    // here it only has to be *a* known region.
    let (ix, iy, iw, ih) = server_entry_icon_rect(0, V_W, 0.0);
    f.cursor = Some((ix + iw * 0.75, iy + ih * 0.5));
    let at = (ix + iw * 0.75, iy + ih * 0.5);
    let geo = build(&f, Some(&atlas), None, V_W, V_H);
    let join = sprite_uv_bounds(&atlas, SERVER_JOIN_SPRITES.1);

    let order = paint_at(&geo, V_W, V_H, at);
    let scrim = order
        .iter()
        .position(|p| *p == Paint::Colour(SERVER_ICON_DARKEN));
    let arrow = order.iter().position(|p| is_sprite(*p, join.0, join.1));
    assert!(
        matches!((scrim, arrow), (Some(s), Some(a)) if a > s),
        "the join arrow ({arrow:?}) must be painted after the scrim ({scrim:?}). icon rect \
         {:?}, order {order:?}",
        (ix, iy, iw, ih)
    );

    // The control: the pre-fix ordering put the scrim on top of all three.
    let legacy = paint_at_legacy(&geo, V_W, V_H, at);
    let l_scrim = legacy
        .iter()
        .position(|p| *p == Paint::Colour(SERVER_ICON_DARKEN));
    let l_arrow = legacy.iter().position(|p| is_sprite(*p, join.0, join.1));
    assert!(
        matches!((l_scrim, l_arrow), (Some(s), Some(a)) if a < s),
        "the control did not fire: under the pre-fix ordering the arrow ({l_arrow:?}) must land \
         before the scrim ({l_scrim:?}). icon rect {:?}, order {legacy:?}",
        (ix, iy, iw, ih)
    );
}
