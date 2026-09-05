//! The maintained read-model: a live projection of the world the driver folds
//! from every [`ClientEvent`].
//!
//! A bot needs to *ask questions* ("where am I", "how much health", "what block
//! is here", "who is nearby"), not just fold the event stream by hand. The
//! driver is the single writer; it updates [`SharedState`] synchronously as each
//! event arrives, then signals a [`Notify`] so blocked waiters re-check their
//! condition. Readers take a short read-lock and clone out a snapshot — they
//! never block the driver, and the driver never holds the lock across an
//! `.await`.
//!
//! ## Why chunk data lives here, not in the event channel
//!
//! Decoded chunk columns are orders of magnitude larger than the scalar events.
//! Routing them through the bounded event channel would let a slow consumer
//! buffer up to *N* whole columns. Instead the version adapter writes each
//! decoded column straight into the client-owned [`World`] through the
//! [`WorldSink`] the driver hands it (see [`SharedState::world_write`]), moving
//! the payload exactly once and never cloning it, and emits only a lightweight
//! [`ClientEvent::ChunkLoaded`] notification carrying the position. World
//! consumers query the store via [`SharedState::block_at`] /
//! [`SharedState::is_chunk_loaded`], or take owned section snapshots via
//! [`SharedState::section_at`] for meshing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, RwLockWriteGuard};

use lodestone_ecs::ecs::entity::Entity;
use lodestone_ecs::session::{
    ServerAlive, ServerBiomeSkyColors, ServerDimension, ServerDimensionType, ServerEntityId,
    ServerGameMode, SessionBossBars, SessionMenus, SessionScoreboard, SessionTabList, Vitals, Xp,
};
use lodestone_ecs::{ChunkWorld, ChunkWorldWrite, EcsHandle, WorldTime};
use lodestone_game::bossbar::BossBarSet;
use lodestone_game::scoreboard::Scoreboard;
use lodestone_game::tablist::TabList;
use lodestone_game::{
    click::{Click, PlayerCtx},
    menu::Menu,
};
use lodestone_model::{
    BlockPos, ChunkPos, ClientAction, ClientEvent, DimensionId, DimensionTypeInfo,
    ConnectionState, EntityAttributeSnapshot, EntityEquipment, EntityPose, EntityVariant, GameMode,
    ItemStack, PlayerListEntry, Reported, ResourceKey, Rotation, Text, Vec3,
};
use lodestone_world::{ChunkPos as WorldChunkPos, ChunkSection, SectionLight, World};
use tokio::sync::Notify;
use uuid::Uuid;

/// Vanilla's own motion-blocking heightmap type's registry id on the
/// 1.21.5+ typed-list wire form (`docs/motion-blocking-heightmap.md`).
/// Duplicated rather than
/// imported: the crate that owns the canonical constant
/// (`lodestone_worldgen::overworld::MOTION_BLOCKING_HEIGHTMAP_TYPE_ID`) is
/// server-only, and this crate must stay reachable from a browser build.
const MOTION_BLOCKING_HEIGHTMAP_TYPE_ID: u32 = 4;

/// An immutable snapshot of the local player's state.
///
/// Fields are `Option` where the server has not told us yet: `position` and
/// `entity_id` are unknown until login and the first teleport, for example.
///
/// # This is a *derived* value, not storage
///
/// Same rule as [`EntityView`], and it arrived for the same reason. Everything
/// here except `position`/`rotation`/`on_ground` is read from
/// `lodestone_ecs::session`'s component set on [`SharedState::session`]:
/// [`Vitals`], [`Xp`], [`ServerEntityId`], [`ServerGameMode`],
/// [`ServerDimension`], [`ServerDimensionType`], [`ServerAlive`]. Those three
/// exceptions are the **local
/// echo** of our own outbound movement ([`SharedState::set_local_movement`]) —
/// genuinely not a fold of anything the server said, which is what lets a bot's
/// `look`/`walk` build on the latest local pose without a round trip.
///
/// The flattened `health: f32` + `health_known: bool` shape (and `xp_*` +
/// `xp_known`) is preserved deliberately: the storage is
/// `Vitals { health: Option<f32>, .. }` and `Xp(Option<(..)>)`, and these fields
/// are that `Option` split in two for a public API whose consumers already read
/// it this way. **Do not add a field here without adding the component it is read
/// from**, or the new field becomes a second source of truth by definition.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerSnapshot {
    /// The local player's entity id, once [`ClientEvent::Login`] has arrived.
    pub entity_id: Option<i32>,
    /// The player's position, once the server has placed us with a teleport.
    pub position: Option<Vec3>,
    /// The player's look direction.
    pub rotation: Rotation,
    /// Whether the player is on the ground (as last reported to the server).
    pub on_ground: bool,
    /// Current health in half-hearts (0.0..=20.0), once known.
    pub health: f32,
    /// Current food level, once known.
    pub food: i32,
    /// Current saturation, once known.
    pub saturation: f32,
    /// Current air supply in ticks (`0..=300`). Unlike `health`/`food`, this
    /// has no `_known` companion: it arrives on entity metadata, not
    /// `set_health`, and a plausible-looking `HudState::MAX_AIR` default before
    /// the first report is the same "not yet reported reads as full" convention
    /// `HudState::default` already uses (`docs/sky-and-air-bubbles.md`).
    pub air: i32,
    /// Whether the player entity is burning — vanilla's own on-fire entity
    /// flag, folded from
    /// `Vitals::on_fire`.
    ///
    /// Unreported reads as `false`, the safe default: an absent flag meaning
    /// "not burning" is the harmless direction, unlike `air` above, whose absence
    /// has to read as *full* or the bubble row would appear on join.
    pub on_fire: bool,
    /// Current game mode, once known.
    pub game_mode: Option<GameMode>,
    /// Current dimension, once known.
    pub dimension: Option<DimensionId>,
    /// The **dimension type** the current dimension points at, as the server
    /// declared it in the Configuration `registry_data`. Read from
    /// [`ServerDimensionType`].
    ///
    /// `None` means the server said nothing usable — **not** "the overworld". A
    /// consumer must state its own fallback; see
    /// `lodestone_shell::mesher::sky_default_for_dimension`.
    pub dimension_type: Option<DimensionTypeInfo>,
    /// Whether the level uses the **flat** world generator, from the login and
    /// respawn packets' own `is_flat` boolean.
    ///
    /// Not part of [`DimensionTypeInfo`] on purpose — it is a level property,
    /// not a registry entry — but it travels with it because vanilla reads the
    /// two together: its own void-darkness-onset-range formula returns `1.0`
    /// when
    /// flat and `32.0` otherwise, and the void fade spans that many blocks
    /// above the dimension type's own `min_y`.
    pub world_is_flat: bool,
    /// Every biome's `minecraft:visual/sky_color` as the server declared it in
    /// the Configuration `registry_data`, **indexed by biome holder
    /// id** and packed `0x00RR_GGBB` in sRGB bytes. Read from
    /// [`ServerBiomeSkyColors`].
    ///
    /// Empty before login and on a server that sent no biome registry. `None` at
    /// an index means that biome declares no sky colour (the Nether and End
    /// biomes). Index it with `ChunkSection::biome_at_block`'s return value —
    /// that integer *is* the holder id, and no other mapping is involved.
    ///
    /// This is the whole table rather than the standing biome's colour because
    /// the standing biome changes as the player walks and nothing on the network
    /// announces it: the lookup has to happen where the camera is.
    pub biome_sky_colors: Arc<[Option<u32>]>,
    /// Whether the player is alive. Set `false` by [`ClientEvent::Death`] and
    /// restored when health becomes positive again after a respawn.
    pub alive: bool,
    /// Whether we have received the initial [`ClientEvent::HealthChanged`].
    pub health_known: bool,
    /// Progress toward the next level, in `0.0..1.0`, once known.
    pub xp_progress: f32,
    /// Current experience level, once known.
    pub xp_level: i32,
    /// Total accumulated experience points, once known.
    pub xp_total: i32,
    /// Whether we have received the initial [`ClientEvent::ExperienceChanged`].
    pub xp_known: bool,
}

impl Default for PlayerSnapshot {
    fn default() -> Self {
        Self {
            entity_id: None,
            position: None,
            rotation: Rotation::default(),
            on_ground: false,
            health: 0.0,
            food: 0,
            saturation: 0.0,
            air: lodestone_game::player_state::HudState::MAX_AIR,
            on_fire: false,
            game_mode: None,
            dimension: None,
            dimension_type: None,
            world_is_flat: false,
            biome_sky_colors: Arc::from([] as [Option<u32>; 0]),
            alive: true,
            health_known: false,
            xp_progress: 0.0,
            xp_level: 0,
            xp_total: 0,
            xp_known: false,
        }
    }
}

/// A view of another entity in the world.
///
/// # This is a *derived* value, not storage
///
/// Since Stage 1 of [`docs/bevy-migration.md`](../../../docs/bevy-migration.md)
/// the authoritative copy of every field below lives in `lodestone-ecs`'s
/// entity component set, folded by the `NetIngest` systems in
/// `lodestone_ecs::ingest`. `Inner` no longer holds a
/// `HashMap<i32, EntityView>`; [`SharedState::entities`] builds these on demand
/// from components ([`entity_view`]).
///
/// The plan permits exactly one intermediate shape here — *components
/// authoritative, this struct derived* — and never the reverse. It survives
/// only because [`crate::ClientHandle::entities`] and its tests still speak
/// this vocabulary; Stage 6 replaces those with ECS queries and this type goes
/// away with them. **Do not add a field here without adding the component it
/// is read from**, or the new field becomes a second source of truth by
/// definition.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityView {
    /// The entity's id.
    pub entity_id: i32,
    /// The entity's UUID, when the spawn carried one.
    pub uuid: Option<Uuid>,
    /// The entity's canonical type key.
    pub entity_type: ResourceKey,
    /// The entity's last known position.
    pub position: Vec3,
    /// The entity's last known rotation.
    pub rotation: Rotation,
    /// The entity's last known head yaw, in degrees.
    ///
    /// Vanilla sends this unconditionally at spawn (`add_entity`) and updates
    /// it independently of body yaw via `rotate_head` — a walking mob's head
    /// tracks its target while its body keeps facing its movement direction,
    /// so this is never derived from `rotation.yaw`.
    pub head_yaw: f32,
    /// The entity's last known velocity, when reported.
    pub velocity: Option<Vec3>,
    /// Whether the entity was last reported on the ground.
    pub on_ground: bool,
    /// Shared entity flags byte (on-fire / crouching / sprinting / …), once a
    /// metadata packet has reported it.
    pub flags: Option<u8>,
    /// The entity's custom name. [`Reported::Reported(None)`](Reported::Reported)
    /// means a name was explicitly cleared; [`Reported::Unreported`] means none
    /// has ever been reported.
    ///
    /// Styled, not flattened — see
    /// [`lodestone_model::event::EntityMetadataUpdate::custom_name`]'s doc;
    /// `lodestone_ecs::entity::CustomName` folds it verbatim into this field.
    pub custom_name: Reported<Text>,
    /// Whether the custom name renders above the entity, once reported.
    pub custom_name_visible: Option<bool>,
    /// The entity's pose, once reported.
    pub pose: Option<EntityPose>,
    /// Current health, once reported (living entities only).
    pub health: Option<f32>,
    /// Whether the entity is a baby, once reported (ageable mobs only).
    pub baby: Option<bool>,
    /// The entity's cosmetic variant (sheep colour, villager profession, horse
    /// colour/markings, biome-specific animal variant, …), once the version
    /// adapter has raised one from a metadata packet.
    ///
    /// `None` means the server has sent no variant override — the renderer
    /// should draw the entity type's vanilla default variant, which is a
    /// different state from a known-but-plain variant. Do not treat `None` as
    /// "unknown".
    pub variant: Option<EntityVariant>,
    /// A creeper's fuse direction (vanilla's own swell-direction metadata
    /// field), once reported —
    /// `-1` while idle or backing off, `1` while counting up to detonation.
    /// **Absent** until the first report, like [`baby`](Self::baby) — which for
    /// an idle, never-approached creeper is forever, since the protocol adapter
    /// synthesises vanilla's own idle default at spawn rather than this field
    /// ever reading a fabricated value. `lodestone-shell::entities`'
    /// `CreeperFuse`/white-flash-overlay chain is the sole consumer; see
    /// `docs/entity-rendering.md`'s "Creeper swell" section.
    pub creeper_swell_dir: Option<i32>,
    /// The entity's attributes, keyed by canonical id, as last reported by
    /// `update_attributes`. Later snapshots for the same attribute replace
    /// earlier ones.
    pub attributes: Vec<EntityAttributeSnapshot>,
    /// The entity's equipped items, keyed by slot, as last reported by
    /// `set_equipment`. Later updates for the same slot replace earlier ones.
    ///
    /// A slot **absent** from this list means the server has never sent an
    /// override for it — the renderer should fall back to that entity type's
    /// vanilla default (usually nothing) — which is a different state from a
    /// slot present with `item: None`, an explicit "this slot is empty"
    /// confirmation from the server. Collapsing the two loses that
    /// distinction, so do not default a missing slot to `None` in-place.
    pub equipment: Vec<EntityEquipment>,
    /// The item stack this entity *displays*, once a metadata packet has
    /// reported it — a dropped item's whole visible identity, and the display
    /// item of thrown projectiles and the eye of ender.
    ///
    /// Same shape as [`custom_name`](Self::custom_name) and for the same
    /// reason: [`Reported::Unreported`] is "the server has never reported this
    /// field", [`Reported::Reported(None)`](Reported::Reported) is the server
    /// explicitly saying the stack is *empty* (which vanilla draws as
    /// nothing). Metadata is incremental, so an update that simply does not
    /// mention the field must leave a previously-known stack alone rather than
    /// clear it — collapsing the two here would make every subsequent
    /// position-only metadata packet erase the item.
    pub item: Reported<ItemStack>,
}

/// The local player's own outbound pose, as we last claimed it — the **whole** of
/// what is left behind [`SharedState`]'s scalar lock. World (chunk) state lives in
/// a separate lock so a chunk write never contends with a scalar read.
///
/// # Why this is all that is left
///
/// Stage 3 **deleted** `players`, `scoreboard`, `boss_bars` and `menus`: they are
/// [`SessionTabList`], [`SessionScoreboard`], [`SessionBossBars`] and
/// [`SessionMenus`] components in [`SharedState::ecs`], folded by
/// `lodestone_ecs::session`'s `NetIngest` systems. The ingest-seam change then
/// deleted the rest of the `PlayerSnapshot` fold — `entity_id`, `health`, `food`,
/// `saturation`, `xp_*`, `game_mode`, `dimension`, `alive` — into that same
/// component set, and with it [`Inner::apply`]'s `Login`, `Respawned`,
/// `HealthChanged`, `Death` and `ExperienceChanged` arms. [`PlayerSnapshot`] is
/// derived now; there is nowhere in this struct for a second copy to live.
///
/// **This is not a fold and that is the point.** `position`/`rotation`/`on_ground`
/// are an echo of what *we* told the server ([`SharedState::set_local_movement`],
/// plus the authoritative correction in [`ClientEvent::TeleportPlayer`]), so a
/// bot's `look`/`walk` can build on the latest local pose without a round trip.
/// The server's own view of where we are is the driver's prediction, in
/// `crate::player::PhysicsState`, and the two are genuinely different facts.
#[derive(Debug, Default, Clone, Copy)]
struct LocalEcho {
    /// The player's position, once the server has placed us with a teleport or we
    /// have moved ourselves.
    position: Option<Vec3>,
    /// The look direction we last claimed.
    rotation: Rotation,
    /// Whether we last reported ourselves on the ground.
    on_ground: bool,
    /// The timestamp a play-state pong echoed from the client's own ping request.
    ///
    /// This is an acknowledgement, not a clock: the shell compares it with its
    /// portable current epoch when it draws the F3 round-trip-time line.
    last_ping_echo_ms: Option<i64>,
}

/// A snapshot of the currently open non-player menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenMenuSnapshot {
    /// Window/container id.
    pub window_id: i32,
    /// Canonical menu type key.
    pub menu_type: ResourceKey,
    /// Screen title.
    pub title: Text,
    /// Predicted menu contents to render.
    pub menu: Menu,
    /// Menu-local `container_set_data` properties (`(property_id, value)`) —
    /// the anvil's XP cost, the enchanting table's three level costs. `Menus`
    /// already decodes and folds this (`Menus::opened_data`); this was the
    /// one hop that dropped it before it ever reached a snapshot — see
    /// `docs/container-cost-screens.md`'s "What is not yet wired" section.
    pub data: Vec<(i32, i32)>,
}

/// A cheap, cloneable handle to the maintained read-model.
///
/// Clones share the same underlying state; the driver holds one clone as the
/// sole writer and the [`crate::ClientHandle`] holds another for reads.
#[derive(Clone)]
pub(crate) struct SharedState {
    inner: Arc<RwLock<LocalEcho>>,
    world: Arc<RwLock<World>>,
    notify: Arc<Notify>,
    /// The bevy_ecs `World` this state is authoritative over: [`WorldTime`], the
    /// entity component set, and (since Stage 3) the session read-model — the
    /// scoreboard, tab list, boss bars and menus — as components on
    /// [`Self::session`].
    ///
    /// # Whose `World` this is, since §4.1(c)
    ///
    /// Either the driver's or its own, and the difference is which constructor
    /// ran. [`SharedState::adopting`] takes the handle a driver already built
    /// (`lodestone_shell::sim::Sim`'s) so ingest folds straight into the `World`
    /// the driver's systems read — that is the unification, and it is what makes
    /// a `Vitals` written here visible to a `GameTick` system without a mirror.
    /// [`SharedState::default`] mints one, for a bot or a test with no driver at
    /// all.
    ///
    /// The direction matters and is not symmetric: the *driver* owns the `World`
    /// and hands it down. A `SharedState` that minted the `World` and let the
    /// driver adopt it would make the `World`'s identity change at connect time,
    /// and `Sim.local` — the local player's `Entity`, held across
    /// `Sim::end_session` by the voluntary-teardown path — would be invalidated
    /// by every reconnect.
    ///
    /// Every access here is a short guard, per [`EcsHandle`]'s lock discipline.
    /// Nothing in this file holds one across an `.await`.
    ecs: EcsHandle,
    /// The session entity in [`Self::ecs`], carrying `lodestone_ecs::session`'s
    /// shared-fold component set.
    ///
    /// Stable for the life of the state. Held rather than looked up by query so
    /// a read is a plain `World::get` under a *read* lock — a `Query` needs
    /// `&mut World` (it caches its `QueryState`) and would contend with the net
    /// thread's ingest writes for nothing at this scale.
    session: Entity,
    /// Whether [`Self::ecs`] carries [`lodestone_ecs::GameEventBus`] — checked
    /// **once**, at construction, and cached here so [`Self::apply`]'s hot
    /// path is a plain `bool` read when the bus is off ("zero
    /// cost when unused"). See [`lodestone_ecs::GameEventBusPlugin`]'s doc for
    /// why a runtime toggle is not needed: a plugin opts in by being present
    /// when the `World` is built, and that is always before a `SharedState`
    /// wraps it.
    game_event_bus_enabled: bool,
    /// Whether [`Self::ecs`] carries [`lodestone_ecs::RawPacketBus`]. This is
    /// cached at construction so the ordinary client does not clone inbound
    /// packet payloads or take an ECS lock when no plugin observes them.
    raw_packet_bus_enabled: bool,
    /// Whether the driver's live [`ConnectionState`](lodestone_model::ConnectionState)
    /// is currently `Play`, kept in lockstep with `Driver::state` by the
    /// `Directive::SetState` arm in `driver.rs`.
    ///
    /// A plain [`AtomicBool`] rather than a field on [`LocalEcho`] behind the
    /// `RwLock`: this is read on the shell's net loop every drain (
    /// a per-tick `Move` submitted while the connection has dropped back into
    /// `Configuration`, e.g. a mid-session dimension-change reconfigure, has no
    /// encode arm and was spamming "action has no packet in current state" once
    /// per tick), so it wants the cheapest possible read rather than a lock
    /// shared with the rest of the echo.
    in_play: Arc<AtomicBool>,
}

impl std::fmt::Debug for SharedState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Avoid locking (and dumping the whole world) in Debug output.
        f.debug_struct("SharedState").finish_non_exhaustive()
    }
}

impl Default for SharedState {
    fn default() -> Self {
        // `new_ingest_handle` (not `new_handle`) because this is the `World`
        // that is *authoritative* over entity state: it carries
        // `IngestPlugin`'s `NetIngest` systems. `WorldTime` is still inserted
        // explicitly rather than by a plugin — see `CorePlugin`'s docs on why
        // the clock's owner must be named at the call site.
        let ecs = lodestone_ecs::new_ingest_handle();
        // Stage 3: `new_ingest_handle` carries `SessionPlugin`'s `NetIngest`
        // systems as well as `IngestPlugin`'s, so this `World` folds the session
        // read-model too. It needs one entity to hang those components off.
        let world = Arc::new(RwLock::new(World::new()));
        let (session, game_event_bus_enabled, raw_packet_bus_enabled) =
            lodestone_ecs::hold_write(&ecs, |world_ecs| {
                world_ecs.insert_resource(WorldTime::default());
                // Stage 4 (§4.1(d)): the chunk store is a resource, and it is the
                // *same* store `world_write` hands the adapter — one `Arc`, two
                // names. A system or plugin in this `World` can therefore read
                // chunks without a second copy existing anywhere.
                world_ecs.insert_resource(ChunkWorld::from_shared(Arc::clone(&world)));
                // The matching write resource shares this `Arc`, so ECS systems
                // that edit chunks use `ChunkWorldWrite` rather than bypassing the
                // synchronization behind `world_write`.
                world_ecs.insert_resource(ChunkWorldWrite::from_shared(Arc::clone(&world)));
                let session = lodestone_ecs::spawn_session(world_ecs);
                // `new_ingest_handle()` does not install either optional bus;
                // cache that fact so the ordinary state performs no bus work.
                let game_event_bus_enabled =
                    world_ecs.contains_resource::<lodestone_ecs::GameEventBus>();
                let raw_packet_bus_enabled =
                    world_ecs.contains_resource::<lodestone_ecs::RawPacketBus>();
                (session, game_event_bus_enabled, raw_packet_bus_enabled)
            });
        Self {
            inner: Arc::new(RwLock::new(LocalEcho::default())),
            world,
            notify: Arc::new(Notify::new()),
            ecs,
            session,
            game_event_bus_enabled,
            raw_packet_bus_enabled,
            in_play: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Pushes `event` onto the plugin event bus, unconditionally —
/// no `match` on `event` anywhere in this function's body, checked by
/// `tests::game_event_bus_write_site_has_no_match_on_the_event` below in the
/// style of `lodestone_model::event::route_tests::route_has_no_catch_all_arm`.
///
/// # Why this cannot become a sixth silent-drop router
///
/// `docs/plugin-api.md`'s doctrine names three existing routers whose
/// terminal `_ =>` arm is an island factory: a wildcard that is
/// indistinguishable, at the call site, from a decision, so a new
/// `ClientEvent` variant can compile with no arm anywhere and reach nothing.
/// A firehose with **no match on the event at all** cannot have that shape —
/// every event that reaches this function reaches the bus, full stop. The
/// caller ([`SharedState::apply`]) still gates *whether* this function runs
/// at all on `SharedState`'s cached `game_event_bus_enabled` flag, but that
/// gate is a boolean feature flag, not a discrimination by event variant.
fn push_to_game_event_bus(world: &mut lodestone_ecs::ecs::world::World, event: &ClientEvent) {
    world.write_message(lodestone_ecs::GameEvent(event.clone()));
}

/// Local block coordinate within a chunk column (0..16).
fn local_xz(coord: i32) -> usize {
    coord.rem_euclid(16) as usize
}

/// The world store keys chunks by [`lodestone_world::ChunkPos`]; the public API
/// speaks [`lodestone_model::ChunkPos`]. Both are `{ x, z }` in chunk units, so
/// the bridge is a field copy. These two helpers are the *only* place the two
/// vocabularies meet.
fn to_world_pos(pos: ChunkPos) -> WorldChunkPos {
    WorldChunkPos::new(pos.x, pos.z)
}

fn to_model_pos(pos: WorldChunkPos) -> ChunkPos {
    ChunkPos::new(pos.x, pos.z)
}

impl SharedState {
    /// A read-model that folds into a `World` **the caller already owns**, hanging
    /// the session components off an entity the caller already spawned.
    ///
    /// This is §4.1(c): `lodestone_shell::sim::Sim` builds the one `World` in the
    /// process (with `IngestPlugin` and `SessionPlugin` among its plugins, so the
    /// fold systems are registered exactly once — `add_systems` does not
    /// deduplicate) and threads the handle down through `NetClient::connect`.
    ///
    /// Deliberately inserts **nothing**. [`Self::default`] seeds `WorldTime` and a
    /// `ChunkWorld`; doing either here would overwrite a live clock and would
    /// steal the chunk-store adoption decision from the driver, which owns it
    /// (`Sim::adopt_live_world`, and the `collide_against_live_world` negative
    /// control that depends on an explicitly empty store). The chunk store is
    /// still reachable as [`Self::chunk_world`] for the driver to adopt when it
    /// chooses.
    ///
    /// Also checks `ecs` once for [`lodestone_ecs::GameEventBus`] — see
    /// [`Self::game_event_bus_enabled`]'s field doc. This is today's real
    /// opt-in path for the plugin event bus: whoever builds `ecs` (currently
    /// `lodestone_shell::sim::Sim`, brokered / out of this pass's scope)
    /// decides by adding `GameEventBusPlugin` before calling this.
    pub(crate) fn adopting(ecs: EcsHandle, session: Entity) -> Self {
        let (game_event_bus_enabled, raw_packet_bus_enabled) =
            lodestone_ecs::hold_read(&ecs, |world| {
                (
                    world.contains_resource::<lodestone_ecs::GameEventBus>(),
                    world.contains_resource::<lodestone_ecs::RawPacketBus>(),
                )
            });
        Self {
            inner: Arc::new(RwLock::new(LocalEcho::default())),
            world: Arc::new(RwLock::new(World::new())),
            notify: Arc::new(Notify::new()),
            ecs,
            session,
            game_event_bus_enabled,
            raw_packet_bus_enabled,
            in_play: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Publishes one inbound packet to the optional raw observation bus before
    /// the version adapter sees it. The payload is cloned only when a plugin
    /// installed [`lodestone_ecs::RawPacketBusPlugin`] in the caller's world.
    pub(crate) fn record_raw_packet(
        &self,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) {
        if self.raw_packet_bus_enabled {
            lodestone_ecs::hold_write(&self.ecs, |world| {
                world.write_message(lodestone_ecs::RawPacket {
                    state,
                    packet_id,
                    payload: payload.to_vec(),
                });
            });
        }
    }

    /// Returns a future-friendly handle used by waiters to be woken when the
    /// state changes. Callers register `notified()` *before* re-checking their
    /// predicate to avoid missing a wake-up.
    pub(crate) fn notifier(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    /// Wakes every current waiter. Called by the driver after each update
    /// (including after a chunk write through [`SharedState::world_write`], so
    /// world-query waiters re-check even if the adapter batches its
    /// notification separately).
    pub(crate) fn wake(&self) {
        self.notify.notify_waiters();
    }

    /// Folds a non-chunk event into the read-model, then wakes waiters.
    ///
    /// Chunk data is written by the adapter through [`SharedState::world_write`]
    /// instead, so the heavy payload is never borrowed or cloned here.
    ///
    /// [`ClientEvent::TimeChanged`], every entity event and (since Stage 3)
    /// every session event are handled *here* rather than inside
    /// [`Inner::apply`]: they live in this state's [`EcsHandle`] (`self.ecs`), a
    /// sibling field `Inner::apply` has no access to. What is left in
    /// `Inner::apply` is the local-player echo and nothing else.
    ///
    /// # Why entity events run a schedule instead of touching components
    ///
    /// The fold *is* the `NetIngest` systems
    /// ([`lodestone_ecs::ingest`]), so this method's job is only to enqueue and
    /// run. `lodestone_ecs::ingest::handles_event` is the routing switch; it
    /// lives beside the systems so the two cannot drift, because an event
    /// routed here that no system folds would vanish silently.
    ///
    /// One event per schedule run, deliberately — that is what makes arrival
    /// order across event families exact without the systems having to
    /// interleave (see the ordering note in `lodestone_ecs::ingest`'s module
    /// docs).
    ///
    /// The `clone()` is the one cost this shape adds over the old in-place fold:
    /// the queue owns its events, and this method only borrows. Entity events are
    /// small (a pose delta, a metadata patch) and arrive at tick rate for ~30
    /// entities, so it is not worth an API change to avoid — but if `apply` ever
    /// takes the event by value, drop it.
    pub(crate) fn apply(&self, event: &ClientEvent) {
        // Push to the optional event bus before routing. This deliberately
        // avoids matching on `event`, so every event variant reaches the bus
        // without needing a parallel routing table here.
        // `self.game_event_bus_enabled` is a plain `bool` cached at
        // construction (`Self::default`/`Self::adopting`), so a client that
        // never opted in pays nothing beyond this one branch — no extra
        // `EcsHandle` lock.
        if self.game_event_bus_enabled {
            lodestone_ecs::hold_write(&self.ecs, |world| {
                push_to_game_event_bus(world, event);
            });
        }
        if let ClientEvent::TimeChanged {
            world_age,
            time_of_day,
        } = event
        {
            // Through `hold_write` rather than `self.ecs.write()` so this hold
            // joins the same `LockHolds` meter the driver's guards do. This is
            // the guard that matters most: `apply` runs *inline in the driver
            // task*, before `events.send(event).await`, so blocking here stops
            // the socket being read — not merely delayed application.
            lodestone_ecs::hold_write(&self.ecs, |world| {
                let mut time = world.resource_mut::<WorldTime>();
                time.age = *world_age;
                time.time_of_day = *time_of_day;
            });
        } else if lodestone_ecs::ingest::handles_event(event)
            || lodestone_ecs::session::handles_event(event)
        {
            // Metered for the same reason as the `TimeChanged` arm above: this
            // hold spans a whole `NetIngest` run, so it is the realistic
            // candidate for the longest ingest-side hold.
            lodestone_ecs::hold_write(&self.ecs, |world| {
                world
                    .resource_mut::<lodestone_ecs::ingest::IngestQueue>()
                    .push(event.clone());
                world.run_schedule(lodestone_ecs::NetIngest);
            });
        } else {
            let mut echo = self.inner.write().unwrap_or_else(|e| e.into_inner());
            echo.apply(event);
        }
        self.wake();
    }

    /// Borrows the client-owned world for writing. The driver hands the returned
    /// guard to the adapter as a [`lodestone_world::WorldSink`] so decoded
    /// chunks are applied in place; the guard is dropped before any waiter is
    /// woken. Callers must not hold it across an `.await`.
    pub(crate) fn world_write(&self) -> RwLockWriteGuard<'_, World> {
        self.world.write().unwrap_or_else(|e| e.into_inner())
    }

    /// The client-owned chunk store, as the `Resource` handle
    /// `docs/bevy-migration.md` §4.1(d) calls for.
    ///
    /// This is a **handle onto the same `World`** [`Self::world_write`] writes
    /// decoded columns into, not a copy — `Arc::ptr_eq`-identical, and
    /// [`crate::state::SharedState::default`] installs the same handle as a
    /// resource in [`Self::ecs`]. A driver that adopts this (the shell does, at
    /// [`crate::ClientHandle::chunk_world`]) is therefore *naming* the client's
    /// store rather than mirroring it, which is what makes the Stage 4 authority
    /// test — one chunk store in the process — mean anything.
    #[must_use]
    pub(crate) fn chunk_world(&self) -> ChunkWorld {
        ChunkWorld::from_shared(Arc::clone(&self.world))
    }

    /// The write half of the store split, on the **same** `Arc`
    /// [`chunk_world`](Self::chunk_world) hands out — the sanctioned route for a
    /// driver (`Sim::adopt_live_world`) or a test harness that must edit the
    /// store, paired with the read handle so the two never name different worlds.
    pub(crate) fn chunk_world_write(&self) -> ChunkWorldWrite {
        ChunkWorldWrite::from_shared(Arc::clone(&self.world))
    }

    /// Records the driver's live `ConnectionState`, kept current by every
    /// `Directive::SetState` the driver executes -- see [`Self::in_play`].
    pub(crate) fn set_in_play(&self, in_play: bool) {
        self.in_play.store(in_play, Ordering::Relaxed);
    }

    /// Whether the driver's connection is currently in the `Play` state.
    ///
    /// `Relaxed` is enough: this gates a per-tick best-effort submission
    /// (drop a `Move` rather than hand it to a driver that can only log and
    /// discard it), not a correctness-critical read, and it carries no other
    /// memory access that needs to happen-before or -after it.
    #[must_use]
    pub(crate) fn in_play(&self) -> bool {
        self.in_play.load(Ordering::Relaxed)
    }

    /// Records the player's own outgoing movement so subsequent look/step
    /// queries build on the latest local position without waiting for the
    /// server to echo it back.
    pub(crate) fn set_local_movement(&self, pos: Vec3, rotation: Rotation, on_ground: bool) {
        {
            let mut echo = self.inner.write().unwrap_or_else(|e| e.into_inner());
            echo.position = Some(pos);
            echo.rotation = rotation;
            echo.on_ground = on_ground;
        }
        self.wake();
    }

    /// The local echo on its own — our last claimed pose, with **no ECS lock
    /// taken**.
    ///
    /// [`Self::position`] and [`Self::rotation`] go through this rather than
    /// through [`Self::player`] deliberately: they are the two reads a moving bot
    /// makes most often, and there is no reason for them to contend with ingest
    /// for the `World` lock just to reach a field that does not live there.
    fn local_echo(&self) -> LocalEcho {
        *self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    /// The player's last claimed position, or `None` before the server has placed
    /// us. Echo-only; takes no ECS lock.
    #[must_use]
    pub(crate) fn position(&self) -> Option<Vec3> {
        self.local_echo().position
    }

    /// The player's last claimed look direction. Echo-only; takes no ECS lock.
    #[must_use]
    pub(crate) fn rotation(&self) -> Rotation {
        self.local_echo().rotation
    }

    /// The most recent timestamp echoed by a play-state pong, if one arrived.
    ///
    /// The value is exactly the `time` the caller put in
    /// [`ClientAction::PingRequest`](lodestone_model::ClientAction::PingRequest),
    /// not a driver-side receipt time. This keeps the clock choice with the
    /// caller that displays or measures the round trip, including wasm builds.
    #[must_use]
    pub(crate) fn last_ping_echo_ms(&self) -> Option<i64> {
        self.local_echo().last_ping_echo_ms
    }

    /// Builds the current player snapshot.
    ///
    /// **Derived, not stored** — see [`PlayerSnapshot`]'s docs. The local echo and
    /// the component set are two locks, and they are taken in that order, once,
    /// with the echo guard **released before** the ECS guard is acquired. Nothing
    /// anywhere takes them the other way round, so there is no ABBA pair to
    /// worry about; the release is for the lock-hold discipline
    /// (`docs/world-unification.md`), not for correctness.
    ///
    /// # This read is ECS-backed now, which moves it under rule 1
    ///
    /// It used to touch only the scalar lock. A caller holding an
    /// `lodestone_ecs::EcsHandle` guard — a driver inside `run_schedule`, say —
    /// must **not** call this: `parking_lot::RwLock` is not reentrant and
    /// `read()` behind a queued writer deadlocks. The one shell call site
    /// (`Sim::refresh_mesh_policy`, and `mesher::snapshot_section_live` behind it)
    /// runs with no guard held.
    #[must_use]
    pub(crate) fn player(&self) -> PlayerSnapshot {
        let echo = self.local_echo();
        lodestone_ecs::hold_read(&self.ecs, |world| {
            let vitals = world
                .get::<Vitals>(self.session)
                .copied()
                .unwrap_or_default();
            let xp = world.get::<Xp>(self.session).and_then(|xp| xp.0);
            PlayerSnapshot {
                entity_id: world.get::<ServerEntityId>(self.session).and_then(|id| id.0),
                position: echo.position,
                rotation: echo.rotation,
                on_ground: echo.on_ground,
                // `Vitals` holds one `Option` per field; the snapshot splits it into a
                // value plus `health_known`, which is the shape `ClientHandle::health`
                // and the HUD already read. Unreported reads as zero *and*
                // `health_known == false`, never as a plausible full bar.
                health: vitals.health.unwrap_or(0.0),
                food: vitals.food.unwrap_or(0),
                saturation: vitals.saturation.unwrap_or(0.0),
                // Unreported reads as full air, not zero — an un-drowning player,
                // matching `HudState::default`'s own convention, rather than the
                // "unknown reads as empty" shape `health`/`food` use (those have a
                // `_known` bit precisely because zero is a plausible real value;
                // air has no such ambiguity worth a second field for).
                air: vitals.air.unwrap_or(lodestone_game::player_state::HudState::MAX_AIR),
                on_fire: vitals.on_fire.unwrap_or(false),
                health_known: vitals.health.is_some(),
                game_mode: world.get::<ServerGameMode>(self.session).and_then(|m| m.0),
                dimension: world
                    .get::<ServerDimension>(self.session)
                    .and_then(|d| d.0.clone()),
                dimension_type: world
                    .get::<ServerDimensionType>(self.session)
                    .and_then(|d| d.info.clone()),
                // Absent component reads as *not* flat, which is also the answer
                // for every protocol family that does not send the flag — and is
                // the conservative one: it applies vanilla's ordinary 32-block
                // void fade rather than suppressing it.
                world_is_flat: world
                    .get::<ServerDimensionType>(self.session)
                    .is_some_and(|d| d.is_flat),
                // A refcount bump, not a copy of the table — see
                // [`ServerBiomeSkyColors`]. Absent component reads as "no biome
                // registry", which the shell renders as its dimension default.
                biome_sky_colors: world
                    .get::<ServerBiomeSkyColors>(self.session)
                    .map_or_else(|| Arc::from([] as [Option<u32>; 0]), |c| Arc::clone(&c.0)),
                // Absent component reads as *alive*, matching `ServerAlive::default`:
                // a client nobody has told otherwise is not dead.
                alive: world
                    .get::<ServerAlive>(self.session)
                    .is_none_or(|alive| alive.0),
                xp_progress: xp.map_or(0.0, |(progress, _, _)| progress),
                xp_level: xp.map_or(0, |(_, level, _)| level),
                xp_total: xp.map_or(0, |(_, _, total)| total),
                xp_known: xp.is_some(),
            }
        })
    }

    /// The local player's own attributes, as `update_attributes` last reported
    /// them.
    ///
    /// Empty before login, and empty on a server that has sent none. This reads
    /// the [`Attributes`](lodestone_ecs::entity::Attributes) component on the
    /// session entity, which only became reachable once
    /// `lodestone_ecs::ingest::apply_local_player_login` put our own id in
    /// `EntityIndex` — before that, `apply_entity_attributes` dropped every
    /// snapshot for the local player on the floor.
    ///
    /// Deliberately **not** routed through [`Self::entity`]: the local player
    /// carries no `EntityKind`/`Position`/`Rotation`/`HeadYaw` (those would
    /// duplicate the driver's `PhysicsState`), so [`entity_view`] cannot build a
    /// view of it and must not be taught to.
    #[must_use]
    pub(crate) fn local_attributes(&self) -> Vec<EntityAttributeSnapshot> {
        lodestone_ecs::hold_read(&self.ecs, |world| {
            world
                .get::<lodestone_ecs::entity::Attributes>(self.session)
                .map(|attributes| attributes.0.clone())
                .unwrap_or_default()
        })
    }

    /// Returns the block-state id at `pos`, or `None` if the containing chunk is
    /// not loaded. The value is the adapter's version-free block-state id; the
    /// id → name mapping is a registry concern, not the client's.
    #[must_use]
    pub(crate) fn block_at(&self, pos: BlockPos) -> Option<u32> {
        let world = self.world.read().unwrap_or_else(|e| e.into_inner());
        let chunk = world.get(WorldChunkPos::from_block(pos.x, pos.z))?;
        Some(
            chunk
                .column
                .get_block(local_xz(pos.x), pos.y, local_xz(pos.z)),
        )
    }

    /// Returns an owned snapshot of the `Arc<ChunkSection>` at `section_index`
    /// within the chunk at `pos`, or `None` if the chunk is not loaded or that
    /// section is elided (all air). The returned `Arc` carries no borrow into the
    /// world and pins no lock: a mesher grabs it, releases the lock, and meshes
    /// off a stable snapshot while chunk streaming continues. A later edit forks
    /// that section copy-on-write, leaving the snapshot untouched.
    ///
    /// Note: this hands out block-state sections only. The column's light — which
    /// lit meshing also needs — is served in parallel by [`section_light`](Self::section_light)
    /// and [`lights_at`](Self::lights_at).
    #[must_use]
    pub(crate) fn section_at(
        &self,
        pos: ChunkPos,
        section_index: usize,
    ) -> Option<Arc<ChunkSection>> {
        self.world
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .section(to_world_pos(pos), section_index)
    }

    /// Returns one owned section snapshot per requested `(chunk, section_index)`,
    /// in order, taking the world read lock exactly once. This is the mesher's
    /// bulk primitive: it pulls a whole 27-section neighbourhood under a single
    /// brief lock, then meshes off the returned `Arc`s with no lock held.
    #[must_use]
    pub(crate) fn sections_at(
        &self,
        requests: &[(ChunkPos, usize)],
    ) -> Vec<Option<Arc<ChunkSection>>> {
        let world = self.world.read().unwrap_or_else(|e| e.into_inner());
        requests
            .iter()
            .map(|(pos, index)| world.section(to_world_pos(*pos), *index))
            .collect()
    }

    /// Returns an owned [`SectionLight`] snapshot of light section
    /// `light_section_index` within the chunk at `pos`, or `None` if the chunk is
    /// not loaded or that light section is out of range.
    ///
    /// This is the light-side companion to [`section_at`](Self::section_at). Light
    /// is indexed in its native *light-section* space (`0` is the section below the
    /// world; light section `i` covers world block-section `i - 1`), which is what
    /// lets a mesher reach the boundary light sections above and below the build
    /// range. Unlike [`section_at`](Self::section_at) an all-air (elided) block
    /// section still yields `Some` light: air carries light a face must sample.
    #[must_use]
    pub(crate) fn section_light(
        &self,
        pos: ChunkPos,
        light_section_index: usize,
    ) -> Option<SectionLight> {
        self.world
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .section_light(to_world_pos(pos), light_section_index)
    }

    /// Returns one owned light snapshot per requested `(chunk, light_section_index)`,
    /// in order, taking the world read lock exactly once — the light-side twin of
    /// [`sections_at`](Self::sections_at) for pulling a whole meshing neighbourhood
    /// under a single brief lock.
    #[must_use]
    pub(crate) fn lights_at(&self, requests: &[(ChunkPos, usize)]) -> Vec<Option<SectionLight>> {
        let world = self.world.read().unwrap_or_else(|e| e.into_inner());
        requests
            .iter()
            .map(|(pos, index)| world.section_light(to_world_pos(*pos), *index))
            .collect()
    }

    /// Returns a `(block section, light section)` snapshot pair per requested
    /// `(chunk, block_section_index, light_section_index)`, in order, under a
    /// single world read lock — so a mesher's geometry and light for a
    /// neighbourhood come from one lock epoch rather than two. The two indices
    /// are distinct spaces (block-section for the `Arc`, light-section for the
    /// light) and are passed straight through with no translation.
    #[must_use]
    pub(crate) fn sections_and_light_at(
        &self,
        requests: &[(ChunkPos, usize, usize)],
    ) -> Vec<(Option<Arc<ChunkSection>>, Option<SectionLight>)> {
        let world = self.world.read().unwrap_or_else(|e| e.into_inner());
        requests
            .iter()
            .map(|(pos, block_index, light_index)| {
                let wp = to_world_pos(*pos);
                (
                    world.section(wp, *block_index),
                    world.section_light(wp, *light_index),
                )
            })
            .collect()
    }

    /// Returns a clone of chunk `pos`'s `MOTION_BLOCKING` heightmap, or `None`
    /// if the chunk is not loaded or carries no such map — an offline/local-gen
    /// chunk built with an empty [`Heightmaps`](lodestone_world::Heightmaps)
    /// (`lodestone_shell::worldgen`'s `LoadedChunk::new(column, light,
    /// Heightmaps::new(), …)`) has none.
    ///
    /// Hands back the whole 16×16 map rather than one column's height so a
    /// caller sampling many blocks in the same chunk — the weather pass's
    /// probe, 21 columns deep at the default radius — pays the world lock
    /// once per chunk rather than once per block, the same shape as
    /// [`section_at`](Self::section_at)/[`sections_at`](Self::sections_at).
    #[must_use]
    pub(crate) fn column_heightmap(&self, pos: ChunkPos) -> Option<lodestone_world::Heightmap> {
        let world = self.world.read().unwrap_or_else(|e| e.into_inner());
        world
            .get(to_world_pos(pos))?
            .heightmaps
            .get(MOTION_BLOCKING_HEIGHTMAP_TYPE_ID)
            .cloned()
    }

    /// Returns the connected dimension's vertical extent as `(min_y, height)`,
    /// or `None` if no column is loaded. Every column in a dimension shares the
    /// same shape — the adapter builds them all from the one dimension type the
    /// server sent at login — so any loaded column is authoritative; `height` is
    /// `section_count * 16`.
    #[must_use]
    pub(crate) fn world_extent(&self) -> Option<(i32, u32)> {
        let world = self.world.read().unwrap_or_else(|e| e.into_inner());
        let column = &world.values().next()?.column;
        Some((
            column.min_y(),
            (column.section_count() * ChunkSection::EDGE) as u32,
        ))
    }

    /// Whether the chunk at `pos` is currently loaded.
    #[must_use]
    pub(crate) fn is_chunk_loaded(&self, pos: ChunkPos) -> bool {
        self.world
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(to_world_pos(pos))
    }

    /// Number of currently loaded chunk columns.
    #[must_use]
    pub(crate) fn loaded_chunk_count(&self) -> usize {
        self.world.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Positions of all currently loaded chunk columns.
    #[must_use]
    pub(crate) fn loaded_chunks(&self) -> Vec<ChunkPos> {
        self.world
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(pos, _)| to_model_pos(*pos))
            .collect()
    }

    /// Derives a single entity view from its components.
    ///
    /// Takes only a **read** lock on the ECS `World`: the lookup goes through
    /// the [`EntityIndex`](lodestone_ecs::entity::EntityIndex) resource and
    /// `World::get_entity`, both of which are `&World` operations. A `Query`
    /// would need `&mut World` (it caches its `QueryState`) and would therefore
    /// contend with the net thread's ingest writes for no benefit at this
    /// entity count.
    #[must_use]
    pub(crate) fn entity(&self, entity_id: i32) -> Option<EntityView> {
        lodestone_ecs::hold_read(&self.ecs, |world| {
            let entity = world
                .resource::<lodestone_ecs::entity::EntityIndex>()
                .get(entity_id)?;
            entity_view(world.get_entity(entity).ok()?)
        })
    }

    /// Derives all currently tracked entities from their components.
    ///
    /// # The local player is excluded, explicitly
    ///
    /// `EntityIndex` now holds our *own* id too
    /// (`lodestone_ecs::ingest::apply_local_player_login`), and this must keep
    /// meaning "the other entities" — the shell maps it straight to render
    /// instances (`NetClient::entity_snapshots`), so including the local player
    /// would draw our own body at our own camera.
    ///
    /// The filter is written out rather than left to fall out of the local
    /// player having no `EntityKind`/`Position`/`Rotation`/`HeadYaw` for
    /// [`entity_view`] to read. That *would* also exclude it today, and it is
    /// exactly the kind of accidental invariant that breaks silently the first
    /// time someone adds one of those components for an unrelated reason.
    #[must_use]
    pub(crate) fn entities(&self) -> Vec<EntityView> {
        lodestone_ecs::hold_read(&self.ecs, |world| {
            world
                .resource::<lodestone_ecs::entity::EntityIndex>()
                .iter()
                .filter_map(|(_, entity)| {
                    let entity = world.get_entity(entity).ok()?;
                    if entity.contains::<lodestone_ecs::LocalPlayer>() {
                        return None;
                    }
                    entity_view(entity)
                })
                .collect()
        })
    }

    /// Clones out the current player-list entries as the model's flat wire
    /// shape.
    ///
    /// **Derived, not stored** — the same one-directional intermediate Stage 1
    /// established for [`EntityView`]: the [`SessionTabList`] component is the
    /// only copy and this rebuilds the model struct for callers that still speak
    /// it. Delta fields are `Some` because a folded entry has a value for each;
    /// the profile UUID remains `None` for protocol families whose player-list
    /// wire format identifies entries by name alone.
    #[must_use]
    pub(crate) fn players(&self) -> Vec<PlayerListEntry> {
        self.tab_list()
            .iter()
            .map(|entry| PlayerListEntry {
                uuid: entry.profile.id,
                name: Some(entry.profile.name.clone()),
                game_mode: Some(entry.game_mode),
                latency: Some(entry.latency),
                display_name: entry.display_name.clone(),
                listed: Some(entry.listed),
                // A folded entry has a concrete collection even when it is
                // empty. The wire `Option` meant a delta omitted this field;
                // the fold has already merged that distinction away.
                properties: Some(
                    entry
                        .profile
                        .properties
                        .iter()
                        .map(|property| lodestone_model::ProfileProperty {
                            name: property.name.clone(),
                            value: property.value.clone(),
                            signature: property.signature.clone(),
                        })
                        .collect(),
                ),
                // Unlike `properties`, `None` here is unambiguous: this
                // player either has announced a session or has not, with no
                // "explicitly empty" third state to distinguish from "no
                // update yet".
                chat_session: entry.chat_session.as_ref().map(|session| {
                    lodestone_model::event::ChatSessionInfo {
                        session_id: session.session_id,
                        public_key: session.public_key.clone(),
                        expires_at: session.expires_at,
                    }
                }),
                // Same "always Some" reasoning as the rest of this literal: a
                // folded entry has a real list-order and hat-visibility value
                // (defaults `0`/`true` if the server never sent either action).
                list_order: Some(entry.list_order),
                hat_visible: Some(entry.show_hat),
            })
            .collect()
    }

    /// Clones out the folded tab list — profiles, latency, game modes, display
    /// names, header and footer.
    ///
    /// This is the richer shape [`Self::players`] is flattened from, and the one
    /// the shell's tab overlay reads (it needs `ordered()`, which the flat list
    /// cannot express).
    #[must_use]
    pub(crate) fn tab_list(&self) -> TabList {
        lodestone_ecs::hold_read(&self.ecs, |world| {
            world
                .get::<SessionTabList>(self.session)
                .map(|list| list.0.clone())
                .unwrap_or_default()
        })
    }

    /// One player's announced chat-signing session, if the tab list has
    /// folded one in for them — the lookup
    /// [`crate::driver::Driver`]'s incoming-chat signature verification needs.
    ///
    /// A single-entry lookup rather than routing every caller through
    /// [`Self::tab_list`]: that clones the whole `HashMap`, which is wasted
    /// work for the common case of checking one sender.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub(crate) fn chat_session_of(&self, id: &uuid::Uuid) -> Option<lodestone_game::tablist::RemoteChatSession> {
        lodestone_ecs::hold_read(&self.ecs, |world| {
            world
                .get::<SessionTabList>(self.session)
                .and_then(|list| list.0.get(id))
                .and_then(|entry| entry.chat_session.clone())
        })
    }

    /// Returns `(world_age, time_of_day)`, read from the [`WorldTime`]
    /// resource in `self.ecs` — the sole backing store since Stage 0 of
    /// `docs/bevy-migration.md` deleted `Inner.world_age`/`Inner.time_of_day`.
    #[must_use]
    pub(crate) fn time(&self) -> (i64, i64) {
        lodestone_ecs::hold_read(&self.ecs, |world| {
            let time = world.resource::<WorldTime>();
            (time.age, time.time_of_day)
        })
    }

    /// Clones out the current folded scoreboard (objectives, scores, the
    /// nineteen display slots and teams).
    ///
    /// The [`SessionScoreboard`] component is the only copy in the process since
    /// Stage 3 — `lodestone_client::scoreboard::Scoreboard` and
    /// `lodestone_shell::sim::Sim::scoreboard` are both gone.
    #[must_use]
    pub(crate) fn scoreboard(&self) -> Scoreboard {
        lodestone_ecs::hold_read(&self.ecs, |world| {
            world
                .get::<SessionScoreboard>(self.session)
                .map(|board| board.0.clone())
                .unwrap_or_default()
        })
    }

    /// Clones out the current boss bars in server insertion (render) order.
    #[must_use]
    pub(crate) fn boss_bars(&self) -> BossBarSet {
        lodestone_ecs::hold_read(&self.ecs, |world| {
            world
                .get::<SessionBossBars>(self.session)
                .map(|bars| bars.0.clone())
                .unwrap_or_default()
        })
    }

    /// Clones out the player inventory menu (window 0) in menu-slot order.
    #[must_use]
    pub(crate) fn player_menu(&self) -> Menu {
        lodestone_ecs::hold_read(&self.ecs, |world| {
            world
                .get::<SessionMenus>(self.session)
                .map_or_else(Menu::player, |menus| menus.0.player().clone())
        })
    }

    /// Clones out the currently open non-player menu, if one is active.
    #[must_use]
    pub(crate) fn open_menu(&self) -> Option<OpenMenuSnapshot> {
        lodestone_ecs::hold_read(&self.ecs, |world| {
            let menus = &world.get::<SessionMenus>(self.session)?.0;
            Some(OpenMenuSnapshot {
                window_id: menus.opened_window_id()?,
                menu_type: menus.opened_menu_type()?.clone(),
                title: menus.opened_title()?.clone(),
                menu: menus.opened()?.clone(),
                data: menus.opened_data().to_vec(),
            })
        })
    }

    /// Asks the optional inventory-click veto, then predicts `click` against
    /// the live menu session and returns the [`ClientAction`] to transmit.
    ///
    /// This **must** run here rather than on a snapshot: prediction mutates
    /// the one authoritative [`SessionMenus`] component (slots, the carried
    /// stack, the crafting grid), and [`open_menu`](Self::open_menu) /
    /// [`player_menu`](Self::player_menu) hand out *clones* with nowhere for
    /// that mutation to land. A caller holding only a snapshot cannot predict
    /// a click; it can only ask this state to do it.
    ///
    /// `None` means a registered veto denied the click. The ask happens before
    /// [`SessionMenus::click_action`], so denial cannot change a slot, cursor,
    /// drag state, or menu state id.
    pub(crate) fn menu_click(&self, click: Click, ctx: PlayerCtx) -> Option<ClientAction> {
        let action = lodestone_ecs::hold_write(&self.ecs, |world| {
            let window_id = world
                .get::<SessionMenus>(self.session)
                .expect("the session entity always carries SessionMenus")
                .0
                .opened_window_id()
                .unwrap_or(0);
            let veto = lodestone_ecs::VerbContext::InventoryClick {
                window_id,
                slot: click.slot,
                button: click.button,
            };
            if world
                .get_resource::<lodestone_ecs::ActionVetoes>()
                .is_some_and(|vetoes| {
                    vetoes.allows(&veto) == lodestone_ecs::veto::Verdict::Deny
                })
            {
                return None;
            }

            Some(
                world
                .get_mut::<SessionMenus>(self.session)
                .expect("the session entity always carries SessionMenus")
                .0
                .click_action(click, ctx),
            )
        });
        let action = action?;
        // The prediction just changed slot contents/the carried stack the UI
        // reads every frame; wake any `wait_for` waiter the same way every
        // other mutator on this state does, so a bot awaiting an inventory
        // change is not left hanging on a lost wakeup.
        self.wake();
        Some(action)
    }

    /// Predicts a `key.drop` press against the one authoritative
    /// [`SessionMenus`], returning whether anything was actually dropped.
    ///
    /// The `bool` is vanilla's own return at this layer: vanilla's
    /// local-player drop routine discards the stack and returns whether the
    /// dropped prediction was non-empty, which the caller uses for exactly
    /// one thing — swinging the arm only when the slot was not empty. The stack
    /// itself stays inside `lodestone_game`; the dropped item entity is
    /// server-authoritative and arrives as an ordinary spawn packet, so no caller
    /// out here needs it.
    ///
    /// Same "must run here, not on a snapshot" argument as
    /// [`menu_click`](Self::menu_click) — and here it is the whole point of the
    /// method. A drop is the one inventory mutation the server performs
    /// **silently**: vanilla's own drop-packet handler calls its drop
    /// routine and returns without sending a slot update, so nothing
    /// will ever arrive to fix a missed prediction. `lodestone_game::menus::Menus::drop_selected`
    /// carries the citations.
    ///
    /// `selected` comes from the caller because the selected hotbar slot is a
    /// *driver*-owned component (`lodestone_ecs::SelectedSlot`, on the driver's
    /// local-player entity) and this state holds only [`Self::session`].
    pub(crate) fn drop_selected(&self, selected: usize, all: bool) -> bool {
        let dropped = lodestone_ecs::hold_write(&self.ecs, |world| {
            world
                .get_mut::<SessionMenus>(self.session)
                .expect("the session entity always carries SessionMenus")
                .0
                .drop_selected(selected, all)
                .is_some()
        });
        self.wake();
        dropped
    }
}

impl LocalEcho {
    /// Applies the server's authoritative correction to our own pose and the
    /// echoed timestamp for the client-initiated latency probe.
    ///
    /// **`TeleportPlayer` and `PongReceived` are the only events left here.** This used to be the
    /// scalar read-model's whole fold; everything else it folded now lives in
    /// components:
    ///
    /// | was an arm here | folded by |
    /// |---|---|
    /// | `Login`, `Respawned` | `lodestone_ecs::session::apply_local_player_state` (game mode, dimension, alive) **and** `lodestone_ecs::ingest::apply_local_player_login` (the entity id, and the `EntityIndex` entry) |
    /// | `HealthChanged`, `Death`, `ExperienceChanged` | `lodestone_ecs::session::apply_local_player_state` |
    /// | the eight entity arms + `apply_metadata` | `lodestone_ecs::ingest` (Stage 1) |
    /// | `PlayerListUpdate`, the scoreboard family, `BossBarUpdate`, the `Menus` family | `lodestone_ecs::session` (Stage 3) |
    ///
    /// `SharedState::apply` routes every one of those to the `NetIngest` schedule
    /// *instead of* here, and that routing stays **exclusive** — an event reaches
    /// one fold, never both. That was the whole design question the vitals collapse
    /// turned on: running both folds would have kept a second `dimension` alive,
    /// which is the duplicate this migration exists to delete. What made the
    /// exclusive answer possible was moving `game_mode`/`dimension`/`alive` into
    /// components too, so no event carries a field this side still owns.
    ///
    /// Note what the deleted `PlayerListUpdate` arm did *not* have: a
    /// `PlayerListRemove` arm, so a player who left the server never left this
    /// read-model. `lodestone_game::tablist::TabList::apply` handles both.
    ///
    /// `PongReceived` is a client-initiated latency acknowledgement rather than
    /// a session component: it preserves the client-supplied timestamp for the
    /// shell's portable-clock display. Chat, `KeepAlive` and `Disconnect` carry
    /// no scalar read-model state.
    /// `ChunkLoaded`/`ChunkUnloaded` are applied by the adapter through the
    /// `WorldSink`, so their heavy payload never reaches this fold at all.
    fn apply(&mut self, event: &ClientEvent) {
        let ClientEvent::TeleportPlayer { pos, rotation, flags } = event else {
            if let ClientEvent::PongReceived { time } = event {
                self.last_ping_echo_ms = Some(*time);
            }
            return;
        };
        let base = self.position.unwrap_or_default();
        self.position = Some(Vec3::new(
            if flags.relative_x {
                base.x + pos.x
            } else {
                pos.x
            },
            if flags.relative_y {
                base.y + pos.y
            } else {
                pos.y
            },
            if flags.relative_z {
                base.z + pos.z
            } else {
                pos.z
            },
        ));
        let base_rot = self.rotation;
        self.rotation = Rotation::new(
            if flags.relative_yaw {
                base_rot.yaw + rotation.yaw
            } else {
                rotation.yaw
            },
            if flags.relative_pitch {
                base_rot.pitch + rotation.pitch
            } else {
                rotation.pitch
            },
        );
    }
}

/// Derives an [`EntityView`] from one entity's components.
///
/// The read side of Stage 1's authority handover: the components are the only
/// copy, and this rebuilds the old struct for the callers that still want it
/// ([`SharedState::entity`], [`SharedState::entities`], and through them
/// [`crate::ClientHandle::entities`]).
///
/// # The three `Reported` states, reconstituted
///
/// `Unreported` / `Reported(None)` / `Reported(Some(v))` map back from
/// **component absent** / present-with-`None` / present-with-`Some`. This is
/// the inverse of the encoding `lodestone_ecs::entity`'s module docs define,
/// and it must stay an exact inverse: reading absence as `Reported(None)` here
/// would tell a caller the server had cleared a field it has never mentioned —
/// which for a dropped item's `item` is the difference between "draw nothing
/// yet" and "this drop is empty forever".
///
/// The plain `Option` fields follow the same rule for the same reason
/// (`velocity`: absent is "never reported", which is not the same as a reported
/// zero), which is why every one of them is a `.map(...)` over
/// `EntityRef::get`, never a `unwrap_or_default`.
fn entity_view(entity: lodestone_ecs::ecs::world::EntityRef<'_>) -> Option<EntityView> {
    use lodestone_ecs::entity as ecs_entity;

    Some(EntityView {
        entity_id: entity.get::<ecs_entity::MinecraftEntityId>()?.0,
        uuid: entity.get::<ecs_entity::EntityUuid>().map(|uuid| uuid.0),
        entity_type: entity.get::<ecs_entity::EntityKind>()?.0.clone(),
        position: entity.get::<ecs_entity::Position>()?.0,
        rotation: entity.get::<ecs_entity::Rotation>()?.0,
        head_yaw: entity.get::<ecs_entity::HeadYaw>()?.0,
        velocity: entity.get::<ecs_entity::Velocity>().map(|v| v.0),
        on_ground: entity
            .get::<ecs_entity::OnGround>()
            .is_some_and(|grounded| grounded.0),
        flags: entity.get::<ecs_entity::EntityFlags>().map(|f| f.0),
        custom_name: entity
            .get::<ecs_entity::CustomName>()
            .map_or(Reported::Unreported, |name| {
                Reported::Reported(name.0.clone())
            }),
        custom_name_visible: entity
            .get::<ecs_entity::CustomNameVisible>()
            .map(|visible| visible.0),
        pose: entity.get::<ecs_entity::Pose>().map(|pose| pose.0),
        health: entity.get::<ecs_entity::Health>().map(|health| health.0),
        baby: entity.get::<ecs_entity::Baby>().map(|baby| baby.0),
        variant: entity
            .get::<ecs_entity::Variant>()
            .map(|variant| variant.0.clone()),
        creeper_swell_dir: entity
            .get::<ecs_entity::CreeperSwellDir>()
            .map(|dir| dir.0),
        attributes: entity
            .get::<ecs_entity::Attributes>()
            .map(|attributes| attributes.0.clone())
            .unwrap_or_default(),
        equipment: entity
            .get::<ecs_entity::Equipment>()
            .map(|equipment| equipment.0.clone())
            .unwrap_or_default(),
        item: entity
            .get::<ecs_entity::DisplayItem>()
            .map_or(Reported::Unreported, |item| {
                Reported::Reported(item.0.clone())
            }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_ecs::player::SelectedSlot;
    use lodestone_ecs::session::{
        CombatSession, ServerDifficulty, ServerSimulationDistance, SessionBlockDestruction,
        SessionCombat, SessionGameRules,
        SessionRecipeBookSettings, SessionServerData, SessionSpawnPoint, SessionTabList,
        SessionWorldBorder,
    };
    use lodestone_model::Difficulty;

    fn state_with_inventory_click_veto(
        verdict: lodestone_ecs::veto::Verdict,
        window_id: i32,
        slot: i32,
    ) -> SharedState {
        let state = SharedState::default();
        let mut vetoes = lodestone_ecs::ActionVetoes::default();
        vetoes.register(
            lodestone_ecs::Verb::InventoryClick,
            "inventory-test",
            0,
            move |ctx| {
                assert_eq!(
                    *ctx,
                    lodestone_ecs::VerbContext::InventoryClick {
                        window_id,
                        slot,
                        button: 0,
                    },
                    "the veto must receive the active window and the raw click coordinates"
                );
                verdict
            },
        );
        state.ecs.write().insert_resource(vetoes);
        state
    }

    fn seed_clickable_hotbar_stack(state: &SharedState) {
        let mut items = vec![None; 46];
        items[36] = Some(ItemStack {
            item: "minecraft:diamond".parse().unwrap(),
            count: 5,
            components: lodestone_model::ItemComponents::default(),
        });
        state.apply(&ClientEvent::ContainerContent {
            window_id: 0,
            state_id: lodestone_model::ContainerStateId::new(7),
            items,
            carried_item: None,
        });
    }

    fn seed_clickable_open_menu(state: &SharedState) {
        state.apply(&ClientEvent::ScreenOpened {
            window_id: 5,
            menu_type: "minecraft:generic_9x1".parse().unwrap(),
            title: Text::literal("Chest"),
        });
        let mut items = vec![None; 45];
        items[0] = Some(ItemStack {
            item: "minecraft:diamond".parse().unwrap(),
            count: 5,
            components: lodestone_model::ItemComponents::default(),
        });
        state.apply(&ClientEvent::ContainerContent {
            window_id: 5,
            state_id: lodestone_model::ContainerStateId::new(7),
            items,
            carried_item: None,
        });
    }

    #[tokio::test]
    async fn denied_inventory_click_does_not_predict_or_wake_waiters() {
        let state = state_with_inventory_click_veto(lodestone_ecs::veto::Verdict::Deny, 5, 0);
        seed_clickable_open_menu(&state);
        let before = state.open_menu().expect("the test container is open");

        let notified = state.notifier().notified_owned();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let _ = state.menu_click(Click::left(0), PlayerCtx::survival());

        assert_eq!(
            state.open_menu(),
            Some(before),
            "denial must leave slots, cursor, and menu state id unchanged"
        );
        assert_eq!(
            state.ecs.read().resource::<lodestone_ecs::ActionVetoes>().stats(),
            lodestone_ecs::VetoStats {
                invocations: 1,
                asked: 1,
                denied: 1,
            },
            "the production path must actually ask the registered veto"
        );
        assert!(
            crate::native_time::timeout(std::time::Duration::from_millis(20), notified)
                .await
                .is_err(),
            "a denied no-op must not wake read-model waiters"
        );
    }

    #[tokio::test]
    async fn allowed_inventory_click_still_predicts_and_wakes_waiters() {
        let state = state_with_inventory_click_veto(lodestone_ecs::veto::Verdict::Allow, 0, 36);
        seed_clickable_hotbar_stack(&state);

        let notified = state.notifier().notified_owned();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let _ = state.menu_click(Click::left(36), PlayerCtx::survival());

        let menu = state.player_menu();
        assert!(menu.slot_item(36).is_none(), "the clicked stack moved off its slot");
        assert_eq!(
            menu.carried().map(lodestone_game::item::ItemStack::count),
            Some(5),
            "the allowed prediction moved the stack onto the cursor"
        );
        assert_eq!(
            menu.state_id(),
            lodestone_model::ContainerStateId::new(8),
            "the allowed predictor advanced its state"
        );
        assert_eq!(
            state.ecs.read().resource::<lodestone_ecs::ActionVetoes>().stats(),
            lodestone_ecs::VetoStats {
                invocations: 1,
                asked: 1,
                denied: 0,
            }
        );
        assert!(
            crate::native_time::timeout(std::time::Duration::from_millis(20), notified)
                .await
                .is_ok(),
            "an allowed prediction must wake read-model waiters"
        );
    }

    /// **The real path, not the fold called directly and not the `NetIngest`
    /// schedule run by hand.** `SharedState::apply` is the exact method the
    /// driver calls for every decoded packet, so this exercises `route()` and
    /// the fold *together* — `lodestone_ecs::session`'s own tests push
    /// straight onto `IngestQueue`, bypassing `handles_event` entirely, so
    /// they could not have caught a routing regression on their own. This is
    /// one of the three `HudState`-shaped islands `docs/event-routing.md`
    /// found: `DifficultyChanged` reached a real, unit-tested fold
    /// (`HudState::apply`) that nothing called.
    #[test]
    fn apply_routes_difficulty_changed_through_the_real_path() {
        let state = SharedState::default();
        {
            let ecs = state.ecs.read();
            assert_eq!(
                ecs.get::<ServerDifficulty>(state.session).unwrap().0,
                None,
                "precondition: unreported before the first packet"
            );
        }
        state.apply(&ClientEvent::DifficultyChanged {
            difficulty: Difficulty::Hard,
            locked: true,
        });
        let ecs = state.ecs.read();
        assert_eq!(
            ecs.get::<ServerDifficulty>(state.session).unwrap().0,
            Some((Difficulty::Hard, true)),
            "DifficultyChanged must reach ServerDifficulty through the real \
             SharedState::apply path, not just through a hand-run schedule"
        );
    }

    /// The real route detector for the F3 simulation-distance line: unlike the
    /// component test, this enters through `SharedState::apply`, the method the
    /// client driver invokes after an adapter emits an event.
    #[test]
    fn apply_routes_simulation_distance_through_the_real_path() {
        let state = SharedState::default();
        state.apply(&ClientEvent::SimulationDistanceChanged { distance: 11 });
        {
            let ecs = state.ecs.read();
            assert_eq!(
                ecs.get::<ServerSimulationDistance>(state.session).unwrap().0,
                Some(11),
                "the adapter's scalar must reach the session component through route()"
            );
        }

        // Exact negative control: an unrelated client-only event must neither
        // be routed into this fold nor overwrite the already observed value.
        state.apply(&ClientEvent::Ping { id: 7 });
        let ecs = state.ecs.read();
        assert_eq!(
            ecs.get::<ServerSimulationDistance>(state.session).unwrap().0,
            Some(11),
            "an unrelated event is not a simulation-distance update"
        );
    }

    /// The adapter invokes `SharedState::apply`, not the session fold directly.
    /// This checks that the route table sends the real server-data packet through
    /// the shared session component that the shell reads for its F3 line.
    #[test]
    fn apply_routes_server_data_through_the_real_path() {
        let state = SharedState::default();
        state.apply(&ClientEvent::ServerDataReceived {
            motd: Text::literal("Copper Canyon"),
            icon: Some(vec![0x89, 0x50, 0x4e, 0x47]),
        });
        {
            let ecs = state.ecs.read();
            let data = ecs
                .get::<SessionServerData>(state.session)
                .expect("local player has the server-data component")
                .0
                .as_ref()
                .expect("ServerDataReceived must reach the session component");
            assert_eq!(data.motd.to_plain_string(), "Copper Canyon");
            assert_eq!(data.icon.as_deref(), Some(&[0x89, 0x50, 0x4e, 0x47][..]));
        }

        // Exact negative control: another client-only packet must not create or
        // overwrite the public identity stored by ServerDataReceived.
        state.apply(&ClientEvent::Ping { id: 7 });
        let ecs = state.ecs.read();
        assert_eq!(
            ecs.get::<SessionServerData>(state.session)
                .unwrap()
                .0
                .as_ref()
                .unwrap()
                .motd
                .to_plain_string(),
            "Copper Canyon",
            "an unrelated event must not masquerade as a server-data update"
        );
    }

    /// The adapter enters through `SharedState::apply`, so this is the route
    /// detector for the combat HUD state rather than a unit test of the fold.
    #[test]
    fn apply_routes_combat_enter_and_end_through_the_real_path() {
        let state = SharedState::default();
        state.apply(&ClientEvent::PlayerCombatEntered);
        state.apply(&ClientEvent::PlayerCombatEntered);
        {
            let ecs = state.ecs.read();
            assert_eq!(
                ecs.get::<SessionCombat>(state.session).unwrap().0,
                Some(CombatSession::Active),
                "repeated enters retain one active server session"
            );
        }

        state.apply(&ClientEvent::PlayerCombatEnded {
            duration_ticks: 240,
        });
        state.apply(&ClientEvent::PlayerCombatEnded { duration_ticks: 7 });
        {
            let ecs = state.ecs.read();
            assert_eq!(
                ecs.get::<SessionCombat>(state.session).unwrap().0,
                Some(CombatSession::Ended { duration_ticks: 7 }),
                "the latest repeated end keeps its exact server duration"
            );
        }

        state.apply(&ClientEvent::Ping { id: 7 });
        let ecs = state.ecs.read();
        assert_eq!(
            ecs.get::<SessionCombat>(state.session).unwrap().0,
            Some(CombatSession::Ended { duration_ticks: 7 }),
            "an unrelated event must not overwrite the combat session"
        );
    }

    /// The driver calls `SharedState::apply` for the adapter's mount-open
    /// event. Unlike ordinary containers, this packet supplies the menu size
    /// itself and has no preceding `ScreenOpened`, so a visible open-menu
    /// snapshot here proves the route and session fold together.
    #[test]
    fn apply_routes_mount_screen_into_the_open_menu_snapshot() {
        let state = SharedState::default();
        state.apply(&ClientEvent::MountScreenOpened {
            container_id: 12,
            inventory_columns: 5,
            entity_id: 77,
        });
        let open = state
            .open_menu()
            .expect("mount screen must reach the shell-facing snapshot");
        assert_eq!(open.window_id, 12);
        assert_eq!(open.menu.slot_count(), 2 + 3 * 5 + 36);

        state.apply(&ClientEvent::Ping { id: 7 });
        assert_eq!(
            state.open_menu().map(|menu| menu.window_id),
            Some(12),
            "control: an unrelated event must not replace the announced mount screen"
        );
    }

    #[test]
    fn name_only_player_identity_reaches_and_leaves_the_public_read_model() {
        let state = SharedState::default();
        state.apply(&ClientEvent::PlayerListUpdate {
            entries: vec![PlayerListEntry {
                uuid: None,
                name: Some("Legacy".into()),
                game_mode: None,
                latency: Some(37),
                display_name: None,
                listed: Some(true),
                properties: None,
                chat_session: None,
                list_order: None,
                hat_visible: None,
            }],
        });

        let players = state.players();
        assert_eq!(players.len(), 1);
        assert_eq!(players[0].uuid, None);
        assert_eq!(players[0].name.as_deref(), Some("Legacy"));

        state.apply(&ClientEvent::PlayerListRemoveByName {
            profile_names: vec!["Legacy".into()],
        });
        assert!(state.players().is_empty());
    }

    /// The second: `BlockDestruction` reached
    /// `lodestone_game::mining::BlockDestructionOverlays::apply`, unit-tested
    /// and consumed nowhere outside its own file.
    #[test]
    fn apply_routes_block_destruction_through_the_real_path() {
        let state = SharedState::default();
        let pos = BlockPos::new(4, 70, 4);
        state.apply(&ClientEvent::BlockDestruction {
            entity_id: 9,
            pos,
            progress: 6,
        });
        let ecs = state.ecs.read();
        assert_eq!(
            ecs.get::<SessionBlockDestruction>(state.session)
                .unwrap()
                .0
                .stage_at(pos),
            Some(6)
        );
    }

    /// The third: `HeldSlotChanged` reached `HudState::select_slot`, unit-tested
    /// and consumed nowhere. `SelectedSlot` is inserted here by hand (the real
    /// client carries it via `spawn_local_player`, which this bare
    /// `SharedState::default` harness does not run) to prove the write side of
    /// the real path; `lodestone_shell::sim::Sim::selected_slot` is the
    /// existing pixel-facing reader — `app.rs`'s hotbar highlight already
    /// calls it, so wiring the fold is the whole fix.
    #[test]
    fn apply_routes_held_slot_changed_through_the_real_path() {
        let state = SharedState::default();
        {
            let mut ecs = state.ecs.write();
            ecs.entity_mut(state.session).insert(SelectedSlot(0));
        }
        state.apply(&ClientEvent::HeldSlotChanged { slot: 4 });
        let ecs = state.ecs.read();
        assert_eq!(ecs.get::<SelectedSlot>(state.session).unwrap().0, 4);
    }

    /// The play-state pong carries the timestamp chosen by the F3 latency
    /// probe, not a server clock. Keeping it in the client read-model is what
    /// lets the shell calculate a round trip with its portable local clock.
    #[test]
    fn apply_retains_the_latest_play_pong_timestamp() {
        let state = SharedState::default();
        assert_eq!(state.last_ping_echo_ms(), None, "no pong before a probe replies");

        state.apply(&ClientEvent::PongReceived { time: 1_700_000_123_456 });
        assert_eq!(state.last_ping_echo_ms(), Some(1_700_000_123_456));

        // A server-initiated ping is the nearby but distinct packet family: it
        // asks the driver for an immediate pong response and must not overwrite
        // the client-initiated probe's sample.
        state.apply(&ClientEvent::Ping { id: 17 });
        assert_eq!(state.last_ping_echo_ms(), Some(1_700_000_123_456));

        state.apply(&ClientEvent::PongReceived { time: 1_700_000_124_017 });
        assert_eq!(state.last_ping_echo_ms(), Some(1_700_000_124_017));
    }

    // ---- the nine world-level admin variants, through the real path ---------
    //
    // These are the same shape as the three gates above and exist for the same
    // reason: `lodestone_ecs::session`'s own tests push straight onto
    // `IngestQueue`, so they prove a *fold* runs and say nothing about whether
    // `route()` sends the event to that fold. Only `SharedState::apply` joins the
    // two halves, and a `Route::NOWHERE` regression is invisible to every test in
    // `lodestone-ecs`.
    //
    // **Verified by an observed control, not a described one.**
    // `ClientEvent::WorldBorderCenterChanged` was reverted to `Route::NOWHERE` and
    // both suites re-run. Measured, rather than predicted:
    //
    // | suite | result under the neuter |
    // |---|---|
    // | `lodestone-client --lib` | 17 passed, **1 failed** — `apply_routes_every_world_border_variant_through_the_real_path`, and nothing else |
    // | `lodestone-ecs --lib` | 149 passed, **1 failed** — `handles_event_covers_exactly_the_session_claimed_variants` |
    //
    // Two things worth keeping from that. First, the detector works and it is
    // specific: one arm broken, exactly one gate here failed, and it named the
    // right variant. Second — and this is the part a prediction would have got
    // wrong — the `lodestone-ecs` **fold** tests
    // (`world_border_family_reaches_the_fold_through_the_real_schedule` and its
    // four siblings) all stayed **green**, because they push onto `IngestQueue`
    // directly and never consult `route()`. What caught it over there was the
    // routing-claim *table*, a different test entirely. So the fold tests really
    // are blind to this class, which is the whole argument for the gates below;
    // there are two independent detectors, and neither is the one you would guess.

    /// `TabListChanged` is the cheapest of the nine and the clearest instance of
    /// the class: `lodestone_game::tablist::TabList::apply` has had a
    /// header/footer arm, and `session::apply_tab_list` has been registered, since
    /// before this routing fix. The event decoded, a tested fold sat waiting, and
    /// `route()` never asked. Nothing but the flag changed for this variant.
    #[test]
    fn apply_routes_tab_list_header_and_footer_through_the_real_path() {
        let state = SharedState::default();
        {
            let ecs = state.ecs.read();
            assert_eq!(
                ecs.get::<SessionTabList>(state.session).unwrap().0.header,
                None,
                "precondition: no header before the first packet"
            );
        }
        state.apply(&ClientEvent::TabListChanged {
            header: lodestone_model::text::Text::literal("HDR"),
            footer: lodestone_model::text::Text::literal("FTR"),
        });
        let ecs = state.ecs.read();
        let list = &ecs.get::<SessionTabList>(state.session).unwrap().0;
        assert_eq!(
            list.header.as_ref().map(lodestone_model::text::Text::to_plain_string),
            Some("HDR".to_owned()),
            "TabListChanged must reach SessionTabList through the real path"
        );
        assert_eq!(
            list.footer.as_ref().map(lodestone_model::text::Text::to_plain_string),
            Some("FTR".to_owned())
        );
    }

    /// All six world-border variants in one gate, each asserted on a field the
    /// others do not write, so a single missing `route()` arm cannot hide behind
    /// its five siblings.
    #[test]
    fn apply_routes_every_world_border_variant_through_the_real_path() {
        let state = SharedState::default();
        {
            let ecs = state.ecs.read();
            assert!(
                !ecs.get::<SessionWorldBorder>(state.session)
                    .unwrap()
                    .0
                    .initialized,
                "precondition: uninitialised border"
            );
        }

        // 1. Initialized — the join/respawn packet, which sets everything.
        state.apply(&ClientEvent::WorldBorderInitialized {
            x: 12.0,
            z: 34.0,
            old_size: 256.0,
            new_size: 256.0,
            lerp_time_ms: 0,
            absolute_max_size: 29_999_984,
            warning_blocks: 11,
            warning_time: 22,
        });
        {
            let ecs = state.ecs.read();
            let b = ecs.get::<SessionWorldBorder>(state.session).unwrap().0;
            assert!(b.initialized, "WorldBorderInitialized");
            assert!((b.center_x - 12.0).abs() < f64::EPSILON);
            assert_eq!(b.warning_blocks.blocks(), 11);
            assert_eq!(b.warning_time.seconds(), 22);
        }

        // 2-5. Each incremental variant, on a distinct field.
        state.apply(&ClientEvent::WorldBorderCenterChanged { x: -7.0, z: 8.0 });
        state.apply(&ClientEvent::WorldBorderSizeChanged { size: 48.0 });
        state.apply(&ClientEvent::WorldBorderWarningDistanceChanged { warning_blocks: 2 });
        state.apply(&ClientEvent::WorldBorderWarningDelayChanged { warning_time: 3 });
        {
            let ecs = state.ecs.read();
            let b = ecs.get::<SessionWorldBorder>(state.session).unwrap().0;
            assert!(
                (b.center_x + 7.0).abs() < f64::EPSILON,
                "WorldBorderCenterChanged did not reach the fold"
            );
            assert!(
                (b.target_size() - 48.0).abs() < f64::EPSILON,
                "WorldBorderSizeChanged did not reach the fold"
            );
            assert_eq!(
                b.warning_blocks.blocks(), 2,
                "WorldBorderWarningDistanceChanged did not reach the fold"
            );
            assert_eq!(
                b.warning_time.seconds(), 3,
                "WorldBorderWarningDelayChanged did not reach the fold"
            );
        }

        // 6. SizeLerping, distinguished from SizeChanged by producing a *moving*
        // extent rather than a static one — so this arm cannot be satisfied by
        // the `SizeChanged` arm above.
        state.apply(&ClientEvent::WorldBorderSizeLerping {
            old_size: 48.0,
            new_size: 96.0,
            lerp_time_ms: 2_000,
        });
        let ecs = state.ecs.read();
        let b = ecs.get::<SessionWorldBorder>(state.session).unwrap().0;
        assert!(
            b.is_resizing(),
            "WorldBorderSizeLerping must produce a moving extent, not a static one"
        );
        assert!((b.target_size() - 96.0).abs() < f64::EPSILON);
    }

    /// `SpawnPositionChanged` — the compass target. `lodestone_render::item_render`
    /// lists `minecraft:compass` among the item-model range properties that are
    /// *deliberately unsourced* "because the datum genuinely is not decoded". It
    /// was decoded; it reached nothing. This is the fold that makes it sourceable.
    #[test]
    fn apply_routes_spawn_position_through_the_real_path() {
        let state = SharedState::default();
        {
            let ecs = state.ecs.read();
            assert!(
                !ecs.get::<SessionSpawnPoint>(state.session)
                    .unwrap()
                    .0
                    .is_reported(),
                "precondition: no spawn reported, so the assert below is not vacuous"
            );
        }
        state.apply(&ClientEvent::SpawnPositionChanged {
            dimension: "minecraft:overworld".parse().unwrap(),
            pos: BlockPos::new(256, 63, -1024),
            angle: 45.0,
            pitch: 0.0,
        });
        let ecs = state.ecs.read();
        let sp = &ecs.get::<SessionSpawnPoint>(state.session).unwrap().0;
        assert!(sp.is_reported());
        assert_eq!(
            sp.pos(),
            Some(BlockPos::new(256, 63, -1024)),
            "SpawnPositionChanged must reach SessionSpawnPoint through the real path"
        );
    }

    /// `GameRulesChanged`. Note the last assertion: absence must stay
    /// distinguishable from `false` even after a successful fold, because vanilla's
    /// `GAME_RULE_VALUES` is request/response rather than broadcast, so a rule the
    /// server never reported is the normal case and not a `false`.
    #[test]
    fn apply_routes_game_rules_through_the_real_path() {
        let state = SharedState::default();
        {
            let ecs = state.ecs.read();
            assert!(
                ecs.get::<SessionGameRules>(state.session)
                    .unwrap()
                    .0
                    .is_empty(),
                "precondition: no rules reported"
            );
        }
        state.apply(&ClientEvent::GameRulesChanged {
            values: vec![
                (
                    "minecraft:immediate_respawn".parse().unwrap(),
                    "true".to_owned(),
                ),
                (
                    "minecraft:players_sleeping_percentage".parse().unwrap(),
                    "50".to_owned(),
                ),
            ],
        });
        let ecs = state.ecs.read();
        let rules = &ecs.get::<SessionGameRules>(state.session).unwrap().0;
        assert_eq!(
            rules.immediate_respawn(),
            Some(true),
            "GameRulesChanged must reach SessionGameRules through the real path"
        );
        assert_eq!(rules.players_sleeping_percentage(), Some(50));
        assert_eq!(
            rules.bool_rule("minecraft:keep_inventory"),
            None,
            "an unreported rule must not read as false"
        );
    }

    /// `RecipeBookSettingsChanged` — the only one of these whose *packet* was
    /// undecoded rather than merely unrouted. `ClientAction::SetRecipeBookSettings`
    /// was already encoded, so before this the round trip was half-open: our book
    /// state could go out and the server's could never come back.
    #[test]
    fn apply_routes_recipe_book_settings_through_the_real_path() {
        use lodestone_model::{RecipeBookType, RecipeBookTypeSettings};
        let state = SharedState::default();
        {
            let ecs = state.ecs.read();
            assert!(
                !ecs.get::<SessionRecipeBookSettings>(state.session)
                    .unwrap()
                    .0
                    .reported,
                "precondition: unreported"
            );
        }
        state.apply(&ClientEvent::RecipeBookSettingsChanged {
            crafting: RecipeBookTypeSettings { open: true, filtering: true },
            furnace: RecipeBookTypeSettings { open: false, filtering: true },
            blast_furnace: RecipeBookTypeSettings { open: true, filtering: false },
            smoker: RecipeBookTypeSettings { open: false, filtering: false },
        });
        let ecs = state.ecs.read();
        let s = ecs
            .get::<SessionRecipeBookSettings>(state.session)
            .unwrap()
            .0;
        assert!(s.reported);
        assert_eq!(
            s.for_type(RecipeBookType::Crafting),
            RecipeBookTypeSettings { open: true, filtering: true },
            "RecipeBookSettingsChanged must reach the fold through the real path"
        );
        assert_eq!(
            s.for_type(RecipeBookType::BlastFurnace),
            RecipeBookTypeSettings { open: true, filtering: false },
            "and each book must keep its own pair"
        );
    }

    /// The server's tab selection is a UI cursor, distinct from advancement
    /// progress. It must still travel through the real session fold because a
    /// clientbound selection otherwise decodes successfully and changes no
    /// screen state.
    #[test]
    fn apply_routes_advancement_tab_selection_through_the_real_path() {
        let state = SharedState::default();
        let tab: lodestone_model::Identifier = "minecraft:adventure/root"
            .parse()
            .expect("valid identifier");
        state.apply(&ClientEvent::AdvancementsTabSelected { tab: Some(tab.clone()) });
        let ecs = state.ecs.read();
        assert_eq!(
            ecs.get::<lodestone_ecs::session::SessionAdvancementTab>(state.session)
                .expect("local player tab component")
                .0,
            Some(tab),
        );
    }

    /// The negative control for the whole block above, and the reason it is not
    /// merely decorative: it pins that `SharedState::apply` really does consult
    /// `route()` rather than folding everything it is handed.
    ///
    /// A client-only ping is deliberately unrelated to the three components.
    /// If it starts mutating any of them, the folds are matching too broadly.
    #[test]
    fn an_unrelated_ping_reaches_none_of_the_new_components() {
        let state = SharedState::default();
        assert!(
            lodestone_model::event::route(&ClientEvent::Ping { id: 7 }).client,
            "premise: the ping must stay outside the session fold"
        );
        state.apply(&ClientEvent::Ping { id: 7 });
        let ecs = state.ecs.read();
        let b = ecs.get::<SessionWorldBorder>(state.session).unwrap().0;
        assert!(!b.initialized, "border must be untouched");
        assert!(
            !ecs.get::<SessionSpawnPoint>(state.session)
                .unwrap()
                .0
                .is_reported(),
            "spawn must be untouched"
        );
        assert!(
            ecs.get::<SessionGameRules>(state.session)
                .unwrap()
                .0
                .is_empty(),
            "rules must be untouched"
        );
    }

    /// **Stage 4's authority test, on the client side.** The `ChunkWorld`
    /// resource in the client's ECS `World`, the handle `ClientHandle::chunk_world`
    /// hands out, and the store the version adapter writes decoded columns into
    /// through `world_write` must all be the *same* `Arc` — not three views that
    /// happen to agree.
    ///
    /// Asserted by pointer identity rather than by data, deliberately: two stores
    /// holding equal chunks would satisfy any content comparison and would still be
    /// the two-worlds defect this stage exists to delete.
    #[test]
    fn the_chunk_world_resource_and_the_adapters_write_target_are_one_store() {
        let state = SharedState::default();
        let handed_out = state.chunk_world();

        let resource_is_the_same = {
            let ecs = state.ecs.read();
            handed_out.is_same_store(ecs.resource::<ChunkWorld>())
        };
        assert!(
            resource_is_the_same,
            "the ECS resource must be the same store `chunk_world()` hands out"
        );

        // And that store is the one `world_write` borrows: writing through the
        // guard is visible through the handle without any propagation step.
        assert_eq!(handed_out.len(), 0);
        assert!(state.world_write().is_empty());
        // `ChunkWorld` exposes no `Arc` — identity is compared by
        // rebuilding a read handle from the same field `world_write` locks and
        // using the store's own `is_same_store`, which is `Arc::ptr_eq` inside.
        let from_write_target = ChunkWorld::from_shared(Arc::clone(&state.world));
        assert!(
            handed_out.is_same_store(&from_write_target),
            "`chunk_world()` must clone the `Arc` `world_write` locks, not a copy of it"
        );
    }

    /// The control for the test above: `is_same_store` really can tell two stores
    /// apart, so the assertion is discriminating rather than trivially true.
    #[test]
    fn two_independent_states_do_not_share_a_chunk_store() {
        let a = SharedState::default();
        let b = SharedState::default();
        assert!(!a.chunk_world().is_same_store(&b.chunk_world()));
    }

    // ---- the plugin event bus ----------------------------------------------

    /// Builds a [`SharedState`] the way [`SharedState::default`] does, except
    /// the underlying `World` carries `GameEventBusPlugin` before
    /// [`SharedState::adopting`] ever sees it — today's real opt-in path (see
    /// that constructor's doc).
    fn state_with_game_event_bus() -> SharedState {
        let mut app = lodestone_ecs::app::App::new();
        app.add_plugins((
            lodestone_ecs::ingest::IngestPlugin,
            lodestone_ecs::SessionPlugin,
            lodestone_ecs::GameEventBusPlugin,
        ));
        let session = lodestone_ecs::spawn_session(app.world_mut());
        let ecs: EcsHandle = std::sync::Arc::new(lodestone_ecs::parking_lot::RwLock::new(
            std::mem::take(app.world_mut()),
        ));
        SharedState::adopting(ecs, session)
    }

    /// Builds the same bus-and-channel shape the production app installs, so
    /// this test exercises the whole path from `SharedState::apply` to a typed
    /// channel consumer rather than only checking that the generic bus queued a
    /// message.
    fn state_with_server_brand_channel() -> SharedState {
        let mut app = lodestone_ecs::app::App::new();
        app.add_plugins((
            lodestone_ecs::ingest::IngestPlugin,
            lodestone_ecs::SessionPlugin,
            lodestone_ecs::ServerBrandChannelPlugin,
        ));
        let session = lodestone_ecs::spawn_session(app.world_mut());
        let ecs: EcsHandle = std::sync::Arc::new(lodestone_ecs::parking_lot::RwLock::new(
            std::mem::take(app.world_mut()),
        ));
        SharedState::adopting(ecs, session)
    }

    /// **The source-scan guard**, in the style of
    /// `lodestone_model::event::route_tests::route_has_no_catch_all_arm`: the
    /// bus's whole safety property is that its one write site has no `match`
    /// on the event at all, so a new `ClientEvent` variant cannot compile
    /// with an arm that silently skips the bus. Reads this file's own source
    /// rather than trusting a comment to stay true.
    #[test]
    fn game_event_bus_write_site_has_no_match_on_the_event() {
        let source = include_str!("state.rs");
        let body = source
            .split_once(
                "fn push_to_game_event_bus(world: &mut lodestone_ecs::ecs::world::World, event: &ClientEvent) {",
            )
            .expect("push_to_game_event_bus must exist in this file")
            .1;
        let body = body
            .split_once("\n}\n")
            .map_or(body, |(before, _)| before);

        assert!(
            !body.contains("match "),
            "push_to_game_event_bus must never match on `event` — a match is \
             exactly the island-factory shape CLAUDE.md calls out for \
             ingest::handles_event/session::handles_event/net::forward. Body:\n{body}"
        );

        // The control, per CLAUDE.md: an absence assertion is worth only as
        // much as the evidence the detector fires.
        assert!(
            "        match event {\n".contains("match "),
            "the detector must see a real match keyword"
        );
    }

    /// **The off-by-default control.** A `SharedState::default()` client
    /// (no `GameEventBusPlugin` anywhere in its `World`) must not be
    /// observable through the bus at all: `Messages<GameEvent>` is not even a
    /// resource, so nothing can have been dropped, delayed, or partially
    /// applied — the bus is simply not there.
    #[test]
    fn a_default_state_has_no_game_event_bus_resource() {
        let state = SharedState::default();
        let ecs = state.ecs.read();
        assert!(
            ecs.get_resource::<lodestone_ecs::ecs::message::Messages<lodestone_ecs::GameEvent>>()
                .is_none(),
            "SharedState::default must not install the bus"
        );
    }

    /// **The on half of the pair**, and the control that proves the off
    /// assertion above is discriminating rather than vacuous: the identical
    /// `apply` call, through a state built with `GameEventBusPlugin`
    /// installed, must land the event in `Messages<GameEvent>`.
    #[test]
    fn apply_reaches_the_game_event_bus_when_a_plugin_opted_in() {
        let state = state_with_game_event_bus();
        assert!(
            state.game_event_bus_enabled,
            "precondition: the cached flag must reflect the installed marker"
        );

        state.apply(&ClientEvent::Ping { id: 5 });

        let ecs = state.ecs.read();
        let messages = ecs
            .get_resource::<lodestone_ecs::ecs::message::Messages<lodestone_ecs::GameEvent>>()
            .expect("GameEventBusPlugin must register Messages<GameEvent>");
        assert_eq!(
            messages.len(),
            1,
            "the Ping must have reached the bus through the real SharedState::apply path"
        );
    }

    /// The generic bus check above is intentionally not enough: it can be green
    /// while a channel decoder or its scheduled fold has no production consumer.
    /// This drives one valid `minecraft:brand` payload through the actual client
    /// state path, then runs the regular game tick and reads the plugin's state.
    #[test]
    fn custom_payload_reaches_the_installed_brand_channel_consumer() {
        let state = state_with_server_brand_channel();
        assert!(
            state.game_event_bus_enabled,
            "precondition: the installed channel must enable the event bus"
        );

        state.apply(&ClientEvent::CustomPayload {
            channel: "minecraft:brand".parse().unwrap(),
            data: vec![6, b'r', b'o', b'u', b't', b'e', b'd'],
        });

        let mut ecs = state.ecs.write();
        ecs.run_schedule(lodestone_ecs::GameTick);
        let reported = ecs
            .get_resource::<lodestone_ecs::ReportedServerBrand>()
            .expect("the installed channel must expose its folded state");
        assert_eq!(reported.brand.as_deref(), Some("routed"));
        assert_eq!(reported.announcements, 1);
    }

    /// A state with the bus disabled must not populate `Messages<GameEvent>`
    /// even when one exists in a *different* state's `World` — this is the
    /// same shape as `two_independent_states_do_not_share_a_chunk_store`
    /// above, applied to the bus's own gate rather than to the chunk store.
    #[test]
    fn a_disabled_state_does_not_touch_an_unrelated_bus() {
        let disabled = SharedState::default();
        let enabled = state_with_game_event_bus();

        disabled.apply(&ClientEvent::Ping { id: 9 });

        let ecs = enabled.ecs.read();
        let messages = ecs
            .get_resource::<lodestone_ecs::ecs::message::Messages<lodestone_ecs::GameEvent>>()
            .expect("the enabled state's own bus must still exist");
        assert_eq!(
            messages.len(),
            0,
            "a disabled state's apply() must never reach a different state's bus"
        );
    }

    /// The raw bus has the same zero-cost default as the decoded event bus:
    /// recording a packet on a state that was not built with the opt-in plugin
    /// leaves no message resource behind.
    #[test]
    fn a_default_state_does_not_record_raw_packets() {
        let state = SharedState::default();
        state.record_raw_packet(ConnectionState::Play, 7, &[0, 255]);

        let ecs = state.ecs.read();
        assert!(
            ecs.get_resource::<lodestone_ecs::ecs::message::Messages<lodestone_ecs::RawPacket>>()
                .is_none(),
            "SharedState::default must not allocate the raw packet bus"
        );
    }
}
