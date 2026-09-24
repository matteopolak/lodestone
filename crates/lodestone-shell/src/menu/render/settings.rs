use super::*;


/// **The settings overlap gate** (the player report of 2026-08-07): a settings
/// list row is **cut** at the band's bottom rather than painted into the gap above
/// the footer.
///
/// This is the pixel half of the defect `Origin::is_scrolling_list_row` fixed.
/// Every settings-tree list draws its rows as *slotted widgets*, so they went down
/// `draw`'s unclipped path while the three `MenuRow::entry`/`account`/`world`
/// screens were clipped — and a row that overran the band painted straight over
/// the footer's Done button.
///
/// **What else already paints here** was asked first, and it is why the rect is
/// the gap *between* the band's bottom and the footer's own top rather than
/// "everything below the band": the footer paints in the latter. Both edges are
/// derived — the band from the `ListSpec::model` the clip is built from, the footer
/// from `options::footer_rects` — and the control is the same frame with its
/// `ListSpec` removed, which takes the unclipped branch and must fail the same
/// assertion.
#[test]
fn a_settings_row_is_cut_at_the_bands_bottom_rather_than_painted_over_the_footer() {
    use crate::menu::options::{self, SettingsPage};

    const H: f32 = 480.0;
    let page = SettingsPage::Video;
    let mut nav = test_nav("settings-clip-pixels");
    let mut ui = UiState::new();
    ui.open_settings();
    // Reached the way a player reaches it: the root page's own Video nav button,
    // found in the row list `app.rs` hit-tests rather than by index. That makes
    // this an anti-island premise too — if the button no longer opens the page,
    // the setup fails instead of the assertion.
    let video_row = nav
        .settings()
        .visible()
        .iter()
        .position(|c| {
            matches!(c.cell, options::Cell::Nav { page: Some(p), .. } if p == page)
        })
        .expect("premise: the root page carries a Video nav button");
    nav.click(&mut ui, video_row);
    assert_eq!(nav.settings().page(), page, "premise: the Video page is up");

    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let frame = frame_for(&ui, &nav, &statuses, &mut fav).expect("settings owns its frame");
    let spec = frame
        .list
        .as_ref()
        .expect("premise: the settings page declares a ListSpec");
    let model = spec.model(H).expect("premise: the Video page scrolls at 480");
    let band_bottom = model.top() + model.height();
    let footer_top = options::footer_rects(V_W, H, 1)
        .first()
        .expect("the footer has a Done button")
        .1;
    assert!(
        footer_top > band_bottom,
        "premise: there must be a gap between the band's bottom ({band_bottom}) and \
         the footer's top ({footer_top})"
    );
    let row_x = options::row_left(V_W, 0);
    let gutter = (row_x, band_bottom, options::BIG_BUTTON_WIDTH, footer_top - band_bottom);

    // Premise: some entry really does straddle the band's bottom edge at this
    // offset, or there is nothing for a clip to cut.
    let entries = page.entries();
    let straddler = (0..entries.len()).find(|e| {
        let (_, y) = options::list_cell_origin(page, *e, 0.0, 0, V_W, H);
        y < band_bottom && y + options::WIDGET_H > band_bottom
    });
    assert!(
        straddler.is_some(),
        "premise: no Video entry straddles the band's bottom at scroll 0, so this \
         gate cannot see a clip"
    );

    let clipped = band_coverage(&geometry(&frame, V_W, H), V_W, H, gutter);
    assert_eq!(
        clipped.count, 0,
        "a settings row painted into the gutter between the list band and the \
         footer: {:?}",
        clipped.bounds
    );

    // -- control, executed ---------------------------------------------------
    let mut unclipped = frame.clone();
    unclipped.list = None;
    let spilled = band_coverage(&geometry(&unclipped, V_W, H), V_W, H, gutter);
    assert!(
        spilled.count > 0,
        "the unclipped frame painted nothing in the gutter either, so the clip is \
         not what the assertion above measured"
    );

    // And the footer's own row is **not** clipped away — the predicate excludes it
    // deliberately, and erasing it would be the opposite defect.
    let done_rect = (footer_top - 1.0, options::DONE_WIDTH);
    let done = band_coverage(
        &geometry(&frame, V_W, H),
        V_W,
        H,
        ((V_W - done_rect.1) * 0.5, done_rect.0, done_rect.1, options::WIDGET_H + 2.0),
    );
    assert!(
        done.count > 0,
        "the footer's Done button was clipped away with the list rows — a footer \
         button is not a list row"
    );
}
/// **The reported bug** (2026-08-09): *"some settings menus (like the Music &
/// Sounds) still overlaps buttons at the bottom … in vanilla the button(s) anchored
/// at the bottom have their own section, with a horizontal bar separating it … the
/// middle section has a bit of a black filter in the background that tints the
/// panorama"*.
///
/// The band's chrome, measured **by location and by exact colour**: the two
/// separators sit on the band's own edges and the tint fills the band and stops
/// there. That is what makes header / content / footer read as three sections, and
/// it is the piece that was missing — the clip already reserved the footer band
/// (see `every_settings_page_keeps_its_footer_band_clear_at_every_scroll_offset`),
/// so a row cut flush against an untinted, unfenced edge was the whole of what the
/// owner saw.
///
/// Every expected value originates **outside this crate**: the four colours are the
/// pixel values of `textures/gui/{header,footer}_separator.png` and
/// `menu_list_background.png` decoded out of the 26.2 `client.jar`, and the two
/// y offsets are vanilla's own list-separator extraction's own y-minus-2 and its
/// own bottom accessor. The
/// rects come from `ListSpec::chrome_rect`, the same call the draw makes.
///
/// **What else paints here** was asked first, and it decides every sample point:
/// the separator rows are outside the band, where only the title (centred, 9 px
/// tall, in the 33 px header) and the footer's Done (a 200 px button 7 px lower)
/// paint — so a sample at the canvas's left edge is clear of both. The tint sample
/// is likewise in the left margin, clear of the 310 px row column.
#[test]
fn the_settings_band_carries_vanillas_separators_and_its_tint() {
    use crate::menu::options::SettingsPage;

    // A canvas short enough that Sound overflows its band, so this is the same
    // frame the report was made against rather than a roomy one.
    const W: f32 = 320.0;
    const H: f32 = 240.0;
    let (nav, ui) = settings_nav_on(SettingsPage::Sound);
    let statuses = crate::menu::status::StatusCache::with_probe(
        crate::menu::status::unavailable_probe(),
    );
    let mut fav = FaviconCache::new();
    let frame = frame_for(&ui, &nav, &statuses, &mut fav).expect("settings owns its frame");
    let spec = frame.list.as_ref().expect("premise: Sound declares a ListSpec");
    let list = spec.model(H).expect("premise: Sound has a band at 240");
    let (cx, cy, cw, ch) = spec
        .chrome_rect(&list, W)
        .expect("premise: a settings list declares canvas-wide chrome");
    assert_eq!(
        (cx, cy, cw, ch),
        (0.0, 33.0, W, H - 33.0 - 33.0),
        "the chrome rect is not the 33 px-header / 33 px-footer band vanilla's \
         own options-list type is sized to"
    );
    let v = geometry(&frame, W, H);

    // A column in the left margin: outside the 310 px row column, so nothing but
    // the chrome can paint here.
    let probe_x = 4.0;
    let at = |y: f32| {
        colour_at(&v, 2.0 * probe_x / W - 1.0, 1.0 - 2.0 * y / H)
    };
    let expect = |y: f32, want: [f32; 4], what: &str| {
        let got = at(y);
        assert!(
            got.is_some_and(|c| c.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.002)),
            "{what} at y={y} is {got:?}, not the decoded {want:?}"
        );
    };
    // `header_separator.png` is light over dark; `footer_separator.png` is the
    // mirror. Sampled at row centres, since a quad spans `y..y+1`.
    expect(cy - SEPARATOR_H + 0.5, SEPARATOR_LIGHT, "the header bar's light row");
    expect(cy - 1.0 + 0.5, SEPARATOR_DARK, "the header bar's dark row");
    expect(cy + ch + 0.5, SEPARATOR_DARK, "the footer bar's dark row");
    expect(cy + ch + 1.5, SEPARATOR_LIGHT, "the footer bar's light row");
    // And the tint, over the band's whole height in that column.
    let margin = (0.0, cy, options::row_left(W, 0), ch);
    let tinted = coverage_of(&v, W, H, margin, LIST_BAND_TINT);
    assert_eq!(
        tinted, 1.0,
        "the band's left margin {margin:?} is only {tinted} tinted — the black filter \
         over the panorama does not cover the band"
    );

    // The tint **stops** at the band. Two rows that must not carry it: one above the
    // header bar, one inside the footer band beside the Done button. This is the
    // "own section" half of the report; a tint that ran the whole canvas would
    // satisfy the assertion above and still be wrong.
    for (y, what) in [
        (cy - SEPARATOR_H - 1.5, "above the header bar"),
        (cy + ch + SEPARATOR_H + 1.5, "below the footer bar"),
    ] {
        let got = at(y);
        assert!(
            !got.is_some_and(|c| c.iter().zip(LIST_BAND_TINT).all(|(a, b)| (a - b).abs() < 0.002)),
            "the band tint leaked {what} (y={y}), so the content band is not fenced off"
        );
    }

    // -- control, executed ---------------------------------------------------
    // The same frame with its `ListSpec` removed draws no chrome at all, so every
    // assertion above is measuring the chrome rather than something else that
    // happens to paint at those rows. Observed failing: `expect` on the header bar's
    // light row reports `None` here.
    let mut bare = frame.clone();
    bare.list = None;
    let bv = geometry(&bare, W, H);
    let bare_at = colour_at(&bv, 2.0 * probe_x / W - 1.0, 1.0 - 2.0 * (cy - SEPARATOR_H + 0.5) / H);
    assert!(
        bare_at.is_none(),
        "a frame with no ListSpec still painted {bare_at:?} where the header bar goes, \
         so the chrome is not what the assertions above measured"
    );
    let bare_tint = coverage_of(&bv, W, H, margin, LIST_BAND_TINT);
    assert_eq!(
        bare_tint, 0.0,
        "a frame with no ListSpec still tinted {bare_tint} of the band"
    );
}

/// **Hover tooltips on the settings tree** — the fourth item of the 2026-08-09
/// report: *"some inputs/buttons have tooltips when i hover"*.
///
/// Four claims, each measured by location and by the tooltip fill's own colour, and
/// the last two are the controls:
///
/// 1. hovering a row that carries a vanilla-own option-instance tooltip paints a `TOOLTIP_BG`
///    box, and it lands **at the cursor** rather than somewhere plausible;
/// 2. its content is really wrapped — a multi-line string produces a box taller than
///    a one-line one, so vanilla's own tooltip max-width is being applied rather than ignored;
/// 3. hovering a row with **no** tooltip in the table paints no box (so the box is
///    not unconditional);
/// 4. hovering where a row *used to be* before the list scrolled paints no box (so
///    the band guard in `menu_row_under` reaches this path too — a hint must not
///    hang over the footer for a row nothing can click).
///
/// The expected values originate outside this crate: which accessors have tooltips
/// and what their text is came out of vanilla's own persisted-options declarations'
/// `cachedConstantTooltip` sites
/// resolved through `en_us.json`, and the box geometry is
/// vanilla's own tooltip-render utility's own padding and mouse offset. The cursor is set through
/// `MenuNav::set_menu_cursor`, the same call `app`'s mouse-move path makes.
#[test]
fn hovering_a_settings_row_shows_its_option_tooltip_and_only_then() {
    use crate::menu::options::{self, SettingsPage};

    const W: f32 = 854.0;
    const H: f32 = 480.0;
    let statuses = crate::menu::status::StatusCache::with_probe(
        crate::menu::status::unavailable_probe(),
    );

    // Accessibility carries the most tooltipped rows of any page.
    let (mut nav, ui) = settings_nav_on(SettingsPage::Accessibility);

    // The two subject rows, found by asking the same `Cell::tooltip` the frame
    // stamps — one with a tooltip and one without.
    let visible = nav.settings().visible();
    let tipped = visible
        .iter()
        .position(|c| c.cell.tooltip().is_some())
        .expect("premise: some Accessibility row has a tooltip");
    let without = visible
        .iter()
        .position(|c| c.cell.tooltip().is_none())
        .expect("premise: some Accessibility row has no tooltip");

    // Put the cursor in the middle of the row's own rect — from `row_rect`, the
    // function the hit-test and the draw both place rows by — through
    // `set_menu_cursor`, the call `app`'s mouse-move path makes. Returns the
    // `TOOLTIP_BG` fill's own bounding box, which is one quad, so `colour_bounds`
    // gives it exactly rather than by sampling.
    let hover_box = |nav: &mut MenuNav,
                     ui: &crate::menu::UiState,
                     row: usize|
     -> (Option<(f32, f32, f32, f32)>, (f32, f32)) {
        let probe = frame_for(ui, nav, &statuses, &mut FaviconCache::new()).expect("a frame");
        let (rx, ry, rw, rh) = row_rect(&probe.rows, row, W, H).expect("the row has a rect");
        nav.set_menu_cursor(rx + rw * 0.5, ry + rh * 0.5, W, H);
        let f = frame_for(ui, nav, &statuses, &mut FaviconCache::new()).expect("a frame");
        let at = f.cursor.expect("the cursor is stamped on the frame");
        (
            colour_bounds(&geometry(&f, W, H), W, H, TOOLTIP_BG),
            at,
        )
    };

    let (tip_box, at) = hover_box(&mut nav, &ui, tipped);
    let (bx, by, _, bh) =
        tip_box.expect("hovering a row with a tooltip painted no TOOLTIP_BG box at all");
    // (1) It is **at the cursor**, not somewhere plausible. Vanilla's own
    // tooltip-render utility's
    // content box starts `MOUSE_OFFSET` right of and above the cursor and the fill is
    // `PAD` larger on every side, so both edges are exact — this canvas is wide and
    // tall enough that `draw_tooltip`'s off-screen nudge cannot fire.
    //
    // The tolerance is the pixel -> NDC -> pixel round trip inside `colour_bounds`,
    // which costs ~4e-6 at this canvas. It is not slack in the prediction.
    let want = (
        at.0 + TOOLTIP_MOUSE_OFFSET - TOOLTIP_PAD,
        at.1 - TOOLTIP_MOUSE_OFFSET - TOOLTIP_PAD,
    );
    assert!(
        (bx - want.0).abs() < 0.001 && (by - want.1).abs() < 0.001,
        "the tooltip's fill is at ({bx}, {by}), not the {want:?} the cursor {at:?} \
         implies"
    );

    // (2) The box's height is **predicted**, and the two hypotheses differ at this
    // input. Vanilla's own client-text-tooltip height accessor is 8 px for one line and
    // `TOOLTIP_LINE_H * n` for n, and the fill adds `2 * PAD`, so:
    //
    // - wrapped at vanilla's own tooltip max-width: `10n + 6`
    // - not wrapped at all, the whole string on one line: `8 + 6` = 14
    //
    // The line count comes from `wrap_measured` at `TOOLTIP_MAX_WIDTH` — the same call
    // the draw makes, in the same font this `geometry` measures with — so the two
    // cannot disagree about the *split*; what is predicted from outside is vanilla's
    // height formula and its 170 px maximum. The `n > 1` assertion is what stops this
    // being an input where both hypotheses coincide, which is the trap that makes a
    // magnitude gate vacuous.
    let text = nav.settings().visible()[tipped]
        .cell
        .tooltip()
        .expect("premise: the row still has its tooltip");
    let lines =
        wrap_measured(&Quads::new(W, H), text, options::TOOLTIP_MAX_WIDTH, usize::MAX).len();
    assert!(
        lines > 1,
        "premise: {text:?} fits on one line in this font, so the wrapped and \
         unwrapped hypotheses agree and the height below measures nothing"
    );
    let wrapped = TOOLTIP_LINE_H * lines as f32 + 2.0 * TOOLTIP_PAD;
    let unwrapped = 8.0 + 2.0 * TOOLTIP_PAD;
    assert!(
        (bh - wrapped).abs() < 0.001,
        "the tooltip box is {bh} px, not the {wrapped} that {lines} wrapped lines \
         imply (an unsplit one would be {unwrapped})"
    );

    // (3) The control: a row with no tooltip in the table shows none. Without this,
    // (1) would pass on a box painted unconditionally.
    let (none_box, _) = hover_box(&mut nav, &ui, without);
    assert_eq!(
        none_box, None,
        "a row with no tooltip still painted a box at {none_box:?}"
    );

    // (4) The control that matters most: scroll the list, then put the cursor back
    // where the tooltipped row used to be. `menu_row_under`'s band guard means
    // nothing there is hoverable, so no box. Video is the page with room to scroll
    // at this canvas.
    let (mut vnav, vui) = settings_nav_on(SettingsPage::Video);
    let mut vfav = FaviconCache::new();
    let vframe = frame_for(&vui, &vnav, &statuses, &mut vfav).expect("a frame");
    let band_bottom = vframe
        .list
        .as_ref()
        .and_then(|s| s.model(H))
        .map(|l| l.bottom())
        .expect("premise: Video has a band");
    // A row whose centre is below the band at scroll 0 — it exists, because Video
    // overflows a 480 px canvas.
    let hidden = (0..vframe.rows.len())
        .find(|&i| {
            vframe.rows[i].is_scrolling_list_row()
                && vframe.rows[i].tooltip.is_some()
                && row_rect(&vframe.rows, i, W, H)
                    .is_some_and(|(_, ry, _, rh)| ry + rh * 0.5 > band_bottom)
        })
        .expect("premise: a tooltipped Video row sits below the band at scroll 0");
    let (hx, hy, hw, hh) = row_rect(&vframe.rows, hidden, W, H).expect("rect");
    vnav.set_menu_cursor(hx + hw * 0.5, hy + hh * 0.5, W, H);
    let f = frame_for(&vui, &vnav, &statuses, &mut vfav).expect("a frame");
    let outside = colour_bounds(&geometry(&f, W, H), W, H, TOOLTIP_BG);
    assert_eq!(
        outside, None,
        "a tooltip appeared at {outside:?} for row {hidden}, which sits below the \
         band at {band_bottom} and cannot be clicked"
    );
    // The premise for (4): the very same row, brought *into* the band, does show one.
    // Without it, the `None` above could mean the row simply has no tooltip text.
    let notches = ((hy + hh * 0.5) - band_bottom + hh) / (options::DEFAULT_ITEM_HEIGHT / 2.0).floor();
    vnav.scroll_active_list(&vui, -notches.ceil(), H);
    let scrolled = frame_for(&vui, &vnav, &statuses, &mut vfav).expect("a frame");
    let (nx, ny, nw, nh) = row_rect(&scrolled.rows, hidden, W, H).expect("rect");
    assert!(
        ny + nh * 0.5 < band_bottom,
        "premise: scrolling did not bring row {hidden} into the band (centre \
         {} vs band bottom {band_bottom})",
        ny + nh * 0.5
    );
    vnav.set_menu_cursor(nx + nw * 0.5, ny + nh * 0.5, W, H);
    let f = frame_for(&vui, &vnav, &statuses, &mut vfav).expect("a frame");
    let inside = colour_bounds(&geometry(&f, W, H), W, H, TOOLTIP_BG);
    assert!(
        inside.is_some(),
        "row {hidden} shows no tooltip even inside the band, so the None above \
         measured the absence of text rather than the band guard"
    );

    // -- the census, both directions -----------------------------------------
    //
    // The expected value comes from the production option rows: they reach 32
    // distinct tooltip accessors. The account-scoped Allow Requests control is
    // an action, not an option row, so its similarly named table key is not
    // part of this option-tooltip census. Asserted in both directions, because
    // either alone is satisfiable by a broken table: a count of *reached*
    // accessors catches a dropped row, and "no table key is unreached" catches
    // a typo'd key — which would otherwise be a tooltip that silently never
    // shows.
    let all: Vec<crate::menu::options::Cell> = [
        SettingsPage::Root,
        SettingsPage::Video,
        SettingsPage::Controls,
        SettingsPage::Mouse,
        SettingsPage::Sound,
        SettingsPage::Chat,
        SettingsPage::Accessibility,
        SettingsPage::Skin,
        SettingsPage::Online,
    ]
    .into_iter()
    .flat_map(|p| options::all_controls(p, false))
    .collect();
    let mut reached: Vec<&'static str> = all
        .iter()
        .filter_map(|c| match c {
            crate::menu::options::Cell::Option(spec) if c.tooltip().is_some() => {
                Some(spec.accessor)
            }
            _ => None,
        })
        .collect();
    reached.sort_unstable();
    reached.dedup();
    assert_eq!(
        reached.len(),
        32,
        "the tooltip table reaches {} distinct accessors, not the 32 the production rows resolve \
         onto rows this tree has: {reached:?}",
        reached.len()
    );
    let accessors: Vec<&'static str> = all
        .iter()
        .filter_map(|c| match c {
            crate::menu::options::Cell::Option(spec) => Some(spec.accessor),
            _ => None,
        })
        .collect();
    let stranded: Vec<&'static str> = options::tooltip_accessors()
        .into_iter()
        .filter(|key| !accessors.contains(key))
        .collect();
    assert!(
        stranded.is_empty(),
        "these tooltip-table keys match no row on any page, so their text can never \
         show and the count above cannot see it: {stranded:?}"
    );
}

/// **The layout half of the same report, and it is a sweep rather than one probe.**
///
/// A page whose rows already fit cannot tell "laid out clear of the footer" from
/// "happens not to overflow", which is exactly how an overlap ships. So this runs
/// every page of vanilla's own options-list type at a canvas where the longest of them overflows by
/// hundreds of pixels, at **every** scroll offset one pixel apart, and requires the
/// strip between the band's footer separator and the footer's own top button to be
/// empty in all of them.
///
/// Three things make it a real measurement rather than a restatement:
///
/// - the offset is driven through `MenuNav::scroll_active_list` — the production
///   wheel path — a twelfth of a notch at a time, so the swept offsets are ones a
///   player can actually reach, not values written into a field;
/// - mismatches are **collected**, not asserted inside the loop, so a regression
///   reports every page and offset it touches instead of aborting on the first;
/// - the premise that a page overflows is asserted per page, and the control is the
///   same frame with its `ListSpec` removed, which must fail.
///
/// The strip's bounds are derived: the band from the `ListSpec::model` the clip is
/// built from, plus `SEPARATOR_H` for the footer bar that legitimately owns the
/// first two rows, up to `options::footer_rects`' own y.
#[test]
fn every_settings_page_keeps_its_footer_band_clear_at_every_scroll_offset() {
    use crate::menu::options::{self, SettingsPage};

    // The shortest canvas any `gui_scale` can produce, so every page that can
    // overflow does.
    const W: f32 = 320.0;
    const H: f32 = 240.0;
    // One pixel of offset: `scroll_rate` is `floor(DEFAULT_ITEM_HEIGHT / 2)` = 12,
    // and the wheel takes notches.
    let px_per_notch = (options::DEFAULT_ITEM_HEIGHT / 2.0).floor();

    let statuses = crate::menu::status::StatusCache::with_probe(
        crate::menu::status::unavailable_probe(),
    );
    let mut overflowed = Vec::new();
    let mut mismatches: Vec<String> = Vec::new();
    for page in [
        SettingsPage::Video,
        SettingsPage::Controls,
        SettingsPage::Mouse,
        SettingsPage::Sound,
        SettingsPage::Chat,
        SettingsPage::Accessibility,
        SettingsPage::Skin,
        SettingsPage::Online,
    ] {
        let (mut nav, ui) = settings_nav_on(page);
        let mut fav = FaviconCache::new();
        let footer_top = options::footer_rects(W, H, page.footer().len() as u8)
            .first()
            .expect("every page has a footer button")
            .1;
        let max = options::list_spec(page, 0.0)
            .model(H)
            .map_or(0.0, |m| m.max_scroll());
        if max > 0.0 {
            overflowed.push(page);
        }
        let mut offset = 0.0f32;
        loop {
            let frame = frame_for(&ui, &nav, &statuses, &mut fav).expect("settings owns a frame");
            let list = frame
                .list
                .as_ref()
                .and_then(|s| s.model(H))
                .expect("premise: every page of vanilla's own options-list type has a band at 240");
            let band_bottom = list.top() + list.height();
            let strip = (
                0.0,
                band_bottom + SEPARATOR_H,
                W,
                footer_top - band_bottom - SEPARATOR_H,
            );
            assert!(strip.3 > 0.0, "premise: {page:?} has a strip to measure");
            let ink = coverage(&geometry(&frame, W, H), W, H, strip);
            if ink != 0.0 {
                mismatches.push(format!(
                    "{page:?} at scroll {offset}: {ink} of {strip:?} painted"
                ));
            }
            if offset >= max {
                break;
            }
            offset += 1.0;
            nav.scroll_active_list(&ui, -1.0 / px_per_notch, H);
        }
    }
    assert!(
        mismatches.is_empty(),
        "settings rows painted into the reserved footer band:\n{}",
        mismatches.join("\n")
    );
    // The premise, stated as a count so it cannot pass vacuously: this sweep is
    // worthless unless some page genuinely has more rows than its band holds.
    assert!(
        overflowed.len() >= 4,
        "only {} of vanilla's own options-list pages overflow a 240 px canvas ({overflowed:?}), so \
         this sweep mostly measured pages that could not overlap anyway",
        overflowed.len()
    );

    // -- control, executed ---------------------------------------------------
    // Sound with no `ListSpec` takes `draw`'s unclipped branch, and its rows run
    // 50 px past the band at 240, so the same strip must be inked.
    let (nav, ui) = settings_nav_on(SettingsPage::Sound);
    let mut fav = FaviconCache::new();
    let mut bare = frame_for(&ui, &nav, &statuses, &mut fav).expect("settings owns a frame");
    let band_bottom = bare
        .list
        .as_ref()
        .and_then(|s| s.model(H))
        .map(|l| l.top() + l.height())
        .expect("premise: Sound has a band");
    let footer_top = options::footer_rects(W, H, 1)[0].1;
    let strip = (
        0.0,
        band_bottom + SEPARATOR_H,
        W,
        footer_top - band_bottom - SEPARATOR_H,
    );
    bare.list = None;
    let spilled = coverage(&geometry(&bare, W, H), W, H, strip);
    assert!(
        spilled > 0.0,
        "the unclipped Sound frame painted nothing in {strip:?} either, so the sweep \
         above is not measuring the clip"
    );
}

/// **The save-list scroll gate**, in three parts, each with its control:
///
/// 1. a row straddling the band's bottom edge is **cut** rather than painted into
///    the gap above the footer — the control is the *same frame with no
///    `ListSpec`*, which takes `draw`'s unclipped branch and must fail the same
///    assertion;
/// 2. the row that was at the top of the band before scrolling is **not drawn
///    there** afterwards, measured by the selection outline's own colour — the
///    control is the pre-scroll arm, where it is;
/// 3. and the band is not simply blank afterwards: the row that *is* there draws.
///
/// **What else already paints here** was asked before believing (1): the row
/// column overlaps the search box above the band and all six footer buttons below
/// it, so an "is anything painted outside the band" rect would have measured the
/// footer. The rect used is the 8 px gutter **between** the band's bottom and the
/// footer grid's own top, and both edges are derived — the band from the
/// `ListSpec::model` the clip itself is built from, the footer from
/// `world_select_slot(Play)`. The title screen is measured in the same rect as a
/// second premise check.
///
/// Part (2) is colour-discriminated for the same reason
/// `every_world_in_the_list_draws_inside_its_own_row_band` had to be: the
/// selection outline is the one thing that separates "row 0 is here" from "some
/// row is here", and the leftmost *ink* inside a content rect is always
/// vanilla's own text-x accessor, whichever row it belongs to.
#[test]
fn a_scrolled_world_list_is_cut_at_the_band_and_stops_drawing_the_rows_above_it() {
    let owned: Vec<String> = (0..25).map(|i| format!("world{i:02}")).collect();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (mut nav, ui) = world_select_nav_with_worlds("ws-scroll-pixels", &names);

    // The band, from the same expression the clip is built from.
    let frame = world_select_frame(&nav, &ui);
    let spec = frame
        .list
        .as_ref()
        .expect("premise: the world list declares a ListSpec, or there is no clip and no scrollbar");
    let model = spec
        .model(V_H)
        .expect("premise: 25 rows in this band scroll");
    let band_top = model.top();
    let band_bottom = band_top + model.height();
    let footer_top = world_select_slot(crate::menu::world_select::WorldSelectButton::Play)
        .resolve(V_W, V_H)
        .1;
    assert!(
        footer_top > band_bottom,
        "premise: there must be a gap between the band's bottom ({band_bottom}) and \
         the footer's top ({footer_top}), or the only rect a cut row could paint in \
         is one the footer paints in too"
    );
    let (row_x, _, row_w, row_h) = world_list_row_rect(0, V_W, 0.0);
    let gutter = (row_x, band_bottom, row_w, footer_top - band_bottom);

    // Premise: the title screen paints nothing in the gutter, so "empty" there is
    // a fact about this screen.
    let title_nav = test_nav("ws-scroll-pixels-title");
    let title_ui = UiState::new();
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let title = frame_for(&title_ui, &title_nav, &statuses, &mut fav).expect("title frame");
    let painted = band_coverage(&geometry(&title, V_W, V_H), V_W, V_H, gutter);
    assert_eq!(
        painted.count, 0,
        "the title screen already paints in the gutter {gutter:?}, so this gate's \
         emptiness assertion measures nothing: {:?}",
        painted.bounds
    );

    // Scroll to a position where a row really does straddle the band's edge.
    let notches = 20.0;
    nav.scroll_active_list(&ui, -notches, V_H);
    let scroll = nav.world_select().scroll();
    assert_eq!(
        scroll,
        notches * model.scroll_rate(),
        "premise: the wheel moved by whole notches of `scrollRate`"
    );
    let straddler = (0..names.len())
        .find(|r| {
            let top = world_list_row_top(*r, scroll);
            top < band_bottom && top + row_h > band_bottom
        })
        .expect("premise: some row straddles the band's bottom edge at this offset");
    assert!(
        world_list_row_visible(straddler, V_H, scroll),
        "premise: the straddling row {straddler} is still a drawn row"
    );

    let frame = world_select_frame(&nav, &ui);
    let clipped = band_coverage(&geometry(&frame, V_W, V_H), V_W, V_H, gutter);
    assert_eq!(
        clipped.count, 0,
        "world row {straddler} straddles the band's bottom edge at scroll {scroll} \
         and painted into the gutter above the footer: {:?}",
        clipped.bounds
    );

    // -- control for (1), executed -------------------------------------------
    // The *same* frame with no `ListSpec` takes `draw`'s unclipped branch. It must
    // fail the assertion above, or the clip is not what made it pass.
    let mut unclipped = world_select_frame(&nav, &ui);
    unclipped.list = None;
    let spilled = band_coverage(&geometry(&unclipped, V_W, V_H), V_W, V_H, gutter);
    assert!(
        spilled.count > 0,
        "the unclipped frame painted nothing in the gutter either, so the clip is \
         not what the assertion above measured — the straddling row may not be \
         drawing at all"
    );

    // -- (2) and (3): the row above the band is no longer drawn there ---------
    // The band's first row position, and the selection outline's own colour there.
    let outline_at_top = |nav: &MenuNav, ui: &UiState| {
        let f = world_select_frame(nav, ui);
        let colour = geometry(&f, V_W, V_H);
        let (rx, ry, _, rh) = world_list_row_rect(0, V_W, nav.world_select().scroll());
        (
            coverage_of(&colour, V_W, V_H, (rx, ry + 2.0, 1.0, rh - 4.0), [1.0, 1.0, 1.0, 1.0]),
            band_coverage(&colour, V_W, V_H, world_list_row_content_rect(0, V_W, 0.0)),
        )
    };
    // Control: before scrolling, row 0 is the selection and it is at the top of
    // the band, so the outline is there.
    let (mut fresh_nav, fresh_ui) = world_select_nav_with_worlds("ws-scroll-pixels-top", &names);
    assert_eq!(fresh_nav.world_select().scroll(), 0.0);
    assert_eq!(fresh_nav.world_select().selected_row(), Some(0));
    let (outline_before, ink_before) = outline_at_top(&fresh_nav, &fresh_ui);
    assert!(
        outline_before > 0.5,
        "premise: with the list at the top, row 0's selection outline is at the \
         band's first row position: coverage {outline_before}"
    );
    assert!(ink_before.count > 0, "premise: and the row draws text there");

    // Scroll by exactly ten rows, so row 10 lands where row 0 was.
    let ten_rows = 10.0 * crate::menu::render::WORLD_LIST_ITEM_H;
    let notches = ten_rows / model.scroll_rate();
    fresh_nav.scroll_active_list(&fresh_ui, -notches, V_H);
    assert_eq!(
        fresh_nav.world_select().scroll(),
        ten_rows,
        "premise: the list moved by exactly ten rows"
    );
    assert!(
        !world_list_row_visible(0, V_H, ten_rows),
        "premise: row 0 is now out of view"
    );
    let f = world_select_frame(&fresh_nav, &fresh_ui);
    let colour = geometry(&f, V_W, V_H);
    let first_slot = world_list_row_content_rect(0, V_W, 0.0);
    let (rx, ry, _, rh) = world_list_row_rect(0, V_W, 0.0);
    let outline_after =
        coverage_of(&colour, V_W, V_H, (rx, ry + 2.0, 1.0, rh - 4.0), [1.0, 1.0, 1.0, 1.0]);
    assert!(
        outline_after < 0.1,
        "row 0 is scrolled out of view but its selection outline is still drawn at \
         the band's first row position: coverage {outline_after}"
    );
    // (3) And the band is not blank: the row that *is* there draws.
    let ink_after = band_coverage(&colour, V_W, V_H, first_slot);
    assert!(
        ink_after.count > 0,
        "the band's first row position is empty after scrolling, so the outline \
         assertion above is satisfied by nothing drawing at all"
    );
}

/// Hover and focus are two facts on this screen, and both reach the draw.
///
/// The bug this rules out is concrete: with one flag, moving the mouse over
/// the footer would pull the keyboard out of the search field. So the
/// assertion is that hovering a button changes what *that button* draws while
/// leaving the focused row alone.
#[test]
fn hovering_a_world_select_button_lights_it_without_moving_focus() {
    use crate::menu::world_select::{SEARCH_FIELD, WorldSelectButton as B};
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    let (mut nav, mut ui) = world_select_nav("ws-hover");
    nav.hover(&ui, B::Back.row());
    let frame = world_select_frame(&nav, &ui);
    assert_eq!(frame.hovered, Some(B::Back.row()));
    assert_eq!(
        frame.selected, SEARCH_FIELD,
        "hovering must not move keyboard focus"
    );

    // Vanilla's sprite argument is its own is-hovered-or-focused predicate, so a hovered
    // *enabled* button draws `widget/button_highlighted`.
    let row = frame.rows[B::Back.row()].clone();
    let draw = |hovered: Option<usize>| {
        let mut f = frame_with(vec![row.clone()], 99);
        f.vanilla = true;
        f.hovered = hovered;
        build(&f, Some(&atlas), None, V_W, V_H).sprite
    };
    let (hi_min, hi_max) = sprite_uv_bounds(&atlas, widget::BUTTON_SPRITES.enabled_focused);
    assert!(
        all_uvs_within(&draw(Some(0)), hi_min, hi_max),
        "a hovered enabled button must sample widget/button_highlighted"
    );
    // The control: unhovered and unfocused, it must not.
    assert!(
        !all_uvs_within(&draw(None), hi_min, hi_max),
        "the detector cannot tell the highlighted sprite apart"
    );

    // A **disabled** hovered button still draws the disabled sprite —
    // `WidgetSprites`' three-argument collapse, the single rule a hand-rolled
    // highlight gets wrong. Edit, not Create, is the disabled control.
    let edit = frame.rows[B::Edit.row()].clone();
    let mut f = frame_with(vec![edit], 99);
    f.vanilla = true;
    f.hovered = Some(0);
    let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;
    let (off_min, off_max) = sprite_uv_bounds(&atlas, widget::BUTTON_SPRITES.disabled);
    assert!(
        all_uvs_within(&sprite, off_min, off_max),
        "a hovered DISABLED Edit must still sample widget/button_disabled"
    );

    // And the click that hover would have preceded does nothing on it, which
    // is the other half of "present but disabled".
    let before = ui.screen();
    assert_eq!(
        nav.click(&mut ui, B::Edit.row()),
        crate::menu::nav::MenuAction::None
    );
    assert_eq!(ui.screen(), before, "clicking Edit must not open anything");
}

/// The search box draws as a **text field**, not as a button — a slotted row
/// carrying an `EditBox` takes `draw_edit_box`'s path and not
/// `draw_widget`'s.
///
/// The discriminator is the synthetic pack itself: `button_pack()` carries
/// `widget/button*` and no `widget/text_field*`, so a field falls back to its
/// flat fill and emits **no sprite quads at all** where a button emits nine.
/// The control is the same row drawn as a button, watched emitting them.
#[test]
fn the_search_box_draws_as_a_field_inside_its_own_slot() {
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    let (mut nav, mut ui) = world_select_nav("ws-search");
    // Upper-case, and `M` first, on purpose: the jar-less font's `M` is
    // `0b10001` in all seven rows (`font::glyph_rows`'s `'M'` arm), so its leftmost lit
    // column sits exactly on the box's `text_x`. That is what lets the x
    // assertion below be an equality rather than a bound — a glyph whose
    // column 0 is blank (`A`, `C`) would put the leftmost vertex a pixel or
    // two right of `text_x` and make the same test unable to tell a 2 px
    // error from a correct draw.
    for ch in "MC".chars() {
        nav.key(&mut ui, MenuKey::Char(ch));
    }
    let frame = world_select_frame(&nav, &ui);
    let row = frame.rows[0].clone();
    assert_eq!(
        row.edit.as_ref().map(|e| e.value().to_string()),
        Some("MC".to_string()),
        "typing on this screen goes into the search box"
    );

    let (fx, fy, fw, fh) = world_select_search_slot().resolve(V_W, V_H);
    let mut f = frame_with(vec![row.clone()], 0);
    f.vanilla = true;
    let drawn = build(&f, Some(&atlas), None, V_W, V_H);
    assert!(
        drawn.sprite.is_empty(),
        "the field sampled a button sprite, so it took draw_widget's path"
    );
    // Its background is the field fill, at the slot's own rect.
    assert!(
        coverage_of(&drawn.colour, V_W, V_H, (fx, fy, fw, fh), FIELD_BG) > 0.5,
        "the search box's fill did not reach {:?}",
        (fx, fy, fw, fh)
    );

    // -- control ---------------------------------------------------------
    // The same row without its `EditBox` is a button, and it must emit the
    // sprite quads the assertion above requires to be absent.
    let mut as_button = row.clone();
    as_button.edit = None;
    as_button.field = false;
    let mut g = frame_with(vec![as_button], 0);
    g.vanilla = true;
    let button_drawn = build(&g, Some(&atlas), None, V_W, V_H);
    assert!(
        !button_drawn.sprite.is_empty(),
        "a button drew no sprites either, so the discriminator measures nothing"
    );

    // The typed text lands inside the box's own text band — every bound asked
    // of a clone repositioned into the slot, exactly as `draw_edit_box` does,
    // rather than restated.
    let mut probe = row.edit.clone().expect("a live box");
    probe.widget.x = fx;
    probe.widget.y = fy;
    probe.widget.width = fw;
    probe.widget.height = fh;
    let state = probe.draw_state(None);
    // The band spans the box's **whole** width, deliberately: the question is
    // where the text starts, so a band that begins at `text_x` would clip the
    // very error it is looking for and pass on a draw 4 px to the left.
    //
    // That makes the *focus outline* the thing to be careful about, and it is
    // what this gate got wrong on its first run. `band_coverage` counts
    // **vertices**, not covered area, and the jar-less outline's bottom bar
    // spans the full field width at `y + h - 2` — inside a `glyph_h`-tall
    // band vertically, with its only vertices at the box's own `x` and
    // `x + width`. So on a focused box the leftmost vertex in this band is the
    // box's edge, not the text's, and the gate accused the draw of painting
    // 4 px left of `text_x` when the draw was right and the 4 px was
    // `BORDER_INSET` in the gate's own reasoning. The focused `EditBox` gate dodges
    // this by insetting its band to `text_x`/`inner_width`; that is the right
    // answer for measuring *what* drew and the wrong one for measuring
    // *where* it started.)
    //
    // So: measure the text on an **unfocused** clone — no outline, no caret,
    // nothing in the box but glyphs — and use the focused draw as the control
    // that this band really can see ink at the box's edge.
    // The band's bottom is the box's own bottom edge, not a glyph-height
    // constant: the control below has to see the fallback outline's bottom
    // bar, which draws at `y + h - 2` — a fixed offset from the box's real
    // bottom, with nothing to do with any text scale. Tying the band to
    // `EDIT_TEXT_SCALE` is what broke this the moment that scale stopped
    // matching the outline's position (`2cd7c58`): the band shrank from
    // 14px to 7px, stopped reaching the bar, and the control then measured
    // text ink starting at `text_x` instead of outline ink at `fx`.
    let band = (fx, state.text_y, fw, (fy + fh) - state.text_y);
    let mut unfocused = row.clone();
    if let Some(e) = unfocused.edit.as_mut() {
        e.widget.focused = false;
    }
    let mut u = frame_with(vec![unfocused], 99);
    u.vanilla = true;
    let quiet = build(&u, Some(&atlas), None, V_W, V_H).colour;
    let inside = band_coverage(&quiet, V_W, V_H, band);
    assert!(
        inside.count > 0,
        "the typed text reached no pixels inside the box's own band {band:?}"
    );
    let bounds = inside.bounds.expect("a non-empty band has bounds");
    assert!(
        (bounds.0 - state.before_x).abs() < 0.01,
        "the text starts at {} where the box's own text_x is {} — a draw using \
         the row's PAD of 6, or the box's own x, fails here; bounds {bounds:?}",
        bounds.0,
        state.before_x
    );
    assert!(
        bounds.2 <= fx + fw + 0.01,
        "the text overran the box's right edge: bounds {bounds:?}"
    );

    // -- control ---------------------------------------------------------
    // The focused draw puts the outline's bottom bar in the same band, with a
    // corner vertex on the box's own `x`. So the band demonstrably *can* see
    // ink `BORDER_INSET` left of `text_x` — which is exactly the error the
    // assertion above denies, and without this the equality could be passing
    // because the band is blind to that column.
    let lit = band_coverage(&drawn.colour, V_W, V_H, band)
        .bounds
        .expect("a focused field paints its outline");
    assert!(
        (lit.0 - fx).abs() < 0.01,
        "the control did not reach the box's edge, so the assertion above is \
         not measuring what it claims: bounds {lit:?}"
    );
    assert!(
        state.before_x - fx > 0.0,
        "premise: text_x is inset from the box's x, or the two measurements \
         above cannot disagree"
    );
}
