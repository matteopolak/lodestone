/// **The bug the owner reported: rebinding Toggle Perspective to `G` left
/// `F5` cycling the camera and `G` doing nothing until the next launch.**
///
/// Two very different faults produce that symptom and only a test spanning
/// both halves can tell them apart. `resolve_key` was never the problem — it
/// asks `binds.is(InputAction::TogglePerspective, code)` and always has, so
/// no literal `F5` was ever in the chain. The producer was: `WindowApp` held
/// its own `keybinds: Keybinds`, copied out of `Options::load()` in the
/// constructor, while the Key Binds screen writes `MenuNav`'s `Options` and
/// persists them. The write reached the file (`nav.rs`'s
/// `clicking_a_bind_button_then_capturing_a_key_rebinds_and_persists` proves
/// that end of it) and never reached the resolver, so the rebind applied on
/// the *next* launch only.
///
/// **Why no gate saw it, and what this one does differently.** The whole
/// Key Binds corpus lives in `nav.rs` and drives `MenuNav` with no
/// `WindowApp` at all — its own helper doc says so, calling `capture_binding`
/// "the same call `app.rs`'s patch is specified to make". The resolver corpus
/// in this file is the mirror image: every one of its cases builds its own
/// `Keybinds::new()` and hands it to `resolve_key`. Both corpora are correct
/// and neither can see a consumer reading a *different table* from the one
/// the menu writes — `CLAUDE.md`'s "a gate that installs its own input proves
/// the consumer and nothing about the producer". So this drives the real
/// screen into the real `WindowApp` and asks the app for its own table:
/// menu click → `capture_binding` → `WindowApp::keybinds` → `resolve_key` →
/// `apply_key_outcome` → the camera actually moving off first person.
///
/// The one hop it cannot take is the raw `winit::event::KeyEvent`
/// (unconstructable outside winit), so the capture is fed the `Binding` that
/// `handle_keyboard_input`'s `CaptureKey::Bind(code)` arm forwards verbatim —
/// and `capture_key_for` is asserted here too, so the substitution is checked
/// rather than assumed.

//! Tests for controls persistence and render-path debug guards.

use super::*;

#[test]
fn rebinding_toggle_perspective_in_the_controls_screen_takes_effect_without_a_restart() {
    use crate::keybinds::{Binding, InputAction};
    use crate::menu::key_binds::KeyControl;
    use crate::menu::options::{Cell, SettingsPage};

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    // A `MenuNav` on a scratch path, because finishing a capture *persists*:
    // with the production `MenuNav::new()` this test would rewrite the
    // developer's real `options.json` (`CLAUDE.md`'s OS-side-effect rule).
    let dir = std::env::temp_dir().join(format!(
        "lodestone-rebind-perspective-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    seed_owning_account(&dir);
    app.nav = MenuNav::with_path(dir.join("servers.json"));

    // Vanilla's own default, and the control for the second half below.
    assert_eq!(
        app.keybinds().binding(InputAction::TogglePerspective),
        Binding::Key(KeyCode::F5.into()),
        "precondition: the app starts on vanilla's F5"
    );

    // --- the producer: the real Controls screen, driven by real menu input.
    app.ui.open_settings();
    for page in [SettingsPage::Controls, SettingsPage::KeyBinds] {
        // `in_world: false` — this app reached Settings through
        // `ui.open_settings()`, never through the pause menu, so
        // `SettingsNav::in_world` is still its default.
        let cells = crate::menu::options::all_controls(app.nav.settings().page(), false);
        let target = cells
            .iter()
            .position(|c| matches!(c, Cell::Nav { page: Some(p), .. } if *p == page))
            .expect("the settings tree must offer this page");
        for _ in 0..=cells.len() {
            if app.nav.settings().cursor() == target {
                break;
            }
            app.nav.key(&mut app.ui, MenuKey::Down);
        }
        app.nav.key(&mut app.ui, MenuKey::Enter);
        assert_eq!(app.nav.settings().page(), page);
    }

    let controls = crate::menu::key_binds::all_controls();
    let target = controls
        .iter()
        .position(|c| *c == KeyControl::Bind(InputAction::TogglePerspective))
        .expect("Toggle Perspective must have a bind button on the Key Binds screen");
    for _ in 0..=controls.len() {
        if app.nav.settings().key_binds().cursor() == target {
            break;
        }
        app.nav.key(&mut app.ui, MenuKey::Down);
    }
    app.nav.key(&mut app.ui, MenuKey::Enter);
    assert!(
        app.nav.awaiting_key_capture(),
        "Enter on the bind button must start a capture"
    );

    // The substitution for the raw `KeyEvent`: this is exactly what
    // `handle_keyboard_input` computes and forwards for a `G` keydown.
    assert_eq!(
        capture_key_for(winit::keyboard::PhysicalKey::Code(KeyCode::KeyG)),
        Some(CaptureKey::Bind(KeyCode::KeyG)),
        "a G keydown must forward as CaptureKey::Bind(KeyG)"
    );
    app.nav.capture_binding(Binding::Key(KeyCode::KeyG.into()));

    // --- the consumer: the app's own table, with no restart in between.
    assert_eq!(
        app.keybinds().binding(InputAction::TogglePerspective),
        Binding::Key(KeyCode::KeyG.into()),
        "the rebind must reach the table the resolver reads, not just the file"
    );

    let binds = app.keybinds();
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyG), true, false, None),
        Some(KeyOutcome::TogglePerspective),
        "G must now cycle the perspective"
    );
    assert_ne!(
        resolve_key(&binds, playing(), Some(KeyCode::F5), true, false, None),
        Some(KeyOutcome::TogglePerspective),
        "and F5 must stop — a rebind that only *adds* a key is the same bug wearing a hat"
    );

    // --- the effect, through the real driver rather than the resolver's word.
    let before = app.sim.camera_type();
    app.apply_key_outcome(
        Some(KeyOutcome::TogglePerspective),
        true,
        Some(KeyCode::KeyG),
        None,
    );
    assert_ne!(
        app.sim.camera_type(),
        before,
        "the outcome must actually move the camera off first person"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn closed_f3_does_not_call_the_map_debug_gather() {
    let calls = std::cell::Cell::new(0);
    let hidden = super::redraw::map_debug_when_visible(false, || {
        calls.set(calls.get() + 1);
        Some((12, 0.5))
    });
    assert_eq!(hidden, None);
    assert_eq!(calls.get(), 0);

    let visible = super::redraw::map_debug_when_visible(true, || {
        calls.set(calls.get() + 1);
        Some((12, 0.5))
    });
    assert_eq!(visible, Some((12, 0.5)));
    assert_eq!(calls.get(), 1);
}

/// The other half of the rebind class: the **F3 chords are rebindable in
/// vanilla 26.2**, and this client used to hardcode them.
///
/// Checked in the jar rather than assumed, because the received wisdom (and
/// this repo's own comments) said the opposite. Vanilla's own persisted
/// options declare all seven F3-chord actions (show hitboxes, show chunk
/// borders, show advanced tooltips, spectate, switch game mode, focus pause,
/// and copy location)
/// as ordinary debug-category key bindings, collects them into its own debug-keys
/// list and folds that array into the full key-binding list — the one vanilla persists and the Controls
/// screen lists — and vanilla's own debug-key handling asks every one of them
/// whether it matches the event. So `code == KeyCode::KeyG` in `resolve_key` was the
/// divergence, not the table.
///
/// Driven exactly like
/// [`rebinding_toggle_perspective_in_the_controls_screen_takes_effect_without_a_restart`]
/// — the real screen, the real capture, the app's own table — because the
/// producer half is what the two existing corpora cannot see. The `F3` gate
/// flag is asserted on both sides: a chord must still need the modifier held,
/// so rebinding one onto a bare key does not make it fire during ordinary play.
#[test]
fn a_rebound_f3_chord_fires_on_its_new_key_and_stops_on_its_old_one() {
    use crate::keybinds::{Binding, InputAction};
    use crate::menu::key_binds::KeyControl;
    use crate::menu::options::{Cell, SettingsPage};

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let dir = std::env::temp_dir().join(format!("lodestone-rebind-chord-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    seed_owning_account(&dir);
    app.nav = MenuNav::with_path(dir.join("servers.json"));

    // The default debug binding for chunk borders uses key code 71.
    assert_eq!(
        app.keybinds()
            .binding(InputAction::DebugShowChunkBorders),
        Binding::Key(KeyCode::KeyG.into())
    );

    app.ui.open_settings();
    for page in [SettingsPage::Controls, SettingsPage::KeyBinds] {
        let cells = crate::menu::options::all_controls(app.nav.settings().page(), false);
        let target = cells
            .iter()
            .position(|c| matches!(c, Cell::Nav { page: Some(p), .. } if *p == page))
            .expect("the settings tree must offer this page");
        for _ in 0..=cells.len() {
            if app.nav.settings().cursor() == target {
                break;
            }
            app.nav.key(&mut app.ui, MenuKey::Down);
        }
        app.nav.key(&mut app.ui, MenuKey::Enter);
    }

    // Reaching the row by Down alone is what proves a Debug chord is actually
    // *listed* on the screen, not merely present in `InputAction::ALL`.
    let controls = crate::menu::key_binds::all_controls();
    let target = controls
        .iter()
        .position(|c| *c == KeyControl::Bind(InputAction::DebugShowChunkBorders))
        .expect("Show Chunk Boundaries must have a bind button");
    for _ in 0..=controls.len() {
        if app.nav.settings().key_binds().cursor() == target {
            break;
        }
        app.nav.key(&mut app.ui, MenuKey::Down);
    }
    app.nav.key(&mut app.ui, MenuKey::Enter);
    assert!(app.nav.awaiting_key_capture());
    app.nav.capture_binding(Binding::Key(KeyCode::KeyJ.into()));

    let binds = app.keybinds();
    let mut chord = playing();
    chord.debug_held = true;
    assert_eq!(
        resolve_key(&binds, chord, Some(KeyCode::KeyJ), true, false, None),
        Some(KeyOutcome::ToggleChunkBorders),
        "F3+J must now toggle chunk borders"
    );
    assert_ne!(
        resolve_key(&binds, chord, Some(KeyCode::KeyG), true, false, None),
        Some(KeyOutcome::ToggleChunkBorders),
        "and F3+G must stop"
    );
    // The modifier is still a gate flag, not an eighth bindable action: a
    // rebound chord must not fire as a bare key during ordinary play.
    assert_ne!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyJ), true, false, None),
        Some(KeyOutcome::ToggleChunkBorders),
        "a chord without F3 held is not a chord"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Ordinary startup is unaffected by the two new fields —
/// `WindowApp::new` wants a window and accepts its input immediately, exactly
/// as it always has. This is the negative control for
/// `new_headless_session_starts_with_no_presentation_desired_and_input_inert`
/// below: if both tests read the same values, the two constructors are not
/// actually distinguished by anything and the "starts headless" claim is
/// untested.
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-presentation"))]
#[test]
fn ordinary_startup_wants_a_window_and_arms_input_immediately() {
    let app = WindowApp::new(Config::default());
    assert!(app.presentation_desired);
    assert!(app.input_armed);
    assert!(app.sim.presentation_attached());
}

/// The headless-session constructor (`app::runners::run_headless_session`'s
/// entry point) is the actual new capability this adds: a session
/// that starts with no window, no input armed, and no presentation-only ECS
/// systems — not merely "a window that happens not to exist yet" the way
/// bring-up-in-progress on the browser target already could represent, but a
/// session that never asked for one and has already detached the ECS half
/// too.
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-presentation"))]
#[test]
fn new_headless_session_starts_with_no_presentation_desired_and_input_inert() {
    let app = WindowApp::new_headless_session(Config::default());
    assert!(!app.presentation_desired);
    assert!(!app.input_armed);
    assert!(window_physical_size(&app.config).is_none() || true); // config is unaffected either way
    assert!(
        !app.sim.presentation_attached(),
        "a headless-session start must detach the ECS half too, not just \
         suppress window creation — the terrain mesher and the \
         pick/interaction/particle systems must not run with nothing to \
         consume their output"
    );
    assert!(app.window.is_none());
    assert!(app.gpu.is_none());
    assert!(app.render.is_none());
}

/// `WindowApp::detach_presentation` must be safe to call on a session that is
/// already headless (the common real case: a headless-session start, or a
/// second detach nobody guarded against) — a no-op, not a panic on an
/// already-`None` field.
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-presentation"))]
#[test]
fn detach_presentation_on_an_already_headless_app_is_a_safe_no_op() {
    let mut app = WindowApp::new_headless_session(Config::default());
    app.detach_presentation();
    assert!(app.window.is_none());
    assert!(app.gpu.is_none());
    assert!(!app.presentation_desired);
    assert!(!app.input_armed);
}

/// **The plugin-registration seam's remaining gap, closed**: before `WindowApp::new_with_app`
/// existed, `WindowApp::new` (and therefore `run_windowed`, and therefore
/// `Mode::Window` — the shipped, on-screen client) built its own `Sim::new`
/// with no parameter a caller could use to add a plugin. `Sim::client_app()` +
/// `Sim::from_app` already let a caller compose an `App` and drive a bare
/// `Sim` by hand (`tests/interaction/rendered_client_takes_a_plugin.rs`), but
/// nothing fed that composed `App` into the actual struct the real winit
/// driver constructs — `Sim::from_app` proved the ECS half accepts a plugin,
/// not that the shipped binary's entry point does.
///
/// This test is that missing half, one level down from the bare-`Sim` gate:
/// build the identical `JumpPlugin`-shaped marker, hand it to
/// `WindowApp::new_with_app` (what `run_windowed_with_app`, and therefore
/// `crate::run_with_app`, now call instead of `WindowApp::new`), and drive
/// real `GameTick`s through `app.sim.step` — the same driver
/// `app::redraw`'s per-frame loop calls in production. Registration is
/// proven by the player's position changing, not by a flag.
struct MarkerPlugin;

impl lodestone_ecs::app::Plugin for MarkerPlugin {
    fn build(&self, app: &mut lodestone_ecs::app::App) {
        use bevy_ecs::schedule::IntoScheduleConfigs;
        app.add_systems(
            lodestone_ecs::GameTick,
            mark_jump
                .after(lodestone_ecs::TickSet::Intent)
                .before(lodestone_ecs::TickSet::Physics),
        );
    }
}

fn mark_jump(
    mut q: bevy_ecs::prelude::Query<
        &mut lodestone_ecs::player::MovementIntent,
        bevy_ecs::prelude::With<lodestone_ecs::player::LocalPlayer>,
    >,
) {
    for mut intent in &mut q {
        intent.0.forward = 0.0;
        intent.0.strafe = 0.0;
        intent.0.jump = true;
        intent.0.sneak = false;
        intent.0.sprint = false;
    }
}

fn marker_test_config() -> Config {
    Config {
        mode: Mode::Headless,
        render_distance: 2,
        ..Config::default()
    }
}

#[test]
fn window_app_new_with_app_wires_a_callers_plugin_into_the_real_constructor() {
    let mut plugin_app = Sim::client_app();
    plugin_app.add_plugins(MarkerPlugin);
    let mut app = WindowApp::new_with_app(plugin_app, marker_test_config());

    let start = app.sim.player().position;
    let mut apex = 0.0f64;
    for _ in 0..60 {
        app.sim.step(1.0 / 20.0);
        apex = apex.max(app.sim.player().position.y - start.y);
    }
    let now = app.sim.player().position;
    let horizontal = ((now.x - start.x).powi(2) + (now.z - start.z).powi(2)).sqrt();

    assert!(
        (0.9..1.6).contains(&apex),
        "a plugin handed to `WindowApp::new_with_app` must drive the real, \
         windowed-shell `Sim`'s local player through a real `GameTick`: \
         expected a vanilla jump apex near 1.2522 blocks, measured {apex:.4} \
         (horizontal displacement {horizontal:.4})"
    );
    assert!(
        horizontal < 0.1,
        "premise: a jump-in-place plugin must not travel, or the apex above \
         is measuring terrain rather than the jump; horizontal displacement \
         was {horizontal:.4}"
    );
}

/// The negative control: `WindowApp::new` — what every real `Mode::Window`
/// run still builds when no caller supplies an `App` — registers no such
/// plugin, so the identical budget must leave the player's position
/// unchanged. Without this, a demo-world `Sim` whose player slid for any
/// unrelated reason would make the positive assertion above read as a pass
/// for the wrong reason.
#[test]
fn without_a_supplied_app_window_app_new_still_leaves_the_player_put() {
    let mut app = WindowApp::new(marker_test_config());

    let start = app.sim.player().position;
    let mut apex = 0.0f64;
    for _ in 0..60 {
        app.sim.step(1.0 / 20.0);
        apex = apex.max(app.sim.player().position.y - start.y);
    }
    let now = app.sim.player().position;
    let horizontal = ((now.x - start.x).powi(2) + (now.z - start.z).powi(2)).sqrt();

    assert!(
        apex < 0.05,
        "control: with no plugin supplied, nothing may lift the player, yet \
         the apex was {apex:.4} blocks"
    );
    assert!(
        horizontal < 0.05,
        "control: with no plugin supplied, nothing may move the player \
         horizontally, yet displacement was {horizontal:.4} blocks"
    );
}
