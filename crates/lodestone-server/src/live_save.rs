//! [`LiveSaveSlot`]: the one piece of [`crate::player_data`] that has to
//! compile on every target, not just native.
//!
//! # What it is
//!
//! A continuously-refreshed, in-memory mirror of the most recent
//! [`PlayerData`](crate::player_data::PlayerData) a connection would save,
//! read back by [`crate::IntegratedServer::shutdown`] to persist a player who
//! never reaches either of `crate::server`'s two deliberate save points.
//!
//! # Why this exists
//!
//! Singleplayer's own shutdown is not a socket close. `IntegratedServer::
//! shutdown` fires a signal that **races** the connection task's whole
//! serving future in a `tokio::select!` (`crate::integrated`'s connection
//! task), and on an ordinary "leave world" the signal wins essentially every
//! time: the serving future — including its own stack-local `player_pos`,
//! `player_rot`, `game_mode` and `inventory` — is dropped mid-`.await`, not
//! returned from. `crate::server`'s disconnect-save arm (the branch where
//! `conn.read_packet()` resolves to `Ok(None)`) is therefore structurally
//! unreachable on that path: it exists for a *real* peer socket closing,
//! which singleplayer's in-process `DuplexStream` never does on its own.
//! That leaves only the periodic ~30-second `vitals_tick` save able to
//! survive a quit, so a `/gamemode`, a move or a pickup inside that window
//! — or before the first tick ever fires — was silently discarded on
//! rejoin, while block edits (flushed by the unrelated world-autosave path)
//! were not.
//!
//! # How this fixes it
//!
//! `crate::server`'s own `serve_play` calls [`LiveSaveSlot::publish`] once
//! per iteration of its own `select!` loop — a cheap in-memory clone, no disk
//! I/O — so the slot always holds a snapshot at most one packet or timer
//! tick stale, regardless of whether the future that built it is later
//! cancelled. [`IntegratedServer::shutdown`](crate::IntegratedServer::shutdown)
//! reads it with [`LiveSaveSlot::take`] **after** joining the connection task
//! (the same ordering the final region flush already uses, and for the same
//! reason: nothing can produce a newer snapshot once that task is known to
//! have stopped) and persists it directly — independent of whether the
//! connection future that built it is still alive to run its own cleanup.
//!
//! # Why this file, and not `player_data.rs`
//!
//! [`crate::player_data`] is `#[cfg(not(target_arch = "wasm32"))]` in full —
//! correctly, because [`PlayerDataStore`](crate::player_data::PlayerDataStore)
//! is a `std::fs` schema over `lodestone-anvil`, and `lodestone-anvil` is not
//! even a wasm32 dependency of this crate (see `Cargo.toml`'s target-split
//! dependency tables). But `LiveSaveSlot` is threaded unconditionally through
//! `crate::server`'s and `crate::integrated`'s connection-setup functions —
//! as a struct field, `::default()`/`::new()` constructions, and function
//! parameters shared by both the native and the wasm32 `serve_play`/
//! `serve_connection_inner` — so the *type* has to exist on every target even
//! though its filesystem-bearing payload cannot.
//!
//! The split: this module compiles unconditionally, and only the payload
//! field plus [`LiveSaveSlot::publish`]/[`LiveSaveSlot::take`] (the two
//! methods that actually name [`PlayerDataStore`](crate::player_data::PlayerDataStore)
//! and [`PlayerData`](crate::player_data::PlayerData)) are
//! `#[cfg(not(target_arch = "wasm32"))]`. On wasm32 the slot is a
//! zero-field, zero-cost handle with no publish/take at all — not a runtime
//! no-op, a *compile-time absent* one, matching "there is no player store in
//! the browser" the way `#[cfg]`-ing the field out (rather than making
//! `publish` silently do nothing there) keeps a wasm32 caller from ever
//! believing a snapshot could exist.
//!
//! Every existing caller of `publish`/`take` already sits inside a
//! `#[cfg(not(target_arch = "wasm32"))]`-gated function
//! (`crate::server::live_publish_player`, `crate::IntegratedServer::
//! shutdown`'s own `take()` call), so gating the methods rather than the type
//! required no caller change at all — only the struct's *declaration* needed
//! to move somewhere the wasm32 build reaches.
//!
//! # How to change it
//!
//! The slot carries the resolved
//! [`PlayerDataStore`](crate::player_data::PlayerDataStore) and `Uuid`
//! alongside the [`PlayerData`](crate::player_data::PlayerData) itself
//! (rather than `IntegratedServer` re-deriving a store from its own chunk
//! source at shutdown) so this stays a pure read-what-was-last-published
//! operation with no second source of truth to keep in step.
//! [`LiveSaveSlot::publish`] is a no-op for a `None` store — the in-memory/LAN
//! case, where there is nothing to persist — matching
//! `crate::server::persist_player`'s own behaviour for the same input.
#[derive(Debug, Clone, Default)]
pub struct LiveSaveSlot(
    #[cfg(not(target_arch = "wasm32"))]
    std::sync::Arc<
        std::sync::Mutex<
            Option<(
                crate::player_data::PlayerDataStore,
                uuid::Uuid,
                crate::player_data::PlayerData,
            )>,
        >,
    >,
);

impl LiveSaveSlot {
    /// A fresh, empty slot — the compatibility value every entry point other
    /// than the singleplayer one passes, mirroring `BlockTickFeed::default()`
    /// and its siblings in `crate::server`: nothing reads a slot the
    /// singleplayer path did not wire a real consumer for.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes the latest snapshot, replacing whatever was there. A `None`
    /// `store` is a no-op — there is nothing to persist into.
    ///
    /// Native only: the payload names
    /// [`PlayerDataStore`](crate::player_data::PlayerDataStore), which does
    /// not exist on wasm32. Every call site already lives inside a
    /// `#[cfg(not(target_arch = "wasm32"))]`-gated function.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn publish(
        &self,
        store: Option<crate::player_data::PlayerDataStore>,
        uuid: uuid::Uuid,
        data: crate::player_data::PlayerData,
    ) {
        let Some(store) = store else {
            return;
        };
        *self.0.lock().expect("live save slot lock poisoned") = Some((store, uuid, data));
    }

    /// Takes the latest snapshot, if [`Self::publish`] was ever called with a
    /// real store.
    ///
    /// Native only, for the same reason [`Self::publish`] is.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn take(
        &self,
    ) -> Option<(
        crate::player_data::PlayerDataStore,
        uuid::Uuid,
        crate::player_data::PlayerData,
    )> {
        self.0.lock().expect("live save slot lock poisoned").take()
    }
}
