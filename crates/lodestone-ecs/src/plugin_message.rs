//! Issue #107 — cross-plugin custom event messages.
//!
//! # What it is
//!
//! The convention and the one piece of machinery that makes it work: how a
//! plugin publishes its own message type so *another* plugin can subscribe
//! **without depending on the publisher's crate**. Bukkit's
//! `pluginManager.callEvent(MyOwnEvent)` plus a listener in an unrelated
//! plugin — how a plugin ecosystem composes (an economy plugin fires
//! `EconomyTransactionEvent`, a stats plugin listens without a compile-time
//! dependency on it).
//!
//! # What was actually missing, re-verified
//!
//! The issue's own analysis is correct and worth restating, because it means
//! this is *mostly* a convention problem: a native plugin is an
//! `impl bevy_app::Plugin` added at compile time, so any plugin can already
//! `#[derive(Message)]` a type and another can read it with `MessageReader` —
//! `bevy_ecs` does not restrict this. What was missing is the **pattern**, an
//! **example proving it end to end**, and one real ergonomic blocker described
//! below.
//!
//! # The three-crate shape
//!
//! ```text
//!   lodestone-shop-api      the public message type, and nothing else
//!        ^          ^
//!        |          |
//!  lodestone-shop   lodestone-shop-stats
//!   (publisher)      (subscriber)
//! ```
//!
//! The subscriber depends on **`-api`**, never on the publisher. That is the
//! whole point: `lodestone-shop-stats/Cargo.toml`'s `[dependencies]` has no
//! `lodestone-shop` in it, and
//! `crates/plugins/lodestone-shop-stats/tests/cross_plugin_message.rs` proves
//! the message still arrives. A `-api` crate is cheap — a message type, a
//! registration plugin, no logic — and it is the unit a third party actually
//! wants to depend on.
//!
//! The alternative the issue floats, one shared `lodestone-plugin-messages`
//! crate everybody opts into, is deliberately **not** what landed: it would be
//! a single file every plugin author has to get their type merged into, which is
//! the opposite of "without a compile-time dependency". Per-family `-api`
//! crates need no coordination at all.
//!
//! # The one real blocker, and why [`PluginMessageAppExt`] exists
//!
//! `bevy_app` **panics** on a duplicate `add_plugins` — measured, not assumed:
//! `Error adding plugin …: plugin was already added in application`. So a naive
//! `PluginMessagePlugin::<ShopPurchase>::default()` added by *both* the
//! publisher and the subscriber is a startup crash, and neither side can know
//! whether the other is installed — which is precisely the situation
//! cross-plugin messaging is about.
//!
//! [`PluginMessageAppExt::add_plugin_message`] is therefore **idempotent by
//! construction**: it checks `is_plugin_added` first, so *every* interested
//! party can declare the message type and the first one wins. Nobody has to
//! document "the publisher registers it" — a rule that fails the moment a
//! subscriber is installed without the publisher, which for a message bus is a
//! completely normal configuration.
//!
//! # Why the aging system is not optional
//!
//! `bevy_ecs`'s `Messages<T>` needs periodic `Messages::update()` or it grows
//! without bound. [`crate::events`] already learned this for `GameEvent` and
//! anchors its own aging system in [`crate::TickSet::Send`]; the same reasoning
//! applies to every plugin-defined message, so [`PluginMessagePlugin`] carries
//! that aging system generically rather than leaving each plugin author to
//! rediscover it. Anchored at [`crate::TickSet::Send`] (last in `GameTick`), so
//! a reader anywhere in `NetIngest` or `GameTick` has already had its chance
//! this tick.
//!
//! A reader in `Update` or `Extract` inherits the same caveat
//! [`crate::events`] documents: it still works, but the buffer is only trimmed
//! on `GameTick`'s cadence.
//!
//! # Native tier only
//!
//! Two guest WASM modules cannot share a Rust type, so none of this applies to
//! the WASM host — that needs its own cross-plugin messaging story, tracked
//! separately in this epic. Stated here so the boundary is explicit rather than
//! assumed to generalise.

use std::marker::PhantomData;

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{IntoScheduleConfigs, Message, Messages, ResMut};

use crate::schedules::GameTick;
use crate::sets::TickSet;

/// Registers a plugin-defined message type `T`: `Messages<T>` itself, plus the
/// per-tick aging system that keeps its double buffer from growing without
/// bound.
///
/// **Prefer [`PluginMessageAppExt::add_plugin_message`] over adding this
/// directly** — adding it twice panics, and "twice" is the normal case when a
/// publisher and a subscriber both declare the type they share. See the module
/// doc.
#[derive(Debug)]
pub struct PluginMessagePlugin<T: Message>(PhantomData<fn() -> T>);

impl<T: Message> Default for PluginMessagePlugin<T> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T: Message> Plugin for PluginMessagePlugin<T> {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<crate::CorePlugin>() {
            app.add_plugins(crate::CorePlugin);
        }
        app.add_message::<T>();
        app.add_systems(GameTick, age_plugin_messages::<T>.in_set(TickSet::Send));
    }
}

/// [`crate::TickSet::Send`]: ages `Messages<T>`'s double buffer once per tick.
/// See the module doc for why nothing else calls this.
fn age_plugin_messages<T: Message>(mut messages: ResMut<Messages<T>>) {
    messages.update();
}

/// The idempotent registration call every party to a shared message type uses.
///
/// Implemented on `App` so it reads as one line in a `Plugin::build`.
pub trait PluginMessageAppExt {
    /// Declare `T` as a cross-plugin message. Safe to call from **any number**
    /// of plugins, in any order — the first call registers, later calls are
    /// no-ops.
    ///
    /// This is the whole ergonomic point: a publisher and a subscriber that have
    /// never heard of each other both call this, neither knows whether the other
    /// is installed, and the `App` builds either way.
    fn add_plugin_message<T: Message>(&mut self) -> &mut Self;
}

impl PluginMessageAppExt for App {
    fn add_plugin_message<T: Message>(&mut self) -> &mut Self {
        if !self.is_plugin_added::<PluginMessagePlugin<T>>() {
            self.add_plugins(PluginMessagePlugin::<T>::default());
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use bevy_app::App;
    use bevy_ecs::message::{MessageReader, MessageWriter};
    use bevy_ecs::prelude::{IntoScheduleConfigs, Message, Messages};
    use bevy_ecs::resource::Resource;
    use bevy_ecs::system::ResMut;

    use super::{PluginMessageAppExt, PluginMessagePlugin};
    use crate::schedules::GameTick;
    use crate::sets::EventPriority;

    /// Stands in for a `-api` crate's published type.
    #[derive(Message, Debug, Clone, PartialEq)]
    struct Purchase {
        coins: u32,
    }

    /// A second type, so the "one registration per type" claim is checked
    /// against two rather than assumed from one.
    #[derive(Message, Debug, Clone, PartialEq)]
    struct Refund;

    #[derive(Resource, Default)]
    struct Seen(Vec<u32>);

    fn publish(mut out: MessageWriter<Purchase>) {
        out.write(Purchase { coins: 5 });
    }

    fn subscribe(mut inbox: MessageReader<Purchase>, mut seen: ResMut<Seen>) {
        for msg in inbox.read() {
            seen.0.push(msg.coins);
        }
    }

    /// The registration is genuinely idempotent — the property the whole
    /// pattern rests on. Two independent "plugins" both declaring the type must
    /// not panic, which is what a bare `add_plugins` would do.
    #[test]
    fn two_independent_registrations_of_the_same_message_do_not_panic() {
        let mut app = App::new();
        app.add_plugin_message::<Purchase>();
        app.add_plugin_message::<Purchase>();
        app.add_plugin_message::<Purchase>();
        assert!(app.world().get_resource::<Messages<Purchase>>().is_some());
    }

    /// **The control for the test above.** A bare `add_plugins` of the same
    /// plugin twice *does* panic, which is what makes `add_plugin_message`'s
    /// `is_plugin_added` check load-bearing rather than defensive decoration.
    /// Without this, the idempotence test could be passing against a bevy that
    /// tolerates duplicates anyway, and the extension trait would be pointless.
    #[test]
    #[should_panic(expected = "already added")]
    fn adding_the_plugin_directly_twice_panics_which_is_why_the_ext_exists() {
        let mut app = App::new();
        app.add_plugins(PluginMessagePlugin::<Purchase>::default());
        app.add_plugins(PluginMessagePlugin::<Purchase>::default());
    }

    /// Distinct types are distinct registrations: registering `Purchase` must
    /// not make `Refund` appear, or `is_plugin_added` would be keyed on
    /// something other than `T` and the idempotence above would silently
    /// suppress a real registration.
    #[test]
    fn registering_one_message_type_does_not_register_another() {
        let mut app = App::new();
        app.add_plugin_message::<Purchase>();
        assert!(app.world().get_resource::<Messages<Purchase>>().is_some());
        assert!(app.world().get_resource::<Messages<Refund>>().is_none());
        app.add_plugin_message::<Refund>();
        assert!(app.world().get_resource::<Messages<Refund>>().is_some());
    }

    /// A message crosses from a writer to a reader that share only the type —
    /// the in-crate rehearsal of what
    /// `crates/plugins/lodestone-shop-stats/tests/cross_plugin_message.rs`
    /// proves across real crate boundaries.
    #[test]
    fn a_message_crosses_from_a_writer_to_an_unrelated_reader() {
        let mut app = App::new();
        app.add_plugin_message::<Purchase>();
        app.init_resource::<Seen>();
        // Ordered through `EventPriority`, the cross-plugin anchor two plugins
        // that have never heard of each other can both name.
        app.add_systems(GameTick, publish.in_set(EventPriority::Low));
        app.add_systems(GameTick, subscribe.in_set(EventPriority::High));

        app.world_mut().run_schedule(GameTick);
        assert_eq!(app.world().resource::<Seen>().0, vec![5]);
    }

    /// **The control for the crossing test**: with no registration, the write is
    /// a documented no-op (`None`) rather than a delivery, so the assertion
    /// above is measuring the registration and not merely that two systems ran.
    #[test]
    fn with_no_registration_a_write_is_a_no_op() {
        let mut world = bevy_ecs::world::World::new();
        assert!(world.write_message(Purchase { coins: 5 }).is_none());
    }

    /// The aging system really runs and really trims — the unbounded-growth
    /// hazard the module doc names. Asserting the *count* of retained messages
    /// after a known number of ticks, not merely that `update` was callable.
    ///
    /// `Messages<T>` is a double buffer, so a message written now survives one
    /// aging pass and is dropped by the second. This asserts that exact
    /// two-pass behaviour rather than "it went down eventually".
    #[test]
    fn the_aging_system_trims_the_double_buffer_after_two_ticks() {
        let mut app = App::new();
        app.add_plugin_message::<Purchase>();

        app.world_mut().write_message(Purchase { coins: 1 });
        assert_eq!(app.world().resource::<Messages<Purchase>>().len(), 1);

        app.world_mut().run_schedule(GameTick);
        assert_eq!(
            app.world().resource::<Messages<Purchase>>().len(),
            1,
            "one aging pass must still retain it — a double buffer, not a single"
        );

        app.world_mut().run_schedule(GameTick);
        assert_eq!(
            app.world().resource::<Messages<Purchase>>().len(),
            0,
            "two aging passes must drop it, or the buffer grows without bound"
        );
    }

    /// **The control for the aging gate.** With `Messages<Purchase>` inserted by
    /// hand and no registered aging system, the buffer *never* shrinks — so the
    /// zero above is the system working, not messages vanishing for some other
    /// reason.
    #[test]
    fn with_no_aging_system_the_buffer_never_shrinks() {
        let mut app = App::new();
        app.add_plugins(crate::CorePlugin);
        app.world_mut().insert_resource(Messages::<Purchase>::default());
        app.world_mut().write_message(Purchase { coins: 1 });
        for _ in 0..10 {
            app.world_mut().run_schedule(GameTick);
        }
        assert_eq!(
            app.world().resource::<Messages<Purchase>>().len(),
            1,
            "no aging system is registered, so nothing can trim this"
        );
    }
}
