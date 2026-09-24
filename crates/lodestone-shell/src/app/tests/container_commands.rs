//! Tests for command-block interaction, row hit testing, and game-mode commands.

use super::*;



#[test]
fn right_clicking_a_command_block_opens_the_edit_screen() {
    use crate::raycast::RayHit;

    // `block_name`, keyed by block-**state** id, is the id space the store and
    // `write_predicted_block` deal in. This used to scan `block_type_name`,
    // whose parameter is a `minecraft:block` **registry** id, so it selected
    // state 407 — `minecraft:cherry_leaves` — and the test agreed with the
    // production bug it was meant to gate (see `command_block_source::
    // mode_for_state`). A `None` here is a broken generated table, not a case to
    // skip: silently returning green is the *precondition* species of vacuous
    // test.
    let state_id = (0u32..lodestone_data::block_states::STATE_COUNT as u32)
        .find(|id| {
            lodestone_data::block_states::block_name(*id)
                .is_some_and(|n| n == "minecraft:command_block")
        })
        .expect("the 26.2 block-state table must contain minecraft:command_block");

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.enter_dev_world();

    // A real command block in the real store the accessor reads.
    let block = [8, 64, 8];
    let world = app.sim.chunk_world_write();
    {
        let mut w = world.write();
        crate::sim::write_predicted_block(&mut *w, block, state_id);
        // The block entity's payload, overwriting the empty record
        // `write_predicted_block`'s `sync_block_entity` just created — the
        // shape a server sends for a command block whose command has been set.
        // Without it the screen opens through the "fail open" default (empty
        // command), and the test's gate — "opens populated with the block's
        // actual command text" — would be untested.
        w.set_block_entity(
            block[0],
            block[1],
            block[2],
            lodestone_data::block_states::StateId::new(state_id)
                .and_then(lodestone_data::block_entity_types::block_entity_type)
                .map(|kind| kind.raw())
                .expect("a command block state owns a block entity type"),
            lodestone_core::Nbt::Compound(vec![
                (
                    "Command".to_string(),
                    lodestone_core::Nbt::String("say hello".into()),
                ),
                ("TrackOutput".to_string(), lodestone_core::Nbt::Byte(1)),
            ]),
        );
    }
    // `face_center` is the real constructor the raycast itself uses, so this
    // cannot disagree with a production hit's shape.
    app.sim
        .set_ray_target_for_test(Some(RayHit::face_center(block, [0, 1, 0])));

    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "precondition: the screen must not already be up, or this proves nothing"
    );

    app.try_use();

    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "a right-click on a command block must open the edit screen — this is \
         the hop that did not exist"
    );
    let state = app
        .nav
        .command_block()
        .expect("the screen's widget state must be built alongside the screen");
    assert_eq!(
        state.to_submit().pos,
        lodestone_model::BlockPos::new(8, 64, 8),
        "and it must open on the block that was actually clicked, not a default          — `to_submit` is what the Done button would actually send"
    );
    assert_eq!(
        state.command.value(),
        "say hello",
        "and it must open populated with the block's actual command text — the \
         issue's gate, carried through the real interaction path: block-entity \
         NBT in the store -> `targeted_command_block` -> the edit screen"
    );
}
/// **The control, run and observed**: the same right-click on a block that is
/// *not* a command block must fall through to the ordinary use path and leave
/// the screen shut.
///
/// Without this, the gate above is satisfied by a `try_use` that opens the
/// command block screen on every right-click anywhere — which would be a far
/// worse bug than the island it replaces, and would read as a pass.
#[test]
fn right_clicking_a_normal_block_does_not_open_the_command_block_screen() {
    use crate::raycast::RayHit;

    // Block-**state** id space, as above: a control that writes some arbitrary
    // block instead of stone still passes, so the wrong accessor made this
    // control weaker than it reads.
    let stone = (0u32..lodestone_data::block_states::STATE_COUNT as u32)
        .find(|id| {
            lodestone_data::block_states::block_name(*id).is_some_and(|n| n == "minecraft:stone")
        })
        .expect("the 26.2 block-state table must contain minecraft:stone");

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.enter_dev_world();

    let block = [8, 64, 8];
    let world = app.sim.chunk_world_write();
    {
        let mut w = world.write();
        crate::sim::write_predicted_block(&mut *w, block, stone);
    }
    // `face_center` is the real constructor the raycast itself uses, so this
    // cannot disagree with a production hit's shape.
    app.sim
        .set_ray_target_for_test(Some(RayHit::face_center(block, [0, 1, 0])));

    app.try_use();

    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "stone is not a command block — the screen must stay shut and the \
         ordinary use path must run"
    );
    assert!(
        app.nav.command_block().is_none(),
        "and no widget state may be built"
    );

    // The other half of the fork: nothing targeted at all.
    app.sim.set_ray_target_for_test(None);
    app.try_use();
    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "and a right-click on empty air must not open it either"
    );
}

/// A framebuffer whose **auto** GUI scale is exactly 1, so a logical pixel is a
/// physical pixel and the coordinates in the command-block tests below need no
/// conversion at all.
///
/// `calculate_gui_scale`'s loop stops when `fb / (scale + 1)` drops below
/// `320x240`: `400 / 2 == 200 < 240`, so it never reaches 2. Asserted in the
/// tests rather than assumed — if this stops being 1 the coordinates below
/// become silently wrong rather than obviously wrong, which is the whole
/// "clicks land one slot off, invisible in a screenshot" failure mode.
const CB_FB_W: u32 = 640;
const CB_FB_H: u32 = 400;

/// **Command-block interaction: a click on the edit screen reaches a row.**
///
/// `0948f59` made the screen *draw*. It still could not be clicked:
/// `app/lifecycle.rs` guarded its `CursorMoved` and `MouseInput` arms on
/// `owns_frame(screen) || is_paused() || is_death()`, and `Screen::
/// CommandBlockEdit` is in none of those — it is an overlay, so `owns_frame` is
/// deliberately `false`. Every click on Done, Cancel, Mode, Conditional and the
/// output toggle was dropped by the match guard before `menu_row_at` was ever
/// called, and `on_screen_frame` had no arm for the screen either, so it would
/// have returned `None` even if a click had got that far. Two missing homes for
/// one screen, the same shape as `0d0ae93`.
///
/// # Why this test lives here and not in `menu/nav.rs`
///
/// `nav::tests::every_mouse_routable_screen_has_a_frame_to_hit_test` exists,
/// passed throughout, and **structurally could not see this**: it hand-copied
/// the driver's routing expression instead of calling it, so it compared two
/// things `nav.rs` controls. This one drives `WindowApp`'s own
/// `menu_row_at_in` — the real frame source, the real scale conversion, the
/// real `row_rect` loop — and asserts the driver's own guard
/// (`routes_menu_input`, now literally the expression in the match guard)
/// answers `true` first.
///
/// # The expected coordinates come from vanilla, not from our frame
///
/// Asking `row_rect` where a row is and then clicking there would be
/// `decode(encode(x)) == x`: it passes for any self-consistent geometry,
/// including one that draws the buttons off-screen. These are computed from
/// vanilla's own command-block-edit-screen layout arithmetic — Done sits
/// at `width/2 - 4 - 150`, Cancel at `width/2 + 4`, both `150x20`
/// at `height/4 + 120 + 12`, and the mode row at `width/2 - 154`,
/// `100x20`, `y = 165`.
#[test]
fn clicking_a_command_block_row_at_its_own_coordinates_activates_that_row() {
    use crate::menu::command_block::{CommandBlockOpen, CommandBlockRow};
    use lodestone_model::CommandBlockMode;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.enter_dev_world();
    app.nav
        .open_command_block(&mut app.ui, CommandBlockOpen::default());

    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "premise: the screen is up — `right_clicking_a_command_block_opens_the_\
         edit_screen` covers the hop that gets it here"
    );
    assert_eq!(
        crate::config::calculate_gui_scale(0, CB_FB_W, CB_FB_H),
        1,
        "premise: at this framebuffer a logical pixel is a physical pixel, so \
         the vanilla-derived coordinates below need no scale conversion"
    );
    // **The link that was broken.** This is the literal expression
    // `app/lifecycle.rs`'s `CursorMoved` and `MouseInput` match guards are
    // written as, so a `false` here is a click that never reaches the body at
    // all — no hit-test, no row, no pixel, and nothing to observe downstream.
    assert!(
        crate::menu::nav::routes_menu_input(&app.ui),
        "the driver's own mouse guard must route to this screen — this was \
         `false`, which is why every click on it was silently dropped"
    );

    // Vanilla's own command-block-edit-screen layout — the footer anchor is
    // `(width/2, height/4 + 120 + 12)` and the buttons are `150x20`.
    let anchor_x = (CB_FB_W as f32 / 2.0).floor();
    let footer_y = (CB_FB_H as f32 / 4.0).floor() + 132.0;
    let done = (anchor_x - 4.0 - 150.0 + 75.0, footer_y + 10.0);
    let cancel = (anchor_x + 4.0 + 75.0, footer_y + 10.0);
    // `:50` — the mode button, `100x20` at `width/2 - 154`, `y = 165`. It
    // shares Done's `dx` exactly, and differs only in `y`, so a hit-test that
    // resolved x and ignored y would answer the same row for both. That is the
    // second hypothesis, not a tolerance.
    let mode = (anchor_x - 154.0 + 50.0, 165.0 + 10.0);

    assert_eq!(
        app.menu_row_at_in(cancel.0, cancel.1, CB_FB_W, CB_FB_H),
        Some(CommandBlockRow::Cancel as usize),
        "a click at Cancel's own vanilla coordinates must resolve to Cancel"
    );
    assert_eq!(
        app.menu_row_at_in(done.0, done.1, CB_FB_W, CB_FB_H),
        Some(CommandBlockRow::Done as usize),
        "and Done's to Done — 150 px apart on the same line, so this is row \
         resolution and not 'every coordinate answers the same row'"
    );
    assert_eq!(
        app.menu_row_at_in(mode.0, mode.1, CB_FB_W, CB_FB_H),
        Some(CommandBlockRow::Mode as usize),
        "and the mode button, which shares Done's x and differs only in y"
    );

    // Now the other half: the resolved row, put through the same
    // `MenuNav::click` the driver calls, must do that row's own thing.
    // Predicted exactly — `next_mode(Redstone) == Sequence` — rather than
    // asserted to have merely changed.
    let row = app
        .menu_row_at_in(mode.0, mode.1, CB_FB_W, CB_FB_H)
        .expect("just asserted");
    assert_eq!(
        app.nav.command_block().map(|s| s.mode),
        Some(CommandBlockMode::Redstone),
        "precondition: a freshly placed command block starts in Redstone mode"
    );
    let action = app.nav.click(&mut app.ui, row);
    app.apply_menu_action(action);
    assert_eq!(
        app.nav.command_block().map(|s| s.mode),
        Some(CommandBlockMode::Sequence),
        "clicking the mode button must cycle Redstone -> Sequence, which is \
         `next_mode`'s own answer — not merely 'the mode changed'"
    );

    // And Cancel, through the same path, closes the screen without sending.
    let row = app
        .menu_row_at_in(cancel.0, cancel.1, CB_FB_W, CB_FB_H)
        .expect("just asserted");
    let action = app.nav.click(&mut app.ui, row);
    app.apply_menu_action(action);
    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "a click on Cancel must close the screen"
    );
    assert!(
        !crate::menu::nav::routes_menu_input(&app.ui),
        "and the mouse must go back to gameplay — the guard is a property of \
         the screen, not a latch"
    );
}

/// **The control for the gate above, run and observed.**
///
/// Two premises that could each make that test pass for the wrong reason:
///
/// 1. If `menu_row_at_in` answered `Some(_)` for *any* coordinate, the three
///    row assertions would be satisfied by an accident of ordering. The
///    backdrop must resolve to no row.
/// 2. If it answered `Some(_)` regardless of which screen is up, the routing
///    fix would be untested — the frame would be coming from somewhere that
///    does not care about `Screen::CommandBlockEdit`. With the screen closed,
///    the very same coordinates must resolve to nothing.
///
/// The second is the sharper one, and it is the one that fires: before the fix
/// `on_screen_frame` had **no arm** for this screen, so the open-screen
/// assertions above and this closed-screen one would have agreed on `None` —
/// the test above would have failed and this one would have passed, which is
/// the correct polarity for a control.
#[test]
fn no_command_block_row_hit_tests_off_the_rows_or_off_the_screen() {
    use crate::menu::command_block::CommandBlockOpen;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.enter_dev_world();
    app.nav
        .open_command_block(&mut app.ui, CommandBlockOpen::default());

    let anchor_x = (CB_FB_W as f32 / 2.0).floor();
    let footer_y = (CB_FB_H as f32 / 4.0).floor() + 132.0;
    let cancel = (anchor_x + 4.0 + 75.0, footer_y + 10.0);

    // (1) The backdrop. `y = 5` is above the title (`TITLE_Y == 20`) and below
    // nothing, so no widget on this screen can claim it.
    assert_eq!(
        app.menu_row_at_in(anchor_x, 5.0, CB_FB_W, CB_FB_H),
        None,
        "the backdrop must resolve to no row, or the gate above is satisfied \
         by a hit-test that answers `Some` everywhere"
    );
    // The gap between Done's bottom (`footer_y + 20`) and the canvas floor.
    assert_eq!(
        app.menu_row_at_in(anchor_x, footer_y + 60.0, CB_FB_W, CB_FB_H),
        None,
        "and so must the gap below the footer buttons"
    );

    // (2) The same Cancel coordinate, with the screen shut. Observed to be
    // `Some(Cancel)` immediately above and `None` here, from one framebuffer
    // and one coordinate — so the difference is the screen and nothing else.
    assert!(
        app.menu_row_at_in(cancel.0, cancel.1, CB_FB_W, CB_FB_H)
            .is_some(),
        "premise: this coordinate does hit a row while the screen is open"
    );
    app.nav.close_command_block(&mut app.ui);
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Playing,
        "premise: the screen is now shut and the world is back"
    );
    assert_eq!(
        app.menu_row_at_in(cancel.0, cancel.1, CB_FB_W, CB_FB_H),
        None,
        "with the screen shut, the same coordinate must hit nothing — a click \
         in the world may never resolve to a command block button"
    );
}

/// F3+F4's cycle visits all four modes and returns, and F3+N's fallback is
/// falling back to Creative when no previous mode is available.
///
/// The cycle is the whole decidable part of the first `ClientAction::ChangeGameMode`
/// producer in the workspace — the variant was encoded by two protocol families
/// and constructed by nothing, so the server's own `ServerBound::ChangeGameMode`
/// arm could never fire.
#[test]
fn the_game_mode_cycle_visits_every_mode_and_returns() {
    use lodestone_model::GameMode;
    let mut mode = GameMode::Survival;
    let mut seen = vec![mode];
    for _ in 0..4 {
        mode = super::session::next_game_mode(Some(mode));
        seen.push(mode);
    }
    assert_eq!(
        seen,
        vec![
            GameMode::Survival,
            GameMode::Creative,
            GameMode::Adventure,
            GameMode::Spectator,
            GameMode::Survival,
        ]
    );
    // No session, or a server that has not reported one, starts at creative.
    assert_eq!(super::session::next_game_mode(None), GameMode::Creative);
}

/// F3+N and F3+F4 resolve to their own outcomes only while F3 is held, and both
/// mark the chord used so releasing F3 does not also toggle the debug overlay.
#[test]
fn the_game_mode_chords_need_the_debug_modifier() {
    let held = KeyGate {
        gameplay: true,
        debug_held: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(held, KeyCode::KeyN, true),
        Some(KeyOutcome::ToggleSpectator)
    );
    assert_eq!(
        resolve(held, KeyCode::F4, true),
        Some(KeyOutcome::CycleGameMode)
    );
    // Without the modifier neither key means anything — the negative half is the
    // point, since an arm that ignored `debug_held` would fire on every F4.
    assert_eq!(resolve(playing(), KeyCode::F4, true), None);
    assert_eq!(resolve(playing(), KeyCode::KeyN, true), None);
    // Release is not a chord.
    assert_eq!(resolve(held, KeyCode::F4, false), None);
}
