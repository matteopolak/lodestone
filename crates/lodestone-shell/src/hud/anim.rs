//! Client-side, cosmetic vitals animations, ported from vanilla's `Hud` class
//! (`.cache/mc/26.2/client-src/net/minecraft/client/gui/Hud.java`) — see
//! `docs/hud-animations.md` for the full citation-by-citation notes.
//!
//! This module currently carries the heart row's flash (blink) and
//! critical-health jitter on a health change (issue #30). The hunger-row
//! wobble and the hotbar item "pop" land in their own follow-up commits,
//! reusing the same wall-clock tick and `jitter` helper defined here.
//!
//! ## Why a wall clock, not the server's game tick
//!
//! Every duration below is copied from `Hud.java` in **vanilla ticks** (20 per
//! second, 50ms each). Nothing reaches `hud.rs` with the server's own tick
//! counter — `HudFrame` carries only current-value vitals (`health`, `food`,
//! …), not a clock, and `sim.rs`/`app.rs` are a different agent's files for
//! this change. Since a vanilla tick *is* 50ms of real time whenever the
//! client is keeping up with the server (the overwhelmingly common case),
//! [`wall_tick`] divides a wall-clock [`std::time::Instant::elapsed`] by 50ms
//! and uses that as a drop-in `tickCount` substitute for this HUD's own
//! purely decorative timers — the same trade already made for the chat
//! caret's blink (`app.rs`'s `chat_caret_visible`: wall time instead of a
//! tick count, "the caret keeps blinking while the game is paused"). None of
//! these timers drive any game state, so drift against the server's real
//! tick count during lag or pause is invisible.
//!
//! Every state machine below is a pure function of an explicit `tick: i64`
//! rather than of `Instant` directly — only [`wall_tick`] itself touches a
//! clock — so all of it is unit-tested with literal tick numbers and no
//! timing flakiness.
//!
//! ## Why the jitter is not vanilla's exact RNG sequence
//!
//! Vanilla reuses one `RandomSource` field, reseeded once per
//! `extractPlayerHealth` call (`Hud.java:783`, `random.setSeed(tickCount *
//! 312871)`) and then consumed sequentially across heart containers and food
//! pips in a fixed draw order. Reproducing that exact sequence buys nothing
//! visible — nobody can screenshot-diff a purely cosmetic jitter against a
//! live server — and `docs/sky-and-air-bubbles.md` already made the identical
//! trade for the star field's RNG ("same distribution shape, different exact
//! positions, a visual choice and not a decode-parity claim"). [`jitter`]
//! below is a small splitmix64-style mix keyed by `(tick, salt)` instead.

use std::time::Instant;

/// Vanilla-tick-equivalent index derived from a wall-clock instant — see the
/// module doc for why this substitutes for the real `tickCount`.
pub(super) fn wall_tick(since: Instant) -> i64 {
    (since.elapsed().as_millis() / 50) as i64
}

/// A splitmix64 finalizer mix — see the module doc's RNG note.
fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    x
}

/// A deterministic `0..bound` draw for a given `tick` and a per-call-site
/// `salt` (so two independent jitters at the same tick do not correlate with
/// each other).
pub(super) fn jitter(tick: i64, salt: u64, bound: u32) -> u32 {
    let seed = (tick as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ salt;
    (mix(seed) % u64::from(bound)) as u32
}

/// Cross-frame heart-row animation state — vanilla's
/// `lastHealth`/`displayHealth`/`healthBlinkTime`/`lastHealthTime`
/// (`Hud.java:167-170`). One instance lives for the life of a
/// [`super::HudRenderer`], so a fresh renderer (a gate, a reconnect) starts
/// idle rather than inheriting a previous connection's blink window.
#[derive(Debug, Clone, Copy)]
pub(super) struct HeartAnim {
    last_health: Option<i32>,
    display_health: i32,
    blink_until_tick: i64,
    caught_up_tick: i64,
}

impl HeartAnim {
    pub(super) fn new() -> Self {
        Self {
            last_health: None,
            display_health: 0,
            blink_until_tick: i64::MIN,
            caught_up_tick: 0,
        }
    }

    /// Advances the state to `tick` for this frame's `health` (in
    /// half-points, `Player.getHealth()`'s unit) and returns vanilla's
    /// `blink` (`Hud.java:766`) and `displayHealth` (`Hud.java:777,782` —
    /// the count the "ghost" heart overlay draws during a blink window).
    ///
    /// `blink` is read from the *previous* call's window before this call's
    /// health comparison updates it — `Hud.java:766` runs before the
    /// `healthBlinkTime` reassignment at `:770`/`:773` — so a hit's blink
    /// becomes visible starting the following tick, not the same one that
    /// registered the change. That one-tick lag is vanilla's, not an
    /// artifact of this port, so it is kept rather than smoothed away.
    pub(super) fn tick(&mut self, tick: i64, health: f32) -> (bool, i32) {
        let blink = self.blink_until_tick > tick && (self.blink_until_tick - tick) / 3 % 2 == 1;

        let current = health.max(0.0).ceil() as i32;
        match self.last_health {
            None => {
                // First observation: prime state without a false blink —
                // vanilla's own `lastHealth`/`displayHealth` fields default to
                // `0`, so a fresh connection at full health would otherwise
                // read as an instantaneous 10-tick "heal" blink. Not a
                // divergence worth keeping: the fields exist so gameplay
                // damage/heals blink, not so the HUD blinks once at connect.
                self.display_health = current;
                self.caught_up_tick = tick;
            }
            Some(last) if current < last => {
                // Damage, `Hud.java:768-770`: 20-tick blink window.
                self.blink_until_tick = tick + 20;
                self.caught_up_tick = tick;
            }
            Some(last) if current > last => {
                // Heal, `Hud.java:771-773`: 10-tick blink window.
                self.blink_until_tick = tick + 10;
                self.caught_up_tick = tick;
            }
            Some(_) => {}
        }
        self.last_health = Some(current);

        // `timeMillis - lastHealthTime > 1000` (`Hud.java:776-779`); 1000ms is
        // 20 ticks at this module's 50ms/tick.
        if tick - self.caught_up_tick > 20 {
            self.display_health = current;
            self.caught_up_tick = tick;
        }

        (blink, self.display_health)
    }
}

/// The critical-health y-jitter (`Hud.java:863-865`): once
/// `currentHealth + absorption <= 4`, every heart **container** redraws with
/// a fresh `0..=1`px offset. Vanilla reseeds one shared RNG stream per
/// `extractPlayerHealth` call (`Hud.java:783`); this keys an independent draw
/// by `(tick, container)` instead — see the module doc.
pub(super) fn heart_jitter(tick: i64, container: usize) -> f32 {
    jitter(tick, 0xBEEF_0000_u64 ^ container as u64, 2) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heart_anim_first_observation_primes_without_a_false_blink() {
        // A fresh connection at any health must not blink — see the `None`
        // arm's doc comment. This is the "not-animating" control: without it,
        // full health would misread as an instantaneous heal on frame one.
        let mut h = HeartAnim::new();
        let (blink, display) = h.tick(0, 20.0);
        assert!(!blink);
        assert_eq!(display, 20);
    }

    #[test]
    fn heart_anim_damage_blinks_for_twenty_ticks_starting_next_tick() {
        let mut h = HeartAnim::new();
        h.tick(0, 20.0); // prime at full health
        let (blink_same_tick, _) = h.tick(0, 14.0); // damage lands at tick 0
        assert!(
            !blink_same_tick,
            "vanilla's own one-tick lag: the hit registers before `healthBlinkTime` \
             is read this call, so blink starts the following tick"
        );

        // Predicted vs. the wrong hypothesis: blink is a 3-on/3-off pattern
        // from tick 1 (registration) through tick 20 (1 + 20 - 1) inclusive,
        // i.e. `(blink_until_tick - tick) / 3 % 2 == 1` with
        // `blink_until_tick == 20`. At tick 1, `(20-1)/3 = 6, 6%2 = 0` → off.
        // The wrong hypothesis ("blinks solid for the whole window") would
        // read `true` at every one of these ticks; the right one alternates.
        let expected_on: Vec<bool> = (1..=20)
            .map(|t| {
                let remaining = 20 - t;
                (remaining / 3) % 2 == 1
            })
            .collect();
        for (t, &want) in (1..=20).zip(expected_on.iter()) {
            let (blink, _) = h.tick(t, 14.0);
            assert_eq!(blink, want, "tick {t}: expected blink={want}");
        }
        // Window closes at tick 20 inclusive (`blink_until_tick > tick`, i.e.
        // strictly greater), so tick 21 must be settled regardless of where
        // in the 3-on/3-off pattern tick 20 landed.
        let (blink_21, _) = h.tick(21, 14.0);
        assert!(!blink_21, "the 20-tick window must be over by tick 21");
    }

    #[test]
    fn heart_anim_heal_blinks_for_ten_ticks_not_twenty() {
        let mut h = HeartAnim::new();
        h.tick(0, 10.0);
        h.tick(0, 16.0); // heal at tick 0 → 10-tick window, not 20
        let (blink_11, _) = h.tick(11, 16.0);
        assert!(!blink_11, "a heal's window is 10 ticks, must be settled by 11");
    }

    #[test]
    fn heart_anim_display_health_lags_then_catches_up_after_one_second() {
        let mut h = HeartAnim::new();
        h.tick(0, 20.0);
        let (_, display_at_hit) = h.tick(0, 10.0); // lose 10 half-points
        assert_eq!(
            display_at_hit, 20,
            "the ghost overlay must still show the pre-damage total the instant \
             damage lands, not the post-damage one"
        );
        let (_, display_at_19) = h.tick(19, 10.0);
        assert_eq!(display_at_19, 20, "must not catch up before 20 ticks (1000ms)");
        let (_, display_at_21) = h.tick(21, 10.0);
        assert_eq!(display_at_21, 10, "must have caught up once the window exceeds 20 ticks");
    }

    #[test]
    fn heart_anim_idle_is_bit_identical_across_repeated_calls() {
        // The "not animating" control: unchanging health at widely separated
        // ticks must never blink and must always report the current health as
        // the display health — the pre-existing (pre-animation) draw exactly.
        let mut h = HeartAnim::new();
        h.tick(0, 20.0);
        for t in [1, 50, 1_000, 100_000] {
            let (blink, display) = h.tick(t, 20.0);
            assert_eq!((blink, display), (false, 20), "tick {t} must be idle");
        }
    }

    #[test]
    fn heart_jitter_is_deterministic_and_within_bounds() {
        for tick in 0..500 {
            for container in 0..10 {
                let j = heart_jitter(tick, container);
                assert!(j == 0.0 || j == 1.0);
                assert_eq!(j, heart_jitter(tick, container), "must be a pure function");
            }
        }
    }

    #[test]
    fn heart_jitter_differs_from_container_to_container_somewhere_in_range() {
        // A control against the "shared value smeared across every slot"
        // failure mode: if every container produced the same jitter this
        // would still pass `heart_jitter_is_deterministic_and_within_bounds`,
        // so assert independence explicitly.
        let vals: Vec<f32> = (0..10).map(|c| heart_jitter(3, c)).collect();
        assert!(
            vals.iter().any(|&v| v != vals[0]),
            "containers must not all draw the same jitter at one tick: {vals:?}"
        );
    }
}
