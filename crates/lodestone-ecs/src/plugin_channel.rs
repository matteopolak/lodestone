//! Issue #301 — typed, per-channel `custom_payload` delivery to a plugin.
//!
//! # What it is
//!
//! The dispatch layer issue #301 asks for, at the one layer where a *plugin*
//! can actually be the consumer: a plugin declares a channel with
//! [`PluginChannel`], calls [`PluginChannelAppExt::add_plugin_channel`] in its
//! own `Plugin::build`, and reads its own decoded type with an ordinary
//! `MessageReader<T>`. No plugin ever matches on
//! [`ClientEvent::CustomPayload`], string-compares a channel name, or parses a
//! [`ResourceKey`] itself.
//!
//! # Why this exists when `ChannelRegistry` already does
//!
//! `lodestone_client::ChannelRegistry` (the same issue) is a **passive fold a
//! caller drives by hand**, and its module doc is explicit that this is
//! deliberate: `lodestone_model::event::route` sends `CustomPayload` to
//! `Route::NOWHERE`, so a registry wired into `Driver` would have nowhere to be
//! configured from. That decision is correct and is **not** reopened here —
//! nothing in this module touches `route`, `Driver`, or the ingest/session/shell
//! pipeline.
//!
//! But it leaves the plugin case unserved, and that is the half of #301 that was
//! an island. A plugin is an `impl bevy_app::Plugin` inside the `App`; it does
//! not own the driver's event stream and has no call site from which to drive a
//! caller-held `ChannelRegistry`. So `ChannelRegistry` is reachable only by an
//! embedder that already owns the stream, and was in fact constructed nowhere
//! outside its own test module. The plugin's stream is
//! [`crate::events::GameEvent`], and that is what this module folds.
//!
//! # The bus is a hard requirement, so this installs it
//!
//! [`crate::events::GameEventBusPlugin`] is opt-in and inserts a marker resource
//! `SharedState` checks **once, at construction**. Without it,
//! `push_to_game_event_bus` never runs and `Messages<GameEvent>` never receives
//! anything — so a plugin that called `add_plugin_channel` and nothing else
//! would compile, register, tick, and receive **zero payloads forever**. That is
//! exactly the island shape this module exists to close, so
//! [`PluginChannelPlugin`] adds the bus itself rather than documenting a second
//! line the plugin author must remember. `channel_dispatch_requires_no_second_opt_in`
//! is the gate.
//!
//! # A bad channel constant is a startup panic, not silence
//!
//! [`PluginChannel::CHANNEL`] is a `&'static str` parsed to a [`ResourceKey`]
//! **once, in `build`**, and a parse failure panics there. The alternative —
//! parse per event, or compare `Display` output — fails *silently*: a typo'd or
//! non-canonical constant simply never matches any payload, and the plugin author
//! sees a channel that is registered, ticking, and permanently empty. A startup
//! panic names the offending type. `a_malformed_channel_constant_panics_at_build`
//! is the control.
//!
//! # Ordering
//!
//! [`dispatch_plugin_channel`] is `.before(EventPriority::Lowest)` in
//! [`GameTick`] — before *every* one of the six cross-plugin tiers, so a
//! subscriber at any tier (including `Lowest`) sees this tick's payloads on this
//! tick rather than the next. It is deliberately not `.in_set(..)` of any tier:
//! a dispatcher that shared a tier with its own subscribers would be unordered
//! against them, which costs a tick at best.
//!
//! `Messages<T>` aging is inherited from [`crate::plugin_message`] rather than
//! reimplemented: `add_plugin_channel` calls
//! [`crate::plugin_message::PluginMessageAppExt::add_plugin_message`], so a
//! channel type is an ordinary cross-plugin message with the documented
//! [`crate::TickSet::Send`] aging point, and two plugins may declare the same
//! channel type idempotently for the same reason.
//!
//! # What this does not do
//!
//! Nothing here *sends* a payload. Outbound is
//! `lodestone_model::ClientAction::SendCustomPayload`, which has no producer in
//! this workspace at all; `minecraft:brand` goes out through the dedicated
//! `ClientAction::SendBrand` from `lodestone_client::driver` on entering
//! `Configuration`. Inbound-only is the whole scope of this module.

use std::marker::PhantomData;

use bevy_app::{App, Plugin};
use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::{IntoScheduleConfigs, Message, ResMut, Resource};
use lodestone_model::{ClientEvent, ResourceKey};

use crate::events::{GameEvent, GameEventBusPlugin};
use crate::plugin_message::PluginMessageAppExt;
use crate::schedules::GameTick;
use crate::sets::EventPriority;

/// A plugin-defined `custom_payload` channel: which channel to listen on, and
/// how to turn its raw bytes into `Self`.
///
/// Implement this on the message type the plugin actually wants to read. The
/// type is an ordinary `#[derive(Message)]` struct — see
/// `crates/plugins/lodestone-server-brand` for the worked example.
pub trait PluginChannel: Message + Sized {
    /// The wire channel identifier, e.g. `"minecraft:brand"`.
    ///
    /// Parsed to a [`ResourceKey`] once at `App`-build time. **A value that
    /// does not parse panics at startup** — see the module doc for why that is
    /// better than never matching.
    const CHANNEL: &'static str;

    /// Decodes one payload's raw bytes.
    ///
    /// Return `None` for a payload that does not have the shape this channel
    /// expects. That is not an error and never disconnects: vanilla's own
    /// fallback for an unparseable payload is `DiscardedPayload`, read-and-drop,
    /// and a third-party channel may legitimately carry several message shapes.
    /// A rejected payload is counted in
    /// [`PluginChannelState::rejected`] so "nothing arrived" and "it arrived and
    /// would not decode" stay distinguishable.
    fn decode(data: &[u8]) -> Option<Self>;
}

/// Per-channel dispatch state, and the resource to assert in a test.
///
/// **Assert this, not `is_plugin_added`.** A plugin whose `build` stopped
/// inserting what its consumers read still passes
/// `App::is_plugin_added::<PluginChannelPlugin<T>>()`; a non-zero
/// [`Self::matched`] is a fact about payloads that really traversed the fold.
#[derive(Resource, Debug)]
pub struct PluginChannelState<T: PluginChannel> {
    key: ResourceKey,
    matched: u64,
    rejected: u64,
    _marker: PhantomData<fn() -> T>,
}

impl<T: PluginChannel> PluginChannelState<T> {
    /// The parsed channel this type listens on.
    #[must_use]
    pub fn key(&self) -> &ResourceKey {
        &self.key
    }

    /// How many payloads on this channel decoded and were written as `T`.
    #[must_use]
    pub fn matched(&self) -> u64 {
        self.matched
    }

    /// How many payloads arrived on this channel but [`PluginChannel::decode`]
    /// refused.
    ///
    /// Exists so a gate can tell "the payload never reached the fold" from "the
    /// payload reached the fold and the decoder rejected it" — two failures that
    /// look identical from the subscriber's side.
    #[must_use]
    pub fn rejected(&self) -> u64 {
        self.rejected
    }
}

/// Registers one [`PluginChannel`] type.
///
/// **Prefer [`PluginChannelAppExt::add_plugin_channel`]** — adding a bevy plugin
/// twice panics, and two plugins sharing one channel type is the normal case.
#[derive(Debug)]
pub struct PluginChannelPlugin<T: PluginChannel>(PhantomData<fn() -> T>);

impl<T: PluginChannel> Default for PluginChannelPlugin<T> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T: PluginChannel> Plugin for PluginChannelPlugin<T> {
    /// # Panics
    ///
    /// If [`PluginChannel::CHANNEL`] is not a parseable [`ResourceKey`]. See the
    /// module doc: the alternative is a channel that is silently never matched.
    fn build(&self, app: &mut App) {
        let key: ResourceKey = T::CHANNEL.parse().unwrap_or_else(|error| {
            panic!(
                "PluginChannel::CHANNEL for `{}` is {:?}, which is not a valid \
                 namespaced identifier: {error}. A channel that does not parse \
                 would never match any payload, so this fails loudly at build \
                 rather than delivering nothing forever (issue #301).",
                std::any::type_name::<T>(),
                T::CHANNEL,
            )
        });

        // The bus is what carries `CustomPayload` to a plugin at all; see the
        // module doc's "hard requirement" section. Idempotent so a plugin that
        // also opted in directly still builds.
        if !app.is_plugin_added::<GameEventBusPlugin>() {
            app.add_plugins(GameEventBusPlugin);
        }

        app.add_plugin_message::<T>();
        app.insert_resource(PluginChannelState::<T> {
            key,
            matched: 0,
            rejected: 0,
            _marker: PhantomData,
        });
        app.add_systems(
            GameTick,
            dispatch_plugin_channel::<T>.before(EventPriority::Lowest),
        );
    }
}

/// Folds this tick's [`GameEvent`]s, writing a `T` for every
/// [`ClientEvent::CustomPayload`] on `T::CHANNEL` that decodes.
///
/// Runs before every [`EventPriority`] tier — see the module doc on ordering.
pub fn dispatch_plugin_channel<T: PluginChannel>(
    mut events: MessageReader<GameEvent>,
    mut out: MessageWriter<T>,
    mut state: ResMut<PluginChannelState<T>>,
) {
    for GameEvent(event) in events.read() {
        let ClientEvent::CustomPayload { channel, data } = event else {
            continue;
        };
        if *channel != state.key {
            continue;
        }
        match T::decode(data) {
            Some(message) => {
                state.matched += 1;
                out.write(message);
            }
            None => state.rejected += 1,
        }
    }
}

/// The idempotent registration call a plugin uses.
pub trait PluginChannelAppExt {
    /// Declares `T` as a plugin channel: installs the game-event bus if needed,
    /// registers `Messages<T>` and its aging system, inserts
    /// [`PluginChannelState<T>`], and schedules the fold.
    ///
    /// Safe to call from any number of plugins, in any order — the first call
    /// registers and later calls are no-ops, for the same reason
    /// [`crate::plugin_message::PluginMessageAppExt::add_plugin_message`] is
    /// idempotent.
    fn add_plugin_channel<T: PluginChannel>(&mut self) -> &mut Self;
}

impl PluginChannelAppExt for App {
    fn add_plugin_channel<T: PluginChannel>(&mut self) -> &mut Self {
        if !self.is_plugin_added::<PluginChannelPlugin<T>>() {
            self.add_plugins(PluginChannelPlugin::<T>::default());
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use bevy_app::App;
    use bevy_ecs::message::MessageReader;
    use bevy_ecs::prelude::{Message, ResMut, Resource};
    use lodestone_model::{ClientEvent, ResourceKey};

    use super::{PluginChannel, PluginChannelAppExt, PluginChannelPlugin, PluginChannelState};
    use crate::events::{GameEvent, GameEventBus};
    use crate::schedules::GameTick;

    /// A channel carrying its payload as raw UTF-8, the simplest real shape.
    #[derive(Message, Debug, Clone, PartialEq, Eq)]
    struct Greeting(String);

    impl PluginChannel for Greeting {
        const CHANNEL: &'static str = "example:greet";

        fn decode(data: &[u8]) -> Option<Self> {
            std::str::from_utf8(data).ok().map(|s| Self(s.to_owned()))
        }
    }

    /// A second type on a *different* channel, to prove the fold discriminates.
    #[derive(Message, Debug, Clone, PartialEq, Eq)]
    struct Other(Vec<u8>);

    impl PluginChannel for Other {
        const CHANNEL: &'static str = "example:other";

        fn decode(data: &[u8]) -> Option<Self> {
            Some(Self(data.to_vec()))
        }
    }

    #[derive(Resource, Default)]
    struct Heard(Vec<String>);

    fn collect(mut inbox: MessageReader<Greeting>, mut heard: ResMut<Heard>) {
        for Greeting(text) in inbox.read() {
            heard.0.push(text.clone());
        }
    }

    fn channel(name: &str) -> ResourceKey {
        name.parse().expect("valid test channel")
    }

    fn payload(name: &str, data: &[u8]) -> GameEvent {
        GameEvent(ClientEvent::CustomPayload {
            channel: channel(name),
            data: data.to_vec(),
        })
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugin_channel::<Greeting>();
        app.init_resource::<Heard>();
        app.add_systems(GameTick, collect);
        app
    }

    /// The headline: a payload on the registered channel reaches an ordinary
    /// `MessageReader<T>` subscriber, decoded, in one tick — and the exact
    /// contents are asserted, not a count.
    #[test]
    fn a_payload_on_the_registered_channel_reaches_a_subscriber_decoded() {
        let mut app = app();
        app.world_mut().write_message(payload("example:greet", b"hi"));
        app.world_mut()
            .write_message(payload("example:greet", b"there"));
        app.world_mut().run_schedule(GameTick);

        assert_eq!(
            app.world().resource::<Heard>().0,
            vec!["hi".to_owned(), "there".to_owned()],
            "both payloads must arrive, decoded, in publish order"
        );
        assert_eq!(
            app.world().resource::<PluginChannelState<Greeting>>().matched(),
            2
        );
    }

    /// **The control for the test above.** An unregistered channel's payload
    /// must reach nothing — otherwise the headline is satisfied by a fold that
    /// forwards every payload regardless of channel.
    #[test]
    fn a_payload_on_another_channel_reaches_nothing() {
        let mut app = app();
        app.world_mut()
            .write_message(payload("example:other", b"not mine"));
        app.world_mut().run_schedule(GameTick);

        assert!(app.world().resource::<Heard>().0.is_empty());
        let state = app.world().resource::<PluginChannelState<Greeting>>();
        assert_eq!(state.matched(), 0);
        assert_eq!(
            state.rejected(),
            0,
            "a foreign channel must not even reach the decoder"
        );
    }

    /// `rejected` must separate "never arrived" from "arrived and would not
    /// decode" — the distinction a subscriber cannot make on its own.
    #[test]
    fn an_undecodable_payload_on_the_right_channel_counts_as_rejected() {
        let mut app = app();
        // Invalid UTF-8: `Greeting::decode` returns `None`.
        app.world_mut()
            .write_message(payload("example:greet", &[0xff, 0xfe]));
        app.world_mut().run_schedule(GameTick);

        assert!(app.world().resource::<Heard>().0.is_empty());
        let state = app.world().resource::<PluginChannelState<Greeting>>();
        assert_eq!(state.matched(), 0);
        assert_eq!(state.rejected(), 1);
    }

    /// The island gate named in the module doc: `add_plugin_channel` alone must
    /// be sufficient. If it stopped installing [`GameEventBusPlugin`], nothing
    /// would ever write `Messages<GameEvent>` in a real client and every
    /// subscriber would be permanently empty — while every other test here,
    /// which writes `GameEvent` by hand, kept passing.
    #[test]
    fn channel_dispatch_requires_no_second_opt_in() {
        let app = app();
        assert!(
            app.world().get_resource::<GameEventBus>().is_some(),
            "add_plugin_channel must install the game-event bus marker \
             `SharedState` checks at construction, or a registered channel \
             receives nothing in a real client"
        );
    }

    /// Two independent plugins declaring the same channel type must both be
    /// able to say so, in either order — bevy panics on a duplicate
    /// `add_plugins`, so this is what makes the API usable by parties that have
    /// never heard of each other.
    #[test]
    fn declaring_the_same_channel_twice_is_idempotent() {
        let mut app = App::new();
        app.add_plugin_channel::<Greeting>();
        app.add_plugin_channel::<Greeting>();
        app.add_plugin_channel::<Greeting>();
        assert!(app.is_plugin_added::<PluginChannelPlugin<Greeting>>());
    }

    /// **The control for idempotency.** Adding the inner plugin directly twice
    /// *does* panic, which is what `add_plugin_channel`'s `is_plugin_added`
    /// check is buying. Without this, the test above could pass against a bevy
    /// that tolerated duplicates.
    #[test]
    #[should_panic(expected = "already added")]
    fn adding_the_channel_plugin_directly_twice_panics() {
        let mut app = App::new();
        app.add_plugins(PluginChannelPlugin::<Greeting>::default());
        app.add_plugins(PluginChannelPlugin::<Greeting>::default());
    }

    /// Two different channels coexist, each with its own state and key.
    #[test]
    fn two_channels_register_independently() {
        let mut app = App::new();
        app.add_plugin_channel::<Greeting>();
        app.add_plugin_channel::<Other>();
        assert_eq!(
            app.world().resource::<PluginChannelState<Greeting>>().key(),
            &channel("example:greet")
        );
        assert_eq!(
            app.world().resource::<PluginChannelState<Other>>().key(),
            &channel("example:other")
        );
    }

    /// A channel constant that is not a valid identifier must panic at build,
    /// naming the type — never register a channel that can never match.
    #[test]
    #[should_panic(expected = "not a valid namespaced identifier")]
    fn a_malformed_channel_constant_panics_at_build() {
        #[derive(Message, Debug)]
        struct Bad;

        impl PluginChannel for Bad {
            // Two separators: `ParseIdentifierError::TooManySeparators`.
            const CHANNEL: &'static str = "a:b:c";

            fn decode(_: &[u8]) -> Option<Self> {
                Some(Self)
            }
        }

        let mut app = App::new();
        app.add_plugin_channel::<Bad>();
    }

    /// A non-`CustomPayload` event must not disturb the fold — the same control
    /// `ChannelRegistry` has for its own `apply`.
    #[test]
    fn a_non_custom_payload_event_is_ignored() {
        let mut app = app();
        app.world_mut()
            .write_message(GameEvent(ClientEvent::Ping { id: 1 }));
        app.world_mut().run_schedule(GameTick);

        assert!(app.world().resource::<Heard>().0.is_empty());
        assert_eq!(
            app.world().resource::<PluginChannelState<Greeting>>().matched(),
            0
        );
    }
}
