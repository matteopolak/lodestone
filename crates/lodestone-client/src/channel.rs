//! Plugin-channel (`custom_payload`) dispatch registry.
//!
//! Before this existed, a decoded `custom_payload` reached exactly one place:
//! the generic [`ClientEvent::CustomPayload`] event, with `channel`/`data`
//! fields a consumer had to filter for by hand on every event that arrived —
//! "no channel-name → handler dispatch/registry layer exists", per the
//! issue's own partial-progress note (`crates/lodestone-client/src/driver.rs`
//! already sends the one channel vanilla itself uses, `minecraft:brand`, but
//! nothing let a caller register interest in a channel of their own).
//! [`ChannelRegistry`] is that layer: register a handler per channel once,
//! then feed it every event and only the channels anyone asked for actually
//! run anything.
//!
//! This is deliberately a passive fold a caller drives by hand — the same
//! shape as [`lodestone_game::tablist::TabList::apply`] and
//! `lodestone_game::scoreboard::Scoreboard`'s equivalent — rather than
//! something wired into [`crate::driver::Driver`] itself.
//! [`lodestone_model::event::route`] deliberately sends `CustomPayload` to
//! `Route::NOWHERE`: nothing in the ingest/session/shell pipeline is meant to
//! consume arbitrary plugin data, so a registry that lived inside the driver
//! would have no natural place to be configured from without reaching into
//! that pipeline. A caller who wants one builds it, registers handlers, and
//! feeds it from its own copy of the event stream — no different from
//! folding a [`TabList`](lodestone_game::tablist::TabList) today.

use std::collections::HashMap;
use std::fmt;

use lodestone_model::{ClientEvent, ResourceKey};

/// Dispatches [`ClientEvent::CustomPayload`] messages to per-channel handlers.
///
/// # Example
///
/// ```
/// use lodestone_client::ChannelRegistry;
/// use lodestone_model::{ClientEvent, ResourceKey};
///
/// let mut registry = ChannelRegistry::new();
/// let channel: ResourceKey = "example:greet".parse().unwrap();
/// registry.register(channel.clone(), |data: &[u8]| {
///     println!("got {} bytes on example:greet", data.len());
/// });
///
/// let event = ClientEvent::CustomPayload {
///     channel,
///     data: vec![1, 2, 3],
/// };
/// assert!(registry.apply(&event));
/// ```
pub struct ChannelRegistry {
    handlers: HashMap<ResourceKey, Box<dyn FnMut(&[u8]) + Send>>,
}

impl fmt::Debug for ChannelRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Handlers are opaque closures; the registered channel set is the
        // only part of this that is meaningfully printable.
        f.debug_struct("ChannelRegistry")
            .field("channels", &self.handlers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelRegistry {
    /// An empty registry with no channels registered.
    #[must_use]
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Registers `handler` to run for every future [`ClientEvent::CustomPayload`]
    /// on `channel`, replacing a previous registration for the same channel.
    ///
    /// `handler` receives the raw payload bytes, undecoded — this crate has no
    /// way to know a third-party channel's shape (see
    /// [`ClientEvent::CustomPayload`]'s own doc).
    pub fn register(&mut self, channel: ResourceKey, handler: impl FnMut(&[u8]) + Send + 'static) {
        self.handlers.insert(channel, Box::new(handler));
    }

    /// Removes the handler for `channel`, if any. Returns whether one was
    /// removed.
    pub fn unregister(&mut self, channel: &ResourceKey) -> bool {
        self.handlers.remove(channel).is_some()
    }

    /// Returns whether a handler is registered for `channel`.
    #[must_use]
    pub fn is_registered(&self, channel: &ResourceKey) -> bool {
        self.handlers.contains_key(channel)
    }

    /// Feeds one event through the registry. Runs the matching channel's
    /// handler if there is one and returns whether it did — the same
    /// "did this aggregate own the event" convention as
    /// [`TabList::apply`](lodestone_game::tablist::TabList::apply).
    ///
    /// A [`ClientEvent`] that is not [`ClientEvent::CustomPayload`], or one
    /// whose channel has no registered handler, is a no-op that returns
    /// `false` — this never disconnects or errors, matching how an unknown
    /// plugin channel is simply data nobody asked for, not a protocol fault.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        let ClientEvent::CustomPayload { channel, data } = event else {
            return false;
        };
        let Some(handler) = self.handlers.get_mut(channel) else {
            return false;
        };
        handler(data);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn channel(name: &str) -> ResourceKey {
        name.parse().expect("valid test channel name")
    }

    #[test]
    fn dispatches_only_the_registered_channel() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        let mut registry = ChannelRegistry::new();
        registry.register(channel("test:mine"), move |data: &[u8]| {
            seen_clone.lock().unwrap().push(data.to_vec());
        });

        let mine = ClientEvent::CustomPayload {
            channel: channel("test:mine"),
            data: vec![1, 2, 3],
        };
        let other = ClientEvent::CustomPayload {
            channel: channel("test:other"),
            data: vec![9, 9, 9],
        };

        assert!(registry.apply(&mine));
        // A channel with no registered handler is a no-op, not an error —
        // the control proving `apply` actually discriminates by channel
        // rather than running every handler for every payload.
        assert!(!registry.apply(&other));

        assert_eq!(*seen.lock().unwrap(), vec![vec![1, 2, 3]]);
    }

    #[test]
    fn non_custom_payload_events_are_untouched() {
        let mut registry = ChannelRegistry::new();
        let mut ran = false;
        registry.register(channel("test:mine"), |_: &[u8]| {
            panic!("must not run for a non-CustomPayload event");
        });
        // `Ping` is an arbitrary event this registry has no business reacting
        // to — the control for "only CustomPayload matters here".
        assert!(!registry.apply(&ClientEvent::Ping { id: 1 }));
        let _ = &mut ran;
    }

    #[test]
    fn unregister_stops_future_dispatch() {
        let mut registry = ChannelRegistry::new();
        registry.register(channel("test:mine"), |_: &[u8]| {
            panic!("must not run after unregister");
        });
        assert!(registry.is_registered(&channel("test:mine")));
        assert!(registry.unregister(&channel("test:mine")));
        assert!(!registry.is_registered(&channel("test:mine")));

        let event = ClientEvent::CustomPayload {
            channel: channel("test:mine"),
            data: vec![],
        };
        assert!(!registry.apply(&event));
    }

    #[test]
    fn replacing_a_registration_drops_the_old_handler() {
        let mut registry = ChannelRegistry::new();
        registry.register(channel("test:mine"), |_: &[u8]| {
            panic!("the old handler must not run once replaced");
        });
        let ran = Arc::new(Mutex::new(false));
        let ran_clone = Arc::clone(&ran);
        registry.register(channel("test:mine"), move |_: &[u8]| {
            *ran_clone.lock().unwrap() = true;
        });

        let event = ClientEvent::CustomPayload {
            channel: channel("test:mine"),
            data: vec![],
        };
        assert!(registry.apply(&event));
        assert!(*ran.lock().unwrap());
    }
}
