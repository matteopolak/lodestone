//! Wire-level plugin messaging: the channel registry and dispatch (issue #335).
//!
//! Vanilla servers support custom plugin-message channels. A client announces
//! which channels it supports during Configuration (and may add or remove some
//! later, in Play); a server-side plugin registers interest in the channels it
//! wants; and the two sides exchange raw payloads on a named channel. This
//! module is the version-free half of that machinery, mirroring the shape
//! [`crate::command::CommandDispatch`] established for commands:
//!
//! * [`PluginChannelRegistry`] — the shared, host-installed registry of
//!   channels the server has registered interest in. Inbound payloads on a
//!   registered channel are dispatched to that channel's handler; payloads on
//!   an unregistered channel are silently dropped, exactly vanilla's
//!   `DiscardedPayload` fallback. It also carries the server→client broadcast
//!   queue: [`PluginChannelRegistry::broadcast`] publishes a payload for every
//!   connection, and each connection's loop drains it, sending only channels
//!   that connection actually supports.
//! * [`ClientChannels`] — one connection's supported-channel set, populated
//!   from the client's `minecraft:register` / `minecraft:unregister` payloads
//!   (see [`apply_custom_payload`](crate::server)). It is the per-connection
//!   filter that decides whether a broadcast reaches this client and whether
//!   the server may push a payload to it directly.
//!
//! # Wire vocabulary
//!
//! This crate is version-free, so the two "control" channels are named as
//! their historical vanilla form: [`REGISTER_CHANNEL`] and
//! [`UNREGISTER_CHANNEL`]. Their payload is a UTF-8, comma-separated list of
//! channel identifiers — the format that was stable from 1.7 through 1.12 and
//! that the modded ecosystem has kept sending since (vanilla 26.2 itself no
//! longer uses either channel; every other `custom_payload` is just a channel
//! plus raw bytes and is never interpreted here).
//!
//! # Scope
//!
//! Deliberately **not** the plugin-facing API: that is issue #77's "plugin
//! framework". This is the wire-level registry and dispatch mechanism it will
//! sit on — receive a payload on a channel, look up registered interest,
//! deliver it, and drop unregistered traffic without error.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

use lodestone_model::ResourceKey;

/// The channel a client uses to announce which channels it supports (issue
/// #335). Payload: a UTF-8 comma-separated list of channel identifiers.
pub const REGISTER_CHANNEL: &str = "minecraft:register";
/// The channel a client uses to withdraw channel support. Same payload format
/// as [`REGISTER_CHANNEL`].
pub const UNREGISTER_CHANNEL: &str = "minecraft:unregister";

/// How many server→client payloads the broadcast queue retains before the
/// oldest is trimmed. Bounded because it is process-lifetime shared state every
/// connection drains and the server appends to — the same reasoning as
/// `PlayerRegistry`'s chat log; a connection that falls behind loses the
/// overflow rather than the whole window.
const OUTBOUND_CAPACITY: usize = 256;

/// A plugin's registered interest in a channel: the inbound dispatch target.
///
/// `&self`, not `&mut self`: a connection task calls this and several
/// connections may exist, so the implementor owns its own synchronisation —
/// the same contract [`crate::command::CommandSink`] gives the command host.
///
/// Must not panic: this runs on a connection task, and a panic here takes the
/// player's connection with it.
pub trait PluginChannelHandler: Send + Sync {
    /// Delivers one inbound payload on `channel`. `channel` is the channel the
    /// handler registered for; `data` is the raw channel-specific bytes.
    fn on_payload(&self, channel: &ResourceKey, data: &[u8]);
}

/// Whether `channel` is the register channel, by namespace/path components
/// rather than string allocation.
fn is_register_channel(channel: &ResourceKey) -> bool {
    channel.namespace() == "minecraft" && channel.path() == "register"
}

/// Whether `channel` is the unregister channel, by namespace/path components.
fn is_unregister_channel(channel: &ResourceKey) -> bool {
    channel.namespace() == "minecraft" && channel.path() == "unregister"
}

/// Splits a register/unregister payload into its channel names.
///
/// The format is the historical vanilla one: a UTF-8 string of
/// comma-separated channel identifiers. A name that fails to parse as a
/// [`ResourceKey`] is skipped rather than fatal — a malformed register payload
/// drops the bad names, not the connection.
fn channel_names_from_payload(data: &[u8]) -> impl Iterator<Item = &str> {
    std::str::from_utf8(data)
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

/// One connection's set of channels the client has declared support for.
///
/// Populated from the client's `minecraft:register` / `minecraft:unregister`
/// custom payloads (issue #335). It has two consumers, both per-connection:
///
/// * the outbound broadcast drain filters every server→client payload through
///   [`supports`](Self::supports), so a client only ever receives a channel it
///   announced;
/// * server-side logic that wants to push a payload directly to *this* client
///   checks [`supports`](Self::supports) first and sends through
///   [`ServerProtocol::encode_custom_payload`](crate::ServerProtocol).
///
/// The register/unregister interpretation lives here (this crate) rather than
/// at the protocol seam because a channel is a channel: the *payload format*
/// is version-free by the module doc's convention, and the protocol seam's job
/// is only to lift `custom_payload` into
/// [`ServerBound::CustomPayload`](crate::ServerBound) unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientChannels {
    supported: BTreeSet<ResourceKey>,
}

impl ClientChannels {
    /// Declares `channel` as supported. Idempotent.
    pub fn register(&mut self, channel: ResourceKey) {
        self.supported.insert(channel);
    }

    /// Withdraws support for `channel`. Idempotent.
    pub fn unregister(&mut self, channel: &ResourceKey) {
        self.supported.remove(channel);
    }

    /// Whether this client has declared support for `channel`.
    #[must_use]
    pub fn supports(&self, channel: &ResourceKey) -> bool {
        self.supported.contains(channel)
    }

    /// Whether the client has declared support for no channels at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.supported.is_empty()
    }

    /// The number of channels the client supports.
    #[must_use]
    pub fn len(&self) -> usize {
        self.supported.len()
    }

    /// Applies a `minecraft:register` payload: every channel named is added to
    /// the supported set. Unparseable names are skipped.
    pub fn apply_register_payload(&mut self, data: &[u8]) {
        for name in channel_names_from_payload(data) {
            if let Ok(channel) = name.parse::<ResourceKey>() {
                self.supported.insert(channel);
            }
        }
    }

    /// Applies a `minecraft:unregister` payload: every channel named is removed
    /// from the supported set. Unparseable names are skipped.
    pub fn apply_unregister_payload(&mut self, data: &[u8]) {
        for name in channel_names_from_payload(data) {
            if let Ok(channel) = name.parse::<ResourceKey>() {
                self.supported.remove(&channel);
            }
        }
    }

    /// Consumes one inbound custom payload, interpreting the register/unregister
    /// control channels.
    ///
    /// Returns whether `channel` was a control channel and `data` was applied to
    /// this connection's supported set. `false` means `channel` is an ordinary
    /// payload channel — the caller decides whether to dispatch it. This is the
    /// single entry point [`crate::server`] uses for every inbound
    /// `custom_payload`, so the register/unregister interpretation lives in one
    /// place (this module's doc comment explains why it lives here rather than
    /// at the protocol seam).
    pub fn apply_custom_payload(&mut self, channel: &ResourceKey, data: &[u8]) -> bool {
        if is_register_channel(channel) {
            self.apply_register_payload(data);
            true
        } else if is_unregister_channel(channel) {
            self.apply_unregister_payload(data);
            true
        } else {
            false
        }
    }
}

/// One server→client payload queued for broadcast to every connection.
struct OutboundPayload {
    channel: ResourceKey,
    data: Vec<u8>,
}

/// The shared, wire-level registry of channels the server has registered
/// interest in, plus the server→client broadcast queue (issue #335).
///
/// Clone it freely: every clone is the same registry, like
/// [`crate::CommandDispatch`] and [`crate::players::PlayerRegistry`]. One is
/// held by whatever owns the world and one by each connection task. The
/// [`Default`] is inert — no registered interest and an empty broadcast queue —
/// which is what lets every pre-existing `serve_connection*` entry point pass
/// one without changing behaviour; a host that wants plugin messaging installs
/// a live one through
/// [`serve_connection_with_plugin_channels`](crate::serve_connection_with_plugin_channels).
#[derive(Clone, Default)]
pub struct PluginChannelRegistry(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    /// Channels the server has registered interest in, by handler. A payload
    /// on a channel not present here is dropped.
    handlers: BTreeMap<ResourceKey, Arc<dyn PluginChannelHandler>>,
    /// Server→client payloads waiting to be drained by each connection, in
    /// broadcast order.
    outbound: VecDeque<OutboundPayload>,
    /// Sequence number of `outbound.front()`, so trimming the front cannot
    /// silently rewind a reader's cursor — the same absolute-cursor shape
    /// `PlayerRegistry`'s chat log uses.
    outbound_base: u64,
}

impl fmt::Debug for PluginChannelRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.0.lock().expect("plugin-channel registry poisoned");
        f.debug_struct("PluginChannelRegistry")
            .field("registered", &inner.handlers.len())
            .field("outbound", &inner.outbound.len())
            .finish()
    }
}

impl PluginChannelRegistry {
    /// A fresh, empty registry — the inert [`Default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers interest in `channel`: inbound payloads on it are dispatched
    /// to `handler` from now on. Re-registering a channel replaces its handler.
    pub fn register(&self, channel: ResourceKey, handler: Arc<dyn PluginChannelHandler>) {
        self.0.lock().expect("plugin-channel registry poisoned").handlers.insert(channel, handler);
    }

    /// Withdraws interest in `channel`; future payloads on it are dropped.
    pub fn unregister(&self, channel: &ResourceKey) {
        self.0.lock().expect("plugin-channel registry poisoned").handlers.remove(channel);
    }

    /// Whether the server has registered interest in `channel`.
    #[must_use]
    pub fn is_registered(&self, channel: &ResourceKey) -> bool {
        self.0
            .lock()
            .expect("plugin-channel registry poisoned")
            .handlers
            .contains_key(channel)
    }

    /// Every channel the server currently has registered interest in — the set
    /// a host advertises to clients (over `minecraft:register`) when it wants
    /// them to start sending on those channels.
    #[must_use]
    pub fn registered_channels(&self) -> Vec<ResourceKey> {
        self.0
            .lock()
            .expect("plugin-channel registry poisoned")
            .handlers
            .keys()
            .cloned()
            .collect()
    }

    /// Delivers one inbound payload on `channel` to its registered handler.
    ///
    /// Returns whether a handler was found and called. `false` is the
    /// unregistered-channel case: the payload is dropped without error, exactly
    /// vanilla's behaviour for a channel nobody owns. The lock is not held
    /// across the handler call — the handler is cloned out first, so a handler
    /// that itself calls back into this registry cannot deadlock.
    pub fn dispatch(&self, channel: &ResourceKey, data: &[u8]) -> bool {
        let handler = self
            .0
            .lock()
            .expect("plugin-channel registry poisoned")
            .handlers
            .get(channel)
            .cloned();
        match handler {
            Some(handler) => {
                handler.on_payload(channel, data);
                true
            }
            None => false,
        }
    }

    /// Publishes one server→client payload on `channel` to **every** connection
    /// that has declared support for it. Connections that did not register the
    /// channel skip it (see [`ClientChannels::supports`]).
    ///
    /// Delivery is asynchronous: the payload lands in the shared broadcast
    /// queue and each connection's loop sends it on its own next drain — the
    /// same append-only-log + per-reader-cursor shape `PlayerRegistry` uses for
    /// chat, because a drain-all feed would hand each payload to whichever
    /// connection's timer fired first and nobody else.
    pub fn broadcast(&self, channel: ResourceKey, data: &[u8]) {
        let mut inner = self.0.lock().expect("plugin-channel registry poisoned");
        inner.outbound.push_back(OutboundPayload {
            channel,
            data: data.to_vec(),
        });
        while inner.outbound.len() > OUTBOUND_CAPACITY {
            inner.outbound.pop_front();
            inner.outbound_base += 1;
        }
    }

    /// Every server→client payload this connection should send since `cursor`,
    /// **filtered to channels the connection supports**.
    ///
    /// `cursor` is advanced past *every* entry — supported or not — so a
    /// channel this connection never registered cannot stall the queue for it:
    /// the client would never send it, so the entry is a skip, not a block.
    ///
    /// If `cursor` has fallen behind the retained window it is snapped forward
    /// to the oldest retained entry, dropping the overflow (a connection that
    /// fell that far behind has missed those payloads regardless).
    pub fn outbound_since(
        &self,
        cursor: &mut u64,
        supported: &ClientChannels,
    ) -> Vec<(ResourceKey, Vec<u8>)> {
        let inner = self.0.lock().expect("plugin-channel registry poisoned");
        if *cursor < inner.outbound_base {
            *cursor = inner.outbound_base;
        }
        let end = inner.outbound_base + inner.outbound.len() as u64;
        if *cursor >= end {
            return Vec::new();
        }
        let skip = (*cursor - inner.outbound_base) as usize;
        let mut out = Vec::new();
        for payload in inner.outbound.iter().skip(skip) {
            if supported.supports(&payload.channel) {
                out.push((payload.channel.clone(), payload.data.clone()));
            }
        }
        *cursor = end;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recording handler: captures every `(channel, data)` delivered to it.
    #[derive(Debug, Default)]
    struct RecordingHandler {
        calls: Mutex<Vec<(ResourceKey, Vec<u8>)>>,
    }

    impl PluginChannelHandler for RecordingHandler {
        fn on_payload(&self, channel: &ResourceKey, data: &[u8]) {
            self.calls
                .lock()
                .expect("recording handler poisoned")
                .push((channel.clone(), data.to_vec()));
        }
    }

    fn key(name: &str) -> ResourceKey {
        name.parse().expect("valid channel name")
    }

    #[test]
    fn dispatch_delivers_registered_channel_and_reports_delivery() {
        let registry = PluginChannelRegistry::new();
        let handler = Arc::new(RecordingHandler::default());
        registry.register(key("mod:foo"), handler.clone());

        assert!(registry.dispatch(&key("mod:foo"), b"hello"));
        let calls = handler.calls.lock().expect("poisoned");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, key("mod:foo"));
        assert_eq!(calls[0].1, b"hello".to_vec());
    }

    #[test]
    fn dispatch_drops_unregistered_channel_without_error() {
        let registry = PluginChannelRegistry::new();
        let handler = Arc::new(RecordingHandler::default());
        registry.register(key("mod:foo"), handler.clone());

        // The unregistered channel is the absence claim; the *same* handler on
        // the registered channel is the control proving the detector fires.
        assert!(!registry.dispatch(&key("mod:unrelated"), b"ignored"));
        assert!(registry.dispatch(&key("mod:foo"), b"delivered"));
        let calls = handler.calls.lock().expect("poisoned");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, key("mod:foo"));
    }

    #[test]
    fn re_registering_a_channel_replaces_its_handler() {
        let registry = PluginChannelRegistry::new();
        let first = Arc::new(RecordingHandler::default());
        let second = Arc::new(RecordingHandler::default());
        registry.register(key("mod:foo"), first.clone());
        registry.register(key("mod:foo"), second.clone());

        registry.dispatch(&key("mod:foo"), b"x");
        assert!(first.calls.lock().expect("poisoned").is_empty());
        assert_eq!(second.calls.lock().expect("poisoned").len(), 1);
    }

    #[test]
    fn registered_channels_lists_interest_in_sorted_order() {
        let registry = PluginChannelRegistry::new();
        registry.register(key("mod:zebra"), Arc::new(RecordingHandler::default()));
        registry.register(key("mod:alpha"), Arc::new(RecordingHandler::default()));
        registry.unregister(&key("mod:zebra"));

        assert_eq!(registry.registered_channels(), vec![key("mod:alpha")]);
        assert!(!registry.is_registered(&key("mod:zebra")));
    }

    #[test]
    fn register_payload_adds_every_named_channel() {
        let mut channels = ClientChannels::default();
        channels.apply_register_payload(b"minecraft:brand,mod:foo,mod:bar");
        assert_eq!(channels.len(), 3);
        assert!(channels.supports(&key("minecraft:brand")));
        assert!(channels.supports(&key("mod:foo")));
        assert!(channels.supports(&key("mod:bar")));
    }

    #[test]
    fn unregister_payload_removes_only_named_channels() {
        let mut channels = ClientChannels::default();
        channels.apply_register_payload(b"mod:foo,mod:bar");
        channels.apply_unregister_payload(b"mod:foo");
        assert!(!channels.supports(&key("mod:foo")));
        assert!(channels.supports(&key("mod:bar")));
    }

    #[test]
    fn register_payload_skips_malformed_and_empty_names() {
        let mut channels = ClientChannels::default();
        // Empty data, trailing comma, and an invalid identifier char (`:` in
        // the path) all parse to nothing instead of an error.
        channels.apply_register_payload(b"");
        assert!(channels.is_empty());
        channels.apply_register_payload(b"mod:foo,,mod:bar:boom,");
        assert_eq!(channels.len(), 1);
        assert!(channels.supports(&key("mod:foo")));
    }

    #[test]
    fn apply_custom_payload_consumes_only_the_control_channels() {
        let mut channels = ClientChannels::default();

        // The register channel is consumed and applied.
        assert!(channels.apply_custom_payload(&key(REGISTER_CHANNEL), b"mod:foo"));
        assert!(channels.supports(&key("mod:foo")));

        // The unregister channel is consumed and applied.
        assert!(channels.apply_custom_payload(&key(UNREGISTER_CHANNEL), b"mod:foo"));
        assert!(!channels.supports(&key("mod:foo")));

        // Any other channel is not a control channel — the caller dispatches it.
        assert!(!channels.apply_custom_payload(&key("mod:foo"), b"data"));
        assert!(channels.is_empty(), "an ordinary payload must not touch support");
    }

    #[test]
    fn register_and_unregister_are_idempotent() {
        let mut channels = ClientChannels::default();
        channels.register(key("mod:foo"));
        channels.register(key("mod:foo"));
        assert_eq!(channels.len(), 1);
        channels.unregister(&key("mod:foo"));
        channels.unregister(&key("mod:foo"));
        assert!(channels.is_empty());
    }

    #[test]
    fn outbound_since_filters_by_connection_support() {
        let registry = PluginChannelRegistry::new();
        let mut supported = ClientChannels::default();
        supported.register(key("mod:foo"));

        registry.broadcast(key("mod:foo"), b"one");
        registry.broadcast(key("mod:unrelated"), b"two");
        registry.broadcast(key("mod:foo"), b"three");

        let mut cursor = 0;
        let got = registry.outbound_since(&mut cursor, &supported);
        // `mod:unrelated` is skipped, not delivered: this client never
        // announced it.
        assert_eq!(
            got,
            vec![
                (key("mod:foo"), b"one".to_vec()),
                (key("mod:foo"), b"three".to_vec()),
            ]
        );
        assert_eq!(cursor, 3);
    }

    #[test]
    fn outbound_since_advances_past_unsupported_entries_without_stalling() {
        let registry = PluginChannelRegistry::new();
        let mut supported = ClientChannels::default();
        supported.register(key("mod:foo"));

        // An unsupported channel between two supported ones: the cursor must
        // advance past it so a later broadcast on a supported channel still
        // arrives, and the *same* later payload must not be re-read twice.
        registry.broadcast(key("mod:unrelated"), b"skip");
        registry.broadcast(key("mod:foo"), b"arrives");
        let mut cursor = 0;
        let first = registry.outbound_since(&mut cursor, &supported);
        assert_eq!(first, vec![(key("mod:foo"), b"arrives".to_vec())]);

        registry.broadcast(key("mod:foo"), b"second");
        let second = registry.outbound_since(&mut cursor, &supported);
        assert_eq!(second, vec![(key("mod:foo"), b"second".to_vec())]);
    }

    #[test]
    fn outbound_since_trims_and_snaps_a_fallen_behind_cursor() {
        let registry = PluginChannelRegistry::new();
        let mut supported = ClientChannels::default();
        supported.register(key("mod:foo"));

        // Push past the capacity so the oldest entries are trimmed.
        for i in 0..OUTBOUND_CAPACITY + 5 {
            registry.broadcast(key("mod:foo"), format!("{i}").as_bytes());
        }

        // A cursor left at 0 has fallen behind the retained window and is
        // snapped forward; the dropped overflow is not replayed.
        let mut stale_cursor = 0;
        let got = registry.outbound_since(&mut stale_cursor, &supported);
        assert_eq!(got.len(), OUTBOUND_CAPACITY);
        assert_eq!(got[0].1, b"5".to_vec());
        assert_eq!(got[got.len() - 1].1, format!("{}", OUTBOUND_CAPACITY + 4).into_bytes());
    }
}
