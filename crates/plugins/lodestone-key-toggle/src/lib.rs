//! A toy input-interception plugin: the first consumer of
//! `lodestone_ecs::input`'s `PluginKeybinds`/`PluginKeyEvent` registration
//! point (issue #162).
//!
//! # What this is
//!
//! On construction, [`KeyTogglePlugin`] claims one physical key in
//! [`lodestone_ecs::KeyInterceptMode::Consume`] and flips a shared boolean
//! every time the shell reports that key pressed — a minimal, real stand-in
//! for the class of client mod issue #162 names (a custom hotkey a plugin
//! wants exclusive use of, e.g. a macro tool or a HUD toggle vanilla has no
//! binding for). It exists to prove the substrate end to end — registration
//! reaches `resolve_key`'s precedence chain, a queued raw key event reaches
//! a plugin's `MessageReader` through a real [`lodestone_ecs::GameTick`]
//! tick — not to be a useful hotkey itself.
//!
//! # What consumes it, and why that is deliberately not the shipped client
//!
//! Same shape as `lodestone-event-logger` and `lodestone-autopilot`
//! (`crates/plugins/README.md`): this crate is a plugin *library*, not a
//! dependency of `lodestone-shell`. A toggle nobody asked for has no
//! business shipping in every client; the route in for someone who wants it
//! is `Sim::client_app()` + `App::add_plugins` + `Sim::from_app`, the same
//! public composition path any third-party embedder uses.
//!
//! # How it works
//!
//! [`KeyTogglePlugin::new`] returns the plugin *and* a [`KeyToggleState`]
//! handle sharing the same `Arc<AtomicBool>` — the plugin's own system
//! flips it, the caller reads it back, mirroring `EventLoggerPlugin::new`'s
//! `(plugin, handle)` shape and for the identical reason: the toggled state
//! needs to be visible to code outside the `World` (a HUD element, a test),
//! and an `Arc` captured by the system's closure costs
//! `System::initialize` nothing to reach.
//!
//! [`KeyTogglePlugin::build`] does the actual registration:
//! `app.world_mut().resource_mut::<PluginKeybinds>().register(key, Consume)`,
//! after ensuring `LocalPlayerPlugin` (which owns that resource) is
//! installed — the same "add my dependency if it is missing" guard
//! `GameEventBusPlugin::build` already uses for `CorePlugin`. From there the
//! flow is entirely `lodestone_ecs::input`'s own machinery: the shell reads
//! `PluginKeybinds::mode_of` before calling `resolve_key`, gets `Consume`,
//! returns `KeyOutcome::PluginConsumed` (so no gameplay binding sharing the
//! key fires), and separately queues the raw transition into
//! `PendingPluginKeyEvents`. [`drain_pending_plugin_key_events`]
//! (`TickSet::Input`) turns that into a real `Messages<PluginKeyEvent>`
//! this plugin's own system reads with an ordinary `MessageReader`.
//!
//! # How to change it
//!
//! This is intentionally minimal — one key, one boolean. A real macro
//! plugin would probably want several keys and richer per-key actions;
//! both are additive over the one thing this crate exists to demonstrate,
//! which is the `PluginKeybinds` → `resolve_key` → `PendingPluginKeyEvents`
//! → `MessageReader<PluginKeyEvent>` pipeline.
//!
//! # Configuration
//!
//! [`KeyTogglePlugin::new`] takes the [`lodestone_ecs::PhysicalKey`] to
//! claim. No env var, flag or constant.
//!
//! # Dependencies
//!
//! `lodestone-ecs` (the registration/event types, `LocalPlayerPlugin`,
//! `TickSet`), `bevy_ecs`/`bevy_app` directly (see this crate's
//! `Cargo.toml` for why a plugin that derives its own `Resource`-adjacent
//! type needs them as a direct dependency, not only reachable through
//! `lodestone_ecs::{ecs, app}`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::message::MessageReader;
use lodestone_ecs::ecs::resource::Resource;
use lodestone_ecs::ecs::schedule::IntoScheduleConfigs;
use lodestone_ecs::ecs::system::ResMut;
use lodestone_ecs::{GameTick, KeyInterceptMode, LocalPlayerPlugin, PhysicalKey, PluginKeyEvent};

/// A read handle onto a [`KeyTogglePlugin`]'s state. Clones share the same
/// underlying flag — this is a handle, not a snapshot, matching
/// `lodestone-event-logger`'s `EventLog`.
#[derive(Clone, Debug)]
pub struct KeyToggleState(Arc<AtomicBool>);

impl KeyToggleState {
    /// Whether the claimed key has been pressed an odd number of times so
    /// far.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The `Resource` half installed into the `World` so the toggling system can
/// reach it without depending on a `Local<T>` (which would make it
/// unreachable from a second system, and this plugin's design intentionally
/// keeps that door open for `docs/plugin-api.md`'s reasons even though only
/// one system uses it today).
#[derive(Resource, Clone, Debug)]
struct KeyToggleTarget {
    key: PhysicalKey,
    flag: Arc<AtomicBool>,
}

/// Claims `key` in [`KeyInterceptMode::Consume`] on the next tick a
/// `PluginKeyEvent` for it arrives, and flips [`KeyToggleState`]'s flag on
/// every **press** (release edges are ignored — a toggle only wants one
/// transition per physical press, not two).
#[derive(Debug)]
pub struct KeyTogglePlugin {
    key: PhysicalKey,
    flag: Arc<AtomicBool>,
}

impl KeyTogglePlugin {
    /// Build a plugin claiming `key`, plus the [`KeyToggleState`] handle a
    /// caller reads the toggle back through.
    #[must_use]
    pub fn new(key: PhysicalKey) -> (Self, KeyToggleState) {
        let flag = Arc::new(AtomicBool::new(false));
        (
            Self {
                key,
                flag: flag.clone(),
            },
            KeyToggleState(flag),
        )
    }
}

impl Plugin for KeyTogglePlugin {
    fn build(&self, app: &mut App) {
        // `PluginKeybinds` lives on `LocalPlayerPlugin` — install it if a
        // caller has not already, the same "add my dependency if missing"
        // guard `lodestone_ecs::GameEventBusPlugin::build` uses for
        // `CorePlugin`.
        if !app.is_plugin_added::<LocalPlayerPlugin>() {
            app.add_plugins(LocalPlayerPlugin);
        }
        app.world_mut()
            .resource_mut::<lodestone_ecs::PluginKeybinds>()
            .register(self.key.clone(), KeyInterceptMode::Consume);
        app.insert_resource(KeyToggleTarget {
            key: self.key.clone(),
            flag: self.flag.clone(),
        });
        app.add_systems(GameTick, toggle_on_claimed_key_press.in_set(lodestone_ecs::TickSet::Intent));
    }
}

/// Reads this tick's [`PluginKeyEvent`]s and flips [`KeyToggleTarget::flag`]
/// on a matching **press**. Ordered in `TickSet::Intent` (after
/// `TickSet::Input`, where `drain_pending_plugin_key_events` writes the
/// messages this reads) purely because that is where a plugin that turned a
/// key into `MovementIntent`/`LookIntent` instead would need to run — this
/// system does not itself write either, but the ordering choice documents
/// the intended anchor for anything shaped like this one.
fn toggle_on_claimed_key_press(
    mut events: MessageReader<PluginKeyEvent>,
    target: ResMut<KeyToggleTarget>,
) {
    for event in events.read() {
        if event.pressed && event.key == target.key {
            target.flag.fetch_xor(true, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use lodestone_ecs::{PendingPluginKeyEvents, PluginKeybinds};

    use super::*;

    /// The full loop, driven through real machinery rather than called
    /// directly: registering the plugin claims the key in `PluginKeybinds`
    /// (what `resolve_key` would read to swallow it ahead of gameplay —
    /// verified directly here too, closing the loop `lodestone-shell`'s own
    /// `resolve_key` tests only go halfway on since they cannot depend on a
    /// plugin crate), and a raw event queued the way the shell's driver
    /// queues one reaches this plugin's system through one real
    /// `GameTick` and flips the shared flag.
    #[test]
    fn a_queued_press_of_the_claimed_key_flips_the_shared_flag_through_a_real_tick() {
        let (plugin, state) = KeyTogglePlugin::new(PhysicalKey::named("KeyG"));
        let mut app = bevy_app::App::new();
        app.add_plugins(plugin);

        // The registration half: `PluginKeybinds` really does carry the
        // claim, in `Consume` mode — the exact fact
        // `lodestone_shell::app::input::resolve_key`'s plugin arm reads.
        assert_eq!(
            app.world()
                .resource::<PluginKeybinds>()
                .mode_of(&PhysicalKey::named("KeyG")),
            Some(KeyInterceptMode::Consume)
        );
        // A different, unclaimed key must not be affected — the same
        // pairwise-distinct-fixture discipline as `lodestone-ecs::input`'s
        // own tests.
        assert_eq!(
            app.world()
                .resource::<PluginKeybinds>()
                .mode_of(&PhysicalKey::named("KeyH")),
            None
        );

        assert!(!state.enabled());

        // Simulate the shell's driver: it would have queued this because
        // `PluginKeybinds::mode_of` returned `Some` above.
        app.world_mut()
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(PluginKeyEvent {
                key: PhysicalKey::named("KeyG"),
                pressed: true,
            });
        app.world_mut().run_schedule(GameTick);

        assert!(state.enabled(), "a claimed key's press must flip the flag");

        // Second press toggles back off.
        app.world_mut()
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(PluginKeyEvent {
                key: PhysicalKey::named("KeyG"),
                pressed: true,
            });
        app.world_mut().run_schedule(GameTick);
        assert!(!state.enabled(), "a second press must toggle back off");
    }

    /// Negative control: a release edge and an unrelated key must not flip
    /// the flag — proof the positive result above is not a fixture that
    /// flips on any event at all.
    #[test]
    fn a_release_or_an_unrelated_key_does_not_flip_the_flag() {
        let (plugin, state) = KeyTogglePlugin::new(PhysicalKey::named("KeyG"));
        let mut app = bevy_app::App::new();
        app.add_plugins(plugin);

        app.world_mut()
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(PluginKeyEvent {
                key: PhysicalKey::named("KeyG"),
                pressed: false,
            });
        app.world_mut()
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(PluginKeyEvent {
                key: PhysicalKey::named("KeyH"),
                pressed: true,
            });
        app.world_mut().run_schedule(GameTick);

        assert!(!state.enabled());
    }
}
