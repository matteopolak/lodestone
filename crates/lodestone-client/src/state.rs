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

use lodestone_model::{
    BlockPos, ChunkPos, ClientEvent, DimensionId, EntityAttributeSnapshot, EntityEquipment,
    EntityMetadataUpdate, EntityMovement, EntityPose, EntityVariant, GameMode, PlayerListEntry,
    ResourceKey, Rotation, Vec3,
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
        }
    }
}

/// A view of another entity in the world.
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
    /// The entity's custom name, once reported. `Some(None)` means a name was
    /// explicitly cleared; `None` means none has ever been reported.
    pub custom_name: Option<Option<String>>,
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
}

/// The mutable scalar state behind the lock. Private; only ever touched under
/// [`SharedState`]'s lock. World (chunk) state lives in a separate lock so a
/// chunk write never contends with a scalar read.
#[derive(Debug, Default)]
struct Inner {
    player: PlayerSnapshot,
    entities: HashMap<i32, EntityView>,
    players: HashMap<Uuid, PlayerListEntry>,
    world_age: i64,
    time_of_day: i64,
    scoreboard: Scoreboard,
    /// Boss bars in server insertion order (render order).
    boss_bars: Vec<BossBar>,
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
}

impl std::fmt::Debug for SharedState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Avoid locking (and dumping the whole world) in Debug output.
        f.debug_struct("SharedState").finish_non_exhaustive()
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            world: Arc::new(RwLock::new(World::new())),
            notify: Arc::new(Notify::new()),
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
    pub(crate) fn apply(&self, event: &ClientEvent) {
        {
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

    /// Clones out a single entity view.
    #[must_use]
    pub(crate) fn entity(&self, entity_id: i32) -> Option<EntityView> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .entities
            .get(&entity_id)
            .cloned()
    }

    /// Clones out all currently tracked entities.
    #[must_use]
    pub(crate) fn entities(&self) -> Vec<EntityView> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .entities
            .values()
            .cloned()
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

    /// Returns `(world_age, time_of_day)`.
    #[must_use]
    pub(crate) fn time(&self) -> (i64, i64) {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        (inner.world_age, inner.time_of_day)
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
}

impl Inner {
    /// Folds one non-chunk event into the model.
    fn apply(&mut self, event: &ClientEvent) {
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
            ClientEvent::TimeChanged {
                world_age,
                time_of_day,
            } => {
                self.world_age = *world_age;
                self.time_of_day = *time_of_day;
            }
            ClientEvent::EntitySpawned {
                entity_id,
                uuid,
                entity_type,
                pos,
                rotation,
                velocity,
            } => {
                self.entities.insert(
                    *entity_id,
                    EntityView {
                        entity_id: *entity_id,
                        uuid: *uuid,
                        entity_type: entity_type.clone(),
                        position: *pos,
                        rotation: *rotation,
                        head_yaw: rotation.yaw,
                        velocity: *velocity,
                        on_ground: false,
                        flags: None,
                        custom_name: None,
                        custom_name_visible: None,
                        pose: None,
                        health: None,
                        baby: None,
                        variant: None,
                        attributes: Vec::new(),
                        equipment: Vec::new(),
                    },
                );
            }
            ClientEvent::EntityMoved {
                entity_id,
                movement,
                rotation,
                on_ground,
            } => {
                if let Some(entity) = self.entities.get_mut(entity_id) {
                    entity.position = match movement {
                        EntityMovement::Absolute(pos) => *pos,
                        EntityMovement::Relative(delta) => entity.position + *delta,
                    };
                    if let Some(rotation) = rotation {
                        entity.rotation = *rotation;
                    }
                    entity.on_ground = *on_ground;
                }
            }
            ClientEvent::EntityVelocity {
                entity_id,
                velocity,
            } => {
                if let Some(entity) = self.entities.get_mut(entity_id) {
                    entity.velocity = Some(*velocity);
                }
            }
            ClientEvent::EntityHeadRotation {
                entity_id,
                head_yaw,
            } => {
                if let Some(entity) = self.entities.get_mut(entity_id) {
                    entity.head_yaw = *head_yaw;
                }
            }
            ClientEvent::EntityRemoved { entity_ids } => {
                for id in entity_ids {
                    self.entities.remove(id);
                }
            }
            ClientEvent::EntityMetadataUpdated {
                entity_id,
                metadata,
            } => {
                if let Some(entity) = self.entities.get_mut(entity_id) {
                    apply_metadata(entity, metadata);
                }
            }
            ClientEvent::EntityAttributesUpdated {
                entity_id,
                attributes,
            } => {
                if let Some(entity) = self.entities.get_mut(entity_id) {
                    for snapshot in attributes {
                        match entity
                            .attributes
                            .iter_mut()
                            .find(|existing| existing.attribute == snapshot.attribute)
                        {
                            Some(existing) => *existing = snapshot.clone(),
                            None => entity.attributes.push(snapshot.clone()),
                        }
                    }
                }
            }
            ClientEvent::EntityEquipmentUpdated {
                entity_id,
                equipment,
            } => {
                if let Some(entity) = self.entities.get_mut(entity_id) {
                    for update in equipment {
                        match entity
                            .equipment
                            .iter_mut()
                            .find(|existing| existing.slot == update.slot)
                        {
                            Some(existing) => *existing = update.clone(),
                            None => entity.equipment.push(update.clone()),
                        }
                    }
                }
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
            // Chat, KeepAlive, Disconnect carry no scalar read-model state.
            // ChunkLoaded / ChunkUnloaded are handled by the adapter through the
            // `WorldSink`, so their heavy payload never reaches this fold; the
            // events themselves are lightweight position-only notifications.
            _ => {}
        }
    }
}

/// Folds a metadata update into an entity view: each field is only overwritten
/// when the update actually carried it, so a partial `set_entity_data` (the
/// common case — the server sends only changed indices) never clobbers
/// previously-known values.
fn apply_metadata(entity: &mut EntityView, metadata: &EntityMetadataUpdate) {
    if let Some(flags) = metadata.flags {
        entity.flags = Some(flags);
    }
    if let Some(custom_name) = &metadata.custom_name {
        entity.custom_name = Some(custom_name.clone());
    }
    if let Some(visible) = metadata.custom_name_visible {
        entity.custom_name_visible = Some(visible);
    }
    if let Some(pose) = metadata.pose {
        entity.pose = Some(pose);
    }
    if let Some(health) = metadata.health {
        entity.health = Some(health);
    }
    if let Some(baby) = metadata.baby {
        entity.baby = Some(baby);
    }
    if let Some(variant) = &metadata.variant {
        entity.variant = Some(variant.clone());
    }
}
