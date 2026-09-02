//! Plugin-facing keyboard interception.
//!
//! # What it is
//!
//! [`PluginKeybinds`] is the registration point client plugins need: a
//! plugin claims a physical key, in [`KeyInterceptMode::Consume`] (nothing
//! below it in the shell's precedence chain sees the key at all) or
//! [`KeyInterceptMode::Observe`] (the plugin is told, but gameplay/chat/menu
//! chrome still resolves the key exactly as if no plugin existed).
//! [`drain_pending_plugin_key_events`] is the raw-input system
//! [`crate::sets::TickSet::Input`]'s own doc comment reserved an anchor for —
//! "whatever eventually reads a keyboard or gamepad as a system" — landed
//! here rather than left empty.
//!
//! # How it works
//!
//! The shell (`lodestone_shell::app::input::resolve_key`) reads
//! [`PluginKeybinds::mode_of`] through a short [`crate::handle::hold_read`]
//! guard — the same synchronous, no-`World`-reentry shape
//! `crate::veto::ActionVetoes::allows` already uses from the same call site
//! class, for the same reason: a key event is resolved inline in the
//! platform's event loop, one tick before any system could see it. A
//! `Consume` claim adds one arm to that precedence chain, ranked *after*
//! chat/menu/container (which keep first claim regardless — see that
//! function's own doc) and *ahead of* every gameplay binding, so a plugin
//! hotkey cannot be shadowed by a rebind onto the same physical key.
//!
//! Either mode also queues the raw transition into [`PendingPluginKeyEvents`]
//! (a plain resource the shell pushes into directly, outside any system — the
//! same shape `lodestone_shell::interact`'s `Attacking`/`RayTarget` resources
//! are fed by raw window input). [`drain_pending_plugin_key_events`], anchored
//! `.in_set(TickSet::Input)`, folds that queue into
//! `bevy_ecs::message::Messages<PluginKeyEvent>` once per tick, so a plugin
//! system anywhere in [`crate::GameTick`] can read it with an ordinary
//! `MessageReader<PluginKeyEvent>` — a `TickSet::Intent` system can turn a
//! claimed key into `MovementIntent`/`LookIntent` the same tick, since
//! `Input` runs before `Intent` in `CorePlugin`'s chain.
//!
//! # How to change it
//!
//! [`PhysicalKey`] is a `String` matching winit's `KeyCode` `Debug` output
//! (`"KeyF"`, `"Space"`, `"F3"`, `"Digit1"`, ...) rather than a mirrored enum,
//! because this crate never depends on winit — it ships to wasm and winit
//! does not (`crates/lodestone-ecs/Cargo.toml`'s dependency list). The shell
//! side of that mapping is `lodestone_shell::app::lifecycle`'s
//! `physical_key_for`, a plain `format!("{code:?}")`. If a second platform
//! (the browser shell) ever wants to feed this, it needs the equivalent
//! mapping from its own key type to the same string vocabulary — nothing
//! here is winit-specific beyond that one naming convention.
//!
//! One entry per key in [`PluginKeybinds`]: a second [`PluginKeybinds::register`]
//! on an already-claimed key replaces the first, matching the "exactly one
//! owner" shape `docs/plugin-api.md`'s doctrine clause 2 asks of every
//! intent-shaped seam in this codebase. Two plugins racing for the same
//! physical key is a plugin-authoring conflict this registry does not
//! arbitrate, the same way two Fabric mods defaulting to the same key is a
//! user's problem to rebind, not the loader's to referee.
//!
//! # Configuration
//!
//! None — no env var, flag or constant. [`PluginKeybinds`]/
//! [`PendingPluginKeyEvents`]/`Messages<PluginKeyEvent>` are unconditionally
//! installed by [`crate::player::LocalPlayerPlugin`] (the same "reaches a
//! running client with no driver-crate change" reasoning
//! [`crate::player::DebugLines`]'s own doc gives), so every shipped `App` has
//! this seam whether or not a plugin uses it.
//!
//! # Dependencies
//!
//! `bevy_ecs` only. No `lodestone-model`/`lodestone-physics` vocabulary is
//! needed here — a physical key press is not a Minecraft concept.

use std::collections::HashMap;

use bevy_ecs::message::{Message, MessageWriter, Messages};
use bevy_ecs::resource::Resource;
use bevy_ecs::system::ResMut;

/// A plugin's own identity for one physical key.
///
/// See the module doc's "how to change it" section for exactly what string
/// this must be — winit's `KeyCode` `Debug` output, verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PhysicalKey(pub String);

impl PhysicalKey {
    /// Build from a key name, matching a winit `KeyCode`'s `Debug` output.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

/// Whether a plugin's claim on a key stops gameplay from also seeing it, or
/// merely rides alongside it. See the module doc for the full precedence
/// story.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyInterceptMode {
    /// Nothing below this claim in the shell's precedence chain — no
    /// gameplay binding sharing the physical key — sees the event at all.
    Consume,
    /// The plugin is told the key transitioned, but gameplay (or whatever
    /// chat/menu/container chrome would otherwise have resolved it to)
    /// still runs exactly as if no plugin existed.
    Observe,
}

/// The plugin-facing keybinding registry: which physical keys a plugin has
/// claimed, and in which mode. See the module doc for the full design.
#[derive(Resource, Debug, Clone, Default)]
pub struct PluginKeybinds(HashMap<PhysicalKey, KeyInterceptMode>);

impl PluginKeybinds {
    /// Claim `key` in `mode`. Idempotent — registering the same key again
    /// (same or a different mode) simply replaces the prior claim; there is
    /// no error to report because there is nothing that could fail.
    pub fn register(&mut self, key: PhysicalKey, mode: KeyInterceptMode) {
        self.0.insert(key, mode);
    }

    /// Release a claim. A no-op if `key` was never registered.
    pub fn unregister(&mut self, key: &PhysicalKey) {
        self.0.remove(key);
    }

    /// The mode a plugin claimed `key` in, or `None` if unclaimed — what
    /// `lodestone_shell::app::input::resolve_key`'s plugin arm reads before
    /// deciding whether to swallow a physical key ahead of gameplay.
    #[must_use]
    pub fn mode_of(&self, key: &PhysicalKey) -> Option<KeyInterceptMode> {
        self.0.get(key).copied()
    }

    /// Whether anything is registered at all — lets the shell skip the ECS
    /// read entirely on the overwhelmingly common "no plugin has claimed any
    /// key" path without the caller needing its own separate flag.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One physical key transition a plugin has claimed via [`PluginKeybinds`]
/// (either mode) — pressed on `true`, released on `false`.
///
/// Observation vocabulary per `docs/plugin-api.md`'s doctrine clause 1: the
/// two facts a real key event carries, nothing else — no `winit::KeyCode`
/// value, no scancode, no modifier state. A plugin that also needs Ctrl/Shift
/// is exactly as blocked as gameplay code is today; neither is modelled as
/// ECS state yet.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct PluginKeyEvent {
    pub key: PhysicalKey,
    pub pressed: bool,
}

/// Raw key transitions the shell has queued for
/// [`drain_pending_plugin_key_events`] to fold into
/// `Messages<PluginKeyEvent>` on this tick's [`crate::sets::TickSet::Input`].
///
/// Written directly by the shell, outside any system — the same shape
/// `lodestone_shell::interact`'s `Attacking`/`RayTarget` resources are fed by
/// raw window input — and drained (not merely read) here, so a key event
/// queued between two ticks is delivered exactly once.
#[derive(Resource, Debug, Clone, Default)]
pub struct PendingPluginKeyEvents(pub Vec<PluginKeyEvent>);

/// [`crate::sets::TickSet::Input`]'s first real occupant: turns this tick's
/// queued raw key transitions into `Messages<PluginKeyEvent>`. See the module
/// doc for the full design and why this closes that set's reserved-but-empty
/// gap.
pub fn drain_pending_plugin_key_events(
    mut pending: ResMut<PendingPluginKeyEvents>,
    mut events: MessageWriter<PluginKeyEvent>,
) {
    for event in pending.0.drain(..) {
        events.write(event);
    }
}

/// Ages `Messages<PluginKeyEvent>`'s double buffer once per tick — the same
/// requirement, and the same anchor (`TickSet::Send`, last), as
/// `crate::events::age_game_event_bus` and for the identical reason: this
/// codebase never calls bevy's own `App::update()`, so nothing else trims the
/// buffer. See that function's doc for the full explanation.
pub fn age_plugin_key_events(mut events: ResMut<Messages<PluginKeyEvent>>) {
    events.update();
}

#[cfg(test)]
mod tests {
    use bevy_ecs::message::MessageReader;
    use bevy_ecs::schedule::IntoScheduleConfigs;
    use bevy_ecs::system::ResMut;
    use bevy_ecs::world::World;

    use super::*;
    use crate::GameTick;

    #[test]
    fn an_unregistered_key_has_no_mode() {
        let binds = PluginKeybinds::default();
        assert_eq!(binds.mode_of(&PhysicalKey::named("KeyF")), None);
        assert!(binds.is_empty());
    }

    #[test]
    fn registering_a_key_reports_its_mode_and_is_no_longer_empty() {
        let mut binds = PluginKeybinds::default();
        binds.register(PhysicalKey::named("KeyF"), KeyInterceptMode::Consume);
        assert_eq!(
            binds.mode_of(&PhysicalKey::named("KeyF")),
            Some(KeyInterceptMode::Consume)
        );
        assert!(!binds.is_empty());
        // A different key, still unregistered: registering one key must not
        // claim every key.
        assert_eq!(binds.mode_of(&PhysicalKey::named("KeyG")), None);
    }

    /// Registering twice replaces rather than errors or stacks — the
    /// "idempotent, single owner" contract the module doc states.
    #[test]
    fn registering_the_same_key_twice_replaces_the_mode() {
        let mut binds = PluginKeybinds::default();
        binds.register(PhysicalKey::named("KeyF"), KeyInterceptMode::Observe);
        binds.register(PhysicalKey::named("KeyF"), KeyInterceptMode::Consume);
        assert_eq!(
            binds.mode_of(&PhysicalKey::named("KeyF")),
            Some(KeyInterceptMode::Consume)
        );
    }

    #[test]
    fn unregistering_clears_the_claim() {
        let mut binds = PluginKeybinds::default();
        binds.register(PhysicalKey::named("KeyF"), KeyInterceptMode::Consume);
        binds.unregister(&PhysicalKey::named("KeyF"));
        assert_eq!(binds.mode_of(&PhysicalKey::named("KeyF")), None);
        assert!(binds.is_empty());
    }

    /// The negative control: two keys with different names are genuinely
    /// distinct entries, not a hash collision or string-prefix mixup — the
    /// pairwise-distinct-fixture discipline `CLAUDE.md` asks for, applied to
    /// a hash map key rather than a wire field.
    #[test]
    fn two_different_keys_do_not_share_a_claim() {
        let mut binds = PluginKeybinds::default();
        binds.register(PhysicalKey::named("KeyF"), KeyInterceptMode::Consume);
        binds.register(PhysicalKey::named("KeyG"), KeyInterceptMode::Observe);
        assert_eq!(
            binds.mode_of(&PhysicalKey::named("KeyF")),
            Some(KeyInterceptMode::Consume)
        );
        assert_eq!(
            binds.mode_of(&PhysicalKey::named("KeyG")),
            Some(KeyInterceptMode::Observe)
        );
    }

    #[derive(Resource, Default)]
    struct SeenEvents(Vec<PluginKeyEvent>);

    fn collect_plugin_key_events(
        mut reader: MessageReader<PluginKeyEvent>,
        mut seen: ResMut<SeenEvents>,
    ) {
        for event in reader.read() {
            seen.0.push(event.clone());
        }
    }

    /// The real system, driven through a real schedule: a raw key transition
    /// queued into [`PendingPluginKeyEvents`] — the way the shell queues one,
    /// outside any system — reaches a plugin's `MessageReader` after
    /// [`crate::GameTick`] runs, and the queue is empty afterward (drained,
    /// not merely read, so nothing double-delivers next tick).
    #[test]
    fn a_queued_key_event_reaches_a_plugin_reader_via_a_real_tick() {
        let mut app = bevy_app::App::new();
        app.init_resource::<PendingPluginKeyEvents>();
        app.add_message::<PluginKeyEvent>();
        app.init_resource::<SeenEvents>();
        app.add_systems(
            GameTick,
            (drain_pending_plugin_key_events, collect_plugin_key_events).chain(),
        );

        app.world_mut()
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(PluginKeyEvent {
                key: PhysicalKey::named("KeyF"),
                pressed: true,
            });

        app.world_mut().run_schedule(GameTick);

        let seen = &app.world().resource::<SeenEvents>().0;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].key, PhysicalKey::named("KeyF"));
        assert!(seen[0].pressed);
        assert!(
            app.world()
                .resource::<PendingPluginKeyEvents>()
                .0
                .is_empty(),
            "the pending queue must be drained, not merely read"
        );
    }

    /// The negative control for the test above: an *unclaimed* tick — nothing
    /// pushed into [`PendingPluginKeyEvents`] — must deliver nothing, so the
    /// positive result above is not a fixture that always reports one event
    /// regardless of what was queued.
    #[test]
    fn an_empty_pending_queue_delivers_no_events() {
        let mut app = bevy_app::App::new();
        app.init_resource::<PendingPluginKeyEvents>();
        app.add_message::<PluginKeyEvent>();
        app.init_resource::<SeenEvents>();
        app.add_systems(
            GameTick,
            (drain_pending_plugin_key_events, collect_plugin_key_events).chain(),
        );

        app.world_mut().run_schedule(GameTick);

        assert!(app.world().resource::<SeenEvents>().0.is_empty());
    }

    /// [`age_plugin_key_events`] genuinely ages the double buffer — a reader
    /// that has already read this tick's batch does not see it again after
    /// aging runs, mirroring
    /// `crate::events::tests::a_written_game_event_reaches_a_reader_after_the_bus_plugin_is_added`'s
    /// sibling coverage for `GameEvent`.
    #[test]
    fn aging_drops_messages_no_reader_will_see_again() {
        let mut world = World::new();
        world.init_resource::<Messages<PluginKeyEvent>>();
        world.write_message(PluginKeyEvent {
            key: PhysicalKey::named("KeyF"),
            pressed: true,
        });

        // `Messages::update()` must run twice before a message written before
        // the *first* call is actually dropped (bevy's double-buffer), so
        // assert the two-call boundary rather than one call being enough —
        // getting this wrong would silently under-test the aging system.
        fn age(world: &mut World) {
            world
                .get_resource_mut::<Messages<PluginKeyEvent>>()
                .unwrap()
                .update();
        }
        age(&mut world);
        assert_eq!(
            world.resource::<Messages<PluginKeyEvent>>().len(),
            1,
            "one update: the message must still be readable"
        );
        age(&mut world);
        assert_eq!(
            world.resource::<Messages<PluginKeyEvent>>().len(),
            0,
            "two updates: the message must now be gone"
        );
    }

    /// [`age_plugin_key_events`] is itself a valid system — checked the cheap
    /// way, by actually registering and running it, rather than merely
    /// compiling its signature.
    #[test]
    fn age_plugin_key_events_runs_as_a_real_system() {
        let mut app = bevy_app::App::new();
        app.add_message::<PluginKeyEvent>();
        app.add_systems(GameTick, age_plugin_key_events);
        app.world_mut().write_message(PluginKeyEvent {
            key: PhysicalKey::named("KeyF"),
            pressed: false,
        });
        app.world_mut().run_schedule(GameTick);
        // No panic, and the resource is still reachable — that is the whole
        // assertion; the aging *behaviour* is covered directly above without
        // needing a schedule at all.
        let _ = app.world().resource::<Messages<PluginKeyEvent>>();
    }
}
