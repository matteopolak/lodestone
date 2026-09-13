use super::*;


#[test]
fn the_server_list_shows_the_motd_players_and_latency_from_a_status() {
    // The content gate: what the status decoder produced has to appear in the
    // row, not merely be cached.
    let mut nav = test_nav("content");
    let mut ui = UiState::new();
    add_server(&mut nav, &mut ui, "HOME", "mc.example.com");

    let mut statuses = StatusCache::with_probe(std::sync::Arc::new(|_| {
        Ok(ServerStatus {
            motd: "A LODESTONE SERVER\nsecond line".into(),
            // No server styling in this fixture: the row must lay out
            // identically to before the styled path existed.
            motd_spans: Vec::new(),
            players: "3/20".into(),
            online: Some(3),
            sample: Vec::new(),
            version: "26.2".into(),
            // Our own protocol, so the row resolves to
            // `ServerState::Successful` and shows a player count rather than
            // the red version string an incompatible server gets.
            protocol: Some(crate::menu::status::STATUS_PROTOCOL),
            favicon_png: None,
            latency_ms: Some(12),
        })
    }));
    let entries = nav.list().entries().to_vec();
    statuses.refresh(&entries);
    let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
    while statuses.pump() == 0 && crate::platform::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).expect("the list draws");
    assert_eq!(
        f.rows.len(),
        1 + crate::menu::nav::SERVER_LIST_BUTTONS.len(),
        "one entry plus vanilla's seven footer buttons"
    );
    assert_eq!(f.rows[0].label, "HOME");
    let view = f.rows[0].entry.as_ref().expect("row 0 is a list entry");
    // The **whole** MOTD, newline included: the wrap to two lines happens at
    // draw time, in the font the draw measures with (`wrap_measured`).
    assert_eq!(view.motd, "A LODESTONE SERVER\nsecond line");
    assert!(!view.motd_is_error);
    // The status column is the player count, not the latency: vanilla puts
    // `formatPlayerCount` there and the round-trip only in the ping *sprite*
    // and its tooltip.
    assert_eq!(view.status, "3/20");
    assert!(!view.status_is_error);
    // 12 ms is the fastest bucket, so five bars. Asserted by identity — a gate
    // that only proved "a ping sprite drew" passes on all five.
    assert_eq!(view.status_sprite, "server_list/ping_5");
    assert!(view.selected, "the one row is the selected one");
    assert!(
        !view.can_move_up && !view.can_move_down,
        "a single row has nowhere to move"
    );
}


/// The three states that are *not* "answered by a compatible server" each get
/// their own sprite, and the assertion is by **identity**: a gate that only
/// proves a ping bar exists passes on all four rendering the same bar.
#[test]
fn every_row_state_resolves_to_its_own_status_sprite() {
    use crate::menu::status::{PINGING_SPRITES, ServerStatus};

    let mut nav = test_nav("states");
    let mut ui = UiState::new();
    add_server(&mut nav, &mut ui, "SLOW", "slow.example");

    // A compatible server, 700 ms — the fourth bucket down.
    let mut statuses = StatusCache::with_probe(std::sync::Arc::new(|_| {
        Ok(ServerStatus {
            motd: "hi".into(),
            players: "1/1".into(),
            protocol: Some(crate::menu::status::STATUS_PROTOCOL),
            latency_ms: Some(700),
            ..Default::default()
        })
    }));
    let entries = nav.list().entries().to_vec();
    // While the probe is in flight the row is `Pending`, which must animate.
    // Read *before* draining, and only asserted to be one of the five frames:
    // which one depends on a clock.
    statuses.refresh(&entries);
    let mut fav = FaviconCache::new();
    let pending = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let pending_view = pending.rows[0].entry.clone().unwrap();
    assert!(
        PINGING_SPRITES.contains(&pending_view.status_sprite),
        "an in-flight row must animate, got {}",
        pending_view.status_sprite
    );
    assert_eq!(
        pending_view.motd, "Pinging...",
        "vanilla overwrites the MOTD while pinging"
    );
    assert!(
        pending_view.status.is_empty(),
        "and blanks the status column"
    );

    let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
    while statuses.pump() == 0 && crate::platform::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let slow = frame_for(&ui, &nav, &statuses, &mut fav).unwrap().rows[0]
        .entry
        .clone()
        .unwrap();
    assert_eq!(slow.status_sprite, "server_list/ping_2", "700 ms is two bars");

    // An answered server speaking a different protocol is *incompatible*, not
    // unreachable: its own sprite, and its version in place of a player count.
    let mut old = StatusCache::with_probe(std::sync::Arc::new(|_| {
        Ok(ServerStatus {
            motd: "hi".into(),
            players: "1/1".into(),
            version: "1.21.11".into(),
            protocol: Some(1),
            latency_ms: Some(5),
            ..Default::default()
        })
    }));
    old.refresh(&entries);
    let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
    while old.pump() == 0 && crate::platform::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let view = frame_for(&ui, &nav, &old, &mut fav).unwrap().rows[0]
        .entry
        .clone()
        .unwrap();
    assert_eq!(view.status_sprite, "server_list/incompatible");
    assert_eq!(view.status, "1.21.11", "the version, where the count goes");
    assert!(view.status_is_error, "and in red");

    // And the four sprites are four different sprites.
    let mut all = vec![
        pending_view.status_sprite,
        slow.status_sprite,
        view.status_sprite,
    ];
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), 3, "two states share a sprite: {all:?}");
}


#[test]
fn a_failed_ping_shows_its_reason_in_the_error_colour() {
    let mut nav = test_nav("failed");
    let mut ui = UiState::new();
    add_server(&mut nav, &mut ui, "DEAD", "dead.example");

    let mut statuses = StatusCache::with_probe(std::sync::Arc::new(|_| {
        Err("connection refused".to_string())
    }));
    let entries = nav.list().entries().to_vec();
    statuses.refresh(&entries);
    let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
    while statuses.pump() == 0 && crate::platform::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let view = f.rows[0].entry.as_ref().expect("row 0 is a list entry");
    // The reason goes in the **MOTD** column and the status column stays
    // empty, which is vanilla's own arrangement: `onPingFailed` sets
    // `data.motd = CANT_CONNECT_MESSAGE` and `data.status` to empty
    //.
    assert_eq!(view.motd, "connection refused");
    assert!(
        view.motd_is_error,
        "a failure must be visually distinct from a MOTD"
    );
    assert!(view.status.is_empty(), "no player count to show");
    assert_eq!(
        view.status_sprite, "server_list/unreachable",
        "an unreachable row gets its own sprite, not a ping bar"
    );
}


/// With nothing to act on, Join / Edit / Delete are **present and inactive** —
/// the three selection-dependent controls. Direct Connection is inactive
/// whatever the selection.
///
/// The control is executed rather than described: adding a server must flip all
/// three, or "they are disabled" would pass on a screen whose buttons are
/// *always* disabled.
#[test]
fn the_footer_buttons_are_present_and_three_are_inactive_with_no_selection() {
    use crate::menu::nav::{SERVER_LIST_BUTTONS, ServerListButton as B};

    let mut nav = test_nav("emptylist");
    let mut ui = UiState::new();
    ui.open_server_list();
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

    // Every one of vanilla's seven is on screen even with an empty list — a
    // missing button is a layout that reads wrong, a greyed-out one reads
    // exactly like vanilla with the feature unavailable.
    assert_eq!(f.rows.len(), SERVER_LIST_BUTTONS.len());
    let row_of = |b: B| {
        SERVER_LIST_BUTTONS
            .iter()
            .position(|x| *x == b)
            .expect("every button is in the table")
    };
    for (i, button) in SERVER_LIST_BUTTONS.iter().enumerate() {
        assert_eq!(
            f.rows[i].label,
            button.label(),
            "row {i} is not {button:?} — the footer order is what click() assumes"
        );
    }
    for b in [B::Select, B::Edit, B::Delete, B::Direct] {
        assert!(!f.rows[row_of(b)].enabled, "{b:?} must be inactive");
    }
    for b in [B::Add, B::Refresh, B::Back] {
        assert!(f.rows[row_of(b)].enabled, "{b:?} must be active");
    }

    // Control: a selection enables three of the four, and Direct Connection
    // stays inactive because nothing here can honour it.
    add_server(&mut nav, &mut ui, "HOME", "mc.example.com");
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let base = 1;
    for b in [B::Select, B::Edit, B::Delete] {
        assert!(
            f.rows[base + row_of(b)].enabled,
            "{b:?} must be active once a row exists"
        );
    }
    assert!(
        !f.rows[base + row_of(B::Direct)].enabled,
        "Direct Connection has no screen to open, selection or not"
    );
}


/// Vanilla's own rects for its own join-multiplayer screen at 854×480, hand-derived
/// from vanilla's own source rather than read back out of the layout — `CLAUDE.md`'s rule
/// that an expected value must originate outside the code under test.
///
/// The derivation, which is what a future reader has to be able to check:
///
/// - Vanilla's own header-and-footer layout, built with a 33 px header and 60 px
///   footer, so its own content-height accessor is
///   `480 - 33 - 60` = **387**, and the list is sized to exactly that
///   (`:61-62`). The content clamp is then `min(33 + 30, 480 - 60 - 387)` =
///   `min(63, 33)` = **33** — flush under the header, because the content
///   fills the band.
/// - Vanilla's own first-entry-y accessor is its own y accessor plus 2 = **35**, and rows stack by
///   `itemHeight` 36 with no gap.
/// - Vanilla's own row-left accessor is `0 + 854/2 - 305/2` = `427 - 152` = **275**. Note the
///   two halvings are separate integer divisions; `(854 - 305) / 2` is 274.
/// - Vanilla's own content-padding constant insets the entry by 2 a side, so content is
///   `(277, 37, 301, 32)` and the 32 is exactly the favicon's height.
/// - Its own status-icon x is its own content-right accessor minus 15 = `578 - 15` = **563**, at
///   its own content-y accessor = 37 — the status icon is *not* vertically centred.
/// - The title is a 9 px vanilla-own string-label widget centred in the 854×33 header frame:
///   `round((33 - 9) / 2)` = **12** from the top, on `width / 2`.
/// - The footer column is `3*100 + 2*4` = 308 wide on its top row and
///   `4*74 + 3*4` = 308 on its lower one — they match, which is why the
///   column is 308 and both rows sit at its left edge — and `20 + 4 + 20` = 44
///   tall. Centred in the 854×60 footer frame pinned at y 420:
///   `((854 - 308) / 2, 420 + (60 - 44) / 2)` = **(273, 428)**.
#[test]
fn the_server_list_rects_are_vanillas_own() {
    use crate::menu::nav::{SERVER_LIST_BUTTONS, ServerListButton as B};

    let expected = [
        // Top row: 100 wide, 104 apart.
        (B::Select, (273.0, 428.0, 100.0, 20.0)),
        (B::Direct, (377.0, 428.0, 100.0, 20.0)),
        (B::Add, (481.0, 428.0, 100.0, 20.0)),
        // Lower row: 74 wide, 78 apart, 24 px below.
        (B::Edit, (273.0, 452.0, 74.0, 20.0)),
        (B::Delete, (351.0, 452.0, 74.0, 20.0)),
        (B::Refresh, (429.0, 452.0, 74.0, 20.0)),
        (B::Back, (507.0, 452.0, 74.0, 20.0)),
    ];
    for (button, want) in expected {
        assert_eq!(
            server_list_footer_slot(button).resolve(V_W, V_H),
            want,
            "{button:?} is not where vanilla puts it"
        );
        // The enum's declared width and the arranged one must agree, or the
        // footer was built with its two rows swapped.
        assert_eq!(
            server_list_footer_slot(button).w,
            button.width(),
            "{button:?}'s arranged width is not its declared one"
        );
    }
    // Both footer gutters are 4 — this screen's, not the pause screen's 8.
    let (sx, _, sw, _) = server_list_footer_slot(B::Select).resolve(V_W, V_H);
    let (dx, ..) = server_list_footer_slot(B::Direct).resolve(V_W, V_H);
    assert_eq!(dx - (sx + sw), 4.0, "top row spacing");
    let (ex, _, ew, _) = server_list_footer_slot(B::Edit).resolve(V_W, V_H);
    let (delx, ..) = server_list_footer_slot(B::Delete).resolve(V_W, V_H);
    assert_eq!(delx - (ex + ew), 4.0, "lower row spacing");
    assert_eq!(SERVER_LIST_BUTTONS.len(), 7);

    // The rows, unscrolled.
    assert_eq!(server_row_rect(0, V_W, 0.0), (275.0, 35.0, 305.0, 36.0));
    assert_eq!(
        server_row_rect(1, V_W, 0.0),
        (275.0, 71.0, 305.0, 36.0),
        "rows stack by itemHeight with no gap"
    );
    assert_eq!(
        server_row_content_rect(0, V_W, 0.0),
        (277.0, 37.0, 301.0, 32.0),
        "CONTENT_PADDING insets the entry by 2, and 36 - 4 is the icon's 32"
    );
    assert_eq!(server_entry_icon_rect(0, V_W, 0.0), (277.0, 37.0, 32.0, 32.0));
    assert_eq!(
        server_status_icon_rect(0, V_W, 0.0),
        (563.0, 37.0, 10.0, 8.0),
        "contentRight - 10 - 5, at contentY"
    );
    // A scroll of one whole row shifts every row up by one `itemHeight`
    // A one-row scroll offset makes row 1 at scroll 0 land exactly where row 0
    // sits at scroll 36.
    assert_eq!(
        server_row_rect(1, V_W, SERVER_LIST_ITEM_H),
        server_row_rect(0, V_W, 0.0),
        "scrolling by one row is the same shift as re-indexing by one row"
    );
    // A *half*-row scroll is expressible at all, which is the whole conversion.
    // 18 px is one wheel notch; the row lands 18 px above where
    // it started, not a whole entry above it and not nowhere.
    assert_eq!(
        server_row_top(0, SERVER_LIST_ITEM_H / 2.0),
        server_row_top(0, 0.0) - 18.0,
        "a one-notch offset moves the row by 18 px — the value a row index \
         could not represent"
    );
    // Vanilla's own row-left accessor is not `(width - rowWidth) / 2`, and the difference shows
    // at an odd canvas: 855/2 = 427 either way here, 856 is where they split.
    assert_eq!(server_row_left(856.0), 276.0, "floor(856/2) - 152");
    assert_eq!(
        (856.0 - SERVER_LIST_ROW_W) / 2.0,
        275.5,
        "control: the naive centring is half a pixel off"
    );

    // The title.
    let title = server_list_title_label();
    assert_eq!(title.text, crate::menu::nav::SERVER_LIST_TITLE);
    assert_eq!((title.dx, title.dy), (0.0, 12.0));
    assert_eq!(title.align, Align::Centre);
    assert_eq!(title.origin, Origin::ScreenTop);
}


/// The whole screen is arranged **once**, at a reference canvas, and every
/// rect is then expressed relative to an [`Origin`]. That is only sound if the
/// arrangement is canvas-independent once so expressed — so re-arrange at three
/// sizes and require identical slots.
///
/// This is what stands between the screen and being correct at 854×480 and
/// wrong everywhere else. It holds because the footer column measures 308 at
/// any width and the content band always starts at the header height (the list
/// is sized to vanilla's own content-height accessor, so the clamp always picks it).
///
/// **Even widths only, and that is a real limit rather than a convenient
/// choice.** `Origin::ScreenBottom`'s x is `width * 0.5` unrounded, while
/// `FrameLayout` truncates its centring, so at an odd logical width the two
/// disagree by half a pixel — the same limit `Screen::WorldSelect`'s footer
/// has, for the same reason. It is invisible in practice because
/// `logical_canvas` divides the framebuffer by an integer scale and can
/// produce a fractional width anyway; the row geometry, which *is* floored
/// per-term, is exact at every width (see `server_row_left`).
#[test]
fn the_server_list_slots_do_not_depend_on_the_reference_canvas() {
    let reference = ServerListBlock::at(SERVER_LIST_REF_CANVAS.0, SERVER_LIST_REF_CANVAS.1);
    for (w, h) in [(320.0, 240.0), (1280.0, 720.0), (1920.0, 1080.0)] {
        let other = ServerListBlock::at(w, h);
        assert_eq!(
            other.content_top, reference.content_top,
            "the content band moved at {w}x{h}"
        );
        for i in 0..reference.footer.len() {
            assert_eq!(
                other.footer_slot(i),
                reference.footer_slot(i),
                "footer slot {i} moved at {w}x{h}"
            );
        }
        // And the slot really resolves to where that canvas' own arrangement
        // put it, which is the assertion that makes the two derivations
        // independent rather than merely equal to each other.
        for (i, want) in other.footer.iter().enumerate() {
            assert_eq!(
                reference.footer_slot(i).resolve(w, h),
                *want,
                "footer slot {i} does not land on {w}x{h}'s own arrangement"
            );
        }
    }
}


/// The discriminator for a hover overlay on this screen
/// is **position**. A gate that proved "an overlay drew in a row" would pass
/// on an overlay nailed to row 0.
///
/// The measurement is the icon-dim quad (`fill(…, -1601138544)`), which is the
/// one part of the overlay that reaches the *colour* stream — the three arrow
/// sprites need an atlas, and they get their own gate below.
#[test]
fn the_hover_overlay_follows_the_cursor_rather_than_the_row() {
    let (nav, ui) = list_nav("hover", &[("A", "a.example"), ("B", "b.example")]);
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let mut f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

    let dim_at = |f: &MenuFrame<'_>| {
        colour_bounds(&geometry(f, V_W, V_H), V_W, V_H, SERVER_ICON_DARKEN)
    };
    // A tolerance, not `assert_eq!`: the measurement round-trips through NDC
    // and back (`2x/w - 1` then its inverse), so 277.0 comes out 277.00003.
    let is = |got: Option<(f32, f32, f32, f32)>, want: (f32, f32, f32, f32), what: &str| {
        let g = got.unwrap_or_else(|| panic!("{what}: nothing drew, expected {want:?}"));
        let near = (g.0 - want.0).abs() < 0.01
            && (g.1 - want.1).abs() < 0.01
            && (g.2 - want.2).abs() < 0.01
            && (g.3 - want.3).abs() < 0.01;
        assert!(near, "{what}: overlay at {g:?}, expected {want:?}");
    };

    // No cursor at all — a keyboard-only session, and every hermetic test.
    // This is also the control that makes the absences below real: if the
    // detector could not see the quad, every assertion here would pass on a
    // screen that never drew one.
    f.cursor = None;
    assert_eq!(dim_at(&f), None, "no cursor must mean no hover overlay");

    // Row 0, then row 1: the same overlay, one `itemHeight` lower.
    let icon0 = server_entry_icon_rect(0, V_W, 0.0);
    f.cursor = Some((icon0.0 + 4.0, icon0.1 + 4.0));
    is(dim_at(&f), icon0, "row 0's icon");
    let icon1 = server_entry_icon_rect(1, V_W, 0.0);
    f.cursor = Some((icon1.0 + 4.0, icon1.1 + 20.0));
    is(dim_at(&f), icon1, "row 1's icon");
    assert_eq!(
        icon1.1 - icon0.1,
        SERVER_LIST_ITEM_H,
        "premise: the two rows are a row apart, or this proves nothing"
    );

    // Vanilla's `hovered` is the *row*, not the icon: the cursor anywhere in
    // the row lights the icon up, and anywhere outside it does not.
    f.cursor = Some((icon0.0 + 200.0, icon0.1 + 4.0));
    is(dim_at(&f), icon0, "the whole row hovers");
    f.cursor = Some((10.0, 10.0));
    assert_eq!(dim_at(&f), None, "the backdrop is not a row");
}


/// The "who's online" tooltip, from the frame side and the draw side
/// together: `server_list_frame` shapes the lines from the sample — vanilla's
/// `... and N more ...` when the sample is short of the count — and
/// `draw_server_entry` only shows the box when the cursor is over the status
/// *text* (the player count), not over the row.
#[test]
fn the_who_is_online_tooltip_lists_the_sample_and_tracks_the_status_text() {
    let (nav, ui) = list_nav("who", &[("A", "a.example"), ("B", "b.example")]);
    let statuses = ok_statuses(
        &nav,
        &[
            ("a.example", "5/20", &["Alice", "Bob"], Some(5)),
            // A server that omits the sample: legal and common, and vanilla's
            // `else { data.playerList = List.of() }`
            // gives it no tooltip.
            ("b.example", "1/20", &[], Some(1)),
        ],
    );
    let mut fav = FaviconCache::new();
    let mut f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

    // The frame resolves the lines once per status, exactly as
    // `ServerStatusPinger` builds `data.playerList` (`:90-110`): the two named
    // players, then the and-more line for the unnamed three.
    let a = f.rows[0].entry.as_ref().expect("row 0 is an entry");
    assert_eq!(
        a.online_players,
        ["Alice", "Bob", "... and 3 more ..."],
        "2 of 5 named must carry vanilla's `... and 3 more ...`"
    );
    assert!(
        f.rows[1].entry.as_ref().unwrap().online_players.is_empty(),
        "an empty sample is an empty tooltip"
    );

    let fill_at = |f: &MenuFrame<'_>| colour_bounds(&geometry(f, V_W, V_H), V_W, V_H, TOOLTIP_BG);

    // The status text is right-aligned to its status icon
    // (`status_x = icon_x - width - spacing`), and the box lands by
    // `DefaultTooltipPositioner` — content at the cursor + (12, -12), here with
    // no edge to clamp — so the fill is `(rx - 3, ry - 3, w + 6, h + 6)`.
    let (icon_x, ..) = server_status_icon_rect(0, V_W, 0.0);
    let (_, cy, ..) = server_row_content_rect(0, V_W, 0.0);
    let status_x = icon_x - text_px("5/20", 1.0) - SERVER_ENTRY_SPACING;
    assert_eq!((status_x, cy), (534.0, 37.0), "premise: the cursor lands on the text");
    f.cursor = Some((status_x + 12.0, cy + 4.0));
    // Width 108 is `"... and 3 more ..."` (18 chars at the fixed 6 px advance);
    // height 30 is three 10 px tooltip lines; fill is the 3 px pad on each side.
    assert_box(
        fill_at(&f),
        (555.0, 26.0, 114.0, 36.0),
        "row 0's tooltip",
    );

    // Inside the row but off the status column: no tooltip — vanilla fires it
    // over the text only, never over the row.
    f.cursor = Some((status_x - 40.0, cy + 4.0));
    assert_eq!(fill_at(&f), None, "inside the row, off the status text");

    // Row 1's status text, over an empty sample: still no tooltip.
    let (_, cy1, ..) = server_row_content_rect(1, V_W, 0.0);
    let sx1 = icon_x - text_px("1/20", 1.0) - SERVER_ENTRY_SPACING;
    f.cursor = Some((sx1 + 12.0, cy1 + 4.0));
    assert_eq!(fill_at(&f), None, "an empty sample draws no tooltip");

    // No cursor — the keyboard-only control (and every hermetic test's default).
    f.cursor = None;
    assert_eq!(fill_at(&f), None, "no cursor means no tooltip");
}


/// The tooltip is drawn **after** the band-clipped rows, so a tooltip for the
/// first row — whose top necessarily reaches above the band, because the status
/// text sits a few pixels below the band's top edge — is not scissored off. A
/// tooltip clipped to the band would measure a box whose top edge is the band's,
/// not the predicted `ry - 3`; asserting the exact unclipped value is what tells
/// "escaped the clip" from "nothing drew".
#[test]
fn the_who_is_online_tooltip_escapes_the_band_clip() {
    let (nav, ui) = list_nav("clip", &[("A", "a.example")]);
    let statuses = ok_statuses(&nav, &[("a.example", "5/20", &["Alice", "Bob"], Some(5))]);
    let mut fav = FaviconCache::new();
    let mut f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

    let (icon_x, ..) = server_status_icon_rect(0, V_W, 0.0);
    let (_, cy, ..) = server_row_content_rect(0, V_W, 0.0);
    let status_x = icon_x - text_px("5/20", 1.0) - SERVER_ENTRY_SPACING;
    // The top of the status text box (mouse-y equal to vanilla's own
    // content-y accessor is inclusive in vanilla), the highest this row can push the tooltip.
    f.cursor = Some((status_x + 12.0, cy));
    let got = colour_bounds(&geometry(&f, V_W, V_H), V_W, V_H, TOOLTIP_BG)
        .expect("the tooltip must draw");
    assert_box(Some(got), (555.0, 22.0, 114.0, 36.0), "the unclipped fill");

    let band_top = f.list.as_ref().unwrap().model(V_H).unwrap().top();
    assert!(
        got.1 < band_top,
        "the fill's top ({}) must reach above the band ({band_top}), or the \
         clip would have cut the tooltip",
        got.1
    );
}


/// The quadrant under the cursor decides which of the three overlay sprites is
/// drawn **highlighted**, and the other two must stay plain. All three blit
/// into the same rect, so this is asserted by atlas region rather than by
/// position — position is what the previous gate covers.
#[test]
fn each_hovered_icon_quadrant_highlights_its_own_sprite() {
    let atlas = server_list_atlas();
    let (nav, ui) = list_nav(
        "quadrants",
        &[("A", "a.example"), ("B", "b.example"), ("C", "c.example")],
    );
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let mut f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

    let region = |id: &str| sprite_uv_bounds(&atlas, id);
    let regions = [
        SERVER_JOIN_SPRITES,
        SERVER_MOVE_UP_SPRITES,
        SERVER_MOVE_DOWN_SPRITES,
    ];
    // The six regions must be disjoint, or "sampled inside X" proves nothing.
    let all: Vec<([f32; 2], [f32; 2])> = regions
        .into_iter()
        .flat_map(|(a, b)| [region(a), region(b)])
        .collect();
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            let (a, b) = (all[i], all[j]);
            assert!(
                a.1[0] <= b.0[0] || b.1[0] <= a.0[0] || a.1[1] <= b.0[1] || b.1[1] <= a.0[1],
                "two overlay sprites share atlas space: {a:?} {b:?}"
            );
        }
    }

    // Row 1 of three, so both move arrows apply.
    let (ix, iy, iw, ih) = server_entry_icon_rect(1, V_W, 0.0);
    let cases = [
        // (cursor, which of the three is highlighted)
        ((ix + iw * 0.75, iy + ih * 0.5), 0usize),
        ((ix + 4.0, iy + 4.0), 1),
        ((ix + 4.0, iy + ih - 4.0), 2),
    ];
    for ((mx, my), highlighted) in cases {
        f.cursor = Some((mx, my));
        let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;
        for (which, (plain, hot)) in regions.into_iter().enumerate() {
            let (p, hgt) = (region(plain), region(hot));
            if which == highlighted {
                assert!(
                    any_quad_within(&sprite, hgt.0, hgt.1),
                    "cursor ({mx}, {my}) must highlight {hot}"
                );
                assert!(
                    !any_quad_within(&sprite, p.0, p.1),
                    "and must not also draw the plain {plain}"
                );
            } else {
                assert!(
                    any_quad_within(&sprite, p.0, p.1),
                    "cursor ({mx}, {my}) must still draw the plain {plain}"
                );
                assert!(
                    !any_quad_within(&sprite, hgt.0, hgt.1),
                    "and must not highlight {hot}"
                );
            }
        }
    }

    // Row 0 has nowhere to move up to, so its arrow must not be drawn at all —
    // vanilla's `if (index > 0)` guard.
    let (ix0, iy0, _, _) = server_entry_icon_rect(0, V_W, 0.0);
    f.cursor = Some((ix0 + 4.0, iy0 + 4.0));
    let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;
    let up = region(SERVER_MOVE_UP_SPRITES.0);
    let up_hot = region(SERVER_MOVE_UP_SPRITES.1);
    assert!(
        !any_quad_within(&sprite, up.0, up.1) && !any_quad_within(&sprite, up_hot.0, up_hot.1),
        "row 0 must draw no move-up arrow"
    );
    let down = region(SERVER_MOVE_DOWN_SPRITES.0);
    assert!(
        any_quad_within(&sprite, down.0, down.1),
        "control: its move-down arrow is there, so the detector works"
    );
    // And with no cursor, none of the six is drawn.
    f.cursor = None;
    let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;
    for (plain, hot) in regions {
        let (p, hgt) = (region(plain), region(hot));
        assert!(!any_quad_within(&sprite, p.0, p.1), "{plain} without a cursor");
        assert!(!any_quad_within(&sprite, hgt.0, hgt.1), "{hot} without a cursor");
    }
}


/// The status sprite is asserted **by identity through the atlas**, at the rect
/// vanilla puts it at: a gate that only proved a ping bar exists passes on all
/// four states rendering the same bar.
///
/// Also the footer's disabled path, per button, by the same joint test — where
/// it landed *and* which region it sampled. The expected sprite comes from
/// `WidgetSprites::get`, never spelled out.
#[test]
fn the_status_sprite_and_the_disabled_footer_sample_the_sprites_they_should() {
    use crate::menu::nav::{SERVER_LIST_BUTTONS, ServerListButton as B};
    use crate::menu::status::{PING_SPRITES, ServerStatus};

    let atlas = server_list_atlas();
    let (mut nav, mut ui) = list_nav("sprites", &[]);
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();

    // Empty list: Join / Edit / Delete / Direct all draw `button_disabled`,
    // each at its own rect, and the other three draw `button`.
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let stream = build(&f, Some(&atlas), None, V_W, V_H).sprite;
    let check = |stream: &[f32], button: B, enabled: bool| {
        let want = widget::BUTTON_SPRITES.get(enabled, false);
        let (min, max) = sprite_uv_bounds(&atlas, want);
        let rect = server_list_footer_slot(button).resolve(V_W, V_H);
        let uvs = uvs_in_dest(stream, V_W, V_H, rect);
        assert!(!uvs.is_empty(), "{button:?} drew nothing at {rect:?}");
        assert!(
            uvs.iter().all(|uv| {
                uv[0] >= min[0] - 1e-6
                    && uv[0] <= max[0] + 1e-6
                    && uv[1] >= min[1] - 1e-6
                    && uv[1] <= max[1] + 1e-6
            }),
            "{button:?} did not sample {want} (enabled={enabled})"
        );
    };
    for button in SERVER_LIST_BUTTONS {
        check(&stream, button, button.enabled(false));
    }

    // Control, executed: a saved server flips three of them, so the assertion
    // above measures the selection and not a screen that is always disabled.
    add_server(&mut nav, &mut ui, "HOME", "mc.example.com");
    let mut statuses = StatusCache::with_probe(std::sync::Arc::new(|_| {
        Ok(ServerStatus {
            motd: "hello".into(),
            players: "2/8".into(),
            protocol: Some(crate::menu::status::STATUS_PROTOCOL),
            latency_ms: Some(400),
            ..Default::default()
        })
    }));
    let entries = nav.list().entries().to_vec();
    statuses.refresh(&entries);
    let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
    while statuses.pump() == 0 && crate::platform::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let stream = build(&f, Some(&atlas), None, V_W, V_H).sprite;
    for button in SERVER_LIST_BUTTONS {
        check(&stream, button, button.enabled(true));
    }
    for b in [B::Select, B::Edit, B::Delete] {
        assert!(b.enabled(true) && !b.enabled(false), "control premise: {b:?}");
    }

    // 400 ms is the middle bucket. Asserted at the status icon's own rect, so
    // this is both "the right sprite" and "in the right place".
    let rect = server_status_icon_rect(0, V_W, 0.0);
    let uvs = uvs_in_dest(&stream, V_W, V_H, rect);
    assert!(!uvs.is_empty(), "no status sprite at {rect:?}");
    let (min, max) = sprite_uv_bounds(&atlas, PING_SPRITES[2]);
    assert!(
        uvs.iter().all(|uv| {
            uv[0] >= min[0] - 1e-6
                && uv[0] <= max[0] + 1e-6
                && uv[1] >= min[1] - 1e-6
                && uv[1] <= max[1] + 1e-6
        }),
        "400 ms must sample {} — three bars",
        PING_SPRITES[2]
    );
    // Control: it is not sampling a *different* bucket's sprite, which is what
    // "some ping bar drew" would have accepted.
    let (fmin, fmax) = sprite_uv_bounds(&atlas, PING_SPRITES[4]);
    assert!(
        !uvs
            .iter()
            .all(|uv| uv[0] >= fmin[0] - 1e-6 && uv[0] <= fmax[0] + 1e-6
                && uv[1] >= fmin[1] - 1e-6
                && uv[1] <= fmax[1] + 1e-6),
        "the detector cannot tell ping_3 from ping_5"
    );
}


#[test]
fn the_error_screen_carries_the_disconnect_reason() {
    // Since `error_frame`'s conversion onto the framework, the reason
    // lives in `notice` (a wrapped, bounded `MenuNotice`, like the
    // account screen's failure message) rather than `message` — a
    // `vanilla` frame suppresses `message` entirely (see `MenuNotice`'s
    // own doc on why an unwrapped line was the bug this pattern fixes).
    let nav = test_nav("err");
    let mut ui = UiState::new();
    ui.begin(SessionKind::Multiplayer);
    ui.session_failed(crate::sim::SessionEnd::disconnected(
            lodestone_model::ResolvedText::literal("Server closed"),
        ));
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    assert!(f.vanilla, "the disconnect screen is on the framework now");
    assert!(f.message.is_none(), "a vanilla frame draws no `message`");
    let notice = f.notice.expect("the reason must reach the screen");
    assert!(notice.text.contains("Server closed"), "{}", notice.text);
    assert_eq!(
        f.rows[0].label, "Back to Title Screen",
        "vanilla's gui.toTitle, since dismiss_error always returns to MainMenu"
    );
    assert!(f.rows[0].slot.is_some(), "the button is vanilla-placed now");
}


#[test]
fn a_favicon_is_decoded_once_not_once_per_frame() {
    // 60 zlib inflations per second per row is the bug this prevents.
    let png = solid_png(8, [1, 2, 3, 255]);
    let mut fav = FaviconCache::new();
    assert!(fav.is_empty());
    let first = fav.get("a.example:25565", &png);
    assert!(first.is_some());
    assert_eq!(fav.len(), 1);
    for _ in 0..100 {
        assert_eq!(fav.get("a.example:25565", &png), first);
    }
    assert_eq!(fav.len(), 1, "one entry per address, whatever the frame count");

    // A failed decode is cached too, or a broken icon retries forever.
    assert!(fav.get("b.example:25565", b"not a png").is_none());
    assert_eq!(fav.len(), 2);
    fav.forget("b.example:25565");
    assert_eq!(fav.len(), 1);
}
