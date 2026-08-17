//! The `/stopwatch` registry (issue #48's remainder) — vanilla's
//! `Stopwatches`/`Stopwatch`, behind [`StopwatchHandle`]. `/stopwatch` and
//! `/execute if`/`unless stopwatch` are its two consumers.
//!
//! # How it works
//!
//! [`StopwatchHandle`] is `Arc<Mutex<HashMap<String, Stopwatch>>>`, shaped
//! like every other store in this module
//! ([`crate::commands::scoreboard_store::ScoreboardHandle`],
//! [`crate::commands::nbt_storage::NbtStorageHandle`]): cheap to clone, rides
//! inside [`crate::world_state::WorldStateHandle`] as a sibling field for the
//! identical reachability reason those two document.
//!
//! A [`Stopwatch`] is vanilla's own two-`long` record —
//! `creation_time`/`accumulated_elapsed`, both epoch milliseconds — and
//! [`Stopwatch::elapsed_millis`] is the same
//! `accumulatedElapsedTime + (currentTime - creationTime)`. `restart` resets
//! both fields (a hard reset, not a pause/resume — vanilla's own
//! `RestartStopwatch` replaces the whole record with `new
//! Stopwatch(currentTime)`, whose constructor zeroes `accumulatedElapsedTime`
//! too), so `accumulated_elapsed` only ever matters after a
//! save/load round trip: `Stopwatches.unpack` seeds it from the persisted
//! value and starts a fresh `creationTime` ticking forward from there.
//!
//! **The clock is [`crate::chat_session::now_millis`], not
//! `std::time::SystemTime::now()`** — this crate links into the wasm32
//! bundle and that call traps at runtime there (`docs/browser-shell-port.md`
//! carries the census); `now_millis` already exists as this crate's one
//! `web_time`-backed portable "now".
//!
//! # What is not built
//!
//! **No persistence.** Vanilla's `Stopwatches` is a `SavedData` written into
//! the world's `data/stopwatches.dat`; this store is process-lifetime only,
//! the same disclosed gap [`crate::chunk::ChunkSource::claim_dragon_fight_start`]'s
//! own doc already accepts for the End-fight flag, for the identical reason
//! (this crate persists scalars through
//! [`crate::world_state::WorldStateHandle::level_data_fields`]/`load_level_data`,
//! and a stopwatch registry has not been added to that pair). A server
//! restart clears every stopwatch.
//!
//! # How to change it
//!
//! Read/write access is via `WorldStateHandle::stopwatches`
//! (`crate::world_state`), never a second constructor — the identical rule
//! every sibling store in this module states, for the identical reason.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// `net.minecraft.world.Stopwatch` — a `(creation_time, accumulated_elapsed)`
/// pair, both epoch milliseconds. See this module's own doc for why
/// `accumulated_elapsed` is always `0` on a freshly created or just-restarted
/// stopwatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stopwatch {
    creation_time: i64,
    accumulated_elapsed: i64,
}

impl Stopwatch {
    fn new(now: i64) -> Self {
        Self { creation_time: now, accumulated_elapsed: 0 }
    }

    /// `Stopwatch.elapsedMilliseconds` — `accumulated + (now - creation)`.
    fn elapsed_millis(&self, now: i64) -> i64 {
        self.accumulated_elapsed + (now - self.creation_time)
    }

    /// `Stopwatch.elapsedSeconds` — `elapsedMilliseconds / 1000.0`.
    fn elapsed_seconds(&self, now: i64) -> f64 {
        self.elapsed_millis(now) as f64 / 1000.0
    }
}

/// A cheap, cloneable handle to one world's stopwatch registry. See the
/// module doc for why this is reached through
/// [`crate::world_state::WorldStateHandle::stopwatches`] rather than
/// constructed directly.
#[derive(Debug, Clone, Default)]
pub struct StopwatchHandle(Arc<Mutex<HashMap<String, Stopwatch>>>);

impl StopwatchHandle {
    /// `StopwatchCommand.createStopwatch` — `Stopwatches.add`. `true` on a
    /// fresh id, `false` when one already exists by that name (the caller's
    /// own `ERROR_ALREADY_EXISTS`, not reported here).
    pub fn create(&self, id: &str) -> bool {
        let now = crate::chat_session::now_millis();
        let mut store = self.0.lock().expect("stopwatch registry lock poisoned");
        if store.contains_key(id) {
            false
        } else {
            store.insert(id.to_string(), Stopwatch::new(now));
            true
        }
    }

    /// `StopwatchCommand.queryStopwatch`'s own read, and `checkStopwatch`'s —
    /// `None` for an id that does not exist, matching both callers' own
    /// `stopwatches.get(id) == null` check.
    #[must_use]
    pub fn elapsed_seconds(&self, id: &str) -> Option<f64> {
        let now = crate::chat_session::now_millis();
        let store = self.0.lock().expect("stopwatch registry lock poisoned");
        store.get(id).map(|stopwatch| stopwatch.elapsed_seconds(now))
    }

    /// `StopwatchCommand.restartStopwatch` — `Stopwatches.update`, replacing
    /// the whole record (see this module's own doc for why that zeroes
    /// `accumulated_elapsed` too, unlike a pause/resume). `false` for an
    /// unknown id, the caller's own `ERROR_DOES_NOT_EXIST`.
    pub fn restart(&self, id: &str) -> bool {
        let now = crate::chat_session::now_millis();
        let mut store = self.0.lock().expect("stopwatch registry lock poisoned");
        match store.get_mut(id) {
            Some(stopwatch) => {
                *stopwatch = Stopwatch::new(now);
                true
            }
            None => false,
        }
    }

    /// `StopwatchCommand.removeStopwatch` — `Stopwatches.remove`. `false`
    /// for an unknown id.
    pub fn remove(&self, id: &str) -> bool {
        let mut store = self.0.lock().expect("stopwatch registry lock poisoned");
        store.remove(id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_refuses_a_duplicate_id() {
        let handle = StopwatchHandle::default();
        assert!(handle.create("minecraft:timer"));
        assert!(!handle.create("minecraft:timer"), "a second create with the same id must be refused");
    }

    #[test]
    fn elapsed_seconds_is_none_for_an_unknown_id() {
        let handle = StopwatchHandle::default();
        assert_eq!(handle.elapsed_seconds("minecraft:nope"), None);
    }

    #[test]
    fn elapsed_seconds_is_non_negative_immediately_after_creation() {
        let handle = StopwatchHandle::default();
        handle.create("minecraft:timer");
        let elapsed = handle.elapsed_seconds("minecraft:timer").expect("just created");
        assert!(elapsed >= 0.0, "elapsed must never be negative: {elapsed}");
        assert!(elapsed < 1.0, "a stopwatch queried immediately should read under a second: {elapsed}");
    }

    #[test]
    fn restart_refuses_an_unknown_id_and_resets_a_known_one() {
        let handle = StopwatchHandle::default();
        assert!(!handle.restart("minecraft:nope"));
        handle.create("minecraft:timer");
        assert!(handle.restart("minecraft:timer"));
        let elapsed = handle.elapsed_seconds("minecraft:timer").expect("still exists");
        assert!(elapsed < 1.0, "a just-restarted stopwatch should read close to zero: {elapsed}");
    }

    #[test]
    fn remove_refuses_an_unknown_id_and_takes_a_known_one_out() {
        let handle = StopwatchHandle::default();
        assert!(!handle.remove("minecraft:nope"));
        handle.create("minecraft:timer");
        assert!(handle.remove("minecraft:timer"));
        assert_eq!(handle.elapsed_seconds("minecraft:timer"), None, "removed means gone");
    }
}
