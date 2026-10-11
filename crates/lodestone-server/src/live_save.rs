//! [`LiveSaveSlot`]: the cancellation-safe mirror of a connection's latest player record, and the
//! memory store a directory-less world saves into.
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
//! # Every target
//!
//! The slot compiles on every target. Its complete-record half carries a
//! [`PlayerDataStore`](crate::player_data::PlayerDataStore) that is a directory on native and a
//! memory map elsewhere, so the connection loop saves the same way in both. The typed native
//! locator half is native only.
//!
//! A slot also owns the memory store a world without a directory falls back to. A browser slot
//! has one from construction, so the periodic save, the disconnect save and a rejoin on the same
//! server go through the loop's one path; a native slot has one only from
//! [`LiveSaveSlot::with_memory_store`], leaving in-memory native worlds unsaved as before.
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
#[derive(Debug, Clone)]
pub struct LiveSaveSlot {
    /// The latest complete record, mirrored for the shutdown flush.
    complete: std::sync::Arc<
        std::sync::Mutex<
            Option<(
                crate::player_data::PlayerDataStore,
                uuid::Uuid,
                crate::player_data::PlayerData,
            )>,
        >,
    >,
    /// Latest bounded native player data, independent from the complete Anvil
    /// snapshot above. Keeping a second slot means the native locator and
    /// independently consumable game mode can survive cancellation without
    /// changing or replacing full player persistence.
    #[cfg(not(target_arch = "wasm32"))]
    native: std::sync::Arc<
        std::sync::Mutex<
            Option<(
                std::sync::Arc<crate::world_storage::WorldStorage>,
                crate::world_storage::NativePlayerData,
            )>,
        >,
    >,
    /// The store a world with no directory of its own saves into.
    memory: Option<crate::player_data::PlayerDataStore>,
}

impl Default for LiveSaveSlot {
    /// An empty slot; in a browser it carries a memory store, because there is no directory.
    fn default() -> Self {
        let memory = cfg!(target_arch = "wasm32").then(crate::player_data::PlayerDataStore::in_memory);
        Self::build(memory)
    }
}

impl LiveSaveSlot {
    fn build(memory: Option<crate::player_data::PlayerDataStore>) -> Self {
        Self {
            complete: std::sync::Arc::default(),
            #[cfg(not(target_arch = "wasm32"))]
            native: std::sync::Arc::default(),
            memory,
        }
    }

    /// A fresh, empty slot — the compatibility value every entry point other
    /// than the singleplayer one passes, mirroring `BlockTickFeed::default()`
    /// and its siblings in `crate::server`: nothing reads a slot the
    /// singleplayer path did not wire a real consumer for.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty slot whose world saves into memory, whatever the target.
    #[must_use]
    pub fn with_memory_store() -> Self {
        Self::build(Some(crate::player_data::PlayerDataStore::in_memory()))
    }

    /// The memory store a world with no directory saves into, if this slot has one.
    #[must_use]
    pub fn memory_store(&self) -> Option<crate::player_data::PlayerDataStore> {
        self.memory.clone()
    }

    /// Publishes the latest snapshot, replacing whatever was there. A `None`
    /// `store` is a no-op — there is nothing to persist into.
    pub fn publish(
        &self,
        store: Option<crate::player_data::PlayerDataStore>,
        uuid: uuid::Uuid,
        data: crate::player_data::PlayerData,
    ) {
        let Some(store) = store else {
            return;
        };
        *self.complete.lock().expect("live save slot lock poisoned") = Some((store, uuid, data));
    }

    /// Takes the latest snapshot, if [`Self::publish`] was ever called with a
    /// real store.
    #[must_use]
    pub fn take(
        &self,
    ) -> Option<(
        crate::player_data::PlayerDataStore,
        uuid::Uuid,
        crate::player_data::PlayerData,
    )> {
        self.complete.lock().expect("live save slot lock poisoned").take()
    }

    /// Publishes the latest bounded native player data, replacing the
    /// previous value. A missing storage handle is a no-op for Anvil and
    /// in-memory worlds; no typed payload is retained in those modes.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn publish_native(
        &self,
        storage: Option<std::sync::Arc<crate::world_storage::WorldStorage>>,
        record: crate::world_storage::NativePlayerData,
    ) {
        let Some(storage) = storage else {
            return;
        };
        *self
            .native
            .lock()
            .expect("native live save slot lock poisoned") = Some((storage, record));
    }

    /// Takes the latest bounded native player data, if one was published. The
    /// integrated shutdown path calls this only after joining the connection
    /// task, so no newer cancelled snapshot can race the final write.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn take_native(
        &self,
    ) -> Option<(
        std::sync::Arc<crate::world_storage::WorldStorage>,
        crate::world_storage::NativePlayerData,
    )> {
        self.native
            .lock()
            .expect("native live save slot lock poisoned")
            .take()
    }
}
