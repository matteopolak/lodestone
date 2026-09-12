use super::*


#[test]
fn owns_frame_excludes_paused_so_the_pause_menu_never_replaces_the_world() {
    // The specific regression this module's docs warn about: adding
    // `Screen::Paused` to `owns_frame` would make `app.rs`'s `draw_menu`
    // return `true` for it, skipping the world/HUD/container render path
    // entirely — the pause menu would work, but the game behind it would
    // stop rendering for as long as it was up.
    assert!(!owns_frame(Screen::Paused));
}
#[test]
fn frame_for_defers_to_an_overlay_for_in_world_settings() {
    // The player report this exists for: Options opened from the pause
    // menu must show the paused *world* behind it, not the main-menu
    // panorama. `frame_for` returning `Some` unconditionally for
    // `Screen::Settings` was exactly the bug — `draw_menu` took the
    // `Clear` pass and the world (and its HUD/container passes) never
    // drew at all.
    //
    // Two controls in one test, by construction rather than assertion:
    // the title-screen route still gets a frame at all (a regression that
    // made *every* Options entry return `None` would still pass a
    // negative-only check), and `owns_frame` staying `true` for both
    // routes is what proves the two are meant to diverge here, not drift
    // apart by accident.
    let nav = test_nav("settings-overlay");
    let mut fav = FaviconCache::new();
    let statuses = StatusCache::with_probe(unavailable_probe());

    let mut from_title = UiState::new();
    from_title.open_settings();
    assert!(!from_title.settings_in_world());
    assert!(
        frame_for(&from_title, &nav, &statuses, &mut fav).is_some(),
        "Options from the title screen must still own the frame — it has \
         no world to show behind it"
    );

    let mut from_pause = UiState::new();
    from_pause.enter_dev_world();
    from_pause.pause();
    from_pause.open_settings_from_pause();
    assert!(from_pause.settings_in_world());
    assert!(
        frame_for(&from_pause, &nav, &statuses, &mut fav).is_none(),
        "in-world Options must defer to an overlay over the still-\
         rendering world, not the Clear pass"
    );
    // `owns_frame` itself is unchanged either way — see its doc.
    assert!(owns_frame(Screen::Settings));
}

/// The Statistics half of the same defect class: a player report
/// (2026-08-15, *"right now our client does not blur and shows a panorama
/// for the statistics page"*) caught that the Statistics screen showed the
/// panorama instead of the paused world it is always opened over. Unlike
/// `Screen::Settings` there is no out-of-world route at all — this is the
/// **only** case, so the general sweep above cannot cover it (it is carved
/// out of that loop for exactly this reason) and this is the dedicated gate.
#[test]
fn frame_for_defers_to_an_overlay_for_statistics() {
    let nav = test_nav("stats-overlay");
    let mut fav = FaviconCache::new();
    let statuses = StatusCache::with_probe(unavailable_probe());

    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.pause();
    ui.open_statistics_from_pause();
    assert_eq!(ui.screen(), Screen::Statistics, "premise: reached the screen");
    assert!(
        frame_for(&ui, &nav, &statuses, &mut fav).is_none(),
        "Statistics must defer to an overlay over the still-rendering world, \
         not the Clear pass — see that arm's own doc"
    );
    assert!(owns_frame(Screen::Statistics), "owns_frame must stay true regardless");

    // And the overlay frame itself carries the fix: `Dim`, not the default
    // `Panorama`, plus the same canvas stamp `frame_for` gives every other
    // screen (a frame built outside `frame_for`'s own `Some` arm does not get
    // it for free — see `stamp_canvas_facts`'s own doc on exactly this gap).
    let overlay = crate::menu::nav::stats_overlay_frame(&ui, &nav)
        .expect("the overlay frame must exist once the screen is open");
    assert_eq!(
        overlay.backdrop,
        MenuBackdrop::Dim,
        "the paused world must stay visible behind Statistics, not the panorama"
    );
    assert_eq!(overlay.gui_scale, nav.gui_scale());
    assert_eq!(overlay.panorama_speed, Some(nav.panorama_speed()));
    assert_eq!(overlay.cursor, nav.menu_cursor());

    // -- control ----------------------------------------------------------
    // `MenuBackdrop::default()` is `Panorama` — the bug this test exists to
    // catch is exactly "the frame's `backdrop` field was left at its
    // `..Default::default()`", so the control proves the assertion above
    // discriminates rather than passing for any backdrop value.
    assert_ne!(
        overlay.backdrop,
        MenuBackdrop::default(),
        "the default backdrop is Panorama, so this control must fail if the \
         fix regresses to it"
    );
}

/// The Social Interactions half of the same defect class as Statistics —
/// found in a backdrop audit rather than a player report this time, but the
/// shape is identical: no out-of-world route at all
/// (`UiState::open_social_from_pause` only opens `Screen::Social` from
/// `Screen::Paused`), so the general sweep above cannot cover it either and
/// this is the dedicated gate.
#[test]
fn frame_for_defers_to_an_overlay_for_social() {
    let nav = test_nav("social-overlay");
    let mut fav = FaviconCache::new();
    let statuses = StatusCache::with_probe(unavailable_probe());

    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.pause();
    ui.open_social_from_pause();
    assert_eq!(ui.screen(), Screen::Social, "premise: reached the screen");
    assert!(
        frame_for(&ui, &nav, &statuses, &mut fav).is_none(),
        "Social must defer to an overlay over the still-rendering world, not \
         the Clear pass — see that arm's own doc"
    );
    assert!(owns_frame(Screen::Social), "owns_frame must stay true regardless");

    let overlay = crate::menu::nav::social_overlay_frame(&ui, &nav)
        .expect("the overlay frame must exist once the screen is open");
    assert_eq!(
        overlay.backdrop,
        MenuBackdrop::Dim,
        "the paused world must stay visible behind Social, not the panorama"
    );
    assert!(
        overlay.blur,
        "vanilla blurs behind Social exactly as it does behind Statistics — \
         see MenuFrame::blur's own doc"
    );
    assert_eq!(overlay.gui_scale, nav.gui_scale());
    assert_eq!(overlay.panorama_speed, Some(nav.panorama_speed()));
    assert_eq!(overlay.cursor, nav.menu_cursor());
    assert!(
        !geometry(&overlay, 1280.0, 720.0).is_empty(),
        "the overlay frame must actually rasterise to vertices, not just \
         carry the right metadata — the same layer a whole corpus stopping \
         one level above the draw has missed before in this codebase"
    );

    // -- control ----------------------------------------------------------
    assert_ne!(
        overlay.backdrop,
        MenuBackdrop::default(),
        "the default backdrop is Panorama, so this control must fail if the \
         fix regresses to it"
    );
}

/// The Server Links screen never had this bug in the first place — it was
/// built overlay-only from the start (see `Screen::ServerLinks`'s own doc) —
/// but the gate is cheap and the shape is identical, so it is worth pinning
/// alongside the two recurrences above rather than trusting the general
/// sweep's silent pass to mean the same thing it means for every other
/// screen.
#[test]
fn frame_for_defers_to_an_overlay_for_server_links() {
    let nav = test_nav("server-links-overlay");
    let mut fav = FaviconCache::new();
    let statuses = StatusCache::with_probe(unavailable_probe());

    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.pause();
    ui.open_server_links_from_pause();
    assert_eq!(ui.screen(), Screen::ServerLinks, "premise: reached the screen");
    assert!(frame_for(&ui, &nav, &statuses, &mut fav).is_none());
    assert!(
        !owns_frame(Screen::ServerLinks),
        "unlike Statistics/Settings, this screen never owned the Clear pass"
    );

    let overlay = crate::menu::nav::server_links_overlay_frame(&ui, &nav)
        .expect("the overlay frame must exist once the screen is open");
    assert_eq!(overlay.backdrop, MenuBackdrop::Dim);
    assert_ne!(overlay.backdrop, MenuBackdrop::default());
}

/// The resource-pack prompt's backdrop, audited alongside the Social fix
/// above and found to carry **two** bugs of the identical defect class,
/// neither reachable from `owns_frame_agrees_with_frame_for_on_every_screen`'s
/// general sweep: that loop only ever reaches this screen the way
/// `every_mouse_routable_screen_has_a_frame_to_hit_test`'s own
/// `"ResourcePackPrompt"` case does not either — through
/// `ui.begin(SessionKind::Multiplayer)`, the *no*-world case, never through a
/// live world. See `nav::resource_pack_prompt_overlay_frame`'s own doc for
/// the record citation this fix is derived from.
#[test]
fn resource_pack_prompt_overlay_frame_is_dim_and_blurred_in_world_and_untouched_otherwise() {
    let mut nav_out = test_nav("prompt-overlay-out-of-world");
    let mut ui_out = UiState::new();
    ui_out.begin(crate::menu::SessionKind::Multiplayer);
    nav_out.show_resource_pack_prompt(
        &mut ui_out,
        &crate::net::PendingResourcePackPrompt::for_test(uuid::Uuid::from_u128(1), false),
    );
    assert_eq!(ui_out.screen(), Screen::ResourcePackPrompt, "premise");
    assert!(!ui_out.resource_pack_prompt_in_world(), "premise: no level yet");
    let out_frame = crate::menu::nav::resource_pack_prompt_overlay_frame(&ui_out, &nav_out)
        .expect("the overlay frame must exist once the screen is open");
    assert_eq!(
        out_frame.backdrop,
        MenuBackdrop::default(),
        "no level -- vanilla's own `level == null` fork wants the panorama, \
         untouched from the frame builder's own default"
    );
    assert!(
        !out_frame.blur,
        "this port scopes the blur pass to render_overlay screens with a \
         live world -- see render::blur's own module doc on the Connecting \
         case being a stated cut, not an oversight"
    );

    let mut nav_in = test_nav("prompt-overlay-in-world");
    let mut ui_in = UiState::new();
    ui_in.enter_dev_world();
    nav_in.show_resource_pack_prompt(
        &mut ui_in,
        &crate::net::PendingResourcePackPrompt::for_test(uuid::Uuid::from_u128(2), false),
    );
    assert_eq!(ui_in.screen(), Screen::ResourcePackPrompt, "premise");
    assert!(ui_in.resource_pack_prompt_in_world(), "premise: a live world");
    let in_frame = crate::menu::nav::resource_pack_prompt_overlay_frame(&ui_in, &nav_in)
        .expect("the overlay frame must exist once the screen is open");
    assert_eq!(
        in_frame.backdrop,
        MenuBackdrop::Dim,
        "a live world must stay visible behind the prompt, not the panorama \
         -- the bug this test exists to catch: `resource_pack_prompt_frame` \
         builds via `..Default::default()`, whose backdrop is `Panorama`, \
         and nothing used to override it"
    );
    assert!(
        in_frame.blur,
        "vanilla's own pack-confirm screen does not override its own in-game-ui predicate either"
    );
    // The second bug this audit found: the canvas stamp was missing
    // entirely, not just the backdrop.
    assert_eq!(in_frame.gui_scale, nav_in.gui_scale());
    assert_eq!(in_frame.panorama_speed, Some(nav_in.panorama_speed()));
    assert_eq!(in_frame.cursor, nav_in.menu_cursor());

    // -- control ------------------------------------------------------------
    assert_ne!(
        in_frame.backdrop,
        out_frame.backdrop,
        "the two cases must actually diverge, or the assertions above are \
         not discriminating between them"
    );
}

/// **The layer `stats.rs`'s own zebra test cannot reach.**
/// `general_row_colour_alternates_and_the_two_shades_are_vanillas_own_argb`
/// (in `menu::stats`) proves `MenuFrame::list_labels[i].colour` is right; it
/// stops one layer above the draw, exactly the shape `CLAUDE.md` warns a
/// whole corpus can share (the menu tab widget's `draw_tab` went dead for
/// this reason). This asserts the real rasterised geometry — through
/// `nav::stats_overlay_frame`, the actual production path now that
/// Statistics is an overlay — carries vanilla's own `0xFFBABABA` (`-4539718`,
/// from vanilla's own stats-screen general-statistics-list entry
/// content-extraction logic) grey somewhere on screen, not only inside the
/// frame's own metadata.
#[test]
fn the_odd_stat_rows_zebra_grey_reaches_real_geometry_not_just_frame_metadata() {
    let nav = test_nav("stats-zebra-geometry");
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.pause();
    ui.open_statistics_from_pause();
    let overlay = crate::menu::nav::stats_overlay_frame(&ui, &nav)
        .expect("the overlay frame must exist once the screen is open");
    let verts = geometry(&overlay, V_W, V_H);

    // Vanilla's own literal, unpacked the same way `stats.rs`'s own test
    // does — not a restated `0.729` float that could quietly drift from it.
    let odd_grey = widget::argb_to_rgba(-4_539_718);
    assert!(
        colour_bounds(&verts, V_W, V_H, odd_grey).is_some(),
        "the odd row's 0xFFBABABA grey never reached a real vertex — the \
         value is right in `general_row_colour` but not painted"
    );

    // The control: plain white (the even rows, and also this shell's default
    // `ACTIVE_LABEL`) must also be present, proving the probe can see text on
    // this screen at all — a probe that found nothing for *either* colour
    // would make the assertion above vacuous.
    assert!(
        colour_bounds(&verts, V_W, V_H, widget::argb_to_rgba(-1)).is_some(),
        "control: the even rows' white never reached a vertex either, so \
         this probe is not actually seeing the stats list"
    );

    // The discriminating control CLAUDE.md's own evidence section asks for:
    // the two shades must not coincide, or a solid-white implementation
    // a solid-white implementation would satisfy the assertion above by
    // accident.
    assert_ne!(odd_grey, widget::argb_to_rgba(-1));
}

/// The unpublished row set — `nav.set_lan_published` is never called, so this
/// is `MenuNav::pause_buttons`'s `PAUSE_BUTTONS` answer. See
/// `the_published_pause_frame_drops_open_to_lan_and_reflows_options` below for
/// the other one, added when scope 2 made this list conditional.
///
/// `set_has_singleplayer_server(true)` states the other half of that same
/// gate explicitly — `MenuNav::open_to_lan_available`'s own doc explains why
/// a session kind of `None` (`enter_dev_world`'s own default) must not read
/// as "offer Open to LAN" the way it used to before that field existed.
#[test]
fn pause_frame_builds_vanillas_ten_widgets_in_order_and_tracks_the_highlight() {
    use crate::menu::nav::{PAUSE_BUTTONS, PauseButton};

    let mut nav = test_nav("pause-frame");
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.pause();
    nav.set_has_singleplayer_server(true);
    // The last index, not 1: this screen reproduces vanilla's whole grid, so
    // Disconnect is the last widget rather than the third. The old version of this
    // test asserted a three-row stack.
    nav.hover(&ui, PAUSE_BUTTONS.len() - 1);

    let f = pause_frame(&nav);
    assert_eq!(
        f.backdrop,
        MenuBackdrop::Dim,
        "the pause menu must dim the live world it is drawn over, not replace it \
         with the panorama"
    );
    assert!(f.vanilla, "and it must be laid out from vanilla's arithmetic");
    // Ten rows are expected: the singleplayer Open to LAN button sits beside
    // Options. Read from the button table rather than restated, so the two cannot
    // drift.
    assert_eq!(f.rows.len(), PAUSE_BUTTONS.len());
    assert_eq!(f.rows[0].label, PauseButton::BackToGame.label());
    assert_eq!(f.rows[1].label, PauseButton::Advancements.label());
    assert_eq!(f.rows[2].label, PauseButton::Statistics.label());
    assert_eq!(f.rows[7].label, PauseButton::Options.label());
    assert_eq!(f.rows[8].label, PauseButton::OpenToLan.label());
    assert_eq!(f.rows[9].label, PauseButton::QuitToTitle.label());
    assert_eq!(
        f.selected,
        PAUSE_BUTTONS.len() - 1,
        "selection follows the nav's pause_index"
    );
    // Seven are live: the three with actions, plus Advancements, Statistics and
    // Player Reporting has a live screen behind it, and Open to LAN has a
    // caller through `IntegratedServer::open_to_lan`
    // (see `PauseButton::enabled`'s own doc for each — what each screen shows is
    // honest-but-limited, not what made the button liveness conditional).
    let live: Vec<&str> = f
        .rows
        .iter()
        .filter(|r| r.enabled)
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(
        live,
        vec![
            "Back to Game",
            "Advancements",
            "Statistics",
            "Player Reporting",
            "Options...",
            "Open to LAN",
            "Disconnect"
        ]
    );
    // The four icon buttons carry a sprite instead of a label.
    assert_eq!(f.rows.iter().filter(|r| r.icon.is_some()).count(), 4);
    assert!(f.rows.iter().all(|r| r.slot.is_some()));
    // And the heading is a positioned label, not the row stack's title.
    assert!(f.title.is_empty());
    assert_eq!(f.labels.len(), 1);
    assert_eq!(f.labels[0].text, "Game Menu");
    assert!(!geometry(&f, 1280.0, 720.0).is_empty());
}

/// Once the hosted world is published, Open to LAN has
/// nothing left to offer (this client has no unpublish/toggle form — see
/// `PauseButton::OpenToLan`'s own doc) and `MenuNav::pause_buttons` omits it
/// rather than merely disabling it.
///
/// The discriminating check is the row **set**, not just a flag on one row:
/// a `enabled: false` row would still show up in `f.rows` at the same
/// length (10) as the unpublished case, so a gate that only checked
/// `f.rows[8].enabled` would pass under the wrong hypothesis (disabled, not
/// omitted) too. Asserting `f.rows.len() == 9` — one less than
/// `pause_frame_builds_vanillas_ten_widgets_in_order_and_tracks_the_highlight`'s
/// 10 — is what a "disabled, not omitted" implementation gets wrong, since a
/// disabled row is still a row. The rect check goes one step further: it also
/// catches a "the row is gone but the grid still leaves the gap where it was"
/// implementation, which a bare row-count check cannot.
#[test]
fn the_published_pause_frame_drops_open_to_lan_and_reflows_options() {
    use crate::menu::nav::{PAUSE_BUTTONS, PAUSE_BUTTONS_PUBLISHED, PauseButton};

    let mut nav = test_nav("pause-frame-published");
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.pause();
    nav.set_lan_published(true);

    // The row **set**: nine, not ten, and Open to LAN is not merely disabled
    // among them — it is absent.
    assert_eq!(nav.pause_buttons(), PAUSE_BUTTONS_PUBLISHED.as_slice());
    assert_ne!(
        PAUSE_BUTTONS_PUBLISHED.len(),
        PAUSE_BUTTONS.len(),
        "premise: the published list really is shorter, or this gate cannot \
         distinguish omission from disablement"
    );
    assert!(
        !PAUSE_BUTTONS_PUBLISHED.contains(&PauseButton::OpenToLan),
        "premise: Open to LAN really is missing from the published list"
    );

    let f = pause_frame(&nav);
    assert_eq!(f.rows.len(), PAUSE_BUTTONS_PUBLISHED.len());
    assert!(
        f.rows.iter().all(|r| r.label != PauseButton::OpenToLan.label()),
        "Open to LAN must not appear anywhere in the published frame's rows"
    );
    assert_eq!(f.rows[0].label, PauseButton::BackToGame.label());
    assert_eq!(f.rows[7].label, PauseButton::Options.label());
    assert_eq!(f.rows[8].label, PauseButton::QuitToTitle.label());

    // The discriminating rect: vanilla's own non-singleplayer branch gives
    // Options the full-width row alone (vanilla's own pause-screen menu
    // construction) once there is no sibling to share it with, and this
    // reproduces that shape rather than leaving a half-width gap where Open
    // to LAN used to sit.
    let options_rect = f.rows[7].slot.expect("Options has a slot").resolve(V_W, V_H);
    let (_, _, options_w, _) = options_rect;
    assert_eq!(
        options_w, 204.0,
        "Options must take the full-width row once published, matching \
         vanilla's non-singleplayer branch — a merely-disabled Open to LAN \
         would leave this at 98.0"
    );
    // And Disconnect's rect is exactly where the unpublished grid puts it —
    // dropping one row's *sibling* must not move any other row, since both
    // grids keep the same five rows (see `pause_menu_grid_with`'s own doc).
    let disconnect_rect = f.rows[8].slot.expect("Disconnect has a slot").resolve(V_W, V_H);
    assert_eq!(
        disconnect_rect,
        pause_slot(PauseButton::QuitToTitle, false).resolve(V_W, V_H),
        "Disconnect's rect must be unchanged by publishing"
    );
}

#[test]
fn the_title_screen_rects_are_vanillas_own() {
    use crate::menu::nav::MainButton as B;
    // Hand-derived from vanilla's own title-screen init and its
    // normal-menu-options construction at 854×480, *not* read back out of
    // `title_slot`: vanilla's own top-position local = 480/4 + 48 = 168, rows every 24 px, the icon
    // row from vanilla's own horizontal-position accessor(n, 3, 20) = 427 - 34 + (n-1)*24, and
    // the Options/Quit pair at `W/2 - 100` / `W/2 + 2`, 98 wide.
    //
    // `title_slot` computes these from an arranged
    // `LinearLayout` column instead of holding them as constants, so this is
    // the **no-move gate** for that conversion: the table is vanilla's own
    // hand arithmetic (which uses no layout class at all) and the values come
    // out of the layout tree. If the two ever disagree, one of them is wrong
    // and this says which button.
    let expected = [
        (B::Singleplayer, (327.0, 168.0, 200.0, 20.0)),
        (B::Multiplayer, (327.0, 192.0, 200.0, 20.0)),
        (B::Realms, (327.0, 216.0, 200.0, 20.0)),
        (B::Friends, (393.0, 240.0, 20.0, 20.0)),
        (B::Language, (417.0, 240.0, 20.0, 20.0)),
        (B::Accessibility, (441.0, 240.0, 20.0, 20.0)),
        (B::Options, (327.0, 264.0, 98.0, 20.0)),
        (B::Quit, (429.0, 264.0, 98.0, 20.0)),
    ];
    for (button, want) in expected {
        assert_eq!(
            title_slot(button).resolve(V_W, V_H),
            want,
            "{button:?} is not where vanilla puts it"
        );
    }
    // The 4 px gutter between Options and Quit is the title screen's, and it
    // is *not* the pause screen's 8 px one — a detail that is easy to
    // conflate, so pin both.
    let (ox, _, ow, _) = title_slot(B::Options).resolve(V_W, V_H);
    let (qx, ..) = title_slot(B::Quit).resolve(V_W, V_H);
    assert_eq!(qx - (ox + ow), 4.0, "title screen gutter");
}

/// The unpublished grid — scope 2 gave `pause_slot` a second
/// `published: true` arrangement (`the_published_pause_screen_drops_open_to_lan`
/// below), so every rect here is pinned with `false` rather than left
/// implicit.
#[test]
fn the_pause_screen_rects_are_vanillas_own() {
    use crate::menu::nav::PauseButton as B;
    // Hand-derived from vanilla's own pause-menu construction
    // through `GridLayout`'s own element-arrangement pass, at 854×480: the 212×166 grid is
    // aligned (0.5, 0.25) so its origin is (321, 78); row y offsets inside it
    // are [0, 70, 94, 118, 142] and each child sits at its own padding.
    //
    // These nine rects are `pause_slot`'s expectation: the values below come
    // out of a real ported
    // `GridLayout`, and the table is the independent derivation they have to
    // agree with. Two derivations of the same arithmetic, one by hand from the
    // Java and one by running a port of it — which is the only shape of gate
    // that can catch a port that is self-consistently wrong.
    let gx = 321.0;
    let gy = 78.0;
    let expected = [
        (B::BackToGame, (gx + 4.0, gy + 50.0, 204.0, 20.0)),
        (B::Advancements, (gx + 4.0, gy + 74.0, 98.0, 20.0)),
        (B::Statistics, (gx + 110.0, gy + 74.0, 98.0, 20.0)),
        (B::ReportBugs, (gx + 60.0, gy + 98.0, 20.0, 20.0)),
        (B::Feedback, (gx + 84.0, gy + 98.0, 20.0, 20.0)),
        (B::Friends, (gx + 108.0, gy + 98.0, 20.0, 20.0)),
        (B::PlayerReporting, (gx + 132.0, gy + 98.0, 20.0, 20.0)),
        // The local hosted-game branch creates two half-width cells, with the same
        // 8 px gutter and row `y` as the Advancements/Statistics pair two rows up.
        // The grid's five
        // row offsets and its 212x166 size are unchanged, which is why this repair
        // touches three lines and not the table.
        (B::Options, (gx + 4.0, gy + 122.0, 98.0, 20.0)),
        (B::OpenToLan, (gx + 110.0, gy + 122.0, 98.0, 20.0)),
        (B::QuitToTitle, (gx + 4.0, gy + 146.0, 204.0, 20.0)),
    ];
    for (button, want) in expected {
        assert_eq!(
            pause_slot(button, false).resolve(V_W, V_H),
            want,
            "{button:?} is not where vanilla puts it"
        );
    }
    // The grid origin itself, spelled out: 0.5/0.25 alignment of 212×166.
    assert_eq!(Origin::PauseGrid.anchor(V_W, V_H), (gx, gy));
    // A full-width pause button starts at `W/2 - 102`, not the title
    // screen's `W/2 - 100`, and the half-width pair has an 8 px gutter, not
    // 4 — both fall out of the 204+8 cell, and both are the details a
    // remembered layout gets wrong.
    assert_eq!(
        pause_slot(B::BackToGame, false).resolve(V_W, V_H).0,
        V_W / 2.0 - 102.0
    );
    let (ax, _, aw, _) = pause_slot(B::Advancements, false).resolve(V_W, V_H);
    let (sx, ..) = pause_slot(B::Statistics, false).resolve(V_W, V_H);
    assert_eq!(sx - (ax + aw), 8.0, "pause screen gutter");
    assert_eq!(
        (ax + aw + sx) / 2.0,
        V_W / 2.0,
        "the half-width pair straddles the centre line"
    );
}

#[test]
fn the_pause_grid_size_is_the_arranged_layouts_own() {
    // `Origin::PauseGrid` aligns the grid's *measured* size in the screen
    // rect, so that size is load-bearing for all nine rects at once — a grid
    // 2 px too wide moves every button 1 px left. `PAUSE_GRID_W`/`_H` are the
    // hand derivation (204 + 4 + 4 wide; 70 + 4 * 24 tall) and this is the
    // only place they are compared with what the port computes.
    assert_eq!(pause_grid_size(), (PAUSE_GRID_W, PAUSE_GRID_H));
    // The same numbers reached the other way, from the arranged tree rather
    // than the cache, so the cache cannot be what is agreeing with itself.
    let grid = pause_menu_grid_with(PAUSE_MENU_PADDING_TOP, false);
    assert_eq!((grid.width(), grid.height()), (212.0, 166.0));
    // And the grid really does hold nine drawable leaves in `PAUSE_BUTTONS`
    // order — the four icon buttons among them come from a *nested*
    // `LinearLayout`, so this is also the assertion that `visit_widgets`
    // flattens the nesting rather than yielding the row as one child.
    assert_eq!(
        layout::widget_rects(&grid).len(),
        crate::menu::nav::PAUSE_BUTTONS.len()
    );
}

/// `pause_grid_size` takes no `published` flag (see its own doc) because the
/// two arranged grids share the same overall size — this is the gate that
/// premise depends on: if a future edit ever made the published Options row
/// change the grid's height or width, this would be the first thing to fail,
/// well before `Origin::PauseGrid`'s callers noticed misplaced rects.
#[test]
fn the_pause_grid_size_matches_whether_or_not_lan_is_published() {
    let unpublished = pause_menu_grid_with(PAUSE_MENU_PADDING_TOP, false);
    let published = pause_menu_grid_with(PAUSE_MENU_PADDING_TOP, true);
    assert_eq!(
        (unpublished.width(), unpublished.height()),
        (published.width(), published.height()),
        "the published grid drops Open to LAN's row *sibling*, not a row — \
         both arrangements keep the same five rows"
    );
    // And the leaf count really did drop by exactly one — the two grids
    // agreeing on size is not evidence they are identical.
    assert_eq!(
        layout::widget_rects(&unpublished).len(),
        crate::menu::nav::PAUSE_BUTTONS.len()
    );
    assert_eq!(
        layout::widget_rects(&published).len(),
        crate::menu::nav::PAUSE_BUTTONS_PUBLISHED.len()
    );
}

#[test]
fn a_changed_cell_padding_moves_every_pause_rect() {
    // Negative control, executed rather than described: change one
    // `LayoutSettings` padding value and the rect assertions must go red. The
    // subject is the real builder with one argument varied, not a copy of it,
    // so this cannot pass by testing something else.
    //
    // `MENU_PADDING_TOP` is row 0's `paddingTop`. Dropping it by 10 must
    // (a) move Back to Game up 10, (b) shrink the grid 10, and therefore
    // (c) move every *later* row up 10 as well — a silently no-op arrange pass
    // would fail all three.
    let real = layout::widget_rects(&pause_menu_grid_with(PAUSE_MENU_PADDING_TOP, false));
    let short = layout::widget_rects(&pause_menu_grid_with(PAUSE_MENU_PADDING_TOP - 10, false));
    assert_eq!(real[0].1, 50.0);
    assert_eq!(short[0].1, 40.0, "row 0's padding must move row 0");
    for (i, (r, s)) in real.iter().zip(&short).enumerate() {
        assert_eq!(
            r.1 - s.1,
            10.0,
            "widget {i} did not move with the row above it"
        );
        assert_eq!(r.0, s.0, "and nothing may move horizontally");
    }
    let grid = pause_menu_grid_with(PAUSE_MENU_PADDING_TOP - 10, false);
    assert_eq!(
        (grid.width(), grid.height()),
        (PAUSE_GRID_W, PAUSE_GRID_H - 10.0),
        "the grid's own height is the sum of its rows, so it must shrink too"
    );
}

#[test]
fn death_frame_builds_vanillas_two_widgets_in_order_and_tracks_the_highlight() {
    use crate::menu::nav::{DEATH_BUTTONS, DeathButton};

    let mut nav = test_nav("death-frame");
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.die(Some(plain_death_message("was slain by a Skeleton")));
    nav.hover(&ui, 1);

    let f = death_frame(&nav, ui.death_message());
    assert_eq!(
        f.backdrop,
        MenuBackdrop::DeathGradient,
        "the death screen must draw vanilla's own red gradient over the live world, \
         not the flat Dim wash the pause screen uses"
    );
    assert!(f.vanilla, "and be laid out from vanilla's arithmetic");
    assert_eq!(f.rows.len(), 2, "vanilla's death screen has two widgets");
    assert_eq!(f.rows[0].label, DeathButton::Respawn.label());
    assert_eq!(f.rows[1].label, DeathButton::TitleScreen.label());
    assert!(
        f.rows.iter().all(|r| r.enabled),
        "unlike title/pause, neither death-screen button is ever disabled"
    );
    assert!(f.rows.iter().all(|r| r.slot.is_some()));
    assert_eq!(f.selected, 1, "selection follows the nav's death_index");
    assert_eq!(DEATH_BUTTONS.len(), 2);

    // The heading is a positioned label (the title), not the row stack's
    // centred title string.
    assert!(f.title.is_empty());
    // Title + message + score.
    assert_eq!(f.labels.len(), 3);
    assert_eq!(f.labels[0].text, "You Died!");
    assert_eq!(f.labels[0].scale, 2.0, "vanilla scales the title 2x");
    assert_eq!(f.labels[1].text, "was slain by a Skeleton");
    assert_eq!(f.labels[2].text, "Score: 0");
    assert!(!geometry(&f, V_W, V_H).is_empty());

    // No message: two labels, not three, and the score line still draws —
    // matching vanilla's own null-check guard on the cause-of-death message.
    let no_message = death_frame(&nav, None);
    assert_eq!(no_message.labels.len(), 2);
    assert_eq!(no_message.labels[0].text, "You Died!");
    assert_eq!(no_message.labels[1].text, "Score: 0");
}

/// The discriminating gate for the death-screen background: vanilla's own
/// background-extraction
/// logic draws a vertical gradient, not a flat wash, so the colour at the top and the
/// bottom of the backdrop quad must differ — a gate sampling one point (or the
/// vertex-*coverage* helpers elsewhere in this file, which count vertices
/// *inside* a probe rect and so read a full-screen quad as zero coverage) would
/// pass under the pre-fix flat `Dim` fill. This reads the quad's own two edge
/// vertices straight out of the geometry instead.
#[test]
fn death_screen_backdrop_is_a_red_gradient_not_a_flat_dim() {
    let mut nav = test_nav("death-gradient");
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.die(Some(plain_death_message("was slain by a Skeleton")));
    nav.hover(&ui, 0);

    let f = death_frame(&nav, ui.death_message());
    assert_eq!(f.backdrop, MenuBackdrop::DeathGradient);
    let geo = build(&f, None, None, 480.0, 320.0);

    // The backdrop is the very first quad `build` emits (see
    // `MenuGeometry::backdrop_floats`): six vertices in
    // `(top, top, bottom, top, bottom, bottom)` order — see
    // `Quads::rect_vgradient`. Vertex 0 sits on the top edge (`y == 0`),
    // vertex 2 on the bottom edge (`y == height`) — two different heights, not
    // one sample point.
    let vertex_colour = |i: usize| -> [f32; 4] {
        let base = i * STRIDE;
        [
            geo.colour[base + 2],
            geo.colour[base + 3],
            geo.colour[base + 4],
            geo.colour[base + 5],
        ]
    };
    let top = vertex_colour(0);
    let bottom = vertex_colour(2);

    assert_eq!(
        top, DEATH_GRADIENT_TOP,
        "the top edge must be vanilla's decoded top colour (ARGB 96,80,0,0 from \
         `fillGradient`'s first int, 1615855616), got {top:?}"
    );
    assert_eq!(
        bottom, DEATH_GRADIENT_BOTTOM,
        "the bottom edge must be vanilla's decoded bottom colour (ARGB \
         160,128,48,48 from `fillGradient`'s second int, -1602211792), got {bottom:?}"
    );
    assert_ne!(
        top, bottom,
        "a flat fill (the pre-fix Dim backdrop) makes every vertex the same \
         colour — exactly the regression this gate exists to catch"
    );
    // Both channels a real vertical gradient must move, not just alpha —
    // guards against a fix that only varies alpha and leaves the colour grey.
    assert!(
        bottom[3] > top[3] + 0.1,
        "alpha must rise noticeably toward the bottom: top {} bottom {}",
        top[3],
        bottom[3]
    );
    assert!(
        bottom[0] > top[0] + 0.1,
        "red must rise noticeably toward the bottom: top {} bottom {}",
        top[0],
        bottom[0]
    );
}

#[test]
fn the_death_screen_rects_are_vanillas_own() {
    use crate::menu::nav::DeathButton as B;
    // Hand-derived from vanilla's own death-screen init logic at
    // 854×480: both buttons are `width/2-100, height/4+72|96, 200x20`,
    // and `height/4+72 == TitleTop.anchor().1 + 24` since `TitleTop` is
    // itself `floor(height/4) + 48` — 168 + 24 = 192, 168 + 48 = 216.
    let expected = [
        (B::Respawn, (327.0, 192.0, 200.0, 20.0)),
        (B::TitleScreen, (327.0, 216.0, 200.0, 20.0)),
    ];
    for (button, want) in expected {
        assert_eq!(
            death_slot(button).resolve(V_W, V_H),
            want,
            "{button:?} is not where vanilla puts it"
        );
    }
}

#[test]
fn the_death_screens_title_is_anchored_on_the_left_quarter_not_the_centre() {
    // The trap named in `Origin::DeathTitle`'s docs: vanilla's own death-screen
    // text-visiting draws the title at `middleLine / 2` where `middleLine ==
    // width / 2`, i.e. `width / 4` — not `width / 2` like every other
    // centred heading in this file (`Origin::ScreenTop`). A layout
    // "corrected" to the screen centre would fail this by a wide margin.
    //
    // `.floor()`ed: `854.0 / 4.0` is `213.5`, not a whole
    // pixel, where vanilla's `this.width / 2 / 2` is two Java integer
    // divisions and can only ever land on a whole pixel.
    assert_eq!(Origin::DeathTitle.anchor(V_W, V_H), ((V_W / 4.0).floor(), 0.0));
    assert_ne!(
        Origin::DeathTitle.anchor(V_W, V_H).0,
        Origin::ScreenTop.anchor(V_W, V_H).0,
        "the death title and the score/message lines are not on the same x"
    );
}

/// Every width-derived [`Origin`] anchor uses the canvas width
/// (always `int`) divided by a constant — Java integer division — so the x
/// term must be `floor`ed. At an *even* width that is invisible, because
/// `width * 0.5` (or `* 0.25`) is already a whole pixel; **no test before
/// this one used an odd width**, which is exactly how the bug shipped. 855
/// is odd and not a multiple of 4 either, so it exercises every one of the
/// affected arms at once.
///
/// Each assertion predicts *both* hypotheses from `width` alone — floored
/// (right) and unfloored (the bug) — and requires landing on the floored
/// one, per CLAUDE.md's magnitude-species rule: asserting only "the anchor
/// moved" or "is not X.5" would pass for nearly any wrong number too.
#[test]
fn odd_width_anchors_are_floored_like_javas_integer_division() {
    let width = 855.0_f32;
    let height = 481.0_f32;

    let floored_half = (width * 0.5).floor();
    let unfloored_half = width * 0.5;
    assert_eq!(floored_half, 427.0, "sanity: floor(855/2) is 427, not 427.5");
    assert_ne!(floored_half, unfloored_half, "sanity: 855 is odd, so the two must differ");

    assert_eq!(
        Origin::ScreenTop.anchor(width, height),
        (floored_half, 0.0),
        "ScreenTop must not land on the unfloored {unfloored_half}"
    );
    assert_eq!(
        Origin::TitleTop.anchor(width, height),
        (floored_half, (height / 4.0).floor() + 48.0),
        "TitleTop's x must not land on the unfloored {unfloored_half}"
    );
    assert_eq!(
        Origin::ScreenBottom.anchor(width, height),
        (floored_half, height),
        "ScreenBottom must not land on the unfloored {unfloored_half}"
    );

    let floored_quarter = (width * 0.25).floor();
    let unfloored_quarter = width * 0.25;
    assert_eq!(floored_quarter, 213.0, "sanity: floor(855/4) is 213, not 213.75");
    assert_ne!(floored_quarter, unfloored_quarter, "sanity: 855/4 is not a whole pixel");
    assert_eq!(
        Origin::DeathTitle.anchor(width, height),
        (floored_quarter, 0.0),
        "DeathTitle must not land on the unfloored {unfloored_quarter}"
    );
}

#[test]
fn every_vanilla_widget_is_on_screen_and_none_overlap() {
    // The layout arithmetic has to hold at more than one canvas size, and a
    // widget that lands on top of another is a hit-test that activates the
    // wrong button.
    let nav = test_nav("vanilla-rects");
    let mut ui = UiState::new();
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let title = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    ui.enter_dev_world();
    ui.pause();
    let pause = pause_frame(&nav);
    ui.enter_dev_world();
    ui.die(Some(plain_death_message("fell from a high place")));
    let death = death_frame(&nav, ui.death_message());

    for (name, frame) in [("title", &title), ("pause", &pause), ("death", &death)] {
        // 320×240 is the smallest canvas `calculate_gui_scale` will produce
        // (see `config.rs`'s MIN_SCALED_*), so it is the real lower bound.
        for (w, h) in [(320.0f32, 240.0f32), (V_W, V_H), (1280.0, 720.0)] {
            let rects: Vec<(f32, f32, f32, f32)> = (0..frame.rows.len())
                .map(|i| row_rect(&frame.rows, i, w, h).expect("a slotted row has a rect"))
                .collect();
            for (i, r) in rects.iter().enumerate() {
                assert!(
                    r.0 >= 0.0 && r.0 + r.2 <= w,
                    "{name} widget {i} off-screen horizontally at {w}x{h}: {r:?}"
                );
                assert!(
                    r.1 >= 0.0 && r.1 + r.3 <= h,
                    "{name} widget {i} off-screen vertically at {w}x{h}: {r:?}"
                );
            }
            for (i, a) in rects.iter().enumerate() {
                for (j, b) in rects.iter().enumerate().skip(i + 1) {
                    let overlap = a.0 < b.0 + b.2
                        && b.0 < a.0 + a.2
                        && a.1 < b.1 + b.3
                        && b.1 < a.1 + a.3;
                    assert!(
                        !overlap,
                        "{name} widgets {i} and {j} overlap at {w}x{h}: {a:?} {b:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn an_overlay_frames_backdrop_is_translucent_unlike_an_ordinary_menus() {
    // The whole point of `MenuFrame::overlay`: the paused world underneath
    // must stay visible, which only holds if the backdrop quad's alpha is
    // measurably below opaque. A negative control (an ordinary, non-overlay
    // frame) proves the opaque case still exists and this isn't just
    // measuring `geometry`'s general output.
    let nav = test_nav("pause-overlay-alpha");
    let overlay = pause_frame(&nav);
    let v = geometry(&overlay, 1280.0, 720.0);
    // The backdrop is the very first quad emitted (vertex 0..6); alpha is
    // the 4th of the 6 floats per vertex ([x, y, r, g, b, a]).
    let backdrop_alpha = v[5];
    assert!(
        backdrop_alpha < 0.9,
        "an overlay backdrop must let the world show through: alpha={backdrop_alpha}"
    );

    let ordinary = frame_with(vec![button("QUIT")], 0);
    let v2 = geometry(&ordinary, 1280.0, 720.0);
    assert!(
        (v2[5] - 1.0).abs() < f32::EPSILON,
        "a non-overlay menu's backdrop must stay opaque: alpha={}",
        v2[5]
    );
}

#[test]
fn the_highlighted_pause_button_is_visibly_different_from_its_neighbours() {
    // Colour-aware, because the fill quad already covers every pixel a
    // border would: `coverage`'s "is anything here" cannot separate the
    // highlighted state from an ordinary row (see `coverage_of`'s docs).
    //
    // This is the *fallback* (no atlas) chrome — flat ROW_SEL / ROW_BG /
    // ROW_OFF fills. The real `widget/button*` sprite selection is gated
    // separately by `the_button_sprite_matches_vanillas_enabled_hovered_rule`.
    let mut nav = test_nav("pause-highlight");
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.pause();

    // Options (index 7) is enabled, so it can actually be highlighted.
    nav.hover(&ui, 7);
    let (w, h) = (V_W, V_H);
    let frame = pause_frame(&nav);
    let sel = geometry(&frame, w, h);
    let mut unsel_frame = pause_frame(&nav);
    unsel_frame.selected = 99;
    let unsel = geometry(&unsel_frame, w, h);
    assert_ne!(sel, unsel, "selecting a pause row must change the geometry");

    // A strip of the button's *interior above its label*: the label's top is
    // `y + (h - 9)/2 + 1` == y+6 for a 20 px button, and the 1 px selection
    // border ends at y+1. Sampling y+2..y+4 therefore measures the fill and
    // only the fill — the first version of this test sampled the whole
    // interior and failed on the disabled row, because `colour_at` returns
    // the *topmost* quad and "Advancements" is dense enough in a 98 px button
    // to push label ink into more than 10 % of the samples.
    let inside = |i: usize| {
        let (x, y, rw, _rh) = row_rect(&frame.rows, i, w, h).expect("a slotted row has a rect");
        (x + 4.0, y + 2.0, rw - 8.0, 2.0)
    };
    assert!(
        coverage_of(&sel, w, h, inside(7), ROW_SEL) > 0.9,
        "the highlighted row is not filled with ROW_SEL: {}",
        coverage_of(&sel, w, h, inside(7), ROW_SEL)
    );
    // Negative control 1: the same rect with nothing selected is ROW_BG, and
    // carries no ROW_SEL at all.
    assert!(
        coverage_of(&unsel, w, h, inside(7), ROW_SEL) < 0.05,
        "an unhighlighted row must not use the selected fill"
    );
    assert!(
        coverage_of(&unsel, w, h, inside(7), ROW_BG) > 0.9,
        "an unhighlighted enabled row should be filled with ROW_BG"
    );
    // Negative control 2: a *disabled* row is a third, distinct colour and
    // never picks up the selected fill even when it is the selection —
    // vanilla's `WidgetSprites::get` gives disabled priority over hovered.
    //
    // The subject is **Report Bugs (index 3)**, not Advancements (index 1):
    // A control pointed at an enabled row measures
    // nothing. Report Bugs has no screen behind it and no plan for one, so it is
    // the stable choice.
    let disabled_row = 3;
    assert!(
        !crate::menu::nav::PAUSE_BUTTONS[disabled_row].enabled(),
        "this control needs a row that is genuinely disabled"
    );
    let mut on_disabled = pause_frame(&nav);
    on_disabled.selected = disabled_row;
    let on_disabled = geometry(&on_disabled, w, h);
    assert!(
        coverage_of(&on_disabled, w, h, inside(disabled_row), ROW_OFF) > 0.9,
        "a disabled row must keep the disabled fill even while highlighted: {}",
        coverage_of(&on_disabled, w, h, inside(disabled_row), ROW_OFF)
    );
    assert!(
        coverage_of(&on_disabled, w, h, inside(disabled_row), ROW_SEL) < 0.05,
        "a disabled row must never draw the selected fill"
    );
    // And the three colours really are distinguishable, so the three
    // assertions above are measurements and not the same one three times.
    assert_ne!(ROW_SEL, ROW_BG);
    assert_ne!(ROW_SEL, ROW_OFF);
    assert_ne!(ROW_BG, ROW_OFF);
}
