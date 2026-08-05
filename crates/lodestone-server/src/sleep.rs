//! Sleeping players and the night-skip vote (issue #325,
//! `docs/plans/world-state.md` S1) — the server half of "a player gets in a
//! bed and, when enough of the world's players are asleep, the night skips to
//! morning."
//!
//! Vanilla's model is a **world-global** vote (`ServerLevel.sleepStatus`, a
//! `SleepStatus` — `net/minecraft/server/level/SleepStatus.java`), not a
//! per-player flag: `updateSleepingPlayers` counts every player, spectators
//! excluded, and the night skips only when enough players are asleep **and**
//! enough of them have been asleep long enough. That vote cannot be computed
//! from one connection's own flags — the straddle the plan's shape-B migration
//! exists to delete — so this module splits it across the two halves that
//! actually own the data:
//!
//! * [`SleepVote`] is the **shared** roster and voter count. Connections call
//!   [`SleepVote::lay_down`]/[`SleepVote::get_up`] on bed entry/exit and feed
//!   the active-player count from the shared [`crate::PlayerRegistry`] where
//!   one exists; the world tick loop reads it. It is the concrete stand-in for
//!   the players-as-shared-state the plan demands, before shape B lands.
//! * [`SleepState`] is owned by the world tick loop (`crate::tick::
//!   run_tick_loop_with_weather`) with no lock, exactly like
//!   [`crate::WeatherState`] and `game_tick`. It records the game tick each
//!   sleeper lay down (only the loop owns the tick counter — the connection
//!   cannot), computes the vote with vanilla's exact arithmetic, and decides
//!   when the night skips.
//!
//! On a skip the loop publishes a [`SleepEvent::SkippedNight`] onto a
//! [`SleepFeed`] — the same snapshot-feed idiom as
//! [`crate::WeatherFeed`]/[`crate::tick::ExplosionFeed`] — which
//! `serve_play`'s `container_sync_tick` arm drains into a real
//! `encode_set_time(game_time, Some(morning))` broadcast. There is **no
//! `SLEEPING_STATUS` packet** in 26.2: a player's lying-down state travels as
//! entity metadata (the `EntityPose::Sleeping` the client already decodes and
//! the plan flags as an unplanned client follow-up), and the night skip is
//! simply the clock jump.
//!
//! # The skip, after vanilla `ServerLevel.java:367-379`
//!
//! The vote passing runs three steps, in vanilla's order:
//!
//! 1. the clock jumps forward to the next morning — `moveToTimeMarker(
//!    WAKE_UP_FROM_SLEEP)` with `ClockTimeMarker(0, 24000)`, i.e.
//!    `ceil(dayTime / 24000) * 24000` — **gated on the `advanceTime` game
//!    rule** (a world with the rule off still wakes everyone; only the clock
//!    stands still);
//! 2. every sleeper wakes — `wakeUpAllPlayers`, which first clears the
//!    `SleepStatus` roster (`removeAllSleepers`) and then calls
//!    `stopSleepInBed(false, false)` per player (non-forceful, so the sleep
//!    counter snaps to its post-wake 100);
//! 3. if it is raining and the `advanceWeather` rule is on, the weather cycle
//!    resets (`resetWeatherCycle`) — the four `WeatherData` scalars return to
//!    their clear values and the rain/thunder levels ramp down through the
//!    weather tick's normal interpolation.
//!
//! The loop also advances a `day_time` counter one per tick, exactly like the
//! `dayTime` field vanilla's `ServerLevel.tickTime` increments (`ServerLevel`
//! keeps `gameTime` and `dayTime` as two counters and `moveToTimeMarker` jumps
//! only `dayTime`, so the two diverge after a skip). It starts at 0 — a fresh
//! world's day time, and the value the join-time full sync
//! (`encode_set_time(0, Some(0))`) anchors the client's day clock to — and
//! the skip sets it to the next multiple of 24000, so a second skip later in
//! the same world jumps to the morning *after* the first, matching what the
//! client's `DayClock` (`crates/protocol/v770/src/adapter.rs`) extrapolates
//! from the re-anchoring it receives.
//!
//! # What is deliberately not here
//!
//! * **The `players_sleeping_percentage`/`advanceTime` game rules** are read
//!   as [`crate::tick::players_sleeping_percentage()`]/
//!   [`crate::tick::advance_time()`], functions returning vanilla's defaults
//!   (`100` / `true`) — the same disclosed gap as
//!   [`crate::tick::advance_weather()`]: this crate has no world-level
//!   `GameRules` registry yet (R1 of the world-state plan), and the
//!   per-connection `WorldAdminState::game_rules` is the wrong side of the
//!   world for a tick loop that runs with no connection at all.
//! * **Bed-entry gates** are unmodelled. Vanilla's `startSleepInBed`
//!   (`ServerPlayer.java:1186-1240`) rejects a bed that is not legally
//!   enterable (blocked, out of reach, monsters within ±8/±5, daytime, the
//!   player flying/creative) with per-reason messages. The monster check is
//!   already a documented remainder in `crate::world_spawn`'s
//!   [`is_legal_bed_respawn`] (it needs a shape-B mob AABB); the daytime check
//!   needs the day clock the *loop* owns, not the connection — so this landing
//!   registers a bed click unconditionally and relies on the 100-tick
//!   deep-sleep threshold to make accidental daytime clicks harmless.
//! * **Spectator exclusion** is unmodelled: this crate has no spectator
//!   concept, so every connected player counts toward the vote
//!   (`SleepStatus.updateSleepingPlayers` filters `!player.isSpectator()`).
//! * **The LAN relay** (`IntegratedServer::bind`) runs the sleep-free
//!   `run_tick_loop` wrapper today, so a LAN world never skips the night yet —
//!   the same gap `#324`'s weather feed has in `bind`, and the same
//!   follow-up: when the world-loop's LAN fan-out lands for weather, sleep
//!   rides it unchanged.
//! * **The client half** (a sleeping player's pose, the "you may not rest now"
//!   overlay, the "N players sleeping" bar) is a flagged-not-planned follow-up
//!   per the plan's S1 — the metadata already decodes.

use std::sync::{Arc, Mutex};

/// Ticks a sleeper must have lain down before the night-skip vote can pass —
/// vanilla `Player::isSleepingLongEnough`: `isSleeping() && sleepCounter >=
/// 100` (`net/minecraft/world/entity/player/Player.java`), the `100` the plan's
/// S1 names as the deep-sleep threshold. It is what stops the vote from
/// passing the instant the Nth player's head touches a pillow.
pub const DEEP_SLEEP_TICKS: u64 = 100;

/// Vanilla's day length in ticks — the period of the `WAKE_UP_FROM_SLEEP`
/// clock marker (`ClockTimeMarker(0, 24000)`), the value the skip's clock jump
/// advances to the next multiple of.
pub const DAY_LENGTH_TICKS: i64 = 24_000;

/// A skip a connection must learn about — the server-side half of vanilla's
/// night-skip broadcast (`ServerLevel.java:367-379`): the world's clock jumped
/// forward to the next morning and every sleeper woke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepEvent {
    /// The vote passed with the `advanceTime` rule on: time jumped to the next
    /// morning. `game_time` is the world-age tick the skip happened at,
    /// `morning` the new day-time anchor — the pair
    /// `encode_set_time(game_time, Some(morning))` re-anchors the client's day
    /// clock to, exactly the `ClientboundSetTimePacket` broadcast vanilla's
    /// skip path sends.
    SkippedNight {
        /// The world-age tick the skip happened at (the loop's `game_tick`).
        game_time: i64,
        /// The new day-time: the next multiple of [`DAY_LENGTH_TICKS`] after
        /// the pre-skip day time.
        morning: i64,
    },
}

/// The shared night-skip vote: which players are in beds right now, and how
/// many players can vote.
///
/// Mirrors vanilla `SleepStatus` (`SleepStatus.java`) as **shared state** — the
/// roster is populated by the connections on bed entry/exit and read by the
/// world tick loop, so the vote is computed over *all* players, never from one
/// connection's own flags. That is the plan's S1 prohibition ("do not build a
/// per-connection approximation; that is the straddle again") made concrete:
/// the per-connection `lay_down`/`get_up`/`set_active` calls only *feed* this
/// handle; the vote itself lives in [`SleepState`], owned by the tick loop.
#[derive(Debug, Clone, Default)]
pub struct SleepVote(Arc<Mutex<SleepVoteInner>>);

/// The locked state behind [`SleepVote`].
#[derive(Debug, Default)]
struct SleepVoteInner {
    /// The server-side ids of players currently in beds. Keyed by the
    /// connection's own player id — a `PlayerRegistry` entity id where one
    /// exists (LAN, and every `serve_play` gate), `crate::server`'s
    /// `LOCAL_PLAYER_ENTITY_ID` otherwise (singleplayer) — so a multi-player
    /// roster never collides two connections under one key.
    sleepers: Vec<i32>,
    /// The number of players who can vote. Vanilla excludes spectators
    /// (`SleepStatus.updateSleepingPlayers`); this crate has no spectator
    /// concept, so every connected player counts.
    active: u32,
}

impl SleepVote {
    /// A fresh vote: no sleepers, no voters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a player lying down in a bed — the connection's `UseItemOn` bed
    /// arm (issue #325). Idempotent: a re-click on the same bed does not
    /// double-count.
    pub fn lay_down(&self, entity_id: i32) {
        let mut inner = self.0.lock().expect("sleep vote lock poisoned");
        if !inner.sleepers.contains(&entity_id) {
            inner.sleepers.push(entity_id);
        }
    }

    /// Records a player getting up — `ServerBound::PlayerCommand`'s
    /// `STOP_SLEEPING` (action `0`), or the tick loop's wake-all
    /// ([`Self::clear`]).
    pub fn get_up(&self, entity_id: i32) {
        self.0
            .lock()
            .expect("sleep vote lock poisoned")
            .sleepers
            .retain(|&id| id != entity_id);
    }

    /// Sets the number of players who can vote — the shared
    /// [`crate::PlayerRegistry`]'s length where one exists (LAN, and every
    /// `serve_play` gate), fed on the connection's `container_sync_tick`. A
    /// connection with no registry (singleplayer's
    /// `open_in_memory_with_mobs`) never calls this, and
    /// [`SleepState::sleepers_needed`]'s `max(1, …)` then yields exactly 1 —
    /// the correct single-player vote.
    pub fn set_active(&self, active: u32) {
        self.0.lock().expect("sleep vote lock poisoned").active = active;
    }

    /// A consistent single-lock read for the tick loop: `(active, sleepers)`.
    pub(crate) fn snapshot(&self) -> (u32, Vec<i32>) {
        let inner = self.0.lock().expect("sleep vote lock poisoned");
        (inner.active, inner.sleepers.clone())
    }

    /// Empties the roster after a skip — vanilla's `wakeUpAllPlayers` calls
    /// `sleepStatus.removeAllSleepers()` first (`ServerLevel.java:373`), so the
    /// next tick does not re-register the just-woken players.
    pub(crate) fn clear(&self) {
        self.0.lock().expect("sleep vote lock poisoned").sleepers.clear();
    }
}

/// One sleeping player, with the game tick their deep-sleep clock started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sleeper {
    /// The player's server-side id — the same key [`SleepVote`] uses.
    entity_id: i32,
    /// The world tick the player lay down; their deep-sleep counter is
    /// `game_tick - since_game_tick`.
    since_game_tick: u64,
}

/// The night-skip vote, computed by the world tick loop from the shared
/// [`SleepVote`]. Owned by the loop with no lock, exactly like
/// [`crate::WeatherState`] and `game_tick` — the plain-struct shape the
/// world-state migration (shape A) turns into a `Resource` mechanically later.
#[derive(Debug, Clone, Default)]
pub struct SleepState {
    /// Every current sleeper, with the game tick they lay down. Recorded here
    /// rather than in [`SleepVote`] because only the loop owns the tick
    /// counter — the connection cannot.
    sleepers: Vec<Sleeper>,
}

impl SleepState {
    /// The next morning after `day_time` — vanilla's `moveToTimeMarker` with
    /// the `WAKE_UP_FROM_SLEEP` marker `ClockTimeMarker(0, 24000)`:
    /// `Mth.ceil(dayTime / 24000.0) * 24000` (`ServerLevel.java`). At
    /// `day_time == 0` (a world's very first tick) that is `0` itself —
    /// sunrise, exactly as vanilla reports it.
    pub fn morning_after(day_time: i64) -> i64 {
        ((day_time + DAY_LENGTH_TICKS - 1) / DAY_LENGTH_TICKS) * DAY_LENGTH_TICKS
    }

    /// Vanilla's `sleepersNeeded(pct)` (`SleepStatus.java:21-23`):
    /// `Math.max(1, Mth.ceil(activePlayers * pct / 100.0F))`. The `max(1, …)`
    /// is what makes a single-player world work with the default `100` rule —
    /// with exactly one active player `ceil(1 * 100 / 100) = 1`, and with no
    /// registry to count at all (singleplayer) it still yields `1`, never `0`.
    pub fn sleepers_needed(active: u32, pct: u32) -> u32 {
        (u64::from(active)
            .saturating_mul(u64::from(pct))
            .div_ceil(100) as u32)
            .max(1)
    }

    /// Reconciles the roster with the shared vote's snapshot: a player who
    /// newly appears is recorded with the current game tick (their deep-sleep
    /// clock starts now), and a player who disappeared is dropped (they woke,
    /// disconnected, or the skip cleared the roster).
    pub fn reconcile(&mut self, sleepers: &[i32], game_tick: u64) {
        for &entity_id in sleepers {
            if !self.sleepers.iter().any(|s| s.entity_id == entity_id) {
                self.sleepers.push(Sleeper {
                    entity_id,
                    since_game_tick: game_tick,
                });
            }
        }
        self.sleepers.retain(|s| sleepers.contains(&s.entity_id));
    }

    /// How many of the current sleepers have been asleep for
    /// [`DEEP_SLEEP_TICKS`] or more at `game_tick` — the "deep sleepers"
    /// count behind [`Self::vote_passes`], exposed for the gate's threshold
    /// assertions.
    pub fn deep_sleepers(&self, game_tick: u64) -> usize {
        self.sleepers
            .iter()
            .filter(|s| game_tick.saturating_sub(s.since_game_tick) >= DEEP_SLEEP_TICKS)
            .count()
    }

    /// Whether the night-skip vote passes — vanilla's `SleepStatus
    /// .areEnoughSleeping` **and** `areEnoughDeepSleeping` together: at least
    /// `sleepersNeeded` players are asleep, and at least `sleepersNeeded` of
    /// them have been asleep for [`DEEP_SLEEP_TICKS`] or more.
    pub fn vote_passes(&self, active: u32, pct: u32, game_tick: u64) -> bool {
        let needed = Self::sleepers_needed(active, pct) as usize;
        self.sleepers.len() >= needed && self.deep_sleepers(game_tick) >= needed
    }

    /// The wake-all — vanilla's `wakeUpAllPlayers` (`ServerLevel.java:367-379`)
    /// per-sleeper `stopSleepInBed(false, false)`. This crate has no
    /// per-player sleep state to clear, so dropping the roster *is* the wake;
    /// the caller is also responsible for [`SleepVote::clear`], which is what
    /// stops the next tick from re-registering them.
    pub fn wake_all(&mut self) {
        self.sleepers.clear();
    }
}

/// A shared feed of night-skip events the world tick loop publishes into and a
/// connection drains — the exact same idiom [`crate::WeatherFeed`]/
/// [`crate::tick::ExplosionFeed`] establish for weather transitions and
/// detonations, applied to the vote's skip broadcast instead. Same
/// single-consumer caveat and the same resolution: singleplayer spawns exactly
/// one connection task per feed instance.
#[derive(Debug, Clone, Default)]
pub struct SleepFeed(Arc<Mutex<Vec<SleepEvent>>>);

impl SleepFeed {
    /// Records one skip for the consumer to learn about on its next
    /// [`drain_all`](Self::drain_all).
    pub fn publish(&self, event: SleepEvent) {
        self.0
            .lock()
            .expect("sleep feed lock poisoned")
            .push(event);
    }

    /// Drains and returns every skip published since the last call — safe only
    /// for exactly one consumer, like every other feed in this crate.
    pub fn drain_all(&self) -> Vec<SleepEvent> {
        std::mem::take(&mut *self.0.lock().expect("sleep feed lock poisoned"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the vote arithmetic to vanilla's `SleepStatus.java:21-23`:
    /// `max(1, ceil(active * pct / 100))`. The four rows are the ones that
    /// matter: a lone player with the default `100` needs 1; two players with
    /// `100` need both; two players at `50` need one (a bare majority can
    /// skip); and with **no** registry to count (singleplayer, `active = 0`)
    /// the `max(1, …)` floor still demands exactly one sleeper rather than
    /// letting the vote pass empty-handed.
    #[test]
    fn sleepers_needed_matches_vanilla() {
        assert_eq!(SleepState::sleepers_needed(1, 100), 1);
        assert_eq!(SleepState::sleepers_needed(2, 100), 2);
        assert_eq!(SleepState::sleepers_needed(2, 50), 1);
        assert_eq!(SleepState::sleepers_needed(3, 50), 2, "ceil, not floor");
        assert_eq!(SleepState::sleepers_needed(0, 100), 1, "the max(1, …) floor");
    }

    /// `morning_after` is `ceil(dayTime / 24000) * 24000` — the `WAKE_UP_FROM_SLEEP`
    /// marker. The partial-day row is the one that matters: at day time 200 a
    /// skip must land on 24000, not on "the next whole day from 0" nor on 200.
    #[test]
    fn morning_after_lands_on_the_next_multiple_of_24000() {
        assert_eq!(SleepState::morning_after(0), 0, "a world's first tick is sunrise");
        assert_eq!(SleepState::morning_after(200), 24_000);
        assert_eq!(SleepState::morning_after(23_999), 24_000);
        assert_eq!(SleepState::morning_after(24_000), 24_000, "already morning stays");
        assert_eq!(SleepState::morning_after(24_100), 48_000, "the second skip");
    }

    /// The deep-sleep threshold gates the vote: `sleepersNeeded - 1` deep
    /// sleepers never pass it, and the Nth does only once their own counter
    /// reaches [`DEEP_SLEEP_TICKS`]. The `reconcile` call is what starts each
    /// sleeper's counter at their own lay-down tick.
    #[test]
    fn vote_waits_for_the_deep_sleep_threshold() {
        let mut state = SleepState::default();
        // Two players, default rule: both must sleep.
        state.reconcile(&[1001, 1002], 10);

        // At tick 109 — 99 ticks after both lay down — neither is deep.
        assert!(!state.vote_passes(2, 100, 109));
        assert_eq!(state.deep_sleepers(109), 0);
        // At tick 110 both cross 100 ticks of sleep.
        assert!(state.vote_passes(2, 100, 110));
        assert_eq!(state.deep_sleepers(110), 2);
    }

    /// The `sleepersNeeded - 1` case never passes no matter how deep — the
    /// plan's scripted scenario's first leg, at the `SleepState` level.
    #[test]
    fn one_fewer_than_needed_never_passes() {
        let mut state = SleepState::default();
        // Three players, `100` rule: two sleepers can never pass.
        state.reconcile(&[1001, 1002], 0);
        assert!(!state.vote_passes(3, 100, DEEP_SLEEP_TICKS + 1));
        // The control: the third joins and the same assert must flip.
        state.reconcile(&[1001, 1002, 1003], 0);
        assert!(state.vote_passes(3, 100, DEEP_SLEEP_TICKS + 1));
    }

    /// A sleeper who gets up drops out of the vote the next reconcile, and
    /// their deep-sleep clock is not preserved if they lie down again.
    #[test]
    fn reconcile_drops_woken_players_and_reclocks_returnees() {
        let mut state = SleepState::default();
        state.reconcile(&[1001, 1002], 0);
        // Player 1001 gets up; 1002 stays.
        state.reconcile(&[1002], 50);
        assert_eq!(state.sleepers.len(), 1);
        assert_eq!(state.sleepers[0].entity_id, 1002);
        // 1001 lies down again at tick 60: their counter restarts, so the
        // previously-elapsed deep sleep does not count.
        state.reconcile(&[1002, 1001], 60);
        assert_eq!(state.sleepers[0].since_game_tick, 50);
        assert_eq!(state.sleepers[1].since_game_tick, 60);
    }

    /// The roster round-trips the shared vote: `lay_down`/`get_up`/`set_active`
    /// populate a snapshot the loop can read, and `clear` empties it (the
    /// wake-all). Also pins idempotence — a re-click on the same bed counts
    /// once.
    #[test]
    fn sleep_vote_round_trips_and_is_idempotent() {
        let vote = SleepVote::new();
        assert_eq!(vote.snapshot(), (0, vec![]));
        vote.set_active(2);
        vote.lay_down(1001);
        vote.lay_down(1001);
        vote.lay_down(1002);
        assert_eq!(vote.snapshot(), (2, vec![1001, 1002]));
        vote.get_up(1001);
        assert_eq!(vote.snapshot(), (2, vec![1002]));
        vote.clear();
        assert_eq!(vote.snapshot(), (2, vec![]));
    }

    /// [`SleepFeed`] round-trips publish → drain in FIFO order, like every
    /// feed in this crate.
    #[test]
    fn sleep_feed_drains_in_order() {
        let feed = SleepFeed::default();
        assert!(feed.drain_all().is_empty());
        feed.publish(SleepEvent::SkippedNight {
            game_time: 200,
            morning: 24_000,
        });
        assert_eq!(
            feed.drain_all(),
            vec![SleepEvent::SkippedNight {
                game_time: 200,
                morning: 24_000,
            }]
        );
        assert!(feed.drain_all().is_empty());
    }
}
