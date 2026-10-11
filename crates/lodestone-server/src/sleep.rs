//! Sleeping players and the night-skip vote — the server half of "a player gets in a
//! bed and, when enough of the world's players are asleep, the night skips to
//! morning."
//!
//! The model is a **world-global** vote, not a per-player flag: it counts
//! every eligible player, and the night skips only when enough players are asleep **and**
//! enough of them have been asleep long enough. That vote cannot be computed
//! from one connection's own flags, so this module splits it across the two halves that
//! actually own the data:
//!
//! * [`SleepVote`] is the **shared** roster and voter count. Connections call
//!   [`SleepVote::lay_down`]/[`SleepVote::get_up`] on bed entry/exit and feed
//!   the active-player count from the shared [`crate::PlayerRegistry`] where
//!   one exists; the world tick loop reads it. It is the concrete stand-in for
//!   the players-as-shared-state the world loop requires.
//! * [`SleepState`] is owned by the world tick loop
//!   (`crate::tick::run_tick_loop_with_weather`) with no lock, exactly like
//!   [`crate::WeatherState`] and `game_tick`. It records the game tick each
//!   sleeper lay down (only the loop owns the tick counter — the connection
//!   cannot), computes the vote with the defined arithmetic, and decides
//!   when the night skips.
//!
//! On a skip the loop publishes a [`SleepEvent::SkippedNight`] onto a
//! [`SleepFeed`] — the same snapshot-feed idiom as
//! [`crate::WeatherFeed`]/[`crate::tick::ExplosionFeed`] — which
//! `serve_play`'s `container_sync_tick` arm drains into a real
//! `encode_set_time(game_time, Some(morning))` broadcast. There is **no
//! `SLEEPING_STATUS` packet** in 26.2: a player's lying-down state travels as
//! entity metadata (the sleeping pose the client already decodes and
//! the plan flags as an unplanned client follow-up), and the night skip is
//! simply the clock jump.
//!
//! # The skip and its broadcast routine
//!
//! The vote passing runs three ordered actions:
//!
//! 1. the clock jumps forward to the next morning using a `(0, 24000)` marker, i.e.
//!    `ceil(day_time / 24000) * 24000` — **gated on the `advance_time` game
//!    rule** (a world with the rule off still wakes everyone; only the clock
//!    stands still);
//! 2. every sleeper wakes, clearing the shared roster and setting each sleep
//!    counter to its post-wake value of 100;
//! 3. if it is raining and the `advanceWeather` rule is on, the weather cycle
//!    resets — the four weather scalars return to
//!    their clear values and the rain/thunder levels ramp down through the
//!    weather tick's normal interpolation.
//!
//! The loop also advances a `day_time` counter one per tick. The world keeps
//! `game_time` and `day_time` as two counters, and a skip jumps
//! only `day_time`, so the two diverge after a skip). It starts at 0 — a fresh
//! world's day time, and the value the join-time full sync
//! (`encode_set_time(0, Some(0))`) anchors the client's day clock to — and
//! the skip sets it to the next multiple of 24000, so a second skip later in
//! the same world jumps to the next morning, matching the day-clock adapter's
//! from the re-anchoring it receives.
//!
//! # What is deliberately not here
//!
//! * **The sleeping-percentage rule** still comes from
//!   [`crate::tick::players_sleeping_percentage()`], which returns its default
//!   of `100` until the tick loop reads that setting from the shared world-rule
//!   store. Time advancement already reads
//!   [`crate::world_state::WorldStateHandle::advance_time`].
//! * **Bed-entry gates** live in the `UseItemOn` bed arm (see [`BedRefusal`]):
//!   reach, an obstructed cell above either half, a bright sky, and a hostile
//!   mob nearby (skipped in creative). The sky check reads the darkness flag the
//!   tick loop publishes on [`SleepVote`] each tick. An angry zombified piglin
//!   does not count as a monster here, as per-mob anger is not queryable, and a
//!   bed another player occupies is not refused.
//! * **Spectator exclusion** is unmodelled: this crate has no spectator
//!   concept, so every connected player counts toward the vote
//!   so every connected player counts toward the vote.
//! * **One vote per world.** [`SleepVote`] and [`SleepFeed`] live on
//!   [`crate::world_state::WorldStateHandle`], so the host, a published
//!   world's guests and open-to-LAN guests all vote in one roster and each
//!   reads every skip through its own [`SleepCursor`].
//! * **The client half** (a sleeping player's pose, the rest prompt
//!   overlay, the "N players sleeping" bar) is a flagged-not-planned follow-up
//!   per the plan's S1 — the metadata already decodes.

use std::sync::{Arc, Mutex};

/// Ticks a sleeper must have lain down before the night-skip vote can pass —
/// `is_sleeping && sleep_counter >= 100`, the deep-sleep threshold. It stops
/// the vote from
/// passing the instant the Nth player's head touches a pillow.
pub const DEEP_SLEEP_TICKS: u64 = 100;

/// Day length in ticks — the period of the `(0, 24000)` clock marker, the value
/// the skip's clock jump
/// advances to the next multiple of.
pub const DAY_LENGTH_TICKS: i64 = 24_000;

/// A skip a connection must learn about: the world's clock jumped forward to
/// the next morning and every sleeper woke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepEvent {
    /// The vote passed with the `advanceTime` rule on: time jumped to the next
    /// morning. `game_time` is the world-age tick the skip happened at,
    /// `morning` the new day-time anchor — the pair
    /// `encode_set_time(game_time, Some(morning))` re-anchors the client's day
    /// clock to, using the normal time-sync packet.
    SkippedNight {
        /// The world-age tick the skip happened at (the loop's `game_tick`).
        game_time: i64,
        /// The new day-time: the next multiple of [`DAY_LENGTH_TICKS`] after
        /// the pre-skip day time.
        morning: i64,
    },
}

/// The shared night-skip vote: which players are currently in beds, and how
/// many players can vote.
///
/// This is **shared state**: the roster is populated by connections on bed
/// entry/exit and read by the
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
    /// The bed head each sleeper lies in, by player id.
    beds: Vec<(i32, lodestone_model::BlockPos)>,
    /// Bed heads a sleeper has lain down in since the tick loop last drained
    /// them; the loop sets each bed's occupied flag, so the change reaches every
    /// connection through the loop's feed.
    newly_occupied: Vec<lodestone_model::BlockPos>,
    /// Bed heads whose sleeper has left since the tick loop last drained them;
    /// the loop frees each (a straw bed is destroyed, any other bed loses its
    /// occupied flag).
    vacated_beds: Vec<lodestone_model::BlockPos>,
    /// The number of players who can vote. This crate has no spectator
    /// concept, so every connected player counts.
    active: u32,
    /// Whether the Overworld sky is dark enough to sleep under, as the tick
    /// loop last computed it from the clock and the weather.
    dark_outside: bool,
}

impl SleepVote {
    /// A fresh vote: no sleepers, no voters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a player lying down in a bed — the connection's `UseItemOn` bed
    /// arm. Idempotent: a re-click on the same bed does not
    /// double-count.
    pub fn lay_down(&self, entity_id: i32) {
        let mut inner = self.0.lock().expect("sleep vote lock poisoned");
        if !inner.sleepers.contains(&entity_id) {
            inner.sleepers.push(entity_id);
        }
    }

    /// Records a player lying down in the bed whose head is at `head`; the bed
    /// is queued to be freed when they get up or are woken.
    pub fn lay_down_in(&self, entity_id: i32, head: lodestone_model::BlockPos) {
        self.lay_down(entity_id);
        let mut inner = self.0.lock().expect("sleep vote lock poisoned");
        if !inner.beds.iter().any(|(id, _)| *id == entity_id) {
            inner.beds.push((entity_id, head));
            inner.newly_occupied.push(head);
        }
    }

    /// Whether a sleeper is in the bed whose head is at `head`, ahead of the
    /// tick loop writing the bed's occupied flag.
    pub fn bed_taken(&self, head: lodestone_model::BlockPos) -> bool {
        self.0.lock().expect("sleep vote lock poisoned").beds.iter().any(|(_, bed)| *bed == head)
    }

    /// Drains the bed heads sleepers have lain down in.
    pub(crate) fn take_newly_occupied(&self) -> Vec<lodestone_model::BlockPos> {
        std::mem::take(&mut self.0.lock().expect("sleep vote lock poisoned").newly_occupied)
    }

    /// Whether the player is currently in a bed.
    pub fn is_sleeping(&self, entity_id: i32) -> bool {
        self.0.lock().expect("sleep vote lock poisoned").sleepers.contains(&entity_id)
    }

    /// Publishes whether the Overworld sky is dark (night or a thunderstorm);
    /// the tick loop owns the clock and the weather, so it feeds this each tick
    /// and a connection's bed click reads it.
    pub fn set_dark_outside(&self, dark: bool) {
        self.0.lock().expect("sleep vote lock poisoned").dark_outside = dark;
    }

    /// Whether the Overworld sky was dark at the last tick.
    pub fn dark_outside(&self) -> bool {
        self.0.lock().expect("sleep vote lock poisoned").dark_outside
    }

    /// Drains the bed heads whose sleepers have left.
    pub(crate) fn take_vacated_beds(&self) -> Vec<lodestone_model::BlockPos> {
        std::mem::take(&mut self.0.lock().expect("sleep vote lock poisoned").vacated_beds)
    }

    /// Records a player getting up — `ServerBound::PlayerCommand`'s
    /// `STOP_SLEEPING` (action `0`), or the tick loop's wake-all
    /// ([`Self::clear`]).
    pub fn get_up(&self, entity_id: i32) {
        let mut inner = self.0.lock().expect("sleep vote lock poisoned");
        inner.sleepers.retain(|&id| id != entity_id);
        if let Some(index) = inner.beds.iter().position(|(id, _)| *id == entity_id) {
            let (_, head) = inner.beds.swap_remove(index);
            inner.vacated_beds.push(head);
        }
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

    /// The number of players who can vote, as last fed by a connection.
    pub fn active(&self) -> u32 {
        self.0.lock().expect("sleep vote lock poisoned").active
    }

    /// A consistent single-lock read for the tick loop: `(active, sleepers)`.
    pub(crate) fn snapshot(&self) -> (u32, Vec<i32>) {
        let inner = self.0.lock().expect("sleep vote lock poisoned");
        (inner.active, inner.sleepers.clone())
    }

    /// Empties the roster after a skip so the next tick does not re-register
    /// the just-woken players.
    pub(crate) fn clear(&self) {
        let mut inner = self.0.lock().expect("sleep vote lock poisoned");
        inner.sleepers.clear();
        let woken = std::mem::take(&mut inner.beds);
        inner.vacated_beds.extend(woken.into_iter().map(|(_, head)| head));
    }
}

/// Gets a connection's player out of bed when the connection ends, whatever
/// the exit path: the roster entry clears and a straw bed they lay in is
/// queued for destruction, as for any other leaving.
#[derive(Debug)]
pub struct SleepSeat {
    vote: SleepVote,
    entity_id: i32,
}

impl SleepSeat {
    /// A seat for `entity_id` on `vote`.
    pub fn new(vote: &SleepVote, entity_id: i32) -> Self {
        Self { vote: vote.clone(), entity_id }
    }
}

impl Drop for SleepSeat {
    fn drop(&mut self) {
        self.vote.get_up(self.entity_id);
    }
}

/// Why a bed click does not put the player to sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BedRefusal {
    /// Neither bed cell is within reach.
    TooFarAway,
    /// A suffocating block sits above the head or the foot.
    Obstructed,
    /// The sky is bright.
    NotDark,
    /// A hostile mob is within 8 blocks horizontally and 5 vertically.
    NotSafe,
    /// Someone is already in the bed.
    Occupied,
}

impl BedRefusal {
    /// The action-bar text the player sees.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::TooFarAway => "You may not rest now; the bed is too far away",
            Self::Obstructed => "This bed is obstructed",
            Self::NotDark => "You can sleep only at night or during thunderstorms",
            Self::NotSafe => "You may not rest now; there are monsters nearby",
            Self::Occupied => "This bed is occupied",
        }
    }
}

/// Whether the bed cell at `cell` is within a player's sleeping reach: 3
/// blocks horizontally and 2 vertically of the cell's bottom centre.
pub(crate) fn bed_cell_in_reach(player: lodestone_model::Vec3, cell: lodestone_model::BlockPos) -> bool {
    (player.x - (f64::from(cell.x) + 0.5)).abs() <= 3.0
        && (player.y - f64::from(cell.y)).abs() <= 2.0
        && (player.z - (f64::from(cell.z) + 0.5)).abs() <= 3.0
}

/// Power of the blast a regular bed makes when used where sleeping is forbidden.
pub const BED_BLAST_POWER: f32 = 5.0;

/// The sky darkening (0 bright to 11 dark) at which the sky counts as dark.
pub const DARK_SKY_DARKENING: u8 = 4;

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
    /// The next morning after `day_time`: `ceil(day_time / 24000.0) * 24000`.
    /// At
    /// `day_time == 0` (a world's very first tick) that is `0` itself —
    /// sunrise, matching the day-clock contract.
    pub fn morning_after(day_time: i64) -> i64 {
        ((day_time + DAY_LENGTH_TICKS - 1) / DAY_LENGTH_TICKS) * DAY_LENGTH_TICKS
    }

    /// Computes `max(1, ceil(active_players * pct / 100))`. The `max(1, …)`
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
    /// clock starts at that tick), and a player who disappeared is dropped (they woke,
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

    /// Whether the night-skip vote passes: at least the required number of players
    /// are asleep, and at least that many have been asleep for
    /// [`DEEP_SLEEP_TICKS`] or more.
    pub fn vote_passes(&self, active: u32, pct: u32, game_tick: u64) -> bool {
        let needed = Self::sleepers_needed(active, pct) as usize;
        self.sleepers.len() >= needed && self.deep_sleepers(game_tick) >= needed
    }

    /// Every current sleeper as `(entity id, lay-down tick)` — the feed used by
    /// the mob simulation for sleeping-player checks and shoulder-ride
    /// dismounts. A plain copy avoids exposing [`Sleeper`] itself.
    #[must_use]
    pub(crate) fn sleepers_snapshot(&self) -> Vec<(i32, u64)> {
        self.sleepers.iter().map(|s| (s.entity_id, s.since_game_tick)).collect()
    }

    /// Wakes every tracked sleeper by clearing the roster. This crate has no
    /// per-player sleep state to clear; the caller also clears
    /// [`SleepVote::clear`] so the next tick does not re-register them.
    pub fn wake_all(&mut self) {
        self.sleepers.clear();
    }
}

/// How many skips the feed keeps for a reader that has not caught up. A skip
/// happens at most once per night, so a connection that falls this far behind
/// has long since been dropped.
const SLEEP_FEED_CAPACITY: usize = 16;

/// The world's night-skip broadcast: the tick loop publishes each skip once and
/// every connection of the world hears it, through a [`SleepCursor`] of its own.
///
/// One feed per world (`crate::world_state::WorldStateHandle::sleep_feed`), so
/// the host, a published world's guests and an open-to-LAN guest all read the
/// same log. A drain would give a skip to whichever connection asked first.
#[derive(Debug, Clone, Default)]
pub struct SleepFeed(Arc<Mutex<SleepLog>>);

#[derive(Debug, Default)]
struct SleepLog {
    /// The position of `events[0]` in the log's whole history.
    first: u64,
    events: std::collections::VecDeque<SleepEvent>,
}

/// One reader's place in a [`SleepFeed`].
#[derive(Debug, Clone, Copy)]
pub struct SleepCursor(u64);

impl SleepFeed {
    /// Records one skip for every cursor to read.
    pub fn publish(&self, event: SleepEvent) {
        let mut log = self.0.lock().expect("sleep feed lock poisoned");
        log.events.push_back(event);
        if log.events.len() > SLEEP_FEED_CAPACITY {
            log.events.pop_front();
            log.first += 1;
        }
    }

    /// A cursor at the end of the log: it sees only skips published after now.
    #[must_use]
    pub fn cursor(&self) -> SleepCursor {
        let log = self.0.lock().expect("sleep feed lock poisoned");
        SleepCursor(log.first + log.events.len() as u64)
    }

    /// The skips published since `cursor` last read, oldest first.
    pub fn since(&self, cursor: &mut SleepCursor) -> Vec<SleepEvent> {
        let log = self.0.lock().expect("sleep feed lock poisoned");
        let start = cursor.0.max(log.first);
        cursor.0 = log.first + log.events.len() as u64;
        log.events.iter().skip((start - log.first) as usize).copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection that ends while its player lies in a straw bed clears the
    /// vote entry and queues the bed's head for destruction; a seat still alive
    /// (the control) does neither.
    #[test]
    fn dropping_a_seat_gets_the_sleeper_up_and_vacates_their_straw_bed() {
        let vote = SleepVote::new();
        let head = lodestone_model::BlockPos::new(4, 70, -2);
        vote.lay_down_in(7, head);
        vote.lay_down(8);
        let seat = SleepSeat::new(&vote, 7);
        assert!(vote.is_sleeping(7));
        assert!(vote.take_vacated_beds().is_empty(), "still seated: nothing vacated");
        drop(seat);
        assert!(!vote.is_sleeping(7));
        assert!(vote.is_sleeping(8), "another player's entry stays");
        assert_eq!(vote.take_vacated_beds(), vec![head]);
    }

    /// Reach is 3 blocks in x and z and 2 in y from the cell's bottom centre.
    #[test]
    fn bed_reach_is_three_by_two_by_three_from_the_cell_bottom_centre() {
        use lodestone_model::{BlockPos, Vec3};
        let cell = BlockPos::new(10, 64, 10);
        assert!(bed_cell_in_reach(Vec3::new(13.5, 66.0, 7.5), cell));
        assert!(!bed_cell_in_reach(Vec3::new(13.51, 64.0, 10.5), cell));
        assert!(!bed_cell_in_reach(Vec3::new(10.5, 66.01, 10.5), cell));
        assert!(!bed_cell_in_reach(Vec3::new(10.5, 61.99, 10.5), cell));
    }

    /// Pins the vote arithmetic to `max(1, ceil(active * pct / 100))`. The
    /// four rows are the ones that
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

    /// `morning_after` is `ceil(day_time / 24000) * 24000`. The partial-day
    /// row is the one that matters: at day time 200 a
    /// skip must land on 24000, not on "the next whole day from 0" nor on 200.
    #[test]
    fn morning_after_lands_on_the_next_multiple_of_24000() {
        assert_eq!(SleepState::morning_after(0), 0, "a world's first tick is sunrise");
        assert_eq!(SleepState::morning_after(200), 24_000);
        assert_eq!(SleepState::morning_after(23_999), 24_000);
        assert_eq!(SleepState::morning_after(24_000), 24_000, "already morning stays");
        assert_eq!(SleepState::morning_after(24_100), 48_000, "the second skip");
    }

    /// The deep-sleep threshold gates the vote: one fewer than the required
    /// number of deep
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

    /// One fewer sleeper than required never passes, no matter how deep the
    /// existing sleepers are.
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
    /// their deep-sleep clock is not preserved if they lie down again — while a
    /// sleeper who never got up keeps the clock they laid down with.
    ///
    /// Both halves matter and they pull in opposite directions, which is what
    /// this test is for: `reconcile` must *re*-clock a returnee and must **not**
    /// re-clock a stayer. [`SleepState::reconcile`]'s push is guarded on
    /// `!self.sleepers.iter().any(...)` for exactly that reason.
    ///
    /// The stayer keeps the tick from the first roster. The returnee receives
    /// a fresh tick when reinserted, so the two recorded values must differ.
    #[test]
    fn reconcile_drops_woken_players_and_reclocks_returnees() {
        // Named so the assertions read against the call that set them rather
        // than against three loose literals.
        const LAY_DOWN: u64 = 0;
        const ONE_GETS_UP: u64 = 50;
        const RETURNS: u64 = 60;

        let mut state = SleepState::default();
        state.reconcile(&[1001, 1002], LAY_DOWN);
        // Player 1001 gets up; 1002 stays.
        state.reconcile(&[1002], ONE_GETS_UP);
        assert_eq!(state.sleepers.len(), 1);
        assert_eq!(state.sleepers[0].entity_id, 1002);
        // 1001 lies down again at tick 60: their counter restarts, so the
        // Sleep recorded before the return does not count.
        state.reconcile(&[1002, 1001], RETURNS);
        assert_eq!(
            state.sleepers[0].since_game_tick, LAY_DOWN,
            "1002 never got up, so its clock must still be the tick it lay down at \
             ({LAY_DOWN}) — not the tick of any later reconcile"
        );
        assert_eq!(
            state.sleepers[1].since_game_tick, RETURNS,
            "1001 did get up, so its clock must restart at the tick it lay down again \
             ({RETURNS}) and its earlier {ONE_GETS_UP} ticks of sleep must not count"
        );
        // The two must differ, or the assertions above are both satisfied by a
        // `reconcile` that stamps every sleeper with the current tick.
        assert_ne!(
            state.sleepers[0].since_game_tick, state.sleepers[1].since_game_tick,
            "a stayer and a returnee must not share a clock"
        );
    }

    /// A straw bed is queued for destruction exactly when its sleeper gets up
    /// or is woken by a skip, and not while they still lie in it.
    #[test]
    fn a_straw_bed_is_vacated_when_its_sleeper_leaves_or_is_woken() {
        let head = lodestone_model::BlockPos::new(1, 64, 2);
        let other = lodestone_model::BlockPos::new(9, 64, 9);
        let vote = SleepVote::new();
        vote.lay_down_in(1, head);
        vote.lay_down_in(2, other);
        assert!(vote.take_vacated_beds().is_empty(), "still asleep");
        vote.get_up(1);
        assert_eq!(vote.take_vacated_beds(), vec![head]);
        assert!(vote.take_vacated_beds().is_empty(), "drained once");
        vote.clear();
        assert_eq!(vote.take_vacated_beds(), vec![other], "a skip wakes the rest");
        vote.get_up(2);
        assert!(vote.take_vacated_beds().is_empty(), "already woken");
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

    /// Every cursor reads each skip once, in order; a late cursor sees only
    /// what follows its creation.
    #[test]
    fn every_cursor_reads_each_skip_once_in_order() {
        let feed = SleepFeed::default();
        let (mut a, mut b) = (feed.cursor(), feed.cursor());
        assert!(feed.since(&mut a).is_empty());
        let skip = |game_time| SleepEvent::SkippedNight { game_time, morning: 24_000 };
        feed.publish(skip(200));
        feed.publish(skip(300));
        let mut late = feed.cursor();
        assert_eq!(feed.since(&mut a), vec![skip(200), skip(300)]);
        assert_eq!(feed.since(&mut b), vec![skip(200), skip(300)], "a second reader is not starved by the first");
        assert!(feed.since(&mut a).is_empty());
        assert!(feed.since(&mut late).is_empty());
        feed.publish(skip(400));
        assert_eq!(feed.since(&mut late), vec![skip(400)]);
    }

    /// A reader further behind than the log keeps resumes at its oldest entry.
    #[test]
    fn a_cursor_behind_the_capacity_resumes_at_the_oldest_kept_skip() {
        let feed = SleepFeed::default();
        let mut cursor = feed.cursor();
        for game_time in 0..(SLEEP_FEED_CAPACITY as i64 + 4) {
            feed.publish(SleepEvent::SkippedNight { game_time, morning: 24_000 });
        }
        let seen = feed.since(&mut cursor);
        assert_eq!(seen.len(), SLEEP_FEED_CAPACITY);
        assert_eq!(seen[0], SleepEvent::SkippedNight { game_time: 4, morning: 24_000 });
    }
}
