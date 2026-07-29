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

use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockWriteGuard};

use lodestone_ecs::{EcsHandle, WorldTime};
use lodestone_game::{
    click::{Click, PlayerCtx},
    menu::Menu,
    menus::Menus,
};
use lodestone_model::{
    BlockPos, ChunkPos, ClientAction, ClientEvent, DimensionId, EntityAttributeSnapshot,
    EntityEquipment, EntityPose, EntityVariant, GameMode,
    ItemStack, PlayerListEntry, Reported, ResourceKey, Rotation, Text, Vec3,
};
use lodestone_world::{ChunkPos as WorldChunkPos, ChunkSection, SectionLight, World};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::scoreboard::{BossBar, Scoreboard, apply_boss_bar};

/// An immutable snapshot of the local player's state.
///
/// Fields are `Option` where the server has not told us yet: `position` and
/// `entity_id` are unknown until login and the first teleport, for example.
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
    /// Current game mode, once known.
    pub game_mode: Option<GameMode>,
    /// Current dimension, once known.
    pub dimension: Option<DimensionId>,
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
            game_mode: None,
            dimension: None,
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

/// The mutable scalar state behind the lock. Private; only ever touched under
/// [`SharedState`]'s lock. World (chunk) state lives in a separate lock so a
/// chunk write never contends with a scalar read.
#[derive(Debug)]
struct Inner {
    player: PlayerSnapshot,
    players: HashMap<Uuid, PlayerListEntry>,
    scoreboard: Scoreboard,
    /// Boss bars in server insertion order (render order).
    boss_bars: Vec<BossBar>,
    menus: Menus,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            player: PlayerSnapshot::default(),
            players: HashMap::new(),
            scoreboard: Scoreboard::default(),
            boss_bars: Vec::new(),
            menus: Menus::new(),
        }
    }
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
    inner: Arc<RwLock<Inner>>,
    world: Arc<RwLock<World>>,
    notify: Arc<Notify>,
    /// The bevy_ecs `World` this state is authoritative over, per
    /// `docs/bevy-migration.md` Stage 0. Currently backs only [`WorldTime`]
    /// (folded from `ClientEvent::TimeChanged` in [`SharedState::apply`], not
    /// `Inner::apply`, since that method has no access to sibling
    /// `SharedState` fields like this one). This is a *separate* `World` from
    /// any `lodestone_ecs::app::App` a driver (e.g. `lodestone-shell`'s
    /// `WindowApp`) owns on its own thread — deliberately: unifying them is a
    /// later stage (§4.1), and `CorePlugin` never inserts `WorldTime` itself
    /// so that split cannot silently become two diverging clocks.
    ecs: EcsHandle,
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
        ecs.write().insert_resource(WorldTime::default());
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            world: Arc::new(RwLock::new(World::new())),
            notify: Arc::new(Notify::new()),
            ecs,
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
    /// [`ClientEvent::TimeChanged`] and every entity event are handled *here*
    /// rather than inside [`Inner::apply`]: they live in this state's
    /// [`EcsHandle`] (`self.ecs`), a sibling field `Inner::apply` has no access
    /// to. Everything else still folds through `Inner::apply` unchanged.
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
            let mut world = self.ecs.write();
            let mut time = world.resource_mut::<WorldTime>();
            time.age = *world_age;
            time.time_of_day = *time_of_day;
        } else if lodestone_ecs::ingest::handles_event(event) {
            let mut world = self.ecs.write();
            world
                .resource_mut::<lodestone_ecs::ingest::IngestQueue>()
                .push(event.clone());
            world.run_schedule(lodestone_ecs::NetIngest);
        } else {
            let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
            inner.apply(event);
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

    /// Records the player's own outgoing movement so subsequent look/step
    /// queries build on the latest local position without waiting for the
    /// server to echo it back.
    pub(crate) fn set_local_movement(&self, pos: Vec3, rotation: Rotation, on_ground: bool) {
        {
            let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
            inner.player.position = Some(pos);
            inner.player.rotation = rotation;
            inner.player.on_ground = on_ground;
        }
        self.wake();
    }

    /// Clones out the current player snapshot.
    #[must_use]
    pub(crate) fn player(&self) -> PlayerSnapshot {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .player
            .clone()
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
    #[must_use]
    pub(crate) fn entities(&self) -> Vec<EntityView> {
        let world = self.ecs.read();
        world
            .resource::<lodestone_ecs::entity::EntityIndex>()
            .iter()
            .filter_map(|(_, entity)| entity_view(world.get_entity(entity).ok()?))
            .collect()
    }

    /// Clones out the current player-list entries.
    #[must_use]
    pub(crate) fn players(&self) -> Vec<PlayerListEntry> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .players
            .values()
            .cloned()
            .collect()
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

    /// Clones out the current folded scoreboard (objectives, scores, display
    /// slots and teams).
    #[must_use]
    pub(crate) fn scoreboard(&self) -> Scoreboard {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .scoreboard
            .clone()
    }

    /// Clones out the current boss bars in server insertion (render) order.
    #[must_use]
    pub(crate) fn boss_bars(&self) -> Vec<BossBar> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .boss_bars
            .clone()
    }

    /// Clones out the player inventory menu (window 0) in menu-slot order.
    #[must_use]
    pub(crate) fn player_menu(&self) -> Menu {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .menus
            .player()
            .clone()
    }

    /// Clones out the currently open non-player menu, if one is active.
    #[must_use]
    pub(crate) fn open_menu(&self) -> Option<OpenMenuSnapshot> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        Some(OpenMenuSnapshot {
            window_id: inner.menus.opened_window_id()?,
            menu_type: inner.menus.opened_menu_type()?.clone(),
            title: inner.menus.opened_title()?.clone(),
            menu: inner.menus.opened()?.clone(),
        })
    }

    /// Predicts `click` against the live [`Menus`] session and returns the
    /// [`ClientAction`] to transmit.
    ///
    /// This **must** run here rather than on a snapshot: prediction mutates
    /// the one authoritative [`Menus`] this state owns (slots, the carried
    /// stack, the crafting grid), and [`open_menu`](Self::open_menu) /
    /// [`player_menu`](Self::player_menu) hand out *clones* with nowhere for
    /// that mutation to land. A caller holding only a snapshot cannot predict
    /// a click; it can only ask this state to do it.
    pub(crate) fn menu_click(&self, click: Click, ctx: PlayerCtx) -> ClientAction {
        let action = {
            let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
            inner.menus.click_action(click, ctx)
        };
        // The prediction just changed slot contents/the carried stack the UI
        // reads every frame; wake any `wait_for` waiter the same way every
        // other mutator on this state does, so a bot awaiting an inventory
        // change is not left hanging on a lost wakeup.
        self.wake();
        action
    }
}

impl Inner {
    /// Folds one non-chunk event into the model.
    fn apply(&mut self, event: &ClientEvent) {
        if self.menus.apply(event) {
            return;
        }
        match event {
            ClientEvent::Login {
                entity_id,
                game_mode,
                dimension,
            } => {
                self.player.entity_id = Some(*entity_id);
                self.player.game_mode = Some(*game_mode);
                self.player.dimension = Some(dimension.clone());
                self.player.alive = true;
            }
            // `Respawned` is *also* how the server reports portal travel, not
            // only death. Without this arm `dimension` froze at whatever the
            // player logged into, so walking into the Nether left every
            // dimension-conditioned rendering decision reading "overworld" —
            // reintroducing the too-bright-Nether bug by traversal rather than
            // by fresh login. `alive` is set here too because a respawn is
            // exactly when the player stops being dead.
            ClientEvent::Respawned {
                dimension,
                game_mode,
                ..
            } => {
                self.player.dimension = Some(dimension.clone());
                self.player.game_mode = Some(*game_mode);
                self.player.alive = true;
            }
            ClientEvent::TeleportPlayer {
                pos,
                rotation,
                flags,
            } => {
                let base = self.player.position.unwrap_or_default();
                let new = Vec3::new(
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
                );
                self.player.position = Some(new);
                let base_rot = self.player.rotation;
                self.player.rotation = Rotation::new(
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
            ClientEvent::HealthChanged {
                health,
                food,
                saturation,
            } => {
                self.player.health = *health;
                self.player.food = *food;
                self.player.saturation = *saturation;
                self.player.health_known = true;
                self.player.alive = *health > 0.0;
            }
            ClientEvent::Death { .. } => {
                self.player.alive = false;
            }
            ClientEvent::ExperienceChanged {
                progress,
                level,
                total,
            } => {
                self.player.xp_progress = *progress;
                self.player.xp_level = *level;
                self.player.xp_total = *total;
                self.player.xp_known = true;
            }
            ClientEvent::PlayerListUpdate { entries } => {
                for entry in entries {
                    self.players.insert(entry.uuid, entry.clone());
                }
            }
            ClientEvent::ObjectiveUpdate {
                name,
                mode,
                display_name,
                render_type,
                number_format,
            } => {
                self.scoreboard.apply_objective(
                    name,
                    *mode,
                    display_name.clone(),
                    *render_type,
                    number_format.clone(),
                );
            }
            ClientEvent::DisplayObjective { slot, objective } => {
                self.scoreboard.apply_display(*slot, objective.as_deref());
            }
            ClientEvent::ScoreUpdate {
                holder,
                objective,
                value,
                display,
                number_format,
            } => {
                self.scoreboard.apply_score(
                    holder,
                    objective,
                    *value,
                    display.clone(),
                    number_format.clone(),
                );
            }
            ClientEvent::ScoreReset { holder, objective } => {
                self.scoreboard
                    .apply_score_reset(holder, objective.as_deref());
            }
            ClientEvent::TeamUpdate { name, action } => {
                self.scoreboard.apply_team(name, action);
            }
            ClientEvent::BossBarUpdate { id, action } => {
                apply_boss_bar(&mut self.boss_bars, *id, action);
            }
            // Every entity event is folded by `lodestone_ecs::ingest`'s
            // `NetIngest` systems instead and never reaches here —
            // `SharedState::apply` routes them by `lodestone_ecs::ingest::handles_event`
            // before this fold is called. The arms that used to live here (spawn,
            // movement, velocity, head rotation, removal, metadata, attributes,
            // equipment) plus the `apply_metadata` helper are *deleted*, not
            // mirrored: `Inner` has no `entities` map for a second copy to live in.
            //
            // Chat, KeepAlive, Disconnect carry no scalar read-model state.
            // ChunkLoaded / ChunkUnloaded are handled by the adapter through the
            // `WorldSink`, so their heavy payload never reaches this fold; the
            // events themselves are lightweight position-only notifications.
            _ => {}
        }
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
