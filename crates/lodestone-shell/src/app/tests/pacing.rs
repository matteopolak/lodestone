//! Tests for fixed-step simulation and focused/unfocused frame pacing.

use super::*;

#[test]
fn a_long_stall_is_clamped_not_replayed() {
    // The reported bug: tab out for a minute, tab back in, and the client
    // tries to run every tick it missed. Sixty seconds is 1200 ticks.
    let stall = Duration::from_secs(60);
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    let step = pacer.begin_frame(t0 + stall, None);

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
    // loud ("if the value changes, reconcile the two caps"). This test
    // documents that reconciliation: §4.1(c) left one accumulator
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
    let step = pacer.begin_frame(t0 + frame, None);
    assert!(
        (step.dt - frame.as_secs_f64()).abs() < 1e-9,
        "60 fps frame was altered: {}",
        step.dt
    );

    // And a 4 fps frame — the rate an occluded window degrades to — must
    // still deliver all 250 ms, i.e. five whole ticks, not be truncated.
    let mut pacer = FramePacer::new(t0);
    let step = pacer.begin_frame(t0 + Duration::from_millis(250), None);
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
        let step = pacer.begin_frame(t0 + Duration::from_secs_f64(f64::from(i) / 120.0), None);
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

/// Owner report: "the block animations seem too fast in general". A previous
/// pass proved the animation *sampling* logic exact (`SpriteAnimation::sample`,
/// the built `water_still`/`water_flow` timelines) and traced the tick source
/// to `Sim::tick_count()`, which `RenderState::update_animation` is fed
/// verbatim from both live call sites (`app/redraw.rs`, `app/runners.rs`). So
/// the remaining question is a measurement, not a re-read: does
/// `Sim::tick_count()` actually advance at 20/s under the **focused,
/// uncapped** loop shape real play uses — `redraw()`'s own path, through
/// [`FramePacer::begin_frame`] with `target_fps: None` — rather than only the
/// unfocused/capped shape [`an_unfocused_window_keeps_ticking_and_presents_at_thirty_fps`]
/// already covers.
///
/// Driven at 240 Hz (12x the tick rate — a real, uncapped display can run
/// this fast) for 3 real seconds. **Two hypotheses, both computed from
/// outside constants, not read back from the code under test:**
/// - correct: real elapsed time drives the accumulator, so ticks ≈
///   `3.0 * 20 = 60` regardless of loop rate.
/// - wrong (a `dt` that tracked the loop's own iteration period instead of
///   real elapsed time, or a `FrameClock` double-stepped by a second driver):
///   would land far from 60 — e.g. exactly `720` if each of the 720
///   iterations banked a full tick unconditionally, or `360` if a stray
///   second `begin_frame`/`step` call doubled every real tick.
///
/// This is the discriminating input the `19..=20`-over-one-second unfocused
/// gate cannot be, alone: that gate's loop (120 Hz) is *closer* to the tick
/// rate, so a 2x-too-fast bug would still land inside its own tolerance band
/// at the one-second horizon. Three seconds at 240 Hz separates "correct" (60)
/// from "2x" (120) and from "6x, i.e. one tick per iteration at 240 Hz banked
/// as if it were the loop's own 1/20 s" (720) by wide margins.
#[test]
fn a_focused_uncapped_loop_advances_ticks_at_twenty_per_second() {
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    // Focused (the `FramePacer::new` default) and uncapped: `target_fps: None`.
    let mut sim = pacing_sim();
    let loop_hz = 240.0_f64;
    let real_seconds = 3.0_f64;
    let iterations = (loop_hz * real_seconds) as u32;
    let mut ticks = 0u64;
    for i in 1..=iterations {
        let now = t0 + Duration::from_secs_f64(f64::from(i) / loop_hz);
        let step = pacer.begin_frame(now, None);
        ticks += ticks_for(&mut sim, step.dt);
    }

    let expected = (real_seconds * 20.0).round() as i64;
    assert!(
        ((expected - 1)..=(expected + 1)).contains(&(ticks as i64)),
        "a focused, uncapped 240 Hz loop over {real_seconds} real seconds must \
         advance ~{expected} ticks (20/s), not {ticks} — {:.2}x real time",
        ticks as f64 / expected as f64,
    );

    // Control, computed and asserted rather than merely described: the wrong
    // hypothesis this test would have to fail to catch. If `dt` had tracked
    // the loop's own period instead of real elapsed time, every one of the
    // `iterations` calls would bank a full `TICK_SECS` (240 Hz's own period
    // exceeds one tick, so each iteration would look like a whole tick owed),
    // landing near `iterations`, not `expected` — the two must be far apart
    // for this input to mean anything.
    assert!(
        i64::from(iterations) - expected > 500,
        "chosen input must separate the two hypotheses widely; iterations={iterations} expected={expected}"
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
        if pacer.begin_frame(now, None).render {
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
    assert!(pacer.begin_frame(resume, None).render, "the first frame back draws");

    let mut after = 0;
    for i in 1..=60u32 {
        if pacer
            .begin_frame(resume + Duration::from_secs_f64(f64::from(i) / 120.0), None)
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
        let step = pacer.begin_frame(t0 + Duration::from_secs_f64(f64::from(i) / 120.0), None);
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
    let step = pacer.begin_frame(t0 + Duration::from_secs(2), None);
    assert!(step.render, "clearing occlusion must restore presentation");
}

#[test]
fn focus_selects_the_control_flow_without_ever_stopping_the_loop() {
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    assert!(matches!(pacer.control_flow(t0, None), ControlFlow::Poll));
    assert!(pacer.focused());

    pacer.set_focused(false);
    match pacer.control_flow(t0, None) {
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

#[test]
fn a_focused_capped_window_is_paced_by_a_wait_not_a_spin() {
    // The busy-wait failure mode the brief names explicitly: a `framerateLimit`
    // below the refresh rate must not turn into `ControlFlow::Poll` calling
    // `begin_frame` every iteration only to find `render == false` — that is a
    // spin loop wearing a frame cap's clothes. `control_flow` must instead
    // report a real `WaitUntil` deadline once a cap is in effect.
    let t0 = Instant::now();
    let pacer = FramePacer::new(t0);
    assert!(
        matches!(pacer.control_flow(t0, None), ControlFlow::Poll),
        "uncapped and focused: vsync paces us, unchanged from before this option"
    );
    match pacer.control_flow(t0, Some(30)) {
        ControlFlow::WaitUntil(_) => {}
        other => panic!("a focused window with a real cap must sleep, not poll: {other:?}"),
    }
}

#[test]
fn a_focused_cap_presents_at_the_capped_rate_not_every_iteration() {
    // Drive a 120 Hz loop (a display comfortably above the cap) with
    // `target_fps = Some(30)` and count presented frames over one simulated
    // second — the same counting shape `an_unfocused_window_keeps_ticking_and_
    // presents_at_thirty_fps` already uses, applied to the *focused* path this
    // test adds.
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    let mut rendered = 0u32;
    for i in 1..=120u32 {
        let now = t0 + Duration::from_secs_f64(f64::from(i) / 120.0);
        if pacer.begin_frame(now, Some(30)).render {
            rendered += 1;
        }
    }
    assert!(
        (30..=31).contains(&rendered),
        "a focused 30 fps cap against a 120 Hz loop should present ~30 frames, got {rendered}"
    );

    // Negative control: the same loop with no cap presents every iteration —
    // proving the 30-vs-120 gap above is the cap's doing, not some other
    // throttle this pacer already applies while focused.
    let mut uncapped = FramePacer::new(t0);
    let mut all_rendered = 0u32;
    for i in 1..=120u32 {
        let now = t0 + Duration::from_secs_f64(f64::from(i) / 120.0);
        if uncapped.begin_frame(now, None).render {
            all_rendered += 1;
        }
    }
    assert_eq!(
        all_rendered, 120,
        "uncapped and focused must render every iteration — the control that \
         makes the capped count above meaningful"
    );
}

#[test]
fn effective_target_fps_matches_vanillas_framerate_limit_tracker() {
    use crate::app::pacing::effective_target_fps;
    use crate::config::InactivityFpsLimit;

    // Unlimited (260) and not idle: no cap at all.
    assert_eq!(effective_target_fps(260, InactivityFpsLimit::Afk, 0.0), None);
    // A real cap, not idle: the raw limit, unaffected by the AFK machinery.
    assert_eq!(
        effective_target_fps(120, InactivityFpsLimit::Afk, 0.0),
        Some(120)
    );
    // `Minimized` never reduces for idle input, however long — only `Afk`
    // does (vanilla's own framerate-limit-tracker gate).
    assert_eq!(
        effective_target_fps(120, InactivityFpsLimit::Minimized, 10_000.0),
        Some(120)
    );
    // SHORT_AFK: `min(limit, 30)` past 60 s idle, vanilla's own formula
    // — a limit *above* 30 gets capped down.
    assert_eq!(
        effective_target_fps(120, InactivityFpsLimit::Afk, 90.0),
        Some(30)
    );
    // ...and a limit already *below* 30 is not raised by it.
    assert_eq!(
        effective_target_fps(20, InactivityFpsLimit::Afk, 90.0),
        Some(20)
    );
    // Unlimited base, still SHORT_AFK: the 30 cap applies with nothing to
    // `min` it against.
    assert_eq!(
        effective_target_fps(260, InactivityFpsLimit::Afk, 90.0),
        Some(30)
    );
    // LONG_AFK: flatly 10 past 600 s, matching the long-idle limit
    //, regardless of the raw limit.
    assert_eq!(
        effective_target_fps(120, InactivityFpsLimit::Afk, 700.0),
        Some(10)
    );
    // Right at the boundary must not have crossed it yet (`>`, not `>=`,
    // mirroring the strict greater-than threshold at 60 seconds.
    assert_eq!(
        effective_target_fps(120, InactivityFpsLimit::Afk, 60.0),
        Some(120)
    );
}
