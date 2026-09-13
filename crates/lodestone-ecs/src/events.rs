//! The plugin event buses: typed, version-free `Message` mirrors for decoded
//! client events and raw packet traffic. A plugin can observe either stream
//! directly rather than polling component state after the fact.
//!
//! # Why `GameEvent` wraps `ClientEvent` rather than inventing a second
//! vocabulary
//!
//! `ClientEvent` is already version-free, `Clone`, and `#[non_exhaustive]`
//! (`lodestone_model::event`) — a second ~107-variant enum mirroring it would
//! be exactly a staleness factory: two enums drift, and nothing forces the
//! second one to grow a variant when the first one does.
//!
//! The `RawPacket` bus is the version-*opaque* inbound half of the plugin event
//! bus: it carries the protocol number, connection state, packet id, and exact
//! body before a version-specific adapter decodes it. `OutboundRawPacket` is
//! its client counterpart: it carries the same metadata and exact encoded body
//! after the adapter (including any decorator) returns it and before transport
//! framing. The
//! `GameEvent` bus is the
//! version-*free*, already-decoded half, and it costs nothing extra to keep in
//! sync because it is not a copy — it is `ClientEvent` itself, one field deep.
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
//! With either bus on, the driver takes the `EcsHandle` write lock to publish
//! into its message queue, and raw observation also copies the packet body.
//! Those are measurable costs this crate has no business imposing on a client
//! that never asked for observation, so both marker resources are absent by
//! default. `SharedState` checks them once at construction (before any
//! hot-path lock is taken) and caches the answers.

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{IntoScheduleConfigs, Message, Messages, ResMut, Resource};
use lodestone_model::{
    ClientEvent, ConnectionState, ContainerStateId, ItemStack, ResourceKey, Text,
};

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

/// A borrowed, typed view of one inventory or menu observation.
///
/// The event bus deliberately carries the model's [`ClientEvent`] rather than a
/// second inventory vocabulary. This view gives a plugin an explicit capability
/// boundary without copying, caching, or reducing the item stack: an
/// [`ItemStack`] reference still exposes its complete modeled component set and
/// its `has_unmodeled` marker. The view is tied to the reader's borrow of the
/// `GameEvent`, so it cannot outlive the message or become a stale menu cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InventoryMenuEvent<'event> {
    /// The full contents of one window, including the carried cursor stack.
    ContainerContent {
        /// Window/container id.
        window_id: i32,
        /// Server synchronization revision for this content.
        state_id: ContainerStateId,
        /// Slot contents in window order.
        items: &'event [Option<ItemStack>],
        /// Item currently carried by the cursor, if any.
        carried_item: Option<&'event ItemStack>,
    },
    /// One slot in an open window changed.
    ContainerSlot {
        /// Window/container id.
        window_id: i32,
        /// Server synchronization revision for this slot update.
        state_id: ContainerStateId,
        /// Slot index in the window.
        slot: i32,
        /// New stack, or `None` when the slot was cleared.
        item: Option<&'event ItemStack>,
    },
    /// One menu-local property changed.
    ContainerData {
        /// Window/container id.
        window_id: i32,
        /// Menu-local property id.
        property: i32,
        /// New property value.
        value: i32,
    },
    /// The server closed a window.
    ScreenClosed {
        /// Window/container id.
        window_id: i32,
    },
    /// The server began opening a window. The menu's slot count arrives in a
    /// later content event for ordinary windows.
    ScreenOpened {
        /// Window/container id.
        window_id: i32,
        /// Canonical menu type key.
        menu_type: &'event ResourceKey,
        /// Styled title supplied by the server.
        title: &'event Text,
    },
    /// A mount inventory opened with its size carried by the open event.
    MountScreenOpened {
        /// Window/container id.
        container_id: i32,
        /// Number of inventory columns announced by the server.
        inventory_columns: i32,
        /// Ridden entity id.
        entity_id: i32,
    },
    /// The selected hotbar slot changed.
    HeldSlotChanged {
        /// New selected slot.
        slot: i32,
    },
    /// The carried cursor stack changed.
    CursorItemChanged {
        /// New cursor stack, or `None` when the cursor was cleared.
        item: Option<&'event ItemStack>,
    },
    /// A native player-inventory slot changed outside the active window.
    InventorySlotChanged {
        /// Player-inventory slot index.
        slot: i32,
        /// New stack, or `None` when the slot was cleared.
        item: Option<&'event ItemStack>,
    },
}

impl GameEvent {
    /// Returns the inventory/menu view for this event, borrowing the original
    /// model values. The production client writes every decoded event to the
    /// opt-in [`GameEventBusPlugin`] without filtering; this method is only a
    /// typed consumer convenience and never becomes a second routing table.
    #[must_use]
    pub fn inventory_menu(&self) -> Option<InventoryMenuEvent<'_>> {
        match &self.0 {
            ClientEvent::ContainerContent {
                window_id,
                state_id,
                items,
                carried_item,
            } => Some(InventoryMenuEvent::ContainerContent {
                window_id: *window_id,
                state_id: *state_id,
                items,
                carried_item: carried_item.as_ref(),
            }),
            ClientEvent::ContainerSlot {
                window_id,
                state_id,
                slot,
                item,
            } => Some(InventoryMenuEvent::ContainerSlot {
                window_id: *window_id,
                state_id: *state_id,
                slot: *slot,
                item: item.as_ref(),
            }),
            ClientEvent::ContainerData {
                window_id,
                property,
                value,
            } => Some(InventoryMenuEvent::ContainerData {
                window_id: *window_id,
                property: *property,
                value: *value,
            }),
            ClientEvent::ScreenClosed { window_id } => {
                Some(InventoryMenuEvent::ScreenClosed {
                    window_id: *window_id,
                })
            }
            ClientEvent::ScreenOpened {
                window_id,
                menu_type,
                title,
            } => Some(InventoryMenuEvent::ScreenOpened {
                window_id: *window_id,
                menu_type,
                title,
            }),
            ClientEvent::MountScreenOpened {
                container_id,
                inventory_columns,
                entity_id,
            } => Some(InventoryMenuEvent::MountScreenOpened {
                container_id: *container_id,
                inventory_columns: *inventory_columns,
                entity_id: *entity_id,
            }),
            ClientEvent::HeldSlotChanged { slot } => {
                Some(InventoryMenuEvent::HeldSlotChanged { slot: *slot })
            }
            ClientEvent::CursorItemChanged { item } => {
                Some(InventoryMenuEvent::CursorItemChanged {
                    item: item.as_ref(),
                })
            }
            ClientEvent::InventorySlotChanged { slot, item } => {
                Some(InventoryMenuEvent::InventorySlotChanged {
                    slot: *slot,
                    item: item.as_ref(),
                })
            }
            _ => None,
        }
    }
}

/// One inbound packet before version-specific decoding.
///
/// This is observation-only: the driver publishes the connection state, packet
/// id, and exact payload it received, while the adapter remains the sole
/// consumer that can turn those bytes into state or directives. Keeping the
/// value in this version-free crate lets a plugin inspect an unknown packet
/// without depending on a protocol family.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct RawPacket {
    /// Negotiated protocol number. This identifies the wire family without
    /// making this version-free crate depend on a concrete adapter.
    pub protocol: i32,
    /// Protocol phase in which the packet arrived.
    pub state: ConnectionState,
    /// Packet id as read from the length-framed packet.
    pub packet_id: i32,
    /// Packet body, excluding the packet id and outer length framing.
    pub payload: Vec<u8>,
}

/// One outbound packet after version-specific encoding and before transport
/// framing.
///
/// This is observation-only. The driver publishes the connection state,
/// packet id, and an owned copy of the exact adapter output, then writes the
/// same bytes to the connection. A reader cannot replace, cancel, reorder, or
/// inject transport data. The bus is native-client only at its producer; the
/// version-free type remains available to plugins on every target, while the
/// WASM event is a bounded copied projection.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct OutboundRawPacket {
    /// Negotiated protocol number. This identifies the wire family without
    /// making this version-free crate depend on a concrete adapter.
    pub protocol: i32,
    /// Protocol phase in which the packet was encoded.
    pub state: ConnectionState,
    /// Packet id returned by the version adapter.
    pub packet_id: i32,
    /// Packet body, excluding the packet id and outer length framing.
    pub payload: Vec<u8>,
}

/// Per-tick bounds for the inbound raw-packet observer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawPacketLimits {
    /// Maximum number of packets accepted between bus aging points.
    pub max_packets_per_tick: usize,
    /// Maximum total payload bytes accepted between bus aging points.
    pub max_payload_bytes_per_tick: usize,
}

impl Default for RawPacketLimits {
    fn default() -> Self {
        Self {
            max_packets_per_tick: 256,
            max_payload_bytes_per_tick: 1024 * 1024,
        }
    }
}

/// Counters for accepted and dropped inbound observations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawPacketStats {
    /// Observations published since the last reset.
    pub published_packets: u64,
    /// Payload bytes copied into published observations since the last reset.
    pub published_payload_bytes: u64,
    /// Observations rejected by either configured bound since the last reset.
    pub dropped_packets: u64,
    /// Payload bytes that were not copied because an observation was dropped.
    pub dropped_payload_bytes: u64,
}

/// Resource that opts a client into bounded inbound raw-packet observation.
#[derive(Resource, Debug)]
pub struct RawPacketBus {
    limits: RawPacketLimits,
    window_packets: usize,
    window_payload_bytes: usize,
    stats: RawPacketStats,
}

impl RawPacketBus {
    /// Creates a bus with explicit per-tick bounds.
    #[must_use]
    pub const fn with_limits(limits: RawPacketLimits) -> Self {
        Self {
            limits,
            window_packets: 0,
            window_payload_bytes: 0,
            stats: RawPacketStats {
                published_packets: 0,
                published_payload_bytes: 0,
                dropped_packets: 0,
                dropped_payload_bytes: 0,
            },
        }
    }

    /// The configured bounds.
    #[must_use]
    pub const fn limits(&self) -> RawPacketLimits {
        self.limits
    }

    /// Current counters. Counters are reset only by [`Self::reset_stats`].
    #[must_use]
    pub const fn stats(&self) -> RawPacketStats {
        self.stats
    }

    /// Clears cumulative counters while retaining the current tick window.
    pub fn reset_stats(&mut self) {
        self.stats = RawPacketStats::default();
    }

    fn reset_window(&mut self) {
        self.window_packets = 0;
        self.window_payload_bytes = 0;
    }

    /// Reserves one observation, or records a drop when the configured bounds
    /// would be exceeded. The caller must write the message only on `true`.
    pub fn try_reserve(&mut self, payload_bytes: usize) -> bool {
        let packet_ok = self.window_packets < self.limits.max_packets_per_tick;
        let bytes_ok = self
            .window_payload_bytes
            .checked_add(payload_bytes)
            .is_some_and(|total| total <= self.limits.max_payload_bytes_per_tick);
        if packet_ok && bytes_ok {
            self.window_packets += 1;
            self.window_payload_bytes += payload_bytes;
            self.stats.published_packets = self.stats.published_packets.saturating_add(1);
            self.stats.published_payload_bytes = self
                .stats
                .published_payload_bytes
                .saturating_add(payload_bytes as u64);
            true
        } else {
            self.stats.dropped_packets = self.stats.dropped_packets.saturating_add(1);
            self.stats.dropped_payload_bytes = self
                .stats
                .dropped_payload_bytes
                .saturating_add(payload_bytes as u64);
            false
        }
    }
}

impl Default for RawPacketBus {
    fn default() -> Self {
        Self::with_limits(RawPacketLimits::default())
    }
}

/// Per-tick bounds for the native outbound raw-packet observer.
///
/// A driver never waits for an observer: when either bound is exhausted it
/// drops the observation, increments [`OutboundRawPacketStats`], and still
/// sends the original packet. This makes observer backpressure explicit and
/// keeps a slow plugin from stalling a live connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundRawPacketLimits {
    /// Maximum number of packets accepted between bus aging points.
    pub max_packets_per_tick: usize,
    /// Maximum total payload bytes accepted between bus aging points.
    pub max_payload_bytes_per_tick: usize,
}

impl Default for OutboundRawPacketLimits {
    fn default() -> Self {
        Self {
            max_packets_per_tick: 256,
            max_payload_bytes_per_tick: 1024 * 1024,
        }
    }
}

/// Counters for accepted and dropped outbound observations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutboundRawPacketStats {
    /// Observations published since the last reset.
    pub published_packets: u64,
    /// Payload bytes copied into published observations since the last reset.
    pub published_payload_bytes: u64,
    /// Observations rejected by either configured bound since the last reset.
    pub dropped_packets: u64,
    /// Payload bytes that were not copied because an observation was dropped.
    pub dropped_payload_bytes: u64,
}

/// Resource that opts a client into outbound raw-packet observation and owns
/// its bounded overflow policy.
///
/// Install [`OutboundRawPacketBusPlugin`] rather than inserting this resource
/// directly. The public constructor on that plugin makes the policy visible at
/// app-build time, before [`lodestone_client::state::SharedState`] caches the
/// opt-in decision.
#[derive(Resource, Debug)]
pub struct OutboundRawPacketBus {
    limits: OutboundRawPacketLimits,
    window_packets: usize,
    window_payload_bytes: usize,
    stats: OutboundRawPacketStats,
}

impl OutboundRawPacketBus {
    /// Creates a bus with explicit per-tick bounds.
    #[must_use]
    pub const fn with_limits(limits: OutboundRawPacketLimits) -> Self {
        Self {
            limits,
            window_packets: 0,
            window_payload_bytes: 0,
            stats: OutboundRawPacketStats {
                published_packets: 0,
                published_payload_bytes: 0,
                dropped_packets: 0,
                dropped_payload_bytes: 0,
            },
        }
    }

    /// The configured bounds.
    #[must_use]
    pub const fn limits(&self) -> OutboundRawPacketLimits {
        self.limits
    }

    /// Current counters. Counters are reset only by [`reset_stats`].
    #[must_use]
    pub const fn stats(&self) -> OutboundRawPacketStats {
        self.stats
    }

    /// Clears cumulative counters while retaining the current tick window.
    pub fn reset_stats(&mut self) {
        self.stats = OutboundRawPacketStats::default();
    }

    /// Resets the per-tick admission window after readers have had their
    /// chance to consume the previous batch.
    fn reset_window(&mut self) {
        self.window_packets = 0;
        self.window_payload_bytes = 0;
    }

    /// Reserves one observation, or records a drop when the configured bounds
    /// would be exceeded. The caller must write the message only when this
    /// returns `true`.
    pub fn try_reserve(&mut self, payload_bytes: usize) -> bool {
        let packet_ok = self.window_packets < self.limits.max_packets_per_tick;
        let bytes_ok = self
            .window_payload_bytes
            .checked_add(payload_bytes)
            .is_some_and(|total| total <= self.limits.max_payload_bytes_per_tick);
        if packet_ok && bytes_ok {
            self.window_packets += 1;
            self.window_payload_bytes += payload_bytes;
            self.stats.published_packets = self.stats.published_packets.saturating_add(1);
            self.stats.published_payload_bytes = self
                .stats
                .published_payload_bytes
                .saturating_add(payload_bytes as u64);
            true
        } else {
            self.stats.dropped_packets = self.stats.dropped_packets.saturating_add(1);
            self.stats.dropped_payload_bytes = self
                .stats
                .dropped_payload_bytes
                .saturating_add(payload_bytes as u64);
            false
        }
    }
}

impl Default for OutboundRawPacketBus {
    fn default() -> Self {
        Self::with_limits(OutboundRawPacketLimits::default())
    }
}

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

/// Registers the version-free raw-packet observation bus and its tick aging
/// system. This plugin is separate from [`GameEventBusPlugin`] so a plugin that
/// needs decoded events does not also pay to clone every inbound payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawPacketBusPlugin {
    limits: RawPacketLimits,
}

impl RawPacketBusPlugin {
    /// Creates the inbound observer with explicit per-tick packet/byte bounds.
    #[must_use]
    pub const fn with_limits(limits: RawPacketLimits) -> Self {
        Self { limits }
    }
}

impl Default for RawPacketBusPlugin {
    fn default() -> Self {
        Self::with_limits(RawPacketLimits::default())
    }
}

impl Plugin for RawPacketBusPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<crate::CorePlugin>() {
            app.add_plugins(crate::CorePlugin);
        }
        app.insert_resource(RawPacketBus::with_limits(self.limits));
        app.add_message::<RawPacket>();
        app.add_systems(GameTick, age_raw_packet_bus.in_set(TickSet::Send));
    }
}

/// Registers the bounded native outbound raw-packet observation bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundRawPacketBusPlugin {
    limits: OutboundRawPacketLimits,
}

impl OutboundRawPacketBusPlugin {
    /// Creates the plugin with explicit per-tick packet and byte bounds.
    #[must_use]
    pub const fn with_limits(limits: OutboundRawPacketLimits) -> Self {
        Self { limits }
    }
}

impl Default for OutboundRawPacketBusPlugin {
    fn default() -> Self {
        Self::with_limits(OutboundRawPacketLimits::default())
    }
}

impl Plugin for OutboundRawPacketBusPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<crate::CorePlugin>() {
            app.add_plugins(crate::CorePlugin);
        }
        app.insert_resource(OutboundRawPacketBus::with_limits(self.limits));
        app.add_message::<OutboundRawPacket>();
        app.add_systems(GameTick, age_outbound_raw_packet_bus.in_set(TickSet::Send));
    }
}

/// `TickSet::Send`: ages [`Messages<GameEvent>`]'s double buffer once per
/// tick. See [`GameEventBusPlugin`]'s doc for why nothing else calls this.
fn age_game_event_bus(mut messages: ResMut<Messages<GameEvent>>) {
    messages.update();
}

/// Ages [`Messages<RawPacket>`] after every reader has observed this tick's
/// inbound packets.
fn age_raw_packet_bus(
    mut messages: ResMut<Messages<RawPacket>>,
    mut bus: ResMut<RawPacketBus>,
) {
    messages.update();
    bus.reset_window();
}

/// Ages outbound observations and reopens the admission window for the next
/// tick only after all readers have consumed the current batch.
fn age_outbound_raw_packet_bus(
    mut messages: ResMut<Messages<OutboundRawPacket>>,
    mut bus: ResMut<OutboundRawPacketBus>,
) {
    messages.update();
    bus.reset_window();
}

#[cfg(test)]
mod tests {
    use bevy_ecs::message::MessageReader;
    use bevy_ecs::resource::Resource;
    use bevy_ecs::system::ResMut;
    use bevy_ecs::world::World;
    use lodestone_model::{ClientEvent, ConnectionState};

    use super::{
        GameEvent, GameEventBus, GameEventBusPlugin, InventoryMenuEvent, OutboundRawPacket,
        OutboundRawPacketBus, OutboundRawPacketBusPlugin, OutboundRawPacketLimits, RawPacket,
        RawPacketBus, RawPacketBusPlugin, RawPacketLimits,
    };
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

    /// The outbound observer has a real, explicit bounded window: the second
    /// packet is dropped without affecting the packet that the driver writes.
    #[test]
    fn outbound_observation_drops_when_its_packet_bound_is_full() {
        let mut app = bevy_app::App::new();
        app.add_plugins(OutboundRawPacketBusPlugin::with_limits(
            OutboundRawPacketLimits {
                max_packets_per_tick: 1,
                max_payload_bytes_per_tick: 64,
            },
        ));
        assert!(app.world().get_resource::<OutboundRawPacketBus>().is_some());

        assert!(app.world_mut().resource_mut::<OutboundRawPacketBus>().try_reserve(2));
        app.world_mut().write_message(OutboundRawPacket {
            protocol: 776,
            state: ConnectionState::Play,
            packet_id: 1,
            payload: vec![0, 255],
        });
        assert!(!app.world_mut().resource_mut::<OutboundRawPacketBus>().try_reserve(2));

        let bus = app.world().resource::<OutboundRawPacketBus>();
        assert_eq!(bus.stats().published_packets, 1);
        assert_eq!(bus.stats().dropped_packets, 1);
        assert_eq!(
            app.world()
                .resource::<bevy_ecs::message::Messages<OutboundRawPacket>>()
                .len(),
            1,
            "only an admitted observation may enter the message queue"
        );

        app.world_mut().run_schedule(GameTick);
        assert!(
            app.world_mut()
                .resource_mut::<OutboundRawPacketBus>()
                .try_reserve(2)
        );

        let mut byte_limited = OutboundRawPacketBus::with_limits(OutboundRawPacketLimits {
            max_packets_per_tick: 2,
            max_payload_bytes_per_tick: 3,
        });
        assert!(byte_limited.try_reserve(2));
        assert!(!byte_limited.try_reserve(2));
        assert_eq!(byte_limited.stats().dropped_payload_bytes, 2);
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

    /// The typed inventory view borrows the decoded stack rather than reducing
    /// it to an item id and count. This matters for menu policy: two equal item
    /// ids with different data components are different stacks, and an
    /// unmodeled component must remain observable as a partial decode.
    #[test]
    fn inventory_menu_view_preserves_item_components_and_has_a_negative_control() {
        let item = lodestone_model::ItemStack {
            item: "minecraft:diamond".parse().expect("valid item key"),
            count: 3,
            components: lodestone_model::ItemComponents {
                damage: Some(17),
                has_unmodeled: true,
                ..Default::default()
            },
        };
        let event = GameEvent(ClientEvent::ContainerContent {
            window_id: 4,
            state_id: lodestone_model::ContainerStateId::new(31),
            items: vec![None, Some(item.clone())],
            carried_item: Some(item.clone()),
        });

        let Some(InventoryMenuEvent::ContainerContent {
            window_id,
            state_id,
            items,
            carried_item,
        }) = event.inventory_menu()
        else {
            panic!("container content must be classified as inventory/menu data");
        };
        assert_eq!(window_id, 4);
        assert_eq!(state_id, lodestone_model::ContainerStateId::new(31));
        assert_eq!(items.len(), 2);
        let observed = items[1].as_ref().expect("the second slot is populated");
        assert_eq!(observed.item, item.item);
        assert_eq!(observed.count, 3);
        assert_eq!(observed.components.damage, Some(17));
        assert!(observed.components.has_unmodeled);
        assert_eq!(carried_item, Some(&item));

        assert!(
            GameEvent(ClientEvent::Ping { id: 9 })
                .inventory_menu()
                .is_none(),
            "unrelated events must not be presented as inventory/menu observations"
        );
    }

    /// Raw observation is opt-in independently of decoded event observation.
    #[test]
    fn the_raw_packet_marker_is_absent_by_default() {
        let world = World::new();
        assert!(world.get_resource::<RawPacketBus>().is_none());
    }

    /// Installing the raw bus exposes its message resource to a reader and
    /// preserves the packet metadata and payload without decoding it.
    #[test]
    fn a_raw_packet_reaches_a_reader_with_exact_bytes() {
        let mut app = bevy_app::App::new();
        app.add_plugins(RawPacketBusPlugin::default());
        app.init_resource::<SeenCount>();

        fn observe(mut packets: MessageReader<RawPacket>, mut count: ResMut<SeenCount>) {
            for packet in packets.read() {
                assert_eq!(packet.protocol, 776);
                assert_eq!(packet.state, ConnectionState::Play);
                assert_eq!(packet.packet_id, 0x2a);
                assert_eq!(packet.payload.as_slice(), [0x00, 0xff, 0x7f]);
                count.0 += 1;
            }
        }

        app.add_systems(GameTick, observe);
        app.world_mut().write_message(RawPacket {
            protocol: 776,
            state: ConnectionState::Play,
            packet_id: 0x2a,
            payload: vec![0x00, 0xff, 0x7f],
        });
        app.world_mut().run_schedule(GameTick);

        assert_eq!(app.world().resource::<SeenCount>().0, 1);
    }

    /// A world without the opt-in plugin has no raw message queue, so the
    /// driver's conditional write cannot allocate or retain packet bytes.
    #[test]
    fn writing_a_raw_packet_without_the_bus_is_a_no_op() {
        let mut world = World::new();
        assert!(world
            .write_message(RawPacket {
                protocol: 776,
                state: ConnectionState::Login,
                packet_id: 3,
                payload: vec![1, 2, 3],
            })
        .is_none());
    }

    /// The inbound observer applies the same explicit packet/byte admission
    /// policy as the outbound observer; an oversized packet never enters the
    /// message queue and therefore cannot retain unbounded transport data.
    #[test]
    fn inbound_observation_drops_when_its_byte_bound_is_full() {
        let mut app = bevy_app::App::new();
        app.add_plugins(RawPacketBusPlugin::with_limits(RawPacketLimits {
            max_packets_per_tick: 2,
            max_payload_bytes_per_tick: 2,
        }));
        let mut bus = app.world_mut().resource_mut::<RawPacketBus>();
        assert!(bus.try_reserve(2));
        assert!(!bus.try_reserve(1));
        assert_eq!(bus.stats().published_packets, 1);
        assert_eq!(bus.stats().dropped_packets, 1);
    }
}
