use super::*;


#[test]
fn a_server_list_tooltip_snaps_its_box_to_the_same_origin_as_its_text() {
    assert_eq!(
        tooltip_content_origin((100.25, 80.25), 40.0, 9.0, 400.0, 300.0),
        (112.0, 68.0)
    );
}
#[cfg(not(feature = "multiplayer"))]
#[test]
fn title_frame_renders_the_multiplayer_disabled_tooltip_on_hover() {
    const W: f32 = 854.0;
    const H: f32 = 480.0;
    let mut nav = test_nav("multiplayer-disabled-tooltip");
    let ui = UiState::new();
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut favicons = FaviconCache::new();

    let before = frame_for(&ui, &nav, &statuses, &mut favicons).expect("title frame");
    let row = before
        .rows
        .iter()
        .position(|row| row.label == "Multiplayer")
        .expect("the Multiplayer title row must remain present");
    assert!(!before.rows[row].enabled, "the row must be disabled");
    assert_eq!(
        before.rows[row].tooltip.as_deref(),
        Some("Multiplayer is disabled in this build of the game."),
        "the title-frame adapter must carry the capability explanation"
    );

    let (x, y, w, h) = row_rect(&before.rows, row, W, H).expect("the title row has a hit box");
    nav.set_menu_cursor(x + w * 0.5, y + h * 0.5, W, H);
    let hovered = frame_for(&ui, &nav, &statuses, &mut favicons).expect("hovered title frame");
    assert!(
        colour_bounds(&geometry(&hovered, W, H), W, H, TOOLTIP_BG).is_some(),
        "hovering Multiplayer must rasterize its explanatory tooltip"
    );
}

#[test]
fn title_frame_has_only_the_release_label_in_its_bottom_corners() {
    let nav = test_nav("title-corner-labels");
    let ui = UiState::new();
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut favicons = FaviconCache::new();

    let frame = frame_for(&ui, &nav, &statuses, &mut favicons).expect("title frame");
    assert_eq!(frame.labels.len(), 1);
    assert_eq!(frame.labels[0].text, "Lodestone (Minecraft 26.2)");
    assert_eq!(frame.labels[0].origin, Origin::BottomLeft);
}

/// Every one of the command block screen's seven interactive
/// rows, plus the read-only "Previous Output" row, at its exact vanilla
/// rect on a real canvas — not a restatement of `command_block.rs`'s own
/// constants, but the actual `MenuFrame`/`row_rect` output a click and a
/// draw both go through.
///
/// `854x480` is the same seed canvas `nav::SEED_CANVAS` uses, so
/// `floor(854/2) == 427` and `floor(480/4) == 120` are the two integer
/// divisions every rect below is built from.
#[test]
fn command_block_rects_match_vanillas_own_arithmetic() {
    use command_block::{CommandBlockOpen, CommandBlockRow, CommandBlockState};

    let state = CommandBlockState::new(CommandBlockOpen::default());
    let frame = command_block_frame(&state, None);
    let (w, h) = (854.0_f32, 480.0_f32);
    let rect = |row: CommandBlockRow| row_rect(&frame.rows, row as usize, w, h).unwrap();

    // Command field: `width/2 - 150, 50, 300, 20`.
    assert_eq!(rect(CommandBlockRow::Command), (277.0, 50.0, 300.0, 20.0));
    // Track Output: `width/2 + 130, 135, 20, 20`.
    assert_eq!(rect(CommandBlockRow::TrackOutput), (557.0, 135.0, 20.0, 20.0));
    // Mode/Conditional/Automatic: shared y 165, widths 100.
    assert_eq!(rect(CommandBlockRow::Mode), (273.0, 165.0, 100.0, 20.0));
    assert_eq!(rect(CommandBlockRow::Conditional), (377.0, 165.0, 100.0, 20.0));
    assert_eq!(rect(CommandBlockRow::Automatic), (481.0, 165.0, 100.0, 20.0));
    // Done/Cancel: `Origin::CommandBlockFooter`'s anchor is
    // `(427, floor(480/4) + 132) == (427, 252)`.
    let done = row_rect(&frame.rows, CommandBlockRow::Done as usize, w, h).unwrap();
    let cancel = row_rect(&frame.rows, CommandBlockRow::Cancel as usize, w, h).unwrap();
    assert_eq!(done, (273.0, 252.0, 150.0, 20.0));
    assert_eq!(cancel, (431.0, 252.0, 150.0, 20.0));
    // Rejected hypothesis: reusing `Origin::TitleTop`'s `floor(h/4) + 48`
    // (`168`, not `252`) — an 84 px miss, not a rounding difference, so a
    // wrong-origin bug here cannot pass by accident.
    assert_ne!(done.1, (h / 4.0).floor() + 48.0);

    // Row 7: the read-only previous-output field, sharing the command
    // field's x and the track-output row's y.
    let previous = row_rect(&frame.rows, command_block::PREVIOUS_OUTPUT_ROW, w, h).unwrap();
    assert_eq!(previous, (277.0, 135.0, 276.0, 20.0));

    // Captions: the default state (fresh `CommandBlockOpen`) is
    // Redstone/Unconditional/Needs-Redstone/track-output-off, matching
    // vanilla's own field initialisers.
    assert_eq!(frame.rows[CommandBlockRow::Mode as usize].label, "Impulse");
    assert_eq!(
        frame.rows[CommandBlockRow::Conditional as usize].label,
        "Unconditional"
    );
    assert_eq!(
        frame.rows[CommandBlockRow::Automatic as usize].label,
        "Needs Redstone"
    );
    assert_eq!(
        frame.rows[CommandBlockRow::TrackOutput as usize].label,
        "X"
    );
    assert_eq!(frame.rows[CommandBlockRow::Done as usize].label, "Done");
    assert_eq!(frame.rows[CommandBlockRow::Cancel as usize].label, "Cancel");
}

/// The tab-completion popup rect, predicted against vanilla's own
/// clamp formula and checked against a rejected hypothesis: forgetting
/// the synthetic-slash offset shift (`command_block`'s module doc) would
/// place the popup 6 px too far right, not merely "somewhere near".
#[test]
fn command_block_suggestion_popup_lands_at_the_predicted_clamped_rect() {
    use command_block::{CommandBlockOpen, CommandBlockState};
    use lodestone_model::command_tree::{CommandTree, NodeKind, RawCommandNode};

    let nodes = vec![
        RawCommandNode {
            kind: NodeKind::Root,
            children: vec![1],
            executable: false,
            restricted: false,
            redirect: None,
        },
        RawCommandNode {
            kind: NodeKind::Literal {
                name: "gamemode".to_string(),
            },
            children: vec![2, 3],
            executable: false,
            restricted: false,
            redirect: None,
        },
        RawCommandNode {
            kind: NodeKind::Literal {
                name: "creative".to_string(),
            },
            children: vec![],
            executable: true,
            restricted: false,
            redirect: None,
        },
        RawCommandNode {
            kind: NodeKind::Literal {
                name: "survival".to_string(),
            },
            children: vec![],
            executable: true,
            restricted: false,
            redirect: None,
        },
    ];
    let tree = CommandTree::new(nodes, 0).unwrap();
    let mut state = CommandBlockState::new(CommandBlockOpen::default());
    state.command.set_value("gamemode c");
    let frame = command_block_frame(&state, Some(&tree));
    let (w, h) = (854.0_f32, 480.0_f32);

    // One row past `PREVIOUS_OUTPUT_ROW`: only "creative" matches "c".
    let popup_row = command_block::PREVIOUS_OUTPUT_ROW + 1;
    assert_eq!(frame.rows.len(), popup_row + 1, "exactly one candidate");
    assert_eq!(frame.rows[popup_row].label, "creative");

    // `start == 9` (see `command_block`'s own completion test), advance
    // 6.0, `BORDER_INSET` 4.0: `unclamped_dx = -150 + 4 + 6*9 = -92`,
    // `x = 427 - 92 = 335` — comfortably inside `[0, 525]`, so the clamp
    // itself is not exercised here (a second test would need a command
    // long enough to push the popup off the right edge).
    let popup_w = 8.0 * 6.0; // "creative", 8 chars, fixed advance 6.0.
    let (px, py, pw, ph) = row_rect(&frame.rows, popup_row, w, h).unwrap();
    assert_eq!((px, py, ph), (335.0, 71.0, 12.0));
    assert_eq!(pw, popup_w + 1.0);

    // Rejected hypothesis: an adapter that forgot to shift the synthetic
    // slash's offset back by one would compute `start == 10`, landing at
    // `427 + (-150 + 4 + 60) == 341` — 6 px right of the real answer, not
    // an imperceptible rounding difference.
    let wrong_dx = 427.0 + (-150.0 + 4.0 + 6.0 * 10.0);
    assert_ne!(px, wrong_dx);
    assert_eq!(wrong_dx - px, 6.0);
}

#[test]
fn owns_frame_agrees_with_frame_for_on_every_screen() {
    // Two definitions of "this renderer owns the frame" that can disagree is
    // how a screen ends up drawn twice, or not at all. Walk every screen and
    // require the predicate and the builder to say the same thing.
    let mut nav = test_nav("owns");
    let mut fav = FaviconCache::new();
    let statuses = StatusCache::with_probe(unavailable_probe());
    // A real cursor position, not the default `None` — so the mechanical
    // check below has something to actually distinguish. A
    // frame that left `cursor` at its own `..Default::default()` of `None`
    // would pass a `None == None` comparison vacuously; this makes the
    // stamped value `Some(_)`, which only the real stamp can produce.
    nav.set_menu_cursor(42.0, 24.0, 854.0, 480.0);

    let mut reached = 0;
    // `Screen::ALL`, not a list restated here: a literal list plus an
    // `assert_eq!(reached, 12)` can drift from the screen inventory. The shared
    // inventory centralizes the set and avoids a second independent literal.
    // The `match` below stays exhaustive,
    // which is what forces a new variant to be given a way to be *reached*;
    // `Screen::ALL`'s own docs say what that does and does not guarantee.
    for screen in Screen::ALL {
        let mut ui = UiState::new();
        match screen {
            // The gate needs no setup at all: with an account in the roster
            // (`test_nav` seeds one) it is an ordinary screen, and `UiState`
            // has no session, so `open_ownership_gate` always lands.
            Screen::Ownership => ui.open_ownership_gate(),
            Screen::MainMenu => {}
            Screen::ServerList => ui.open_server_list(),
            Screen::ServerEdit => {
                ui.open_server_list();
                ui.open_server_edit();
            }
            Screen::WorldSelect => ui.open_world_select(),
            Screen::Settings => ui.open_settings(),
            Screen::Accounts => ui.open_accounts(),
            Screen::Connecting => ui.begin(SessionKind::Multiplayer),
            Screen::Playing => ui.enter_dev_world(),
            Screen::Chat => {
                ui.enter_dev_world();
                ui.open_chat();
            }
            Screen::Container => {
                ui.enter_dev_world();
                ui.open_container();
            }
            Screen::CommandBlockEdit => {
                ui.enter_dev_world();
                ui.open_command_block();
            }
            Screen::SignEdit => {
                ui.enter_dev_world();
                ui.open_sign_edit();
            }
            Screen::BookEdit => {
                ui.enter_dev_world();
                ui.open_book_edit();
            }
            Screen::BookView => {
                ui.enter_dev_world();
                ui.open_book_view();
            }
            Screen::SpectatorMenu => {
                ui.enter_dev_world();
                ui.open_spectator_menu();
            }
            Screen::Paused => {
                ui.enter_dev_world();
                ui.pause();
            }
            Screen::Death => {
                ui.enter_dev_world();
                ui.die(Some(plain_death_message("blew up")));
            }
            Screen::Error => {
                ui.begin(SessionKind::Multiplayer);
                ui.session_failed(crate::sim::SessionEnd::disconnected(
            lodestone_model::ResolvedText::literal("connection refused"),
        ));
            }
            Screen::Credits => {
                ui.enter_dev_world();
                ui.show_credits();
            }
            Screen::Friends => ui.open_friends_from_title(),
            Screen::Social => {
                ui.enter_dev_world();
                ui.pause();
                ui.open_social_from_pause();
            }
            Screen::Statistics => {
                ui.enter_dev_world();
                ui.pause();
                ui.open_statistics_from_pause();
            }
            // Server Links is `owns_frame == false` unconditionally (see its
            // own doc) — it never owns the Clear pass, so `frame_for` must
            // answer `None` here exactly as it does for `Screen::Paused`.
            Screen::ServerLinks => {
                ui.enter_dev_world();
                ui.pause();
                ui.open_server_links_from_pause();
            }
            Screen::Advancements => {
                ui.enter_dev_world();
                ui.pause();
                ui.open_advancements_from_pause();
            }
            Screen::CreateWorld => {
                ui.open_world_select();
                ui.open_create_world();
            }
            // Reached the way a player reaches it — through the world
            // list — because `open_confirm` guards on that screen for the reason
            // its own doc gives.
            Screen::Confirm => {
                ui.open_world_select();
                ui.open_confirm();
            }
            // Reached the way a live push reaches it — over the loading
            // screen, matching `open_resource_pack_prompt`'s guard.
            Screen::ResourcePackPrompt => {
                ui.begin(SessionKind::Multiplayer);
                ui.open_resource_pack_prompt();
            }
        }
        assert_eq!(ui.screen(), screen, "failed to reach {screen:?}");
        reached += 1;
        let built = frame_for(&ui, &nav, &statuses, &mut fav).is_some();
        if matches!(screen, Screen::Statistics | Screen::Social) {
            // The one other documented exception, alongside `Screen::Settings`
            // while `settings_in_world()`: this loop reaches Statistics and
            // Social the only way a player can (`ui.open_statistics_from_pause()`/
            // `ui.open_social_from_pause()`, both always in-world — there is
            // no title-screen route for this test to take instead, unlike
            // Settings), so it always hits the case where `frame_for`
            // deliberately answers `None` and `owns_frame` deliberately stays
            // `true` (see each arm's own doc).
            // `frame_for_defers_to_an_overlay_for_statistics`/`_for_social`
            // below are what cover the frame this loop cannot reach.
            assert!(!built, "{screen:?} must defer to the overlay path");
            assert!(
                owns_frame(screen),
                "owns_frame({screen:?}) must stay true regardless — see that arm's own doc"
            );
        } else {
            assert_eq!(
                built,
                owns_frame(screen),
                "owns_frame and frame_for disagree about {screen:?}"
            );
        }
        // And a frame it claims must actually be drawable.
        if built {
            let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
            // A vanilla-laid-out screen has no centred heading string — its
            // heading is the logo texture (title), a positioned `MenuLabel`
            // (pause), or a real widget row with no separate label at all
            // (Statistics' tab bar — vanilla draws no "Statistics"
            // heading either, see `stats::frame`'s own doc), so requiring
            // `title` would be requiring the *un*-vanilla layout. It must
            // still say something.
            if f.vanilla {
                assert!(
                    f.logo || !f.labels.is_empty() || !f.rows.is_empty(),
                    "{screen:?} is vanilla-laid-out but draws no logo, label or row"
                );
            } else {
                assert!(!f.title.is_empty(), "{screen:?} has no title");
            }
            assert!(
                !geometry(&f, 1280.0, 720.0).is_empty(),
                "{screen:?} draws nothing"
            );
            // The mechanical check here asks for: every screen `frame_for`
            // returns `Some` for must carry the four canvas facts
            // `stamp_canvas_facts` stamps — `cursor` in particular, since a
            // frame with `cursor: None` has no hover affordance at all
            // (`render::draw_widget`'s `hovered` only ever comes from
            // `MenuFrame::hovered`, which is a *different* fact — see
            // `create_world.rs`'s own doc on that split). This is a tripwire
            // for the *architectural* shape of the bug — a screen arm that
            // stops going through `frame_for`'s unconditional `.map(stamp_
            // canvas_facts)`, or one that sets its own conflicting value —
            // not a substitute for `frame_for_defers_to_an_overlay_for_
            // in_world_settings` below: that test is what covers the one
            // screen (`Settings` while `settings_in_world()`) that
            // deliberately returns `None` here and reaches the draw through
            // a *second* path (`nav::settings_overlay_frame`) this loop never
            // visits at all, exactly as `owns_frame`'s own doc says.
            assert_eq!(
                f.cursor,
                nav.menu_cursor(),
                "{screen:?}'s cursor is not the shared canvas stamp"
            );
            assert_eq!(
                f.gui_scale,
                nav.gui_scale(),
                "{screen:?}'s gui_scale is not the shared canvas stamp"
            );
            assert_eq!(
                f.panorama_speed,
                Some(nav.panorama_speed()),
                "{screen:?}'s panorama_speed is not the shared canvas stamp"
            );
            assert_eq!(
                f.list,
                nav.active_list(&ui),
                "{screen:?}'s list is not the shared canvas stamp"
            );
        }
    }
    // Derived, not restated. This no longer catches "a screen was added"
    // (`Screen::ALL` is what does, as far as anything can) — what it still
    // catches is this loop silently skipping one, e.g. a `continue` added to
    // the reach-the-screen `match` above.
    assert_eq!(
        reached,
        Screen::ALL.len(),
        "the loop skipped a screen it was handed"
    );
    let _ = &mut nav;
}

/// The credits frame has one enabled row (Done), a title label, and a
/// non-empty body notice, all resolving on-canvas — the same shape
/// `error_frame`'s callers already get for free through the sweep above,
/// spelled out here because `credits_frame` takes no arguments (unlike
/// `error_frame`, which the sweep exercises through `ui.error()`) and so
/// is otherwise only reached indirectly.
#[test]
fn credits_frame_has_one_live_row_a_title_and_a_body() {
    let f = credits_frame();
    assert_eq!(f.rows.len(), 1, "one control: Done");
    assert!(f.rows[0].enabled);
    assert_eq!(f.rows[0].label, "Done");
    assert_eq!(f.selected, 0);
    assert!(f.vanilla, "laid out the same way error_frame is");
    assert!(!f.labels.is_empty(), "a title label must be present");
    assert!(
        f.notice.as_ref().is_some_and(|n| !n.text.is_empty()),
        "a body notice must be present and non-empty"
    );
    let (w, h) = (1280.0, 720.0);
    assert!(
        !geometry(&f, w, h).is_empty(),
        "the frame must draw something"
    );
    let (rx, ry, rw, rh) = f.rows[0].slot.unwrap().resolve(w, h);
    assert!(
        rx >= 0.0 && ry >= 0.0 && rx + rw <= w && ry + rh <= h,
        "the Done button must resolve on-canvas: ({rx}, {ry}) {rw}x{rh}"
    );
}
