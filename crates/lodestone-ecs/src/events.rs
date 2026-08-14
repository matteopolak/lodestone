//! The plugin event bus: a typed, version-free `Message` mirror
//! of every [`ClientEvent`] the client decodes, for a plugin that wants to
//! *observe* the event stream directly rather than poll component state
//! after the fact.
//!
//! # Why `GameEvent` wraps `ClientEvent` rather than inventing a second
//! vocabulary
//!
//! `ClientEvent` is already version-free, `Clone`, and `#[non_exhaustive]`
//! (`lodestone_model::event`) — a second ~107-variant enum mirroring it would
//! be exactly the staleness factory `CLAUDE.md` calls this repo's
//! most-documented defect class: two enums drift, and nothing forces the
//! second one to grow a variant when the first one does. `docs/bevy-migration.md`
//! §5.1's original `RawPacket` proposal is the version-*opaque* half of the
//! plugin event bus (wire bytes, still unbuilt); this is the version-*free*,
//! already-decoded half, and it costs nothing extra to keep in sync because it
//! is not a copy —
//! it is `ClientEvent` itself, one field deep.
//!
//! # Why this cannot become a sixth silent-drop router
//!
//! `docs/plugin-api.md`'s doctrine names three existing routers
//! (`lodestone_ecs::ingest::handles_event`, `lodestone_ecs::session::handles_event`,
//! `lodestone_shell::net::forward`) whose terminal `_ =>` arm is an
//! "island-factory": a wildcard that is indistinguishable, at the call site,
//! from a decision, so a new `ClientEvent` variant can compile with no route
//! to any of them and vanish silently. The single write site for this bus
//! (`lodestone_client::state::SharedState::apply`'s `push_to_game_event_bus`)
//! is the opposite shape *by construction*: it does not match on the event at
//! all, so there is no arm to forget. See
//! `lodestone_client::state::tests::game_event_bus_write_site_has_no_match_on_the_event`
//! for the source-scan guard that keeps it that way.
//!
//! # Gated off by default
//!
//! With the bus on, every event — including the ones `SharedState::apply`
//! currently routes to `LocalEcho` with no ECS lock at all — has to take the
//! `EcsHandle` write lock to reach [`Messages<GameEvent>`]. That is a real,
//! measurable cost this crate has no business imposing on every client that
//! never asked for the bus, so [`GameEventBus`] is a marker resource nothing
//! inserts by default: `SharedState` checks for it once, at construction
//! (before any hot-path lock is taken), and caches the answer. See that
//! type's own doc for the exact mechanics.

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{IntoScheduleConfigs, Message, Messages, ResMut, Resource};
use lodestone_model::ClientEvent;

use crate::schedules::GameTick;
use crate::sets::TickSet;

/// One [`ClientEvent`] the client decoded, mirrored onto the plugin event
/// bus. A plugin reads these with a `bevy_ecs::message::MessageReader<GameEvent>`
/// system param, ordered against other plugins with
/// [`crate::sets::EventPriority`].
///
/// `pub` field, not an accessor: a plugin already depends on
/// `lodestone-model` for every other vocabulary type
/// (`docs/plugin-api.md`'s "the plugin API and the internal API are the same
/// thing"), so there is nothing to hide behind a method here.
#[derive(Message, Debug, Clone, PartialEq)]
pub struct GameEvent(pub ClientEvent);

/// Marker resource a plugin's own [`Plugin::build`] inserts (directly, or by
/// adding [`GameEventBusPlugin`]) to opt into the bus.
///
/// # Why a marker resource rather than a runtime toggle
///
/// A bevy plugin is registered once, at `App`-construction time, before the
/// `World` it configures is ever wrapped in an
/// [`crate::EcsHandle`](`crate::handle::EcsHandle`) and handed to a driver —
/// there is no "later" at which a plugin list changes underneath a running
/// client. `lodestone_client::state::SharedState` therefore checks for this
/// resource **once**, in its constructor, and caches the answer as a plain
/// `bool`: enabling the bus is a decision made when the `World` is built, not
/// a lever pulled mid-session, so nothing needs to be `Arc<AtomicBool>` or
/// re-checked per event.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct GameEventBus;

/// Registers the bus on an `App`: the [`GameEventBus`] marker,
/// [`Messages<GameEvent>`] itself (`add_message`), and the system that ages
/// the message double-buffer once per [`crate::GameTick`].
///
/// # The aging system is not optional
///
/// `bevy_ecs`'s `Messages<T>` needs periodic `Messages::update()` calls to
/// drop messages nobody will read again — normally driven by a system bevy's
/// own `Main`/`First` schedule runs every `App::update()`. This codebase
/// never calls `App::update()`: the driver runs `NetIngest`/`GameTick`/
/// `Extract` by hand (`docs/bevy-migration.md` §4.1(b)), so without a system
/// of our own, a long-running client with the bus enabled would grow
/// `Messages<GameEvent>` without bound. [`age_game_event_bus`] is that
/// system, anchored at [`TickSet::Send`] (last), so any reader ordered
/// anywhere in `GameTick` — or in `NetIngest`, which runs before it — has
/// already had its chance to read this tick's batch before it ages out.
///
/// A reader living in `Update` or `Extract` is a **named follow-up**: those
/// schedules have no aging system of their own yet, so a `MessageReader`
/// there would still work (messages are still written and still readable),
/// but the buffer would only ever be trimmed by `GameTick`'s system, on
/// whatever cadence `GameTick` itself runs at — fine for the toy plugin,
/// which reads from `GameTick`, but a real HUD-facing observer in `Extract`
/// should get its own aging point before this ships beyond a toy.
#[derive(Debug, Default)]
pub struct GameEventBusPlugin;

impl Plugin for GameEventBusPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<crate::CorePlugin>() {
            app.add_plugins(crate::CorePlugin);
        }
        app.init_resource::<GameEventBus>();
        app.add_message::<GameEvent>();
        app.add_systems(GameTick, age_game_event_bus.in_set(TickSet::Send));
    }
}

/// `TickSet::Send`: ages [`Messages<GameEvent>`]'s double buffer once per
/// tick. See [`GameEventBusPlugin`]'s doc for why nothing else calls this.
fn age_game_event_bus(mut messages: ResMut<Messages<GameEvent>>) {
    messages.update();
}

#[cfg(test)]
mod tests {
    use bevy_ecs::message::MessageReader;
    use bevy_ecs::resource::Resource;
    use bevy_ecs::system::ResMut;
    use bevy_ecs::world::World;
    use lodestone_model::ClientEvent;

    use super::{GameEvent, GameEventBus, GameEventBusPlugin};
    use crate::GameTick;

    #[derive(Resource, Default)]
    struct SeenCount(u32);

    fn count_game_events(mut events: MessageReader<GameEvent>, mut count: ResMut<SeenCount>) {
        for _ in events.read() {
            count.0 += 1;
        }
    }

    /// The marker resource is genuinely absent on a bare `World` — the "gated
    /// off by default" half of the design, checked as a fact about a fresh
    /// `World` rather than assumed.
    #[test]
    fn the_marker_resource_is_absent_by_default() {
        let world = World::new();
        assert!(world.get_resource::<GameEventBus>().is_none());
    }

    /// The positive half of the on/off pair: installing [`GameEventBusPlugin`]
    /// makes the marker present and makes `Messages<GameEvent>`
    /// (`bevy_ecs::message::Messages`) a real resource a
    /// `MessageReader<GameEvent>` system can observe.
    #[test]
    fn a_written_game_event_reaches_a_reader_after_the_bus_plugin_is_added() {
        let mut app = bevy_app::App::new();
        app.add_plugins(GameEventBusPlugin);
        app.init_resource::<SeenCount>();
        app.add_systems(GameTick, count_game_events);

        assert!(
            app.world().get_resource::<GameEventBus>().is_some(),
            "GameEventBusPlugin must insert the marker resource"
        );

        app.world_mut()
            .write_message(GameEvent(ClientEvent::Ping { id: 7 }));
        app.world_mut().run_schedule(GameTick);

        assert_eq!(app.world().resource::<SeenCount>().0, 1);
    }

    /// The negative control for the test above: on a `World` that never added
    /// [`GameEventBusPlugin`], `Messages<GameEvent>` does not exist, so
    /// `write_message` is a documented, harmless no-op (`None`) rather than a
    /// panic — proving the "gated off" half is not merely "the plugin exists
    /// but the tests never noticed a required panic".
    #[test]
    fn writing_a_game_event_with_no_bus_installed_is_a_harmless_no_op() {
        let mut world = World::new();
        assert!(
            world
                .write_message(GameEvent(ClientEvent::Ping { id: 1 }))
                .is_none()
        );
    }

    /// Sanity on the wrapped type: a `GameEvent` really does carry the exact
    /// `ClientEvent` it was built from, with no lossy conversion — the whole
    /// point of wrapping rather than re-deriving a second vocabulary.
    #[test]
    fn game_event_round_trips_the_client_event_unchanged() {
        let event = ClientEvent::Ping { id: 42 };
        let wrapped = GameEvent(event.clone());
        assert_eq!(wrapped.0, event);
    }
}
