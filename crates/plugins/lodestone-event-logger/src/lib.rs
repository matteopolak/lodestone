//! A toy `EventPriority::Monitor` reader plugin: the first consumer of
//! `lodestone_ecs`'s bevy `Message`-based plugin event bus (`RawPacket`/
//! `GameEvent`), and a worked example of the monitor-priority read-only tier:
//! a handler guaranteed to run after every other priority, including
//! cancellation, and structurally unable to mutate state.
//!
//! # What this is
//!
//! A logging/statistics plugin, in Bukkit's own vocabulary for
//! `EventPriority.MONITOR`: it observes every [`GameEvent`] the client
//! decodes and keeps a running count per [`ClientEvent`] variant name, with
//! no opinion about any of them and no way to affect anything else that
//! reads state afterward. It exists to prove the substrate end to end —
//! bus on, event written, `EventPriority::Monitor`-ordered reader sees it,
//! structurally checked read-only — not to be a useful logger.
//!
//! # What consumes it, and why that is deliberately not the shipped client
//!
//! This crate is consumed only by its own test — nothing in the shipped client
//! registers it. That **is the intended design**, not an outstanding wiring gap.
//!
//! A logger plugin is in the same category as `lodestone-autopilot`, which
//! `lodestone_shell::sim::build` removed from the shipped client on purpose and
//! documents at length: the client does not navigate itself, and it does not
//! log every decoded packet into an unbounded `Vec` either. `GameEventBus` is
//! opt-in for exactly this reason — `lodestone_ecs::events`' own doc calls it
//! "zero cost when unused", and a default registration here would turn that cost
//! on for every player to serve no player. The route in is the documented one:
//! `Sim::client_app()` + `add_plugins` + `Sim::from_app`.
//!
//! What *was* genuinely missing is a consumer proving the plugin is reachable at
//! all. `tests/observes_the_game_event_bus.rs` registers it correctly but then
//! supplies its own events, so it could not distinguish a registered plugin from
//! an unreachable one. `tests/observes_a_real_session.rs` is that consumer: a
//! real integrated server, the real 26.2 wire format and the real client driver,
//! with this plugin registered through the same public composition path a
//! third-party embedder uses. See `docs/plugin-api.md`.
//!
//! # How it works
//!
//! [`EventLoggerPlugin::new`] returns the plugin *and* an [`EventLog`] handle
//! sharing the same `Arc<Mutex<..>>` — the plugin captures one clone in its
//! registered system's closure, the caller keeps the other to read counts
//! back out. Deliberately **not** an ECS resource: a `Local<T>` (the
//! system-private half of an `EventPriority::Monitor` system) is invisible
//! to anything outside that one system, and a `ResMut<T>` resource would
//! itself be the mutable-World access `EventLoggerPlugin::build` calls
//! [`lodestone_ecs::assert_monitor_system_is_read_only`] on before
//! registering it, to rule out. An `Arc<Mutex<_>>` captured by the system's
//! closure is genuinely outside the `World` — reading or writing it costs
//! `System::initialize` nothing, so a Monitor-tier plugin can still report
//! its findings anywhere it likes (a file, a metrics endpoint, an
//! in-process channel), exactly as Bukkit's own MONITOR loggers do.
//!
//! # How to change it
//!
//! This is intentionally minimal. A real logging plugin would probably want
//! to filter by event kind rather than count everything, and would want a
//! way to flush/export the log instead of an in-memory `Vec`. Both are
//! additive: neither touches the one thing this crate exists to demonstrate,
//! which is the `GameEventBusPlugin` → `MessageReader<GameEvent>` →
//! `EventPriority::Monitor` pipeline.
//!
//! # Configuration
//!
//! None. `EventLoggerPlugin::new()` takes no arguments.
//!
//! # Dependencies
//!
//! `lodestone-ecs` (the bus, the priority tiers, the read-only check),
//! `lodestone-model` (`ClientEvent`, wrapped by `GameEvent`), `bevy_ecs`/
//! `bevy_app` directly (see this crate's `Cargo.toml` for why a plugin that
//! derives its own `Resource`/`Plugin`-adjacent types needs them as a direct
//! dependency, not only reachable through `lodestone_ecs::{ecs, app}`).

use std::sync::{Arc, Mutex};

use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::message::MessageReader;
use lodestone_ecs::ecs::schedule::IntoScheduleConfigs;
use lodestone_ecs::{EventPriority, GameEvent, GameEventBusPlugin, GameTick};
use lodestone_model::ClientEvent;

/// A read handle onto the log an [`EventLoggerPlugin`] is filling. Clones
/// share the same underlying `Vec` — this is a handle, not a snapshot.
#[derive(Clone, Default, Debug)]
pub struct EventLog {
    events: Arc<Mutex<Vec<ClientEvent>>>,
}

impl EventLog {
    /// A clone of every event observed so far, in arrival order.
    ///
    /// Clones rather than borrows: the lock is held only for the copy, so a
    /// caller never blocks the `GameTick` system that is still appending to
    /// the same `Vec` from a different thread.
    #[must_use]
    pub fn events(&self) -> Vec<ClientEvent> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// How many events have been observed so far. Cheaper than
    /// `self.events().len()` when the caller does not need the events
    /// themselves.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Whether nothing has been observed yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The plugin half of the pair [`EventLoggerPlugin::new`] returns.
///
/// `impl Plugin` rather than a bare system so it can install
/// [`GameEventBusPlugin`] for a caller who has not already added it — the
/// same `is_plugin_added` guard `lodestone_ecs::ingest::IngestQueuePlugin`
/// uses, so adding this plugin twice, or alongside another plugin that also
/// wants the bus, never double-registers `Messages<GameEvent>`.
#[derive(Debug)]
pub struct EventLoggerPlugin {
    log: Arc<Mutex<Vec<ClientEvent>>>,
}

impl EventLoggerPlugin {
    /// Builds a fresh logger and the [`EventLog`] handle that reads it back.
    #[must_use]
    pub fn new() -> (Self, EventLog) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                log: Arc::clone(&events),
            },
            EventLog { events },
        )
    }
}

impl Plugin for EventLoggerPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<GameEventBusPlugin>() {
            app.add_plugins(GameEventBusPlugin);
        }

        // Monitor-priority read-only is checked, not assumed. A closure over an `Arc<Mutex<_>>`
        // captured by value has no ECS-visible parameters of its own beyond
        // `MessageReader<GameEvent>`, which is read-only (see
        // `lodestone_ecs::events`'s doc), so this call should never panic for
        // *this* system — but the point of calling it is that a future edit
        // that adds, say, a `ResMut` to "also count something" would make it
        // panic at plugin-build time instead of silently breaking MONITOR's
        // guarantee.
        let logged = Arc::clone(&self.log);
        let observe = move |mut events: MessageReader<GameEvent>| {
            let mut log = logged
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for GameEvent(event) in events.read() {
                log.push(event.clone());
            }
        };
        lodestone_ecs::assert_monitor_system_is_read_only(observe.clone());
        app.add_systems(GameTick, observe.in_set(EventPriority::Monitor));
    }
}
