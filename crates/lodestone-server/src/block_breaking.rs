//! Server-side block-break validation: instant-break detection, destroy-progress
//! timing, and an interaction-range check (issue #531, plus the one-shot-block
//! bug that motivated it).
//!
//! # What it is
//!
//! `crate::server`'s `apply_block_action` used to be a three-line state machine:
//! `StartDestroy` recorded a position, `StopDestroy` set it to air. Two defects
//! fell out of that, and they are opposite ends of the same missing computation.
//!
//! * **One-shot blocks could not be broken at all.** A client that knows the
//!   block is instant sends `START_DESTROY_BLOCK` *and nothing else* — vanilla's
//!   `MultiPlayerGameMode` concludes an instant break there, because the block
//!   is already gone locally (`lodestone-game`'s `mining` module says so in its
//!   own `destroyed` field doc, and it is issue #387 seen from the server side).
//!   So `pending_break` was set and never consumed, and sugar cane, grass,
//!   flowers and every other zero-hardness block were unbreakable.
//! * **Anything could be broken instantly.** With no timing check, a
//!   `StartDestroy` + `StopDestroy` pair back to back broke obsidian, or
//!   bedrock, from any distance.
//!
//! This module is the shared arithmetic both fixes need: vanilla's
//! `BlockBehaviour.getDestroyProgress` over the jar-derived
//! [`lodestone_data::hardness`] table and the
//! [`lodestone_data::tool::mining`] tool census the *client's* mining predictor
//! already reads. The issue's own framing — the same table sitting one crate
//! away from the server that ought to be checking it — is the whole fix.
//!
//! # How it works
//!
//! [`progress_per_tick`] is vanilla's `getDestroyProgress`:
//! `dig_speed / hardness / divider`, divider `30` with the correct tool and
//! `100` without. Then:
//!
//! * `StartDestroy` breaks immediately when [`progress_per_tick`] is `>= 1.0`
//!   (vanilla's `"insta mine"` branch), which is exactly every zero-hardness
//!   block.
//! * Otherwise the dig is recorded as a [`PendingBreak`], and `StopDestroy`
//!   replays vanilla's `progress_per_tick * (ticks_spent + 1) >= 0.7` test
//!   ([`STOP_DESTROY_PROGRESS`]) before breaking.
//! * **A `StopDestroy` that falls short of `0.7` does not refuse the break — it
//!   *defers* it.** [`PendingBreak::deferred`] is vanilla's `hasDelayedDestroy`:
//!   the dig keeps accruing progress on the server's own clock after the packet,
//!   and the block breaks the tick [`DELAYED_DESTROY_PROGRESS`] is reached. See
//!   [`PendingBreak::deferred_break_ready`] and `crate::server`'s `vitals_tick`
//!   arm, which is the tick hook that polls it.
//! * An unbreakable block (`hardness == -1.0`, i.e. bedrock and barrier) yields
//!   `0.0` progress per tick, so it never satisfies any of the tests at any tick
//!   count. That is the property, not a special case.
//!
//! # How to change it, and the gotchas
//!
//! **This is deliberately a *plausible* check, not an exact port, and
//! [`UNTRACKED_SPEED_HEADROOM`] is why.** Vanilla's `getDestroyProgress` reads
//! the whole player: Efficiency, Haste, Mining Fatigue, Aqua Affinity, the
//! `block_break_speed` attribute, whether the eyes are in water and whether the
//! feet are on the ground. This crate tracks *none* of those — it has no
//! attribute map, no effect list and no game-mode state. A strict port would
//! therefore reject legitimate breaks by a player with an enchanted tool, which
//! is a far worse bug than the one being fixed. So the server's estimate is
//! multiplied by a fixed headroom that comfortably exceeds any real speed-up
//! (Efficiency V on a matching tool is ~4.3x, Haste II a further 1.6x) before
//! the comparison. The check still rejects the thing the issue is about by
//! orders of magnitude — see the tests at the bottom of this file.
//!
//! If the server ever grows real per-player attributes and effects, feed them in
//! and drop the headroom; the shape of [`progress_per_tick`] is already vanilla's.
//! `lodestone-game`'s `BreakInputs` is the full-fidelity client-side twin and is
//! the thing to mirror — this crate does not depend on it (see
//! `Cargo.toml`'s note on keeping the *client* vocabulary out of the browser
//! bundle), which is why the two-line formula is restated here rather than
//! shared.
//!
//! **Creative mode is not modelled**, here or anywhere in this crate: vanilla's
//! `instabuild` branch destroys any block on `StartDestroy`, and a creative
//! client that sends no `StopDestroy` for stone will find it does not break.
//! That is a pre-existing gap (nothing in `lodestone-server` tracks a game
//! mode), named rather than silently half-fixed.
//!
//! # Configuration
//!
//! [`STOP_DESTROY_PROGRESS`], [`UNTRACKED_SPEED_HEADROOM`] and
//! [`MAX_INTERACTION_DISTANCE`] are the three knobs, each documented on itself.
//!
//! # Dependencies
//!
//! [`lodestone_data::hardness`] and [`lodestone_data::tool`] for the censuses,
//! `crate::mobs::block_state_id` to resolve a `ChunkSource` state string to the
//! global state id both censuses key on, and `lodestone_model` for the
//! vocabulary. Names no packet and no protocol version.

use lodestone_model::{BlockPos, ItemStack, Vec3};

use crate::vitals::EYE_HEIGHT;

/// Vanilla's `STOP_DESTROY_BLOCK` acceptance threshold: a dig that has accrued
/// at least this much progress breaks the block
/// (`ServerPlayerGameMode.handleBlockBreakAction`'s `destroyProgress >= 0.7F`).
///
/// It is `0.7` rather than `1.0` because the client is the authority on when it
/// released the button and the server's tick accounting is one tick coarser than
/// the client's; vanilla defers the remaining 30% to `hasDelayedDestroy`, which
/// [`PendingBreak::deferred`] now models.
pub const STOP_DESTROY_PROGRESS: f32 = 0.7;

/// The progress a *deferred* dig must reach for the block to finally break —
/// vanilla's `tick()` test `destroyProgress >= 1.0F` on `hasDelayedDestroy`.
///
/// This is the whole block, not [`STOP_DESTROY_PROGRESS`]: the `0.7` shortcut is
/// a concession to the client having released the button, and a dig that did not
/// earn it pays the full price on the server's own clock instead.
pub const DELAYED_DESTROY_PROGRESS: f32 = 1.0;

/// Multiplier applied to the server's destroy-progress estimate to absorb every
/// speed input this crate does not track.
///
/// See the module docs: the server has no attributes, effects or enchantments,
/// so its estimate is a *lower bound* on the player's real dig speed. `8.0`
/// comfortably clears the realistic worst case (Efficiency V ≈ 4.3x × Haste II
/// 1.6x ≈ 6.8x) while still leaving the check three orders of magnitude away
/// from accepting an instant obsidian break.
pub const UNTRACKED_SPEED_HEADROOM: f32 = 8.0;

/// How far from the player's eyes a block may be and still be breakable.
///
/// Vanilla is `Player.isWithinBlockInteractionRange(pos, 1.0)`: the
/// `block_interaction_range` attribute (default `4.5`) plus a 1.0 padding,
/// measured to the closest point of the block's box. This crate measures to the
/// block *centre* from the eye position instead, which is up to ~0.87 further,
/// so the constant is rounded up to `6.0` rather than reproducing `5.5`. The
/// point is to reject a break from across the world, which is what the issue
/// reports; shaving the last half-block is not worth per-block AABB geometry
/// here.
pub const MAX_INTERACTION_DISTANCE: f64 = 6.0;

/// One connection's in-progress dig — the version-free analogue of vanilla's
/// `destroyPos` + `destroyProgressStart` pair.
///
/// Recording `progress_per_tick` at `StartDestroy` rather than recomputing it at
/// `StopDestroy` is vanilla's `sameDestroyTarget` behaviour approximated cheaply:
/// the dig is priced against the block and tool the player actually started on,
/// so swapping to a faster tool mid-dig does not retroactively shorten it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PendingBreak {
    /// The block being dug.
    pub(crate) pos: BlockPos,
    /// [`progress_per_tick`] for the state and held item at `StartDestroy`.
    pub(crate) progress_per_tick: f32,
    /// The server tick `StartDestroy` arrived on, or `None` on a target with no
    /// tick clock to read (`wasm32` — see `crate::server`'s `serve_play`). A
    /// `None` here means the timing test is skipped for this dig; the range and
    /// hardness tests still apply.
    pub(crate) start_tick: Option<u64>,
    /// Vanilla's `hasDelayedDestroy`: the client has already sent its
    /// `StopDestroy` and the dig fell short of [`STOP_DESTROY_PROGRESS`], so this
    /// dig is no longer waiting on a packet — it is waiting on the *clock*, and
    /// the server's own tick loop finishes it once
    /// [`DELAYED_DESTROY_PROGRESS`] is reached.
    ///
    /// Why this exists rather than a refusal: for any block the client does not
    /// treat as instant, `StopDestroy` **is** the ordinary end of a dig, and the
    /// server's estimate is always a little behind the client's own progress. A
    /// shortfall is the expected case on a slow block, not a cheat, so vanilla
    /// keeps ticking (`ServerPlayerGameMode.tick`) rather than rolling the client
    /// back. Refusing here made every non-instant block unbreakable: hold the
    /// mouse on stone, release, nothing happens.
    pub(crate) deferred: bool,
}

impl PendingBreak {
    /// The dig's accrued progress as of `now`, as a fraction of the block —
    /// vanilla's `getDestroyProgress * (ticksSpentDestroying + 1)`, with
    /// [`UNTRACKED_SPEED_HEADROOM`] folded in.
    ///
    /// `None` when either side has no tick to compare (see
    /// [`start_tick`](Self::start_tick)) — "no timing evidence", which each
    /// caller resolves in its own safe direction rather than inventing a number.
    fn progress_at(&self, now: Option<u64>) -> Option<f32> {
        let (Some(start), Some(now)) = (self.start_tick, now) else {
            return None;
        };
        let elapsed = (now.saturating_sub(start) + 1) as f32;
        Some(self.progress_per_tick * UNTRACKED_SPEED_HEADROOM * elapsed)
    }

    /// This dig moved into vanilla's `hasDelayedDestroy` state — the same
    /// `progress_per_tick` and the same start tick
    /// (`delayedTickStart = destroyProgressStart`), so the deferred continuation
    /// is priced on one unbroken clock instead of restarting from the
    /// `StopDestroy`.
    ///
    /// `None` for a dig that could never finish: an unbreakable block, or one
    /// with no clock to defer against (whose `StopDestroy` is accepted outright,
    /// so this is unreachable there anyway). That `None` is what stops a bedrock
    /// dig parking a doomed deferred break in the slot forever.
    pub(crate) fn defer(self) -> Option<Self> {
        if self.progress_per_tick <= 0.0 || self.start_tick.is_none() {
            return None;
        }
        Some(Self {
            deferred: true,
            ..self
        })
    }

    /// Whether a deferred dig has now earned its block — vanilla's
    /// `tick()`/`incrementDestroyProgress` test against
    /// [`DELAYED_DESTROY_PROGRESS`].
    ///
    /// `false` for a dig still waiting on its `StopDestroy`
    /// ([`deferred`](Self::deferred) is `false`), so the server's tick hook can
    /// call this unconditionally on whatever is in the slot.
    pub(crate) fn deferred_break_ready(&self, now: Option<u64>) -> bool {
        self.deferred
            && self.progress_per_tick > 0.0
            && self
                .progress_at(now)
                .is_some_and(|progress| progress >= DELAYED_DESTROY_PROGRESS)
    }

    /// Whether a `StopDestroy` arriving on `now` may break this block —
    /// vanilla's `getDestroyProgress * (ticksSpentDestroying + 1) >= 0.7F`, with
    /// [`UNTRACKED_SPEED_HEADROOM`] folded in.
    ///
    /// Always `true` when either side has no tick to compare (see
    /// [`start_tick`](Self::start_tick)); always `false` for an unbreakable
    /// block, whose `progress_per_tick` is `0.0`. A `false` on a breakable block
    /// is **not** a refusal — the caller defers the dig with [`Self::defer`].
    pub(crate) fn may_break_at(&self, now: Option<u64>) -> bool {
        if self.progress_per_tick <= 0.0 {
            return false;
        }
        self.progress_at(now)
            .is_none_or(|progress| progress >= STOP_DESTROY_PROGRESS)
    }
}

/// Vanilla `BlockBehaviour.getDestroyProgress` for `block_state` in `held`'s
/// hands, as a fraction of the block accrued per server tick.
///
/// `block_state` is a `ChunkSource::block_state` string (a bare name, or one
/// with properties). Returns `None` for a state neither census knows, which
/// callers read as "do not validate" rather than "unbreakable" — an unknown
/// state is our gap, not the client's cheat.
///
/// * `>= 1.0` — an instant break: the block goes on `StartDestroy`.
/// * `0.0` — unbreakable (`hardness == -1.0`), so no tick count ever breaks it.
#[must_use]
pub(crate) fn progress_per_tick(block_state: &str, held: Option<&ItemStack>) -> Option<f32> {
    // `_or_default`, not the exact lookup: both censuses read below are keyed by
    // state id but carry a per-*block* value, and a bare name like
    // `"minecraft:sugar_cane"` is not in the exact index at all (every state of
    // it carries `age`). Missing there produced a `None` here, which the caller
    // reads as "unknown, do not validate" — and that path still waits for a
    // `StopDestroy` that an instant block never sends. See
    // `crate::mobs::block_state_id_or_default`.
    let state_id = crate::mobs::block_state_id_or_default(block_state)?;
    let hardness = lodestone_data::hardness::hardness(state_id)?.hardness;
    if hardness < 0.0 {
        return Some(0.0);
    }
    let mining = lodestone_data::tool::mining(held, state_id)?;
    let divider = if mining.correct_tool { 30.0 } else { 100.0 };
    // Zero hardness divides to `+inf`, which is the instant-break signal the
    // caller tests with `>= 1.0` — exactly as vanilla's own float division does.
    Some(mining.speed / hardness / divider)
}

/// Whether `pos` is close enough to a player whose **feet** are at `feet` to be
/// broken — vanilla's `isWithinBlockInteractionRange`, simplified to a
/// centre-to-eye distance (see [`MAX_INTERACTION_DISTANCE`]).
///
/// `feet` is `None` before the client has reported a position, in which case this
/// returns `true`: "no data yet, don't guess" is the same choice `vitals_tick`'s
/// submersion probe already makes, and refusing breaks until the first movement
/// packet would be a new bug.
#[must_use]
pub(crate) fn within_interaction_range(feet: Option<Vec3>, pos: BlockPos) -> bool {
    let Some(feet) = feet else {
        return true;
    };
    let eye = Vec3::new(feet.x, feet.y + EYE_HEIGHT, feet.z);
    let centre = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    );
    let (dx, dy, dz) = (centre.x - eye.x, centre.y - eye.y, centre.z - eye.z);
    dx * dx + dy * dy + dz * dz <= MAX_INTERACTION_DISTANCE * MAX_INTERACTION_DISTANCE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug the owner reported: a zero-hardness block must be an instant
    /// break, so `StartDestroy` alone is enough. Bare-handed, because that is how
    /// grass gets pulled.
    #[test]
    fn one_shot_blocks_break_on_the_start_action() {
        for name in [
            "minecraft:short_grass",
            "minecraft:dandelion",
            "minecraft:sugar_cane",
            "minecraft:torch",
        ] {
            let per = progress_per_tick(name, None)
                .unwrap_or_else(|| panic!("{name} is not in the hardness census"));
            assert!(
                per >= 1.0,
                "{name}: progress_per_tick {per} must reach 1.0 in one tick — \
                 otherwise the client's start-only instant break is still dropped"
            );
        }
    }

    /// Bedrock's `-1.0` hardness must survive any tick count, including the
    /// `None`-clock path that skips the timing test.
    #[test]
    fn an_unbreakable_block_never_breaks() {
        let per = progress_per_tick("minecraft:bedrock", None).expect("bedrock is in the census");
        assert_eq!(per, 0.0, "bedrock must price at zero progress per tick");
        let dig = PendingBreak {
            pos: BlockPos::new(0, 0, 0),
            progress_per_tick: per,
            start_tick: Some(0),
            deferred: false,
        };
        assert!(!dig.may_break_at(Some(0)));
        assert!(!dig.may_break_at(Some(100_000)));
        assert!(!dig.may_break_at(None), "no clock must not mean no hardness");
        // And the deferred continuation must not become a back door into it:
        // bedrock is not deferrable at all, so no tick ever finishes it.
        assert!(
            dig.defer().is_none(),
            "an unbreakable block must not arm a deferred break"
        );
        let doomed = PendingBreak {
            deferred: true,
            ..dig
        };
        assert!(!doomed.deferred_break_ready(Some(100_000)));
    }

    /// Issue #531's headline: obsidian must not break on a back-to-back
    /// `StartDestroy`/`StopDestroy` pair, and must break once a plausible number
    /// of ticks has passed.
    #[test]
    fn obsidian_rejects_an_instant_stop_and_accepts_a_real_dig() {
        let per = progress_per_tick("minecraft:obsidian", None).expect("obsidian is in the census");
        let dig = PendingBreak {
            pos: BlockPos::new(4, 64, 4),
            progress_per_tick: per,
            start_tick: Some(10),
            deferred: false,
        };
        assert!(
            !dig.may_break_at(Some(10)),
            "a same-tick stop broke obsidian bare-handed (per-tick {per})"
        );
        assert!(!dig.may_break_at(Some(30)), "1.5s bare-handed is not obsidian");
        // Bare-handed obsidian is 250s in vanilla; even with the whole 8x
        // headroom the accepted point is far beyond any of the above.
        let needed = (STOP_DESTROY_PROGRESS / (per * UNTRACKED_SPEED_HEADROOM)).ceil() as u64;
        assert!(
            needed > 100,
            "the headroom has been raised far enough to make this vacuous: \
             obsidian accepted after {needed} ticks"
        );
        assert!(dig.may_break_at(Some(10 + needed)));
    }

    /// The regression the deferred path exists for: the *ordinary* dig, where
    /// `StartDestroy` and `StopDestroy` reach the server on the same tick (a
    /// local integrated server reads both from one buffer). Refusing that made
    /// every non-instant block unbreakable, which is worse than the cheat #531
    /// was closing.
    ///
    /// Deepslate rather than stone because it is the block the `block_edit` gate
    /// digs, and it is slower than stone — so if this passes, stone does.
    #[test]
    fn a_same_tick_stop_defers_and_then_breaks() {
        let per =
            progress_per_tick("minecraft:deepslate[axis=y]", None).expect("deepslate is in census");
        let dig = PendingBreak {
            pos: BlockPos::new(0, -50, 0),
            progress_per_tick: per,
            start_tick: Some(7),
            deferred: false,
        };
        assert!(
            !dig.may_break_at(Some(7)),
            "a same-tick stop is still not an instant deepslate break"
        );
        let deferred = dig.defer().expect("deepslate is deferrable");
        assert!(
            !deferred.deferred_break_ready(Some(7)),
            "deferring must not itself finish the dig"
        );
        // Predicted from the two constants, not read back off the code: the
        // deferred dig owes the *whole* block, so it lands at
        // ceil(1.0 / (per * headroom)) - 1 ticks after the start.
        let needed = (DELAYED_DESTROY_PROGRESS / (per * UNTRACKED_SPEED_HEADROOM)).ceil() as u64 - 1;
        assert!(
            !deferred.deferred_break_ready(Some(7 + needed - 1)),
            "the deferred break landed early: {needed} ticks was the prediction"
        );
        assert!(
            deferred.deferred_break_ready(Some(7 + needed)),
            "the deferred break never landed after {needed} ticks"
        );
        // A dig still waiting on its `StopDestroy` must never be finished by the
        // tick hook, however long it has been held.
        assert!(!dig.deferred_break_ready(Some(7 + needed * 100)));
    }

    #[test]
    fn a_break_from_across_the_world_is_rejected() {
        let feet = Some(Vec3::new(0.5, 64.0, 0.5));
        assert!(within_interaction_range(feet, BlockPos::new(0, 63, 0)));
        assert!(within_interaction_range(feet, BlockPos::new(3, 64, 0)));
        assert!(!within_interaction_range(feet, BlockPos::new(40, 64, 0)));
        assert!(!within_interaction_range(feet, BlockPos::new(0, 200, 0)));
        assert!(
            within_interaction_range(None, BlockPos::new(40, 64, 0)),
            "no reported position must not block breaking"
        );
    }
}
