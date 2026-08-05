//! `app`'s unit tests, unwrapped verbatim out of `app.rs`.
//!
//! Kept as a single file on purpose: splitting it would rename every test
//! path (`app::tests::foo` -> `app::tests::input::foo`), and those names are
//! cited in issues and commit messages across the repo.

use super::*;

/// Java's `String.hashCode()`, computed by hand from the well-known
/// public algorithm — an oracle that lives outside this file, per
/// `CLAUDE.md`'s evidence standard. `"hello"`: `h = 0`, then
/// `104, 3325, 103183, 3198781, 99162322` after `'h','e','l','l','o'`
/// (`h = h*31 + c` each step) — a commonly-cited constant, reproduced
/// here from the formula rather than trusted from memory alone.
#[test]
fn java_string_hash_code_matches_the_known_constant() {
    assert_eq!(java_string_hash_code("hello"), 99_162_322);
    assert_eq!(java_string_hash_code(""), 0);
}

/// **Issue #47's queued patch, exercised through production code.**
///
/// The command-block screen's Done button computed a fully-tested payload
/// and **dropped it on the floor** — `activate_command_block_row`'s `Done`
/// arm bound it to `let _submit` because `MenuAction` had no variant to
/// carry it and `app.rs` had no arm to consume it. This drives the whole
/// chain rather than re-asserting either half: the real
/// [`crate::menu::nav::MenuNav::key`] on the real `Done` row produces the
/// action, the real [`WindowApp::apply_menu_action`] consumes it, and the
/// `ClientAction` is read off the socket seam a live session would write to.
///
/// **The expected value is predicted, not round-tripped.** Every field is
/// stated from the edits made below (a typed command, a cycled mode, two
/// toggles) rather than from `to_submit()`'s own output, so a payload that
/// dropped or transposed a field fails here — `decode(encode(x)) == x` would
/// not.
///
/// **Negative control, executed:** deleting the
/// `MenuAction::SetCommandBlock` arm from `apply_menu_action` (replacing it
/// with `{}`) makes this fail at `try_recv`, `Err(Empty)` — nothing reaches
/// the socket. That is the island this patch closes, and it is invisible to
/// `cargo check`: an arm that matches and does nothing compiles perfectly.
///
/// Reachability is a **separate** and still-open matter: nothing opens this
/// screen from a real interaction (no command-block block-entity NBT decode,
/// no `interact.rs` trigger), which is issue #442. This test opens it
/// directly, exactly as `MenuNav::open_command_block` is written to allow.
#[test]
fn the_command_block_done_button_sends_a_real_set_command_block_action() {
    use crate::menu::command_block::{CommandBlockOpen, CommandBlockRow, COMMAND_BLOCK_ROWS};
    use crate::menu::nav::MenuKey;
    use lodestone_model::{BlockPos, CommandBlockMode};

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);

    // `MenuNav::open_command_block` and `UiState::open_command_block` both
    // guard on `Screen::Playing` (a command block is opened from the world,
    // not from a menu), so reach that first — `enter_dev_world` is the
    // headless entry point's own route to it.
    app.ui.enter_dev_world();

    // Open the screen on a specific block with known stored contents, then
    // *edit* it — an unedited screen would let a `to_submit` that returned
    // `CommandBlockOpen`'s values verbatim pass.
    let pos = BlockPos::new(12, -7, 340);
    app.nav.open_command_block(
        &mut app.ui,
        CommandBlockOpen {
            pos,
            command: "say hi".into(),
            track_output: false,
            previous_output: None,
            mode: CommandBlockMode::Redstone,
            conditional: false,
            automatic: false,
        },
    );
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "precondition: the screen must actually be open, or every key below \
         lands somewhere else"
    );

    // Type into the command field, through the real key path.
    for ch in "!".chars() {
        let action = app.nav.key(&mut app.ui, MenuKey::Char(ch));
        app.apply_menu_action(action);
    }
    // Cycle the mode once (Redstone -> its successor) and flip two toggles,
    // each by activating that row the way a click or Enter does.
    for row in [
        CommandBlockRow::Mode,
        CommandBlockRow::TrackOutput,
        CommandBlockRow::Conditional,
    ] {
        let idx = COMMAND_BLOCK_ROWS
            .iter()
            .position(|r| *r == row)
            .expect("every CommandBlockRow is in COMMAND_BLOCK_ROWS");
        let action = app.nav.click(&mut app.ui, idx);
        app.apply_menu_action(action);
    }

    // Read the mode the cycle actually produced from the screen itself, so
    // this test does not hardcode `next_mode`'s table (which has its own
    // gate in `command_block.rs`) — but every *other* field is predicted.
    let expected_mode = app
        .nav
        .command_block()
        .expect("the screen is still open")
        .mode;
    assert_ne!(
        expected_mode,
        CommandBlockMode::Redstone,
        "precondition: cycling the mode must have changed it, or this field \
         is not under test"
    );

    // Nothing may have reached the socket yet — the control for the
    // assertion below, and it is not vacuous: the toggle rows above all
    // return `MenuAction::None`, so a `_ =>` arm that sent something for
    // every action would be caught here.
    assert!(
        actions.try_recv().is_err(),
        "no action may be sent before Done is pressed"
    );

    // Press Done.
    let done = COMMAND_BLOCK_ROWS
        .iter()
        .position(|r| *r == CommandBlockRow::Done)
        .expect("Done is a CommandBlockRow");
    let action = app.nav.click(&mut app.ui, done);
    assert!(
        matches!(action, crate::menu::nav::MenuAction::SetCommandBlock(_)),
        "the Done row must produce the action, not swallow it: {action:?}"
    );
    app.apply_menu_action(action);

    // And it reached the wire, with exactly the edited payload.
    let sent = actions
        .try_recv()
        .expect("Done must put a ClientAction on the outbound seam");
    assert_eq!(
        sent,
        lodestone_model::ClientAction::SetCommandBlock {
            pos,
            command: "say hi!".into(),
            mode: expected_mode,
            track_output: true,
            conditional: true,
            automatic: false,
        },
        "the action must carry the screen's edits, field for field"
    );

    // Vanilla closes after sending (`CommandBlockEditScreen.java:111-114`).
    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "Done sends and then closes"
    );
}

/// `WorldOptions.parseSeed` (issue #190): a valid `i64` literal is used
/// verbatim (vanilla tries `Long.parseLong` first), whitespace is
/// trimmed, and non-numeric text falls back to the Java hash — not a new
/// rule, just `parse_seed` calling straight through to the constant test
/// above.
#[test]
fn parse_seed_follows_vanillas_own_rule() {
    assert_eq!(parse_seed("12345"), 12345);
    assert_eq!(parse_seed("-42"), -42);
    assert_eq!(parse_seed("  42  "), 42, "vanilla trims before parsing");
    assert_eq!(
        parse_seed("hello"),
        99_162_322,
        "non-numeric text must hash exactly like Java's own String.hashCode, \
         not this crate's own notion of a hash"
    );
}

/// An empty seed means "random" (`WorldOptions.defaultWithRandomSeed`) —
/// asserted by absence of a fixed answer, the only honest assertion for
/// "random": two draws must not collide (astronomically unlikely for a
/// real `i64` random source, impossible for a constant-returning bug).
#[test]
fn empty_seed_is_random_not_a_fixed_fallback() {
    let a = parse_seed("");
    let b = parse_seed("   ");
    assert_ne!(
        a, b,
        "two empty-seed draws must not produce the same i64 — a constant \
         here would silently make every \"random\" world identical"
    );
}

/// Issue #190's queued patch, driven end to end: two different
/// `WorldCreationConfig`s (the exact type `Screen::CreateWorld` collects)
/// resolved through the *production* `resolve_launch_seed` must generate
/// **different real terrain** at the same coordinate — not merely
/// different `i64`s, which `parse_seed`'s own tests above already cover
/// and which would be the isolated-unit species of this gate. And the
/// same config must reproduce identical terrain.
///
/// `lodestone_server::overworld_generator` is exactly what
/// `crate::net::run`'s `Origin::Integrated` arm calls with this
/// function's resolved seed (`net.rs:1354` at the time of writing) — so
/// this proves the seed that would reach the wire, not a stand-in.
#[test]
fn resolved_seeds_from_different_world_creation_configs_generate_different_terrain() {
    let config_a = crate::menu::create_world::WorldCreationConfig {
        seed: "100".to_string(),
        ..Default::default()
    };
    let config_b = crate::menu::create_world::WorldCreationConfig {
        seed: "999999".to_string(),
        ..Default::default()
    };

    let seed_a = resolve_launch_seed(Some(&config_a));
    let seed_b = resolve_launch_seed(Some(&config_b));
    assert_eq!(seed_a, 100);
    assert_eq!(seed_b, 999_999);

    let column_a = lodestone_server::overworld_generator(seed_a).column(0, 0);
    let column_b = lodestone_server::overworld_generator(seed_b).column(0, 0);

    let mut differences = 0usize;
    for lz in 0..16usize {
        for lx in 0..16usize {
            for y in (column_a.min_y()..column_a.min_y() + column_a.height()).step_by(4) {
                if column_a.block_state(lx, y, lz) != column_b.block_state(lx, y, lz) {
                    differences += 1;
                }
            }
        }
    }
    assert!(
        differences > 0,
        "two different entered seeds must generate different terrain \
         somewhere in the same column — the config's seed is reaching \
         nowhere if this is 0"
    );

    // Reproducibility: the same config, resolved and generated twice,
    // must be byte-identical — `overworld_generator` is a pure function
    // of its seed, and this is the exact call `net.rs::run` makes, called
    // twice rather than reimplemented.
    let seed_a_again = resolve_launch_seed(Some(&config_a));
    assert_eq!(seed_a_again, seed_a, "the same typed seed must resolve identically");
    let column_a_again = lodestone_server::overworld_generator(seed_a_again).column(0, 0);
    for lz in 0..16usize {
        for lx in 0..16usize {
            for y in column_a.min_y()..column_a.min_y() + column_a.height() {
                assert_eq!(
                    column_a.block_state(lx, y, lz),
                    column_a_again.block_state(lx, y, lz),
                    "the same seed must reproduce identical terrain at ({lx},{y},{lz})"
                );
            }
        }
    }
}

/// `None` (`Screen::WorldSelect`'s Play Selected World) must still resolve
/// to the bundled world's own seed — the pre-#190 behaviour, unchanged.
#[test]
fn no_config_resolves_to_the_bundled_worlds_seed() {
    assert_eq!(
        resolve_launch_seed(None),
        crate::menu::world_select::BUNDLED_WORLD.seed
    );
}

/// A cheap sim: headless mode with the smallest render distance that still
/// generates real terrain, so physics ticks do real collision work.
fn pacing_sim() -> Sim {
    // Explicitly the demo-world fixture: this needs real terrain so the
    // physics ticks do collision work, and the client `Sim::new` has none.
    Sim::with_demo_world(Config {
        mode: Mode::Headless,
        render_distance: 2,
        ..Config::default()
    })
}

/// Ticks a real `Sim` executes when advanced by `dt` in one call.
fn ticks_for(sim: &mut Sim, dt: f64) -> u64 {
    let before = sim.tick_count();
    sim.step(dt);
    sim.tick_count() - before
}

/// Issue #444, `discreteMouseScroll`: the delta collapses to its **sign**, and the
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

    // `Math.signum(0.0)` is **0.0**, not 1.0. `f64::signum` disagrees, so this is the
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

/// Issue #203: at the vanilla default sensitivity (`1.0`), one wheel
/// notch (`LineDelta` magnitude `1.0`) must move exactly one hotbar slot
/// — the pre-#203 behaviour — so the sensitivity feature is provably a
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
/// (`ScrollWheelHandler.java:14-16`): three-quarters of a slot built up
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

/// Issue #61: the hotbar belongs to the world, not to active play.
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
    // `Connecting` reaches the world render path (it is not an `owns_frame`
    // screen) but has no world yet; the menu screens never get here at all
    // because `draw_menu` returns first — asserted anyway so a future
    // `owns_frame` change cannot quietly turn this into `true` everywhere.
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

#[test]
fn vanillas_cap_is_ten_ticks_of_real_time() {
    // Guards the constant against a silent edit. 10 ticks × 50 ms = 500 ms;
    // read from Minecraft.java:262 / :1176 (see `MAX_TICKS_PER_UPDATE`).
    assert_eq!(MAX_TICKS_PER_UPDATE, 10);
    assert!((MAX_CATCHUP_SECS - 0.5).abs() < 1e-12, "{MAX_CATCHUP_SECS}");
}

#[test]
fn a_long_stall_is_clamped_not_replayed() {
    // The reported bug: tab out for a minute, tab back in, and the client
    // tries to run every tick it missed. Sixty seconds is 1200 ticks.
    let stall = Duration::from_secs(60);
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    let step = pacer.begin_frame(t0 + stall);

    assert!(
        (step.dt - MAX_CATCHUP_SECS).abs() < 1e-12,
        "a {stall:?} stall must be clamped to {MAX_CATCHUP_SECS}s, got {}",
        step.dt
    );

    // Drive a *real* sim with it and count the ticks that actually run.
    let mut sim = pacing_sim();
    let clamped = ticks_for(&mut sim, step.dt);
    assert!(
        clamped <= u64::from(MAX_TICKS_PER_UPDATE),
        "catch-up must never exceed vanilla's cap, got {clamped}"
    );

    // Measured: **10**. It used to be 5, because `Sim::step` applied its own,
    // tighter `dt.clamp(0.0, 0.25)` to the accumulator before the tick loop and
    // so silently halved this pacer's budget. That assertion said as much out
    // loud ("if this changed, reconcile the two caps") and this is the change
    // that reconciled them: §4.1(c) left one accumulator
    // (`lodestone_ecs::FrameClock`) on one policy
    // (`lodestone_ecs::MAX_CATCH_UP_SECS`), and the surviving number is
    // vanilla's ten — the only one of the two candidates with an external
    // oracle. See that constant's docs for the full argument.
    assert_eq!(
        clamped,
        u64::from(MAX_TICKS_PER_UPDATE),
        "one clamp now: `FrameClock::begin_frame` banks at most \
         {MAX_CATCHUP_SECS} s, so a maximal stall runs exactly vanilla's \
         {MAX_TICKS_PER_UPDATE} catch-up ticks"
    );
    // …and the shell's clamp *is* the ECS's, not a second one that happens to
    // agree. A copy that agreed today is how the five-vs-ten divergence
    // started.
    assert!(
        (MAX_CATCHUP_SECS - lodestone_ecs::MAX_CATCH_UP_SECS).abs() < 1e-12,
        "app.rs and lodestone-ecs must not carry two catch-up budgets"
    );

    // -- negative control ------------------------------------------------
    // Prove the detector fires: the same real `Sim`, driven the
    // *proportional* way the bug describes (one tick's worth of dt at a
    // time until the stall is consumed), executes the full 1200 ticks. If
    // `tick_count` could not observe a burst, this would not move either.
    let mut control = pacing_sim();
    let mut unclamped = 0u64;
    for _ in 0..(stall.as_secs_f64() / TICK_SECS) as u32 {
        unclamped += ticks_for(&mut control, TICK_SECS);
    }
    assert_eq!(unclamped, 1200, "control must replay every missed tick");
    assert!(
        unclamped > clamped * 100,
        "clamp must be a large reduction: {clamped} vs {unclamped}"
    );
}

#[test]
fn a_normal_frame_is_untouched_by_the_clamp() {
    // The clamp must be invisible at playable frame rates, or it would be
    // silently dropping game time during ordinary play (which is exactly
    // what a too-tight cap does: at 4 fps a 0.25 s cap discards 75% of it).
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    let frame = Duration::from_micros(16_667); // 60 fps
    let step = pacer.begin_frame(t0 + frame);
    assert!(
        (step.dt - frame.as_secs_f64()).abs() < 1e-9,
        "60 fps frame was altered: {}",
        step.dt
    );

    // And a 4 fps frame — the rate an occluded window degrades to — must
    // still deliver all 250 ms, i.e. five whole ticks, not be truncated.
    let mut pacer = FramePacer::new(t0);
    let step = pacer.begin_frame(t0 + Duration::from_millis(250));
    let mut sim = pacing_sim();
    assert_eq!(ticks_for(&mut sim, step.dt), 5);
}

#[test]
fn an_unfocused_window_keeps_ticking_and_presents_at_thirty_fps() {
    // The whole point: presentation throttles, simulation does not.
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    pacer.set_focused(false);

    let mut sim = pacing_sim();
    let mut rendered = 0u32;
    let mut ticks = 0u64;
    // One simulated second at a 120 Hz loop rate.
    for i in 1..=120u32 {
        let step = pacer.begin_frame(t0 + Duration::from_secs_f64(f64::from(i) / 120.0));
        if step.render {
            rendered += 1;
        }
        ticks += ticks_for(&mut sim, step.dt);
    }

    // 19 or 20: one simulated second at 20 Hz, modulo where the fixed-step
    // residual happens to land (1/120 is not exact in binary, so the last
    // tick can fall just past the second boundary).
    assert!(
        (19..=20).contains(&ticks),
        "unfocused must still tick at ~20 Hz, got {ticks}"
    );
    assert!(
        (30..=31).contains(&rendered),
        "unfocused presentation should be ~30 fps, got {rendered}"
    );
    assert!(
        u64::from(rendered) > ticks,
        "sanity: 30 fps presentation must still outpace 20 Hz ticking"
    );
}

/// Counts frames a naive "elapsed since the last presented frame" gate would
/// deliver over `iters` iterations of a `loop_hz` loop. This is verbatim the
/// implementation [`FramePacer`] used to have — including the `as_secs_f64()`
/// comparison against a `1.0 / 30.0` target, which is part of why it drifted:
/// a `Duration` is whole nanoseconds, so an interval that lands on
/// 33 333 333 ns is *always* a hair short of 1/30 s and the very iteration
/// that should have presented never does.
fn naive_gate_frames(loop_hz: u32, iters: u32) -> u32 {
    let target_secs = 1.0 / f64::from(UNFOCUSED_FPS);
    let t0 = Instant::now();
    let mut last_render = t0;
    let mut n = 0;
    for i in 1..=iters {
        let now = t0 + Duration::from_secs_f64(f64::from(i) / f64::from(loop_hz));
        if now.saturating_duration_since(last_render).as_secs_f64() >= target_secs {
            last_render = now;
            n += 1;
        }
    }
    n
}

/// Same span, driven through the real pacer while unfocused.
fn paced_frames(loop_hz: u32, iters: u32) -> u32 {
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    pacer.set_focused(false);
    let mut n = 0;
    for i in 1..=iters {
        let now = t0 + Duration::from_secs_f64(f64::from(i) / f64::from(loop_hz));
        if pacer.begin_frame(now).render {
            n += 1;
        }
    }
    n
}

#[test]
fn the_unfocused_frame_schedule_does_not_drift_below_its_target() {
    // The bug, and the negative control for the fix. A 30 fps limiter that
    // quietly delivers 26 fps is the whole reason the deadline is absolute:
    // the naive gate can only fire on a loop iteration, and each firing
    // pushes the next deadline out by however far it overshot.
    //
    // Measured, one simulated second each:
    //   loop     naive   paced   target
    //   120 Hz     26      30      30
    //    75 Hz     25      30      30
    //    77 Hz     26      30      30
    for loop_hz in [120u32, 75, 77, 144, 240] {
        let naive = naive_gate_frames(loop_hz, loop_hz);
        let paced = paced_frames(loop_hz, loop_hz);
        assert!(
            (UNFOCUSED_FPS..=UNFOCUSED_FPS + 1).contains(&paced),
            "at {loop_hz} Hz the absolute schedule delivered {paced}, \
             wanted {UNFOCUSED_FPS}"
        );
        // The control must be observed *failing* the same assertion, or this
        // test proves only that some number came out of some function.
        assert!(
            naive < UNFOCUSED_FPS,
            "control did not fire at {loop_hz} Hz: the naive gate delivered \
             {naive}, so this test is not measuring the drift it exists for"
        );
    }
    // Exact pre-fix number at the loop rate the sibling test uses, pinned so
    // a future refactor that reintroduces drift is unambiguous.
    assert_eq!(naive_gate_frames(120, 120), 26);
}

#[test]
fn coming_back_from_a_stall_resumes_the_rate_rather_than_replaying_a_backlog() {
    // The presentation-side twin of the catch-up-tick bug: a schedule that
    // advanced by whole intervals *unconditionally* would owe 3600 frames
    // after a two-minute stall and present them as fast as the loop spins.
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    pacer.set_focused(false);
    // Two minutes with no iterations at all, then a tight 120 Hz loop for
    // half a second.
    let resume = t0 + Duration::from_secs(120);
    assert!(pacer.begin_frame(resume).render, "the first frame back draws");

    let mut after = 0;
    for i in 1..=60u32 {
        if pacer
            .begin_frame(resume + Duration::from_secs_f64(f64::from(i) / 120.0))
            .render
        {
            after += 1;
        }
    }
    // Half a second at 30 fps is 15 frames. The backlog would be ~3600.
    assert!(
        (14..=16).contains(&after),
        "expected the steady ~30 fps rate after resuming, got {after} frames \
         in 0.5 s — a replayed backlog looks like ~60 (loop-rate-bound)"
    );
}

#[test]
fn an_occluded_window_skips_presenting_entirely_but_still_ticks() {
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    pacer.set_occluded(true);

    let mut sim = pacing_sim();
    let mut ticks = 0u64;
    for i in 1..=120u32 {
        let step = pacer.begin_frame(t0 + Duration::from_secs_f64(f64::from(i) / 120.0));
        assert!(!step.render, "occluded windows must not acquire a drawable");
        ticks += ticks_for(&mut sim, step.dt);
    }
    assert!(
        (19..=20).contains(&ticks),
        "occluded must still tick at ~20 Hz, got {ticks}"
    );

    // Control: the identical loop with occlusion cleared *does* render, so
    // the assertion above is testing occlusion and not a dead pacer.
    pacer.set_occluded(false);
    let step = pacer.begin_frame(t0 + Duration::from_secs(2));
    assert!(step.render, "clearing occlusion must restore presentation");
}

#[test]
fn focus_selects_the_control_flow_without_ever_stopping_the_loop() {
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    assert!(matches!(pacer.control_flow(t0), ControlFlow::Poll));
    assert!(pacer.focused());

    pacer.set_focused(false);
    match pacer.control_flow(t0) {
        ControlFlow::WaitUntil(at) => {
            let slice = at.saturating_duration_since(t0);
            assert!(
                slice < Duration::from_secs_f64(TICK_SECS),
                "background poll {slice:?} must wake faster than one 50 ms tick, \
                 or the sim falls behind the server while merely unfocused"
            );
        }
        other => panic!("unfocused must sleep, not spin or wait forever: {other:?}"),
    }
    assert!(!pacer.focused());
}

// -- key dispatch and precedence ----------------------------------------
//
// These drive [`resolve_key`] directly. It is the whole of the key chain's
// decision-making, so a precedence regression shows up here rather than
// needing a window, a GPU and a live `Sim` to observe.

use crate::keybinds::{Binding, InputAction};

/// The gate while the world is being played normally.
fn playing() -> KeyGate {
    KeyGate {
        gameplay: true,
        ..KeyGate::default()
    }
}

fn resolve(gate: KeyGate, code: KeyCode, pressed: bool) -> Option<KeyOutcome> {
    resolve_key(&Keybinds::new(), gate, Some(code), pressed, false)
}

/// Like [`resolve`], but with Control held — only the drop-key tests need
/// this axis, so it is a separate helper rather than a fifth argument on
/// every existing call above.
fn resolve_ctrl(gate: KeyGate, code: KeyCode, pressed: bool) -> Option<KeyOutcome> {
    resolve_key(&Keybinds::new(), gate, Some(code), pressed, true)
}

/// Issue #15's last hop: an F-key has no printable `text`, so it is
/// exactly the case `menu_key_for` drops and `capture_key_for` must not.
/// `F1` (not `F5`, which `resolve_key`'s own default table already binds
/// to `TogglePerspective` — picking a bound key here would prove nothing
/// about the *unbound*, no-text case a real Controls-menu rebind targets)
/// persists as vanilla's own `"key.keyboard.f1"`.
#[test]
fn capture_key_for_forwards_a_function_key() {
    assert_eq!(
        capture_key_for(PhysicalKey::Code(KeyCode::F1)),
        Some(CaptureKey::Bind(KeyCode::F1)),
        "an F-key must reach the capture as a bindable key, not be \
         dropped the way menu_key_for drops it"
    );
}

/// Escape must cancel through the ordinary `MenuKey` path
/// (`CaptureKey::Cancel`), never through `capture_binding` — the latter
/// is exactly the `Pause`-unbinding hazard `capture_binding`'s own doc
/// warns about, and this is the one physical key capture must special-case
/// rather than forward.
#[test]
fn capture_key_for_treats_escape_as_cancel_not_a_binding() {
    assert_eq!(
        capture_key_for(PhysicalKey::Code(KeyCode::Escape)),
        Some(CaptureKey::Cancel)
    );
}

/// A printable key must forward too — a capture target is not always an
/// unprintable one (most vanilla rebinds are ordinary letters), so this
/// is the control proving `capture_key_for` is not secretly just
/// `menu_key_for` under another name.
#[test]
fn capture_key_for_forwards_a_printable_key_too() {
    assert_eq!(
        capture_key_for(PhysicalKey::Code(KeyCode::KeyF)),
        Some(CaptureKey::Bind(KeyCode::KeyF))
    );
}

/// No `KeyCode` exists to persist for an unidentified physical key, so
/// there is nothing to bind — matches `menu_key_for`'s own `_ => {}`.
#[test]
fn capture_key_for_ignores_an_unidentified_key() {
    assert_eq!(
        capture_key_for(PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified
        )),
        None
    );
}

/// Every key the default table binds, with what it should resolve to while
/// playing. Written out rather than derived from the table, so this is a
/// second statement of intent and not a restatement of the implementation.
fn default_playing_expectations() -> Vec<(KeyCode, KeyOutcome)> {
    vec![
        (KeyCode::KeyW, KeyOutcome::Movement(Action::Forward, true)),
        (KeyCode::KeyS, KeyOutcome::Movement(Action::Back, true)),
        (KeyCode::KeyA, KeyOutcome::Movement(Action::Left, true)),
        (KeyCode::KeyD, KeyOutcome::Movement(Action::Right, true)),
        (KeyCode::Space, KeyOutcome::Movement(Action::Jump, true)),
        (KeyCode::ShiftLeft, KeyOutcome::Movement(Action::Sneak, true)),
        (
            KeyCode::ControlLeft,
            KeyOutcome::Movement(Action::Sprint, true),
        ),
        (KeyCode::KeyE, KeyOutcome::OpenContainer),
        (KeyCode::KeyT, KeyOutcome::OpenChat { command: false }),
        (KeyCode::Slash, KeyOutcome::OpenChat { command: true }),
        (KeyCode::Tab, KeyOutcome::PlayerList(true)),
        (KeyCode::F5, KeyOutcome::TogglePerspective),
        (KeyCode::F3, KeyOutcome::ToggleDebugOverlay),
        (KeyCode::Escape, KeyOutcome::Pause),
        (KeyCode::Digit1, KeyOutcome::SelectSlot(0)),
        (KeyCode::Digit2, KeyOutcome::SelectSlot(1)),
        (KeyCode::Digit3, KeyOutcome::SelectSlot(2)),
        (KeyCode::Digit4, KeyOutcome::SelectSlot(3)),
        (KeyCode::Digit5, KeyOutcome::SelectSlot(4)),
        (KeyCode::Digit6, KeyOutcome::SelectSlot(5)),
        (KeyCode::Digit7, KeyOutcome::SelectSlot(6)),
        (KeyCode::Digit8, KeyOutcome::SelectSlot(7)),
        (KeyCode::Digit9, KeyOutcome::SelectSlot(8)),
    ]
}

#[test]
fn the_default_bindings_dispatch_exactly_as_they_did_before_the_refactor() {
    // The no-regression gate for the whole change: every key the hardcoded
    // chain used to handle still resolves to the same effect.
    for (code, want) in default_playing_expectations() {
        assert_eq!(
            resolve(playing(), code, true),
            Some(want),
            "{code:?} regressed"
        );
    }
}

#[test]
fn the_hotbar_number_keys_select_the_slot_one_below_their_digit() {
    // Called out as one of the two things most likely to break quietly: the
    // digits are 1..9 and the slots are 0..8, so an off-by-one here shifts
    // every hotbar key by one and looks almost right.
    let digits = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    for (i, code) in digits.into_iter().enumerate() {
        assert_eq!(
            resolve(playing(), code, true),
            Some(KeyOutcome::SelectSlot(i)),
            "{code:?} should select slot {i}"
        );
    }
    // Digit0 is unbound in vanilla and must stay unbound — binding it to
    // slot 9 would be a tenth hotbar slot that does not exist.
    assert_eq!(resolve(playing(), KeyCode::Digit0, true), None);
    // Releasing a hotbar key does nothing (it is not a held state).
    assert_eq!(resolve(playing(), KeyCode::Digit1, false), None);
}

#[test]
fn slash_opens_chat_with_the_command_prefix_and_t_opens_it_without() {
    // The other quiet-breakage candidate. The distinction is a single bool,
    // and getting it backwards means every chat message starts with `/`
    // (or no command can ever be typed).
    assert_eq!(
        resolve(playing(), KeyCode::Slash, true),
        Some(KeyOutcome::OpenChat { command: true })
    );
    assert_eq!(
        resolve(playing(), KeyCode::KeyT, true),
        Some(KeyOutcome::OpenChat { command: false })
    );

    // …and the prefix follows the *`key.command` binding*, not the physical
    // slash key. Rebinding chat and command to other keys must carry the
    // distinction with them.
    let mut binds = Keybinds::new();
    binds.set(InputAction::Command, Binding::Key(KeyCode::Backquote));
    binds.set(InputAction::Chat, Binding::Key(KeyCode::KeyY));
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::Backquote), true, false),
        Some(KeyOutcome::OpenChat { command: true })
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyY), true, false),
        Some(KeyOutcome::OpenChat { command: false })
    );
    // The old keys stop opening chat at all.
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::Slash), true, false),
        None
    );
}

#[test]
fn an_open_container_swallows_every_gameplay_key() {
    // The precedence that matters most: while a container is up, keys must
    // not reach gameplay.
    //
    // Two gates are checked, and the second is the one that actually tests
    // the *arm*. In production `container_open` implies `!gameplay` (the
    // screen is `Container`, so `accepts_gameplay_input()` is false), which
    // means the first gate would swallow most keys through the `gate.gameplay`
    // guards even if the container arm were deleted — a vacuous test of the
    // "world" species, passing because of the input it was handed rather than
    // the code it names. The `gameplay: true` gate cannot occur in practice
    // but isolates the container arm: with it, *only* the arm's early return
    // stands between these keys and gameplay.
    for gate in [
        KeyGate {
            container_open: true,
            ..KeyGate::default()
        },
        KeyGate {
            container_open: true,
            gameplay: true,
            ..KeyGate::default()
        },
    ] {
        for (code, would_have) in default_playing_expectations() {
            // Escape and the inventory key have their own jobs on this screen,
            // and since #378 part 3 so do the nine number keys — they issue a
            // `SWAP` against the hovered slot rather than being swallowed.
            // Their own test is `the_number_keys_swap_with_the_hovered_slot`
            // below; excluding them here is not weakening this test, because
            // what it asserts is that nothing reaches *gameplay*, and
            // `ContainerSwap` is not a gameplay outcome.
            if matches!(code, KeyCode::Escape | KeyCode::KeyE)
                || hotbar_slot_for(&Keybinds::new(), code).is_some()
            {
                continue;
            }
            assert_eq!(
                resolve(gate, code, true),
                None,
                "{code:?} leaked through an open container (gate {gate:?})"
            );
            // -- negative control -----------------------------------------
            // The same key on the same table *does* resolve while playing, so
            // this test is observing the swallow and not a dead resolver.
            assert_eq!(
                resolve(playing(), code, true),
                Some(would_have),
                "control failed: {code:?} does nothing even while playing, so \
                 asserting it is swallowed proves nothing"
            );
        }
    }
}

#[test]
fn the_inventory_key_closes_a_container_and_escape_pauses_instead() {
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(gate, KeyCode::KeyE, true),
        Some(KeyOutcome::CloseContainer)
    );
    // Escape is resolved by the arm *above* the container arm, so it pauses
    // (and `Pause`'s handler closes the menu on the way). If the container
    // arm were moved above it, this would be `CloseContainer` and Escape
    // would stop reaching the pause screen from an open inventory.
    assert_eq!(resolve(gate, KeyCode::Escape, true), Some(KeyOutcome::Pause));
    // A key release while a container is open does nothing at all — but must
    // also not fall through to the gameplay arms.
    assert_eq!(resolve(gate, KeyCode::KeyE, false), None);
    assert_eq!(resolve(gate, KeyCode::KeyW, false), None);
}

/// Issue #378 part 3. Vanilla's `1`–`9` **do not** change the selected hotbar
/// slot while a container screen is open; they issue a `ContainerInput::SWAP`
/// with that hotbar index against the hovered slot
/// (`AbstractContainerScreen.checkHotbarKeyPressed`,
/// `AbstractContainerScreen.java:506-522`, and the number keys are handled in
/// `Minecraft.handleKeybinds` only when `screen == null`).
///
/// Before this they fell into the container arm's swallow: they neither
/// selected a slot — correct — nor swapped, which is the gap.
#[test]
fn the_number_keys_swap_with_the_hovered_slot_instead_of_selecting_one() {
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    let digits = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    for (i, code) in digits.into_iter().enumerate() {
        // The button number is the hotbar index, `0..=8` — vanilla passes the
        // loop counter straight through as `buttonNum`.
        assert_eq!(
            resolve(gate, code, true),
            Some(KeyOutcome::ContainerSwap { button: i as i32 }),
            "{code:?} must swap with hotbar index {i} while a container is open"
        );
        // -- the two controls -------------------------------------------
        // 1. The same key while *playing* still selects the slot. Without
        //    this, a resolver that had simply lost `SelectSlot` altogether
        //    would satisfy the assertion above.
        assert_eq!(
            resolve(playing(), code, true),
            Some(KeyOutcome::SelectSlot(i)),
            "control failed: {code:?} no longer selects a hotbar slot in the \
             world either, so this is not a container-specific route"
        );
        // 2. A key *release* is not a swap. Vanilla acts on `keyPressed`
        //    only, and a swap on both edges would fire every action twice.
        assert_eq!(
            resolve(gate, code, false),
            None,
            "{code:?} released must do nothing"
        );
    }
    // And the outcome is genuinely distinct from selecting a slot: nothing in
    // the container arm may produce `SelectSlot`, or the hotbar would jump
    // under an open inventory.
    for code in digits {
        assert!(
            !matches!(resolve(gate, code, true), Some(KeyOutcome::SelectSlot(_))),
            "{code:?} must not change the selected slot behind a screen"
        );
    }
}

/// The off-hand key's container half (issues #378 part 3 / #382).
///
/// `key.swapOffhand` defaults to `F` (`Options.java:663`, GLFW keysym 70).
/// It could not be added while `key.lodestone.toggleFly` squatted on `F`;
/// #382 deleted that binding, and this is the assertion that the freed key
/// actually reaches `Click::offhand_swap` rather than merely existing in
/// the table.
#[test]
fn the_offhand_key_swaps_with_slot_forty_while_a_container_is_open() {
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(gate, KeyCode::KeyF, true),
        Some(KeyOutcome::ContainerSwap {
            button: OFFHAND_SWAP_BUTTON
        }),
        "F must issue a SWAP against the off-hand's native slot"
    );
    // -- three controls, each for a different way this could be hollow ---
    // 1. The button number is the off-hand's, not a hotbar index. `40` is
    //    outside `0..=8`, so a resolver that had fallen through to
    //    `hotbar_slot_for` cannot satisfy this.
    assert!(
        !(0..=8).contains(&OFFHAND_SWAP_BUTTON),
        "control failed: 40 overlaps the hotbar range, so the assertion \
         above cannot distinguish the two routes"
    );
    // 2. A release is not a swap — vanilla acts on `keyPressed` only.
    assert_eq!(resolve(gate, KeyCode::KeyF, false), None);
    // 3. **The gameplay half is a different outcome, not the same one.**
    //    This line used to assert `None` with a note saying that landing
    //    #378's gameplay half should come here and change it on purpose.
    //    Issue #385 is that landing, and this is the change: with no screen
    //    open the key must resolve to the *bare action*, never to a
    //    `ContainerSwap` — a resolver that reused `ContainerSwap` here would
    //    hit-test a slot that does not exist and silently do nothing.
    assert_eq!(
        resolve(playing(), KeyCode::KeyF, true),
        Some(KeyOutcome::SwapOffhand),
        "with no screen open the off-hand key is a ServerboundPlayerAction, \
         not a container click (#385)"
    );
    assert_ne!(
        resolve(playing(), KeyCode::KeyF, true),
        resolve(gate, KeyCode::KeyF, true),
        "the two routes must not collapse into one outcome — that is the \
         conflation #385 exists to prevent"
    );
}

/// Issue #385, the gameplay half: `F` in the world **reaches the wire** as
/// `ClientAction::SwapItemWithOffhand`.
///
/// Two hops, both asserted, because either alone is satisfiable by a dead
/// chain: `resolve_key` producing the outcome proves nothing about the
/// driver, and a `NetClient` that accepts an action proves nothing about the
/// keybind. The `match` arm between them is the piece a compiler *cannot*
/// check — an arm that resolved and then did nothing would be exactly the
/// island `CLAUDE.md` §1 names.
///
/// What this deliberately does not assert is the **bytes**. Those are pinned
/// where they belong, against the jar's own declared layout, in
/// `crates/protocol/v770/tests/interaction_actions.rs`
/// (`swap_item_with_offhand_is_byte_exact_against_the_jars_enum_order`) —
/// asserting them again here off our own encoder would be
/// `decode(encode(x))` with extra steps.
#[test]
fn the_offhand_key_in_the_world_sends_the_swap_action_to_the_wire() {
    assert_eq!(
        resolve(playing(), KeyCode::KeyF, true),
        Some(KeyOutcome::SwapOffhand),
        "hop 1: the keybind must resolve"
    );

    // Hop 2: the driver's arm. `offhand_swap_action` is what it calls; the
    // loopback below is what proves an accepted action is observable.
    let (net, actions) = NetClient::loopback();
    let action = offhand_swap_action(Some(lodestone_client::GameMode::Survival))
        .expect("a survival player may swap");
    net.send_action(action);
    assert_eq!(
        actions.try_recv(),
        Ok(lodestone_model::ClientAction::SwapItemWithOffhand),
        "hop 2: the action must reach the outbound channel"
    );
    assert!(
        actions.try_recv().is_err(),
        "exactly one action per press — a doubled send would swap twice and \
         land back where it started, which looks identical to doing nothing"
    );
}

/// **The spectator control**, and the one guard vanilla actually applies
/// (`Minecraft.java:1901`, re-checked server-side at
/// `ServerGamePacketListenerImpl.java:1295`).
///
/// Watched failing: with the `Spectator` arm removed,
/// `offhand_swap_action(Spectator)` returns the action and the first
/// assertion below reports `Some(SwapItemWithOffhand)`.
///
/// The other three modes are the positive control. Without them this passes
/// just as well against a function that returns `None` unconditionally — i.e.
/// against the feature not existing at all, which is the state this issue
/// found.
#[test]
fn a_spectator_does_not_send_the_offhand_swap_and_everyone_else_does() {
    use lodestone_client::GameMode;
    assert_eq!(
        offhand_swap_action(Some(GameMode::Spectator)),
        None,
        "a spectator has no inventory to swap; vanilla declines to send"
    );
    for mode in [
        GameMode::Survival,
        GameMode::Creative,
        GameMode::Adventure,
    ] {
        assert_eq!(
            offhand_swap_action(Some(mode)),
            Some(lodestone_model::ClientAction::SwapItemWithOffhand),
            "{mode:?} must still swap — otherwise the guard above is \
             indistinguishable from the feature being absent"
        );
    }
    // Before login there is no mode. Sending is the better default: refusing
    // input until a mode arrives would make the key dead during the join
    // window, and the server re-checks anyway.
    assert_eq!(
        offhand_swap_action(None),
        Some(lodestone_model::ClientAction::SwapItemWithOffhand),
        "an unknown game mode must not read as spectator"
    );
}

// -- the drop key (`Q`), the two proven islands ------------------------
//
// `Click::drop_one`/`drop_stack`/`do_throw` (`lodestone-game`, #27) and
// `ClientAction::DropSelectedItem`/`DropSelectedItemStack` were each built,
// encoded and round-trip tested with zero producers before this. One
// binding closes both — see `InputAction::Drop`'s and `KeyOutcome::
// ContainerDrop`/`Drop`'s docs for the vanilla source this mirrors.

/// The gameplay half, mirroring `the_offhand_key_swaps_with_slot_forty_
/// while_a_container_is_open`'s shape: both resolve to a *different*
/// outcome than the container half, and `ctrl` must reach the outcome
/// unchanged from what `resolve_key` was handed.
#[test]
fn q_drops_one_while_playing_and_ctrl_q_drops_the_stack() {
    assert_eq!(
        resolve(playing(), KeyCode::KeyQ, true),
        Some(KeyOutcome::Drop { ctrl: false })
    );
    assert_eq!(
        resolve_ctrl(playing(), KeyCode::KeyQ, true),
        Some(KeyOutcome::Drop { ctrl: true })
    );
    // A release does nothing — vanilla's `keyDrop.consumeClick()` only
    // ever fires on the down edge.
    assert_eq!(resolve(playing(), KeyCode::KeyQ, false), None);
}

/// The container half — vanilla's `AbstractContainerScreen.keyPressed`
/// (`:495-501`) reached through `resolve_key`'s `container_open` arm.
#[test]
fn q_issues_a_container_drop_while_a_container_is_open() {
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(gate, KeyCode::KeyQ, true),
        Some(KeyOutcome::ContainerDrop { ctrl: false })
    );
    assert_eq!(
        resolve_ctrl(gate, KeyCode::KeyQ, true),
        Some(KeyOutcome::ContainerDrop { ctrl: true })
    );
    assert_eq!(resolve(gate, KeyCode::KeyQ, false), None);
    // -- the two-mechanisms control, same shape as the off-hand key's own --
    assert_ne!(
        resolve(playing(), KeyCode::KeyQ, true),
        resolve(gate, KeyCode::KeyQ, true),
        "the container and gameplay routes must not collapse into one \
         outcome, or the container click would fire in the world (no menu \
         to hit-test) or vice versa"
    );
}

/// `key.drop` must not have been swallowed as an unrecognised key behind
/// an open container before this landed — the negative control for the
/// island itself, run against the pre-fix shape by simulating what an
/// unbound `InputAction::Drop` would have produced.
#[test]
fn an_unbound_drop_key_is_swallowed_behind_a_container_and_dead_in_the_world() {
    let mut binds = Keybinds::new();
    binds.set(InputAction::Drop, Binding::Unbound);
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve_key(&binds, gate, Some(KeyCode::KeyQ), true, false),
        None,
        "watched failing before this test existed: with the real binding \
         still assigned, this line reported Some(ContainerDrop {{ .. }})"
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyQ), true, false),
        None
    );
}

/// Hop 1 (`resolve_key`) and hop 2 (the driver's action, factored into
/// [`drop_selected_action`] the same way `offhand_swap_action` is) for the
/// gameplay half, mirroring `the_offhand_key_in_the_world_sends_the_swap_
/// action_to_the_wire`.
#[test]
fn the_drop_key_in_the_world_sends_the_drop_action_to_the_wire() {
    assert_eq!(
        resolve(playing(), KeyCode::KeyQ, true),
        Some(KeyOutcome::Drop { ctrl: false }),
        "hop 1: the keybind must resolve"
    );

    let (net, actions) = NetClient::loopback();
    let action = drop_selected_action(Some(lodestone_client::GameMode::Survival), false)
        .expect("a survival player may drop");
    net.send_action(action.clone());
    assert_eq!(
        actions.try_recv(),
        Ok(lodestone_model::ClientAction::DropSelectedItem),
        "hop 2: the action must reach the outbound channel"
    );
    assert!(actions.try_recv().is_err(), "exactly one action per press");

    // And the `ctrl` axis selects the *other* wire action, not a flag on
    // the same one — `DropSelectedItem`/`DropSelectedItemStack` are two
    // separate `ClientAction` variants, not one with a bool field.
    let stack_action =
        drop_selected_action(Some(lodestone_client::GameMode::Survival), true)
            .expect("a survival player may drop the whole stack");
    assert_eq!(
        stack_action,
        lodestone_model::ClientAction::DropSelectedItemStack
    );
    assert_ne!(action, stack_action);
}

/// The spectator control, the one guard vanilla applies
/// (`Minecraft.java:1908`) — same shape as `a_spectator_does_not_send_
/// the_offhand_swap_and_everyone_else_does`, watched failing the same way:
/// remove the `Spectator` arm from `drop_selected_action` and the first
/// assertion below reports `Some(DropSelectedItem)`.
#[test]
fn a_spectator_does_not_send_the_drop_action_and_everyone_else_does() {
    use lodestone_client::GameMode;
    assert_eq!(
        drop_selected_action(Some(GameMode::Spectator), false),
        None,
        "a spectator has nothing to drop; vanilla declines to send"
    );
    assert_eq!(
        drop_selected_action(Some(GameMode::Spectator), true),
        None,
        "the ctrl axis must not bypass the spectator guard"
    );
    for mode in [
        GameMode::Survival,
        GameMode::Creative,
        GameMode::Adventure,
    ] {
        assert_eq!(
            drop_selected_action(Some(mode), false),
            Some(lodestone_model::ClientAction::DropSelectedItem),
            "{mode:?} must still drop — otherwise the guard above is \
             indistinguishable from the feature being absent"
        );
    }
    // Before login there is no mode; sending is the better default, same
    // reasoning as `offhand_swap_action`'s own `None` case.
    assert_eq!(
        drop_selected_action(None, false),
        Some(lodestone_model::ClientAction::DropSelectedItem),
        "an unknown game mode must not read as spectator"
    );
}

#[test]
fn an_open_chat_prompt_swallows_every_key_into_the_editor() {
    // `W` must type a `w`, not walk.
    let gate = KeyGate {
        chat_open: true,
        ..KeyGate::default()
    };
    for (code, _) in default_playing_expectations() {
        assert_eq!(
            resolve(gate, code, true),
            Some(KeyOutcome::Chat),
            "{code:?} should route to the chat editor"
        );
    }
    // Including keys nothing is bound to — the editor wants those too.
    assert_eq!(resolve(gate, KeyCode::KeyZ, true), Some(KeyOutcome::Chat));
    // And an unnameable physical key still reaches the editor, whose `text`
    // may be the only thing that identifies it.
    assert_eq!(
        resolve_key(&Keybinds::new(), gate, None, true, false),
        Some(KeyOutcome::Chat)
    );
}

#[test]
fn a_menu_screen_outranks_the_chat_prompt_and_everything_below_it() {
    let gate = KeyGate {
        menu: true,
        ..KeyGate::default()
    };
    for (code, _) in default_playing_expectations() {
        assert_eq!(resolve(gate, code, true), Some(KeyOutcome::Menu));
    }
    // Both flags set: the menu wins. This is the documented order, and a
    // swapped pair would send the edit form's keystrokes to the chat buffer.
    let both = KeyGate {
        menu: true,
        chat_open: true,
        container_open: true,
        gameplay: true,
    };
    assert_eq!(resolve(both, KeyCode::KeyW, true), Some(KeyOutcome::Menu));
    assert_eq!(resolve(both, KeyCode::Escape, true), Some(KeyOutcome::Menu));
    // Chat outranks the container and gameplay in turn.
    let chat_over_container = KeyGate {
        chat_open: true,
        container_open: true,
        gameplay: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(chat_over_container, KeyCode::KeyE, true),
        Some(KeyOutcome::Chat)
    );
}

#[test]
fn gameplay_bindings_are_inert_when_no_screen_accepts_gameplay_input() {
    // Every flag false: no menu, no chat, no container, and not playing —
    // e.g. the loading screen. Only the two ungated arms may still fire.
    let gate = KeyGate::default();
    for (code, _) in default_playing_expectations() {
        let got = resolve(gate, code, true);
        match code {
            // `Pause` is intentionally ungated: Escape must work on the
            // loading and error screens, which is how it did before.
            KeyCode::Escape => assert_eq!(got, Some(KeyOutcome::Pause)),
            // So is the debug overlay — it is an instrument, and gating it
            // on `Playing` would make it unavailable exactly when a stuck
            // connection is the thing being debugged.
            KeyCode::F3 => assert_eq!(got, Some(KeyOutcome::ToggleDebugOverlay)),
            _ => assert_eq!(got, None, "{code:?} fired outside gameplay"),
        }
    }
}

#[test]
fn held_bindings_report_both_edges_and_one_shot_bindings_only_the_press() {
    // Movement and the player list are held states; the rest are one-shots.
    // A one-shot that fired on release would double-toggle perspective, and
    // a held binding gated on `pressed` would stick on forever.
    assert_eq!(
        resolve(playing(), KeyCode::KeyW, false),
        Some(KeyOutcome::Movement(Action::Forward, false))
    );
    assert_eq!(
        resolve(playing(), KeyCode::Tab, false),
        Some(KeyOutcome::PlayerList(false))
    );
    for one_shot in [
        KeyCode::KeyE,
        KeyCode::KeyT,
        KeyCode::Slash,
        KeyCode::KeyF,
        KeyCode::F5,
        KeyCode::F3,
        KeyCode::Escape,
        KeyCode::Digit1,
    ] {
        assert_eq!(
            resolve(playing(), one_shot, false),
            None,
            "{one_shot:?} must not fire on release"
        );
    }
}

#[test]
fn a_rebind_moves_the_behaviour_to_the_new_key_and_off_the_old_one() {
    let mut binds = Keybinds::new();
    binds.set(InputAction::Inventory, Binding::Key(KeyCode::KeyI));
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyI), true, false),
        Some(KeyOutcome::OpenContainer)
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyE), true, false),
        None,
        "the old default must stop opening the inventory"
    );
    // …and the rebound key also closes the container, because both sites ask
    // the table rather than naming `KeyE`.
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve_key(&binds, gate, Some(KeyCode::KeyI), true, false),
        Some(KeyOutcome::CloseContainer)
    );
    assert_eq!(
        resolve_key(&binds, gate, Some(KeyCode::KeyE), true, false),
        None
    );
}

#[test]
fn unbinding_an_action_disables_it_without_disturbing_the_rest() {
    let mut binds = Keybinds::new();
    binds.set(InputAction::Jump, Binding::Unbound);
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::Space), true, false),
        None
    );
    // The neighbouring arms are untouched.
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyW), true, false),
        Some(KeyOutcome::Movement(Action::Forward, true))
    );
}

#[test]
fn attack_and_use_are_keyboard_dispatchable_once_rebound_off_the_mouse() {
    // Under the defaults these arms are dormant, because attack and use are
    // mouse-bound — assert that, so "it works" cannot be an accident of the
    // key path firing too.
    assert_eq!(resolve(playing(), KeyCode::KeyR, true), None);

    let mut binds = Keybinds::new();
    binds.set(InputAction::Attack, Binding::Key(KeyCode::KeyR));
    binds.set(InputAction::Use, Binding::Key(KeyCode::KeyV));
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyR), true, false),
        Some(KeyOutcome::Attack(true))
    );
    // Hold-to-dig: the release edge must arrive, or mining never stops.
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyR), false, false),
        Some(KeyOutcome::Attack(false))
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyV), true, false),
        Some(KeyOutcome::Use(true))
    );
    // The release edge must arrive too, or `ReleaseUseItem` never sends —
    // the exact bug this test's sibling assertions exist to catch (a bow
    // or shield cannot complete a use without it).
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyV), false, false),
        Some(KeyOutcome::Use(false))
    );
}

#[test]
fn the_mouse_path_resolves_the_default_attack_and_use_buttons() {
    // The mouse half of dispatch, which is why `Binding` is not `KeyCode`.
    let binds = Keybinds::new();
    assert_eq!(
        mouse_action_for(&binds, MouseButton::Left),
        Some(InputAction::Attack)
    );
    assert_eq!(
        mouse_action_for(&binds, MouseButton::Right),
        Some(InputAction::Use)
    );
    // Middle **is** a gameplay binding now: `key.pickItem` defaults to
    // `Type.MOUSE, 2` (`Options.java:669`), so it is the primary route for
    // pick-item rather than a rebound one. This assertion previously read
    // `None`, which was correct only while pick-item did not exist — the
    // premise went stale when the binding landed, not the code.
    assert_eq!(
        mouse_action_for(&binds, MouseButton::Middle),
        Some(InputAction::PickItem)
    );

    // Swapping the two buttons is a supported rebind.
    let mut swapped = binds;
    swapped.set(InputAction::Attack, Binding::Mouse(MouseButton::Right));
    swapped.set(InputAction::Use, Binding::Mouse(MouseButton::Left));
    assert_eq!(
        mouse_action_for(&swapped, MouseButton::Right),
        Some(InputAction::Attack)
    );
    assert_eq!(
        mouse_action_for(&swapped, MouseButton::Left),
        Some(InputAction::Use)
    );
}

#[test]
fn a_movement_action_can_be_driven_from_a_mouse_button() {
    // Not something vanilla offers, but it falls out of `Binding` covering
    // both input kinds — and the mouse handler routes it, so it is not an
    // island.
    let mut binds = Keybinds::new();
    binds.set(InputAction::Jump, Binding::Mouse(MouseButton::Middle));
    let action = mouse_action_for(&binds, MouseButton::Middle);
    assert_eq!(action, Some(InputAction::Jump));
    assert_eq!(action.and_then(InputAction::movement), Some(Action::Jump));
}

#[test]
fn an_unnameable_physical_key_is_ignored_by_the_binding_chain() {
    // `PhysicalKey::Unidentified` reaches the menu and chat arms (tested
    // above) but must not match any binding — there is nothing to match on.
    assert_eq!(
        resolve_key(&Keybinds::new(), playing(), None, true, false),
        None
    );
}

/// **Pressing Play Selected World reaches a running integrated server**
/// (issue #287).
///
/// This is the anti-island gate for singleplayer, and it is the only test
/// anywhere that crosses *every* seam of it in one go: the registry's
/// serverbound lookup, the boxed `ServerProtocol`, the net thread, the
/// in-memory duplex, `IntegratedServer`'s serving loop, the real v770 wire
/// format, and the client's decode — ending at a `NetUpdate` the shell's own
/// frame loop consumes.
///
/// The button half is `menu::nav`'s
/// `play_selected_world_asks_the_app_to_start_singleplayer`, which asserts the
/// click produces `MenuAction::Singleplayer(None)`; `apply_menu_action`'s arm
/// between the two is a single call this file can be read for. The seam
/// *without* the shell is `crates/protocol/v770/tests/singleplayer_seam.rs`.
///
/// **Chunks, not just login, is the load-bearing assertion.** Login is five
/// `ServerProtocol` methods with no trait defaults, so it cannot silently fall
/// through the box; terrain is where a half-wired server shows up, and it is
/// also the only thing here that proves the *world* exists rather than just a
/// handshake. A world that logs in and streams nothing is precisely the shape
/// of the chunk-blackout failures `CLAUDE.md` records.
///
/// `view_radius = 0` is one column: the bundled generator costs ~12 ms per
/// column, and one is enough to prove terrain crosses the wire (its *content*
/// is verified block-for-block in `lodestone-server`'s own tests, against a
/// JVM oracle rather than against our encoder).
#[test]
fn pressing_play_reaches_a_running_integrated_server() {
    let protocol = Config::default().protocol;
    let seed = crate::menu::world_select::BUNDLED_WORLD.seed;
    // `None` world dir: this gate is about the seam reaching a running server,
    // not about persistence (issue #468 gates that in
    // `tests/singleplayer_persistence.rs`), and an in-memory world leaves
    // nothing in the developer's real data directory.
    let net = match launch_singleplayer(protocol, 0, None, seed, None) {
        Ok(net) => net,
        Err(e) => {
            // A build with no hostable family must *report*, which is the
            // `--no-default-features` contract. In the default build (`live`)
            // this is a failure, not a skip.
            assert!(
                !cfg!(feature = "live"),
                "the default build must be able to host singleplayer: {e}"
            );
            assert!(matches!(e, LaunchError::NoVersionFamily { .. }));
            return;
        }
    };

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut logged_in = false;
    let mut chunks = 0usize;
    let mut errors: Vec<String> = Vec::new();
    while Instant::now() < deadline && !(logged_in && chunks > 0) {
        for update in net.poll() {
            match update {
                crate::net::NetUpdate::LoggedIn { .. } => logged_in = true,
                crate::net::NetUpdate::Chunk { .. } => chunks += 1,
                // Collected rather than ignored: an `Error`/`Disconnected`
                // here is the actual diagnosis, and without it the failure
                // message would only say "timed out".
                crate::net::NetUpdate::Error(e) => errors.push(e),
                crate::net::NetUpdate::Disconnected(reason) => {
                    errors.push(format!("disconnected: {reason:?}"));
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        logged_in,
        "the client never logged in to the integrated server; errors: {errors:?}"
    );
    assert!(
        chunks > 0,
        "logged in but no terrain arrived — the server is serving nothing; \
         errors: {errors:?}"
    );
    assert!(
        errors.is_empty(),
        "the session reported errors while starting: {errors:?}"
    );
}

/// **Issue #189's queued patch, exercised through production code.**
///
/// `crate::menu::social::entries_from_tablist` was pure and unit-tested
/// with **no caller anywhere in the shell** — `docs/social-interactions.md`'s
/// own "Decorative" section. This does not call it a second time by hand
/// (that would just be the existing unit test again, which proves
/// nothing about production); it drives the actual chain: a real
/// `WindowApp`, a `SessionTabList` folded through the same `NetIngest`
/// schedule the net thread runs, and `drive_ui_from_session` itself —
/// the method `redraw()` calls every frame.
#[test]
fn drive_ui_from_session_refreshes_the_social_roster_from_the_real_tab_list() {
    use crate::net::NetUpdate;
    use lodestone_client::{ClientEvent, GameMode, PlayerListEntry};
    use uuid::Uuid;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    // `drive_ui_from_session`'s refresh is guarded on `SessionPhase::Connected`
    // — reach it the same way `sim/tests.rs`'s own tab-list test does,
    // through a real `NetUpdate`, not by poking a private field.
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    assert_eq!(
        app.sim.session_phase(),
        crate::sim::SessionPhase::Connected,
        "precondition: the refresh guard reads this, so it must actually be live"
    );

    let alice = Uuid::from_u128(1);
    let bob = Uuid::from_u128(2);
    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::PlayerListUpdate {
            entries: vec![
                PlayerListEntry {
                    uuid: bob,
                    name: Some("Bob".into()),
                    game_mode: Some(GameMode::Creative),
                    latency: Some(20),
                    display_name: None,
                    listed: Some(true),
                },
                PlayerListEntry {
                    uuid: alice,
                    name: Some("Alice".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(10),
                    display_name: None,
                    listed: Some(true),
                },
            ],
        });

    // Precondition: nothing has refreshed the screen model yet — proves the
    // assertion below actually exercises `drive_ui_from_session`, not some
    // earlier call this test forgot about.
    assert!(
        app.nav.social().entries().is_empty(),
        "precondition: the roster must still be empty before the real call runs"
    );

    app.drive_ui_from_session();

    let names: Vec<&str> = app
        .nav
        .social()
        .entries()
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["Alice", "Bob"],
        "the roster must reflect the real folded tab list, in vanilla's display order"
    );
}

/// Issue #192's last hop, exercised through production code exactly like
/// the social-roster test above: `menu::UiState::show_credits` and
/// `net::NetUpdate::WinGame` both already existed, individually tested,
/// with **nothing calling either from the other** — the credits screen was
/// reachable only from a test, and `WinGame` only reached a channel no
/// one drained into UI state. This drives the real chain end to end: a
/// real `WindowApp`, a real `NetUpdate::WinGame` through the loopback
/// feed (the same seam `NetClient::run`'s background thread publishes
/// into in production, once `net::forward` — separately proven by
/// `forward_translates_win_game_into_the_credits_signal` — turns the real
/// decoded `ClientEvent::WinGame` into it), `Sim::poll_net`'s real
/// `WinGame` arm, and `drive_ui_from_session` itself.
#[test]
fn drive_ui_from_session_opens_credits_on_the_real_win_game_event() {
    use crate::net::NetUpdate;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    // Reach a live-gameplay screen the same way `on_credits` (`menu/
    // nav.rs`'s own test helper) does — `show_credits` only leaves from
    // `Playing | Chat | Container | Paused`, matching `die`'s guard.
    app.ui.enter_dev_world();
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Playing,
        "precondition: must be on a live-gameplay screen before WinGame arrives"
    );
    assert!(
        !app.sim.has_won(),
        "precondition: nothing has signalled a win yet"
    );

    feed.send(NetUpdate::WinGame).unwrap();
    app.sim.step(1.0 / 20.0);
    assert!(
        app.sim.has_won(),
        "Sim::poll_net's real WinGame arm must latch the win"
    );
    // Precondition restated after the poll but before the real call this
    // test exercises, so the assertion below cannot be explained by
    // something upstream having already moved the screen.
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Playing,
        "precondition: drive_ui_from_session has not run yet"
    );

    app.drive_ui_from_session();

    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Credits,
        "the real WIN_GAME event (GAME_EVENT code 4, ClientPacketListener.java:1548) \
         must open the credits screen"
    );
}

/// Live gate for issue #25: `ShellWeatherProbe::precipitation` must reach
/// a real per-column snow/rain decision now that the biome-climate lane
/// is wired, not the `Rain` it answered unconditionally before this
/// session (`app.rs`'s own history — see the #25 report).
///
/// Connects directly through `ClientBuilder`, bypassing `NetClient`'s
/// background thread so the raw event stream can be read here: the real
/// `ClientEvent::BiomeClimates` is captured off it and folded into a
/// `BiomeClimateCell` **by hand, with the same call** `net::forward`'s
/// arm makes — proving the fold, not merely trusting it — while every
/// other event is drained so the driver's bounded channel never blocks.
/// Mirrors `net::tests::live_entity_light_at_distinguishes_loaded_from_unloaded`'s
/// shape.
///
/// The expected precipitation per sampled column is computed **here**,
/// independently of both `ShellWeatherProbe` and `lodestone_render::
/// weather` — the raw climate is pulled straight off the `BiomeClimateCell`
/// and vanilla's own threshold is applied by hand, quoted from the
/// decompiled source rather than from this crate's constant:
/// `Biome.java:176`, `return this.getTemperature(pos, seaLevel) >= 0.15F;`
/// (`warmEnoughToRain`, called from `getPrecipitationAt` at `:108`). A
/// wrong threshold in either implementation would show up as a mismatch
/// against this independently-computed expectation rather than agreeing
/// with itself — the `decode(encode(x)) == x` trap `CLAUDE.md` warns
/// about, avoided by never calling `precipitation_for_temperature`/
/// `height_adjusted_temperature` from this test.
///
/// ```text
/// cargo test -p lodestone-shell --features live --lib \
///     app::tests::live_precipitation_matches_vanillas_own_threshold_for_real_biomes \
///     -- --ignored --nocapture
/// ```
#[cfg(feature = "live")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the lodestone-survival server on 127.0.0.1:25565"]
async fn live_precipitation_matches_vanillas_own_threshold_for_real_biomes() {
    use crate::net::BiomeClimateCell;
    use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
    use lodestone_render::WeatherProbe as _;
    use lodestone_testsupport::{poll_until, unique_username};

    let user = unique_username();
    let protocol = 776; // vanilla 26.2 — the `live` feature's compiled-in family
    let adapter = lodestone_registry::adapter_for_protocol(protocol)
        .expect("the `live` feature compiles a family in for protocol 776");
    let (handle, mut events) = ClientBuilder::new(
        ServerAddress {
            host: "127.0.0.1".into(),
            port: 25565,
        },
        LoginProfile {
            username: user.clone(),
            uuid: uuid::Uuid::new_v4(),
        },
        adapter,
    )
    .connect()
    .await
    .expect("connect to lodestone-survival on 127.0.0.1:25565");

    let climates = Arc::new(BiomeClimateCell::default());
    let climates_thread = Arc::clone(&climates);
    let drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let lodestone_model::ClientEvent::BiomeClimates {
                temperatures,
                downfall,
                has_precipitation,
            } = event
            {
                // The exact fold `net::forward`'s `BiomeClimates` arm
                // makes — called here by hand since this test bypasses
                // `forward` entirely to read the raw stream.
                climates_thread.apply(&temperatures, &downfall, &has_precipitation);
            }
        }
    });

    assert!(
        poll_until(
            Duration::from_secs(30),
            Duration::from_millis(100),
            || async {
                handle
                    .players()
                    .into_iter()
                    .find(|p| p.name.as_deref() == Some(user.as_str()))
            }
        )
        .await
        .is_some(),
        "player {user} never reached Play on the oracle"
    );

    let dims = poll_until(
        Duration::from_secs(10),
        Duration::from_millis(100),
        || async { handle.world_dimensions() },
    )
    .await
    .expect("world dimensions never arrived");

    let loaded = poll_until(
        Duration::from_secs(15),
        Duration::from_millis(200),
        || async {
            let chunks = handle.loaded_chunks();
            if chunks.is_empty() { None } else { Some(chunks) }
        },
    )
    .await
    .expect("no chunks streamed in within 15s of login");

    // The registry (and with it `BiomeClimates`) lands at `Login`, ahead
    // of chunk data, but poll rather than assume the ordering: this test
    // cares about the fold having happened, not about racing it.
    assert!(
        poll_until(Duration::from_secs(10), Duration::from_millis(100), || {
            let climates = Arc::clone(&climates);
            async move { climates.get(0).map(|_| ()) }
        })
        .await
        .is_some(),
        "ClientEvent::BiomeClimates never arrived — the climate table is still empty"
    );

    let handle = Arc::new(handle);
    let probe = ShellWeatherProbe {
        light: 1.0,
        sky_visible: true,
        handle: Some(Arc::clone(&handle)),
        biome_climates: Some(Arc::clone(&climates)),
    };

    // Sample a real column in the middle of a loaded chunk, at mid-build-
    // height. `checked` and `snow_seen`/`rain_seen` are reported in the
    // panic message so a failure names the real biome and climate
    // involved, not just "mismatch".
    let mut checked = 0usize;
    let mut mismatches: Vec<String> = Vec::new();
    for chunk in loaded.iter().take(16) {
        let y = dims.min_y + (dims.height as i32 / 2);
        let block_x = chunk.x * 16 + 8;
        let block_z = chunk.z * 16 + 8;
        let base_si = dims.min_y.div_euclid(16);
        let si = y.div_euclid(16) - base_si;
        if si < 0 || (si as usize) >= dims.section_count() {
            continue;
        }
        let Some(section) = handle.section_at(*chunk, si as usize) else {
            continue;
        };
        let biome = section.biome_at_block(8, y.rem_euclid(16) as usize, 8);
        let Some(climate) = climates.get(usize::try_from(biome).unwrap_or(usize::MAX)) else {
            continue;
        };
        let (Some(temperature), Some(has_precipitation)) =
            (climate.temperature, climate.has_precipitation)
        else {
            continue;
        };
        checked += 1;

        // Independent re-derivation, not a call to `lodestone_render::
        // weather`: vanilla's own height falloff
        // (`Biome.getHeightAdjustedTemperature`, `Biome.java:112-121`)
        // and its own rain/snow threshold (`Biome.java:176`, `0.15F`).
        let above = (y - crate::worldgen::SEA_LEVEL) as f32;
        let adjusted = if above > 0.0 {
            temperature - above * 0.05 / 40.0
        } else {
            temperature
        };
        let expected = if !has_precipitation {
            lodestone_render::Precipitation::None
        } else if adjusted >= 0.15 {
            lodestone_render::Precipitation::Rain
        } else {
            lodestone_render::Precipitation::Snow
        };

        let actual = probe.precipitation(block_x, y, block_z);
        println!(
            "chunk {chunk:?} biome {biome} temperature={temperature} \
             has_precipitation={has_precipitation} adjusted={adjusted} -> {expected:?}"
        );
        if actual != expected {
            mismatches.push(format!(
                "chunk {chunk:?} biome {biome} temperature={temperature} \
                 has_precipitation={has_precipitation} adjusted={adjusted}: \
                 expected {expected:?}, probe returned {actual:?}"
            ));
        }
    }

    assert!(
        checked > 0,
        "no loaded column resolved a section + biome + climate — the wiring \
         chain (section_at → biome_at_block → BiomeClimateCell) never \
         produced real data to check against"
    );
    assert!(
        mismatches.is_empty(),
        "{}/{checked} sampled columns disagreed with vanilla's own threshold: \
         {mismatches:#?}",
        mismatches.len()
    );

    drain.abort();
}

/// **Issue #436's `SessionRecipeBookSettings` island, closed through
/// production code.**
///
/// `RECIPE_BOOK_SETTINGS` (76) decoded and folded as of `fd53995` and
/// **nothing read it**: the recipe-book panel started closed and unfiltered
/// on every join no matter what the server said. This does not call
/// `RecipeBookSettings::for_type` a second time by hand — that would be the
/// existing unit test again, which proves nothing about production. It drives
/// the real chain: a real `WindowApp`, a real `ClientEvent` folded through the
/// same `NetIngest` schedule the net thread runs, and
/// `drive_ui_from_session` itself — the method `redraw()` calls every frame.
///
/// The `open` bit is the pixel-visible one: `RecipePanelState::open` is what
/// `recipe_panel_geometry` turns into the panel body's vertices, gated by
/// `an_open_panel_covers_its_own_screen_rect` / the closed-panel control in
/// `recipe_book_wiring.rs`.
#[test]
fn drive_ui_from_session_restores_the_recipe_book_panel_the_server_reported() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_model::event::RecipeBookTypeSettings;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);

    // The restore only runs with a recipe-book-bearing menu on screen: the
    // player inventory's own 2x2 grid makes `recipe_book_type_for` answer
    // `Crafting`. Reach it the way a player does.
    app.ui.enter_dev_world();
    app.ui.open_container();

    // Precondition, and it is load-bearing: an unreported record is all-false,
    // which is indistinguishable from "the server wants it closed". If the
    // panel were somehow already open, the assertion below would pass without
    // the restore ever running.
    assert!(
        !app.recipe_panel.open,
        "precondition: the panel must start closed — that is the defect being fixed"
    );
    app.drive_ui_from_session();
    assert!(
        !app.recipe_panel.open,
        "control: with nothing reported, the restore must NOT fire — otherwise \
         this gate cannot tell a real restore from the default it replaces"
    );

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookSettingsChanged {
            crafting: RecipeBookTypeSettings { open: true, filtering: true },
            furnace: RecipeBookTypeSettings::default(),
            blast_furnace: RecipeBookTypeSettings::default(),
            smoker: RecipeBookTypeSettings::default(),
        });

    app.drive_ui_from_session();

    assert!(
        app.recipe_panel.open,
        "the crafting book's reported `open` must reach the panel the draw reads"
    );
    assert!(
        app.recipe_panel.filtering,
        "and so must `filtering` — the All/Craftable state"
    );

    // The latch: a user who closes the panel must not have it reopened on the
    // very next frame by the same reported settings.
    app.recipe_panel.open = false;
    app.drive_ui_from_session();
    assert!(
        !app.recipe_panel.open,
        "the restore is once per book type, not every frame — otherwise it \
         would fight the user's own clicks"
    );
}

/// The **negative control** for the gate above, run and observed: the furnace
/// book's settings must not restore into a crafting panel.
///
/// # What this control can and cannot see — measured, not assumed
///
/// Neutering `for_type(book_type)` to `settings.furnace` fails **both** this
/// test and the positive one above (observed). So the pair really does pin the
/// per-type read in that direction.
///
/// It does **not** catch a restore hardcoded to `settings.crafting`: that was
/// tried, and both tests stayed green. The reason is a property of the
/// harness, not of the assertions — `active_container_menu` here resolves to
/// the *player inventory*, whose 2×2 grid makes `recipe_book_type_for` answer
/// `Crafting`, so `crafting` **is** the correct field for every scenario this
/// harness can construct. Putting a furnace on screen needs a server-opened
/// menu (`Sim::open_menu`), which this loopback feed has no route to.
///
/// Recorded rather than quietly left as a gap: a control whose premise is
/// false fails in the safe-looking direction, and the way to find that out is
/// to run the neuter and watch it *not* fire. Whoever gains a furnace-menu
/// harness should extend this test rather than write a third one.
#[test]
fn a_crafting_panel_does_not_restore_the_furnace_books_settings() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_model::event::RecipeBookTypeSettings;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();
    app.ui.open_container();

    // Only the *furnace* book is open, and it is a different book than the
    // player-inventory crafting grid on screen.
    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookSettingsChanged {
            crafting: RecipeBookTypeSettings::default(),
            furnace: RecipeBookTypeSettings { open: true, filtering: true },
            blast_furnace: RecipeBookTypeSettings::default(),
            smoker: RecipeBookTypeSettings::default(),
        });

    app.drive_ui_from_session();

    assert!(
        !app.recipe_panel.open,
        "the furnace book's `open` must NOT open the crafting panel — the \
         restore has to read `for_type(book_type)`, not the first field"
    );
    assert!(
        !app.recipe_panel.filtering,
        "same for `filtering`"
    );
}

/// **Issue #436's `SessionGameRules` island, closed through production code.**
///
/// `doImmediateRespawn` is the most user-visible game rule there is: vanilla
/// never puts the death screen up at all when it is on. `SessionGameRules`
/// was folded, reset on quit-to-title and gated through the real
/// `SharedState::apply` path with **no reader anywhere in the shell**, so the
/// rule did nothing.
///
/// Drives the real chain: a real `WindowApp`, a real `NetUpdate::Death`
/// through the loopback feed (`Sim::poll_net`'s own arm, which sets the `Dead`
/// marker), a real `ClientEvent::GameRulesChanged` through the same
/// `NetIngest` schedule the net thread runs, and `drive_ui_from_session`
/// itself — the method `redraw()` calls every frame.
#[test]
fn immediate_respawn_skips_the_death_screen_entirely() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::GameRulesChanged {
            values: vec![(
                "immediate_respawn".parse().expect("valid identifier"),
                "true".into(),
            )],
        });
    assert_eq!(
        app.sim.game_rules().immediate_respawn(),
        Some(true),
        "precondition: the rule must actually have folded, or this gate is \
         measuring the default and not the rule"
    );

    feed.send(NetUpdate::Death { message: "you died".into() }).unwrap();
    app.sim.step(1.0 / 20.0);
    assert!(
        app.sim.is_dead(),
        "precondition: the death must have landed, or 'no death screen' is vacuous"
    );

    app.drive_ui_from_session();

    assert!(
        !app.ui.is_death(),
        "with doImmediateRespawn on, the death screen must never appear — not \
         'appear and close next frame', which would flash it for a frame"
    );
    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::Death,
        "and the screen state must not be Death by any other route"
    );
}

/// **The negative control, run and observed**: the *same* death with the rule
/// off must still raise the death screen.
///
/// Without this, `immediate_respawn_skips_the_death_screen_entirely` is
/// satisfied by a client that never shows a death screen at all — which is
/// exactly the state a broken `is_dead` or a broken loopback feed would
/// produce, and it would read as a pass.
#[test]
fn without_the_rule_the_same_death_still_raises_the_death_screen() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();

    // Explicitly `false`, not merely absent: `Some(false)` and `None` take
    // different branches in `immediate_respawn()`, and the shipped behaviour
    // must be identical for both.
    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::GameRulesChanged {
            values: vec![(
                "immediate_respawn".parse().expect("valid identifier"),
                "false".into(),
            )],
        });
    assert_eq!(app.sim.game_rules().immediate_respawn(), Some(false));

    feed.send(NetUpdate::Death { message: "you died".into() }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.drive_ui_from_session();

    assert!(
        app.ui.is_death(),
        "with the rule off, the death screen must still appear — this is what \
         proves the gate above is measuring the rule and not a client that \
         never shows the screen"
    );
}

/// **Issue #47's last hop, closed: a real right-click opens the command block
/// edit screen.**
///
/// `Screen::CommandBlockEdit`, `command_block::CommandBlockState` and
/// `render::command_block_frame` landed in `c76510b` real and unit-tested, and
/// `UiState::open_command_block`/`MenuNav::open_command_block` had **zero
/// production callers** — the screen was reachable only from a test. Issue
/// #436's ledger entry.
///
/// This drives the production path: a real `WindowApp`, a real command block
/// written into the real `ChunkWorld`, a real `RayTarget` (what the crosshair
/// raycast writes), and `WindowApp::try_use` — the method the `KeyOutcome::
/// Use(true)` arm now calls instead of `Sim::use_item`.
#[test]
fn right_clicking_a_command_block_opens_the_edit_screen() {
    use crate::raycast::RayHit;

    let Some(state_id) = (0u32..40_000).find(|id| {
        lodestone_data::block_states::block_type_name(*id)
            .is_some_and(|n| n == "minecraft:command_block")
    }) else {
        return;
    };

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.enter_dev_world();

    // A real command block in the real store the accessor reads.
    let block = [8, 64, 8];
    let world = app.sim.chunk_world();
    {
        let mut w = world.write();
        crate::sim::write_predicted_block(&mut *w, block, state_id);
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

    let Some(stone) = (0u32..4096).find(|id| {
        lodestone_data::block_states::block_type_name(*id).is_some_and(|n| n == "minecraft:stone")
    }) else {
        return;
    };

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.enter_dev_world();

    let block = [8, 64, 8];
    let world = app.sim.chunk_world();
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

/// **Issue #474's second half: a click on the command block screen reaches a
/// row.**
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
/// `AbstractCommandBlockEditScreen.java`'s own arithmetic — `:71` places Done
/// at `width/2 - 4 - 150`, `:74` places Cancel at `width/2 + 4`, both `150x20`
/// at `height/4 + 120 + 12`, and `:50` puts the mode row at `width/2 - 154`,
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

    // `AbstractCommandBlockEditScreen.java:71,74` — the footer anchor is
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
