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

use std::sync::{Arc, RwLock, RwLockWriteGuard};

use lodestone_ecs::ecs::entity::Entity;
use lodestone_ecs::session::{
    ServerAlive, ServerBiomeSkyColors, ServerDimension, ServerDimensionType, ServerEntityId,
    ServerGameMode, SessionBossBars, SessionMenus, SessionScoreboard, SessionTabList, Vitals, Xp,
};
use lodestone_ecs::{ChunkWorld, EcsHandle, WorldTime};
use lodestone_game::bossbar::BossBarSet;
use lodestone_game::scoreboard::Scoreboard;
use lodestone_game::tablist::TabList;
use lodestone_game::{
    click::{Click, PlayerCtx},
    menu::Menu,
};
use lodestone_model::{
    BlockPos, ChunkPos, ClientAction, ClientEvent, DimensionId, DimensionTypeInfo,
    EntityAttributeSnapshot, EntityEquipment, EntityPose, EntityVariant, GameMode,
    ItemStack, PlayerListEntry, Reported, ResourceKey, Rotation, Text, Vec3,
};
use lodestone_world::{ChunkPos as WorldChunkPos, ChunkSection, SectionLight, World};
use tokio::sync::Notify;
use uuid::Uuid;

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
    /// `HudState::default` already uses (`docs/air-supply.md`).
    pub air: i32,
    /// Whether the player entity is burning — `Entity.FLAG_ONFIRE`, folded from
    /// `Vitals::on_fire` (issue #112).
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
    /// declared it in the Configuration `registry_data` (issue #288). Read from
    /// [`ServerDimensionType`].
    ///
    /// `None` means the server said nothing usable — **not** "the overworld". A
    /// consumer must state its own fallback; see
    /// `lodestone_shell::mesher::sky_default_for_dimension`.
    pub dimension_type: Option<DimensionTypeInfo>,
    /// Every biome's `minecraft:visual/sky_color` as the server declared it in
    /// the Configuration `registry_data` (issue #96), **indexed by biome holder
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
    pub custom_name: Reported<String>,
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
        let session = {
            let mut world_ecs = ecs.write();
            world_ecs.insert_resource(WorldTime::default());
            // Stage 4 (§4.1(d)): the chunk store is a resource, and it is the
            // *same* store `world_write` hands the adapter — one `Arc`, two
            // names. A system or plugin in this `World` can therefore read
            // chunks without a second copy existing anywhere.
            world_ecs.insert_resource(ChunkWorld::from_shared(Arc::clone(&world)));
            lodestone_ecs::spawn_session(&mut world_ecs)
        };
        Self {
            inner: Arc::new(RwLock::new(LocalEcho::default())),
            world,
            notify: Arc::new(Notify::new()),
            ecs,
            session,
        }
    }
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
    pub(crate) fn adopting(ecs: EcsHandle, session: Entity) -> Self {
        Self {
            inner: Arc::new(RwLock::new(LocalEcho::default())),
            world: Arc::new(RwLock::new(World::new())),
            notify: Arc::new(Notify::new()),
            ecs,
            session,
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
        let world = self.ecs.read();
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
                .and_then(|d| d.0.clone()),
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
        self.ecs
            .read()
            .get::<lodestone_ecs::entity::Attributes>(self.session)
            .map(|attributes| attributes.0.clone())
            .unwrap_or_default()
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
        let world = self.ecs.read();
        let entity = world
            .resource::<lodestone_ecs::entity::EntityIndex>()
            .get(entity_id)?;
        entity_view(world.get_entity(entity).ok()?)
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
        let world = self.ecs.read();
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
    }

    /// Clones out the current player-list entries as the model's flat wire
    /// shape.
    ///
    /// **Derived, not stored** — the same one-directional intermediate Stage 1
    /// established for [`EntityView`]: the [`SessionTabList`] component is the
    /// only copy and this rebuilds the model struct for callers that still speak
    /// it. Every field is `Some` because a folded entry *has* a value for each
    /// (the `Option`s on the wire mean "this delta did not mention the field",
    /// and the fold has already merged them).
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
        self.ecs
            .read()
            .get::<SessionTabList>(self.session)
            .map(|list| list.0.clone())
            .unwrap_or_default()
    }

    /// Returns `(world_age, time_of_day)`, read from the [`WorldTime`]
    /// resource in `self.ecs` — the sole backing store since Stage 0 of
    /// `docs/bevy-migration.md` deleted `Inner.world_age`/`Inner.time_of_day`.
    #[must_use]
    pub(crate) fn time(&self) -> (i64, i64) {
        let world = self.ecs.read();
        let time = world.resource::<WorldTime>();
        (time.age, time.time_of_day)
    }

    /// Clones out the current folded scoreboard (objectives, scores, the
    /// nineteen display slots and teams).
    ///
    /// The [`SessionScoreboard`] component is the only copy in the process since
    /// Stage 3 — `lodestone_client::scoreboard::Scoreboard` and
    /// `lodestone_shell::sim::Sim::scoreboard` are both gone.
    #[must_use]
    pub(crate) fn scoreboard(&self) -> Scoreboard {
        self.ecs
            .read()
            .get::<SessionScoreboard>(self.session)
            .map(|board| board.0.clone())
            .unwrap_or_default()
    }

    /// Clones out the current boss bars in server insertion (render) order.
    #[must_use]
    pub(crate) fn boss_bars(&self) -> BossBarSet {
        self.ecs
            .read()
            .get::<SessionBossBars>(self.session)
            .map(|bars| bars.0.clone())
            .unwrap_or_default()
    }

    /// Clones out the player inventory menu (window 0) in menu-slot order.
    #[must_use]
    pub(crate) fn player_menu(&self) -> Menu {
        self.ecs
            .read()
            .get::<SessionMenus>(self.session)
            .map_or_else(Menu::player, |menus| menus.0.player().clone())
    }

    /// Clones out the currently open non-player menu, if one is active.
    #[must_use]
    pub(crate) fn open_menu(&self) -> Option<OpenMenuSnapshot> {
        let world = self.ecs.read();
        let menus = &world.get::<SessionMenus>(self.session)?.0;
        Some(OpenMenuSnapshot {
            window_id: menus.opened_window_id()?,
            menu_type: menus.opened_menu_type()?.clone(),
            title: menus.opened_title()?.clone(),
            menu: menus.opened()?.clone(),
        })
    }

    /// Predicts `click` against the live menu session and returns the
    /// [`ClientAction`] to transmit.
    ///
    /// This **must** run here rather than on a snapshot: prediction mutates
    /// the one authoritative [`SessionMenus`] component (slots, the carried
    /// stack, the crafting grid), and [`open_menu`](Self::open_menu) /
    /// [`player_menu`](Self::player_menu) hand out *clones* with nowhere for
    /// that mutation to land. A caller holding only a snapshot cannot predict
    /// a click; it can only ask this state to do it.
    pub(crate) fn menu_click(&self, click: Click, ctx: PlayerCtx) -> ClientAction {
        let action = {
            let mut world = self.ecs.write();
            world
                .get_mut::<SessionMenus>(self.session)
                .expect("the session entity always carries SessionMenus")
                .0
                .click_action(click, ctx)
        };
        // The prediction just changed slot contents/the carried stack the UI
        // reads every frame; wake any `wait_for` waiter the same way every
        // other mutator on this state does, so a bot awaiting an inventory
        // change is not left hanging on a lost wakeup.
        self.wake();
        action
    }
}

impl LocalEcho {
    /// Applies the server's authoritative correction to our own pose.
    ///
    /// **`TeleportPlayer` is the only event left here.** This used to be the
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
    /// Chat, `KeepAlive` and `Disconnect` carry no scalar read-model state.
    /// `ChunkLoaded`/`ChunkUnloaded` are applied by the adapter through the
    /// `WorldSink`, so their heavy payload never reaches this fold at all.
    fn apply(&mut self, event: &ClientEvent) {
        let ClientEvent::TeleportPlayer {
            pos,
            rotation,
            flags,
        } = event
        else {
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
        assert!(
            Arc::ptr_eq(handed_out.shared(), &state.world),
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
}
