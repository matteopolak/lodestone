//! Tests for scroll handling, HUD visibility, and terminal input adapters.

use super::*;

/// Discrete scrolling collapses the delta to its **sign**, and the
/// sensitivity multiply happens **after**.
///
/// The order is the whole content of this gate, because both orders "work" on the
/// common case (a single `LineDelta` notch of 1.0 at sensitivity 1.0 gives 1.0 either
/// way). They diverge exactly where a player would notice, and the wrong hypothesis is
/// *computed* here rather than described:
///
/// | input | vanilla, `signum` then scale | reversed, scale then `signum` |
/// |---|---|---|
/// | `dy = 0.4`, sens `2.0` | **2.0** | 1.0 |
/// | `dy = 12.0` (trackpad), sens `0.5` | **0.5** | 1.0 |
///
/// Reversed, `signum` would eat the sensitivity entirely and cap wheel speed at one
/// notch — i.e. turning this row on would silently break the sensitivity row. That is
/// the defect a direction-only assertion cannot see.
#[test]
fn discrete_scrolling_takes_the_sign_before_sensitivity_scales_it() {
    // Off: the raw delta passes through, scaled. This is also the proof the option is
    // a pure addition — a trackpad's fractional delta is still proportional.
    assert_eq!(scale_scroll(0.4, false, 2.0), 0.8);
    assert_eq!(scale_scroll(12.0, false, 0.5), 6.0);

    // On: sign first, then scale.
    let small = scale_scroll(0.4, true, 2.0);
    assert_eq!(small, 2.0, "a sub-notch delta becomes a full notch, then doubles");
    let reversed_small = (0.4_f64 * 2.0).signum();
    assert_ne!(
        small, reversed_small,
        "scale-then-signum gives {reversed_small}, so this gate does not discriminate"
    );

    let big = scale_scroll(12.0, true, 0.5);
    assert_eq!(big, 0.5, "a 12 px trackpad delta becomes one notch, then halves");
    let reversed_big = (12.0_f64 * 0.5).signum();
    assert_ne!(
        big, reversed_big,
        "scale-then-signum gives {reversed_big}, so this gate does not discriminate"
    );

    // Direction survives the collapse.
    assert_eq!(scale_scroll(-7.5, true, 1.0), -1.0);
    assert_eq!(scale_scroll(7.5, true, 1.0), 1.0);

    // The external floating-point sign rule yields **0.0** for `0.0`, not 1.0.
    // `f64::signum` disagrees, so this is the
    // one place the Java and Rust primitives are not interchangeable — without the
    // explicit zero case a stationary wheel would emit a notch per event.
    assert_eq!(scale_scroll(0.0, true, 1.0), 0.0, "a zero delta must stay zero");
    assert_eq!(
        0.0_f64.signum(),
        1.0,
        "premise: f64::signum(0.0) really is 1.0, which is why the guard exists"
    );

    // And it composes with the hotbar's accumulator rather than replacing it: at a
    // low sensitivity a discrete notch still needs several gestures to move a slot.
    let mut accum = 0.0;
    assert_eq!(accumulate_scroll(&mut accum, scale_scroll(0.1, true, 0.5)), 0);
    assert_eq!(accumulate_scroll(&mut accum, scale_scroll(0.1, true, 0.5)), 1);
}
/// At the default sensitivity (`1.0`), one wheel
/// notch (`LineDelta` magnitude `1.0`) must move exactly one hotbar slot
/// — the default behavior — so the sensitivity feature is provably a
/// pure addition, not a regression of the common case.
#[test]
fn accumulate_scroll_moves_one_slot_per_notch_at_default_sensitivity() {
    let mut accum = 0.0;
    assert_eq!(accumulate_scroll(&mut accum, 1.0 * 1.0), 1);
    assert_eq!(accum, 0.0, "a whole-notch scroll must leave no carry");
    assert_eq!(accumulate_scroll(&mut accum, -1.0 * 1.0), -1);
}

/// A sensitivity below 1.0 must take more than one notch to move a slot
/// — the exact scaled amount, not merely "less than at 1.0". At `0.25`,
/// four notches of `1.0` each accumulate to exactly one slot, with the
/// third notch still producing zero.
#[test]
fn accumulate_scroll_carries_a_fractional_remainder_at_low_sensitivity() {
    let mut accum = 0.0;
    let scaled = 1.0 * 0.25_f64;
    assert_eq!(accumulate_scroll(&mut accum, scaled), 0);
    assert_eq!(accumulate_scroll(&mut accum, scaled), 0);
    assert_eq!(accumulate_scroll(&mut accum, scaled), 0);
    assert!(
        (accum - 0.75).abs() < 1e-12,
        "three quarter-notches must carry exactly 0.75, not round or clamp: got {accum}"
    );
    assert_eq!(
        accumulate_scroll(&mut accum, scaled),
        1,
        "the fourth quarter-notch must complete the first slot"
    );
    assert!(accum.abs() < 1e-12, "the completed slot must consume the whole carry");
}

/// A sensitivity above 1.0 must cross more than one slot per notch —
/// the exact scaled amount again, not a threshold on the existing ±1
/// step. At `10.0`, one notch is 10 whole slots with no carry.
#[test]
fn accumulate_scroll_moves_several_slots_per_notch_at_high_sensitivity() {
    let mut accum = 0.0;
    assert_eq!(accumulate_scroll(&mut accum, 1.0 * 10.0), 10);
    assert_eq!(accum, 0.0);
}

/// A direction reversal must drop the old carry rather than fight it
///: three-quarters of a slot built up
/// scrolling one way must not partially cancel a fresh scroll the other
/// way, or a player flicking back and forth would see scroll amounts
/// depend on unrelated history.
#[test]
fn accumulate_scroll_resets_the_carry_on_direction_reversal() {
    let mut accum = 0.0;
    assert_eq!(accumulate_scroll(&mut accum, 0.75), 0);
    assert!((accum - 0.75).abs() < 1e-12);
    // Reversed direction: a naive `accum += scaled` would land at
    // `0.75 - 0.25 = 0.5`, still short of a slot. The reset makes this
    // scroll's own `-0.25` the entire story.
    assert_eq!(accumulate_scroll(&mut accum, -0.25), 0);
    assert!(
        (accum - -0.25).abs() < 1e-12,
        "the old positive carry must be discarded, not partially offset: got {accum}"
    );
}

/// Large scroll events collapse the whole-notch count to its **sign** before it
/// becomes a hotbar-slot step, discarding any
/// magnitude beyond one rather than queuing it for a later event.
///
/// A single large delta cannot by itself prove this — six notches producing a
/// step of six (the wrong hypothesis) and six notches producing a step of one
/// (reference behavior) are the two things this test exists to tell apart, so the
/// assertion has to compare against the actual six, not merely check the step
/// is "smaller than something". Six is not a rounded-up guess: it is what a
/// real macOS trackpad flick produces through this shell's own pipeline —
/// `wheel_notches`' `PixelDelta` arm is `p.y * PRECISE_SCROLL_SCALE` (`0.1`),
/// so a 60-point single-event `PixelDelta` (an ordinary flick, well under
/// what a hard fling reports) already yields six notches at the default
/// `mouseWheelSensitivity` of `1.0` — the exact shape of the owner's report.
#[test]
fn hotbar_scroll_step_collapses_accumulated_magnitude_to_sign() {
    let flick_notches = 60.0 * PRECISE_SCROLL_SCALE;
    assert_eq!(flick_notches, 6.0, "premise: a 60pt flick really is six notches");
    let scaled = scale_scroll(flick_notches, false, 1.0);
    let mut accum = 0.0;
    let whole = accumulate_scroll(&mut accum, scaled);
    assert_eq!(
        whole, 6,
        "premise: the accumulator itself must still report the full six-notch \
         magnitude — this test is not about accumulate_scroll, which is correct \
         and already gated elsewhere"
    );

    assert_eq!(
        hotbar_scroll_step(whole),
        1,
        "vanilla advances the hotbar by exactly one slot per scroll event, \
         never by the event's whole-notch magnitude; the wrong hypothesis \
         (passing `whole` straight through) would give 6 here"
    );
    assert_eq!(hotbar_scroll_step(-whole), -1, "direction must survive the collapse");
    assert_eq!(hotbar_scroll_step(1), 1, "an ordinary single-notch event is unaffected");
    assert_eq!(hotbar_scroll_step(-1), -1);
    assert_eq!(hotbar_scroll_step(0), 0, "no whole notch, no step");
}

/// The hotbar belongs to the world, not to active play.
///
/// Oracle is vanilla, not our own reasoning — see `hud_follows_world`'s docs
/// for the four source lines. The regression was one boolean
/// (`self.ui.is_playing()`, *named* `crosshair`) gating both the reticle and
/// the hotbar, so opening the pause menu or the inventory took the hotbar with
/// it.
#[test]
fn the_hotbar_survives_every_screen_drawn_over_the_world() {
    use crate::menu::Screen;

    for screen in [
        Screen::Playing,
        Screen::Chat,
        Screen::Container,
        Screen::Paused,
        Screen::Death,
    ] {
        assert!(
            hud_follows_world(screen),
            "{screen:?} draws the world, so it must draw the world's hotbar"
        );
    }

    // -- negative control ------------------------------------------------
    // The predicate has to be able to say no, or the loop above is vacuous.
    // `Connecting` has no world yet; the menu screens never get here at all
    // because `draw_menu` returns first. Since `Connecting` is an
    // `owns_frame` screen, so it is one of the `draw_menu`-returns-first set —
    // asserted anyway, because the world-path hotbar gate must never come true
    // for a screen that draws no world.
    for screen in [
        Screen::Connecting,
        Screen::MainMenu,
        Screen::ServerList,
        Screen::ServerEdit,
        Screen::Settings,
        Screen::Error,
    ] {
        assert!(
            !hud_follows_world(screen),
            "{screen:?} has no world on screen, so it must have no hotbar"
        );
    }
}

/// The two questions must not collapse back into one boolean. `Paused` is the
/// screen that separates them: the crosshair goes, the hotbar stays.
#[test]
fn the_crosshair_and_the_hotbar_disagree_behind_a_screen() {
    let mut ui = UiState::new();
    ui.begin(SessionKind::Singleplayer);
    ui.session_ready();
    assert!(ui.is_playing(), "a ready session is in the world");
    assert!(hud_follows_world(ui.screen()));

    ui.pause();
    assert!(
        !ui.is_playing(),
        "the reticle's gate must go false behind the pause menu"
    );
    assert!(
        hud_follows_world(ui.screen()),
        "the hotbar's gate must stay true behind the pause menu"
    );
}

/// The terminal pointer adapter must activate the same row/action boundary as
/// the window path. Clicking the first title row is expected to open the shared
/// world-selection screen; a direct terminal-only menu implementation would
/// leave the title screen unchanged.
#[cfg(feature = "window")]
#[test]
fn terminal_pointer_click_reaches_shared_menu_state_transition() {
    use crate::menu::Screen;

    let mut app = WindowApp::new(Config::default());
    const WIDTH: u32 = 800;
    const HEIGHT: u32 = 600;
    let (x, y) = {
        let frame = crate::menu::nav::on_screen_frame(
            &app.ui,
            &app.nav,
            app.sim.death_message(),
            &app.statuses,
            &mut app.favicons,
        )
        .expect("the title screen must expose a render frame");
        let (logical_width, logical_height) =
            crate::menu::render::logical_canvas(frame.gui_scale, WIDTH, HEIGHT);
        let scale = crate::config::calculate_gui_scale(frame.gui_scale, WIDTH, HEIGHT).max(1)
            as f32;
        let (row_x, row_y, row_width, row_height) =
            crate::menu::render::row_rect(&frame.rows, 0, logical_width, logical_height)
                .expect("the first title row must have a hit area");
        ((row_x + row_width * 0.5) * scale, (row_y + row_height * 0.5) * scale)
    };
    assert_eq!(app.ui.screen(), Screen::MainMenu);
    app.terminal_pointer_button_at(
        crate::container::MenuButton::Left,
        true,
        x,
        y,
        WIDTH,
        HEIGHT,
    );
    assert_eq!(
        app.ui.screen(),
        Screen::WorldSelect,
        "terminal click must pass through MenuNav's ownership reconciliation"
    );
}

#[cfg(feature = "window")]
#[test]
fn terminal_enter_uses_the_same_menu_action_as_the_pointer() {
    use crate::menu::Screen;

    let mut app = WindowApp::new(Config::default());
    assert_eq!(app.ui.screen(), Screen::MainMenu);
    app.terminal_menu_key(crate::menu::nav::MenuKey::Enter);
    assert_eq!(
        app.ui.screen(),
        Screen::WorldSelect,
        "terminal Enter must dispatch through MenuNav and the shared action handler"
    );
}

#[test]
fn terminal_escape_and_wheel_reach_the_shared_playing_actions() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.begin(SessionKind::Singleplayer);
    app.ui.session_ready();
    assert!(app.ui.is_playing());

    app.terminal_scroll(1.0);
    assert_eq!(
        app.sim.selected_slot(),
        8,
        "terminal wheel-up must select the previous shared hotbar slot"
    );

    app.terminal_escape();
    assert!(app.ui.is_paused(), "terminal Escape must use the shared pause action");

    app.ui.open_container();
    assert!(app.terminal_has_container());
    app.terminal_escape();
    assert!(!app.terminal_has_container());
    assert!(app.ui.is_playing());
}

#[test]
fn terminal_middle_click_reaches_the_shared_pick_action() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    app.sim.set_ray_target_for_test(Some(crate::raycast::RayHit::face_center(
        [3, 70, -2],
        [0, 1, 0],
    )));

    app.terminal_mouse_pick_item(true);

    assert_eq!(
        actions.try_recv(),
        Ok(lodestone_model::ClientAction::PickItemFromBlock {
            pos: lodestone_model::BlockPos::new(3, 70, -2),
            include_data: true,
        }),
        "terminal middle-click must feed the same targeted pick action as the window path"
    );
}

#[test]
fn deferred_middle_pick_drops_failed_capture_and_replays_once_after_success() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.enter_dev_world();
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    app.sim.set_ray_target_for_test(Some(crate::raycast::RayHit::face_center(
        [3, 70, -2],
        [0, 1, 0],
    )));

    app.pending_pick = Some(PendingPick {
        include_data: true,
        requested_at: Instant::now(),
    });
    app.grabbed = false;
    app.replay_pending_pick();
    assert!(actions.try_recv().is_err(), "a failed grab must not emit a pick");
    assert!(app.pending_pick.is_none(), "failed grabs clear the one-shot request");

    app.pending_pick = Some(PendingPick {
        include_data: true,
        requested_at: Instant::now(),
    });
    app.grabbed = true;
    app.replay_pending_pick();
    assert_eq!(
        actions.try_recv(),
        Ok(lodestone_model::ClientAction::PickItemFromBlock {
            pos: lodestone_model::BlockPos::new(3, 70, -2),
            include_data: true,
        })
    );
    assert!(app.pending_pick.is_none(), "successful dispatch consumes the request");
    app.replay_pending_pick();
    assert!(actions.try_recv().is_err(), "one middle-click emits exactly once");
}

#[test]
fn deferred_middle_pick_expires_and_drops_when_play_ends() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.enter_dev_world();
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    app.sim.set_ray_target_for_test(Some(crate::raycast::RayHit::face_center(
        [3, 70, -2],
        [0, 1, 0],
    )));

    app.pending_pick = Some(PendingPick {
        include_data: false,
        requested_at: Instant::now() - PENDING_PICK_TIMEOUT - Duration::from_millis(1),
    });
    app.grabbed = true;
    app.replay_pending_pick();
    assert!(app.pending_pick.is_none(), "expired requests must be discarded");
    assert!(actions.try_recv().is_err());

    app.ui.on_escape();
    app.pending_pick = Some(PendingPick {
        include_data: false,
        requested_at: Instant::now(),
    });
    app.replay_pending_pick();
    assert!(app.pending_pick.is_none(), "menu transitions must clear pending picks");
    assert!(actions.try_recv().is_err());
}
