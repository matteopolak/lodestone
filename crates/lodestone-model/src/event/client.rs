//! Canonical clientbound event carriers.

use uuid::Uuid;

use crate::*;
use super::*;

/// Things that happen to the client after a version adapter lifts a packet into
/// the canonical model.
///
/// **Adding a variant here is not enough to make it reach anything.** Every new
/// variant must also be given an arm in [`route`], which is an exhaustive match
/// in this same crate and therefore a *compile error* until you write it. See
/// [`Route`] and `docs/event-routing.md`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    /// The client entered the game world.
    Login {
        /// Local player entity id.
        entity_id: i32,
        /// Current game mode.
        game_mode: GameMode,
        /// Current dimension.
        dimension: DimensionId,
    },
    /// Chat or system text was received.
    Chat {
        /// Message text.
        text: Text,
        /// Message kind.
        kind: ChatKind,
        /// The sender's profile UUID — the filter key. Only a signed
        /// player-chat message carries one on the wire (`PLAYER_CHAT`); system,
        /// disguised, and action-bar messages have none (the server
        /// pre-decorates the display name into the text), and the legacy
        /// protocol families' chat packets carry no sender field at all. A
        /// consumer that filters by hidden senders must treat `None` as "not a
        /// player message" and show it.
        sender: Option<Uuid>,
        /// Signed-chat acknowledgement metadata, when this chat contributes to
        /// the last-seen acknowledgement window.
        ack: Option<ChatAckInfo>,
    },
    /// The server disconnected the client.
    Disconnect {
        /// Disconnect reason.
        reason: Text,
    },
    /// The session ended because of a **client-side** failure: a transport
    /// error, a read timeout, an adapter rejection, an online-mode
    /// authentication failure. The opposite of [`ClientEvent::Disconnect`],
    /// which is the *server* telling us why.
    ///
    /// # Why this is an event and not just a return value
    ///
    /// The driver already returns every one of these as a
    /// `SessionOutcome::Failed(ClientError)`, and that value is unreachable to
    /// a consumer that only holds a shared handle: taking it consumes the
    /// handle by value, so a shell holding an `Arc<ClientHandle>` cannot. What
    /// such a consumer observes instead is the event stream simply *ending* —
    /// indistinguishable from a clean close, which is why a failed join used to
    /// reach the screen as a synthesised "stream closed" while the real cause
    /// went only to the log. Emitting the failure means the terminal reason
    /// travels the same channel every other session event does, and arrives
    /// before the channel closes.
    ///
    /// # The payload is a plain `String`, deliberately
    ///
    /// Unlike [`ClientEvent::Disconnect`]'s [`Text`], nothing here came off the
    /// wire and nothing is translatable: this is *our* error, rendered with its
    /// full `source()` chain, and a consumer that wants a `Text` wraps it in a
    /// literal node (which every translator is a no-op on).
    SessionFailed {
        /// The error and its `source()` chain, joined with `": "`.
        reason: String,
    },
    /// A keep-alive challenge was received.
    KeepAlive {
        /// Keep-alive id.
        id: i64,
    },
    /// A ping challenge was received (distinct from keep-alive; used for
    /// latency measurement outside the tick-driven keep-alive cadence).
    Ping {
        /// Id that must be echoed back via [`crate::ClientAction::PongResponse`].
        id: i32,
    },
    /// The player was teleported.
    TeleportPlayer {
        /// Target position or relative delta indicated by `flags`.
        pos: Vec3,
        /// Target rotation or relative rotation indicated by `flags`.
        rotation: Rotation,
        /// Relative component flags.
        flags: TeleportFlags,
        /// Velocity correction when the protocol carries one. Older families
        /// use `None`, preserving their stop-on-teleport behavior.
        velocity: Option<TeleportVelocity>,
    },
    /// An entity appeared in the world.
    EntitySpawned {
        /// Entity id.
        entity_id: i32,
        /// Entity UUID when known.
        uuid: Option<Uuid>,
        /// Canonical entity type key.
        entity_type: ResourceKey,
        /// Spawn position.
        pos: Vec3,
        /// Spawn rotation.
        rotation: Rotation,
        /// Spawn velocity when known.
        velocity: Option<Vec3>,
    },
    /// A player entity's profile name was supplied with its spawn identity.
    ///
    /// Protocol 5 carries this beside the remote player's UUID in its named
    /// spawn packet. Keeping it separate from [`ClientEvent::PlayerListUpdate`]
    /// preserves the only wire-authored correlation between that UUID-bearing
    /// entity and this era's name-keyed player-list row.
    PlayerProfileNamed {
        /// Server-assigned entity id receiving the profile name.
        entity_id: i32,
        /// Profile name exactly as supplied by the entity spawn packet.
        profile_name: String,
    },
    /// An entity moved or rotated.
    EntityMoved {
        /// Entity id.
        entity_id: i32,
        /// Movement payload.
        movement: EntityMovement,
        /// New rotation when included.
        rotation: Option<Rotation>,
        /// Whether the entity is on the ground.
        on_ground: bool,
    },
    /// An entity received a position, rotation, and velocity correction whose
    /// components may independently be relative to its current state.
    EntityTeleported {
        /// Entity id.
        entity_id: i32,
        /// Target position or per-axis delta indicated by `flags`.
        pos: Vec3,
        /// Target rotation or per-component delta indicated by `flags`.
        rotation: Rotation,
        /// Relative position and rotation components.
        flags: TeleportFlags,
        /// Velocity correction carried by the packet.
        velocity: TeleportVelocity,
        /// Whether the entity is on the ground.
        on_ground: bool,
    },
    /// An entity's velocity changed.
    EntityVelocity {
        /// Entity id.
        entity_id: i32,
        /// Velocity vector.
        velocity: Vec3,
    },
    /// One or more entities were removed.
    EntityRemoved {
        /// Removed entity ids.
        entity_ids: Vec<i32>,
    },
    /// An entity's metadata changed (spawn-time or incremental).
    ///
    /// The adapter has already resolved the version-specific indices and
    /// serializers into the version-free [`EntityMetadataUpdate`]; only the
    /// fields the packet carried are `Some`.
    EntityMetadataUpdated {
        /// Entity id.
        entity_id: i32,
        /// The fields this packet updated.
        metadata: EntityMetadataUpdate,
    },
    /// A falling block's imitated block state, from its spawn packet's
    /// **Object Data** field.
    ///
    /// # Why this is its own event and not a field on [`EntitySpawned`]
    ///
    /// `ADD_ENTITY`'s trailing VarInt is vanilla's "Object Data": one field whose
    /// meaning is decided entirely by the entity type, and which each type reads in
    /// its own recreate-from-packet override. Vanilla's own falling-block
    /// entity's is
    /// to resolve the packet's Object Data field to a block state by its global
    /// state id and store it. Lowering it as a
    /// per-type event rather than as an opaque integer on the shared spawn event
    /// keeps the *interpretation* in the adapter that has the version's state table,
    /// which is the same reason [`EntityMetadataUpdated`](Self::EntityMetadataUpdated)
    /// carries resolved fields rather than raw indices.
    ///
    /// **This is the only channel by which the state travels.**
    /// Vanilla's own falling-block synced-data registration registers only its
    /// start-position field and nothing
    /// else, so the block state is never in a `SET_ENTITY_DATA` packet. A consumer
    /// that ignores this draws every falling block as whatever state id `0` happens
    /// to be, with nothing logged anywhere.
    ///
    /// Emitted immediately after the entity's own [`EntitySpawned`](Self::EntitySpawned),
    /// so a consumer keyed on the entity id always has the entity first.
    FallingBlockState {
        /// Entity id.
        entity_id: i32,
        /// The block state the entity is imitating. Its source tag is retained
        /// until a version-aware consumer can resolve it safely.
        block_state: BlockStateRef,
    },
    /// A projectile's **owner** entity id, from its spawn packet's
    /// **Object Data** field — the same trailing VarInt
    /// [`FallingBlockState`](Self::FallingBlockState) reads, under the reading
    /// `Projectile.getAddEntityPacket` gives it.
    ///
    /// `Projectile` writes `owner == null ? 0 : owner.getId()` there, and
    /// `FishingHook` overrides that to `owner == null ? this.getId() : owner.getId()`
    /// so the field is never `0` for a hook. Like the falling block's state, this
    /// is the **only** channel it travels on: neither `Projectile` nor
    /// `FishingHook.defineSynchedData` registers an owner accessor, so no
    /// `SET_ENTITY_DATA` packet ever carries it and a consumer that ignores this
    /// event can never learn who cast the rod.
    ///
    /// Emitted immediately after the entity's own [`EntitySpawned`](Self::EntitySpawned),
    /// so a consumer keyed on the entity id always has the entity first.
    ///
    /// Adapters emit this only for the types whose Object Data they have
    /// *established* means an owner id — today that is `minecraft:fishing_bobber`,
    /// the one type with a live consumer (the line drawn back to the caster's
    /// hand). Widening it to every `Projectile` subclass is a decode change, not a
    /// new event.
    ProjectileOwner {
        /// The projectile's entity id.
        entity_id: i32,
        /// The owner's entity id, as the spawn packet's Object Data field
        /// reported it.
        owner_id: i32,
    },
    /// An entity's attributes were (re)published.
    ///
    /// Each snapshot fully replaces the named attribute's base value and modifier
    /// set for that entity; attributes not named are left unchanged.
    EntityAttributesUpdated {
        /// Entity id.
        entity_id: i32,
        /// The attributes carried by this packet.
        attributes: Vec<EntityAttributeSnapshot>,
    },
    /// One or more equipment slots changed on an entity.
    EntityEquipmentUpdated {
        /// Entity id.
        entity_id: i32,
        /// Updated equipment slots.
        equipment: Vec<EntityEquipment>,
    },
    /// Player health, food, or saturation changed.
    HealthChanged {
        /// Current health.
        health: f32,
        /// Current food level.
        food: i32,
        /// Current saturation.
        saturation: f32,
    },
    /// The player died. The server holds a dead player on the death screen and
    /// stops streaming chunks until it receives a respawn request, so a headless
    /// client must react to this (see the client's respawn policy).
    Death {
        /// The death message shown on the death screen.
        message: Text,
    },
    /// World time changed.
    TimeChanged {
        /// Total world age.
        world_age: i64,
        /// Current time of day.
        time_of_day: i64,
    },
    /// Weather state or intensity changed.
    ///
    /// Fields are optional because the server can send one aspect at a time:
    /// start/stop raining, rain level, or thunder level.
    WeatherChanged {
        /// Whether rain is now active, when that changed.
        raining: Option<bool>,
        /// Rain intensity, when that changed.
        rain_level: Option<f32>,
        /// Thunder intensity, when that changed.
        thunder_level: Option<f32>,
    },
    /// The local player's game mode changed.
    GameModeChanged {
        /// New game mode.
        game_mode: GameMode,
    },
    /// The world's default spawn position changed.
    SpawnPositionChanged {
        /// Dimension containing the spawn position.
        dimension: DimensionId,
        /// New default spawn block position.
        pos: BlockPos,
        /// Spawn yaw in degrees.
        angle: f32,
        /// Spawn pitch in degrees.
        pitch: f32,
    },
    /// The local player's ability flags or movement speeds changed.
    AbilitiesChanged {
        /// Whether the player is invulnerable.
        invulnerable: bool,
        /// Whether the player is currently flying.
        flying: bool,
        /// Whether the player may fly.
        can_fly: bool,
        /// Whether the player may instantly build/break.
        instabuild: bool,
        /// Flying speed multiplier.
        flying_speed: f32,
        /// Walking speed multiplier.
        walking_speed: f32,
    },
    /// A positioned sound should play.
    Sound {
        /// Canonical sound event key.
        sound: ResourceKey,
        /// Sound source category.
        category: SoundCategory,
        /// Sound origin.
        pos: Vec3,
        /// Volume multiplier.
        volume: f32,
        /// Pitch multiplier.
        pitch: f32,
        /// Optional fixed audible range overriding the volume-derived default.
        fixed_range: Option<f32>,
        /// Random seed for deterministic sound variant selection.
        seed: i64,
    },
    /// A sound attached to an entity should play.
    EntitySound {
        /// Canonical sound event key.
        sound: ResourceKey,
        /// Sound source category.
        category: SoundCategory,
        /// Entity id the sound follows.
        entity_id: i32,
        /// Volume multiplier.
        volume: f32,
        /// Pitch multiplier.
        pitch: f32,
        /// Optional fixed audible range overriding the volume-derived default.
        fixed_range: Option<f32>,
        /// Random seed for deterministic sound variant selection.
        seed: i64,
    },
    /// A level event occurred at a block position.
    ///
    /// The event code is Mojang's gameplay-level event code. It is not a
    /// registry id for blocks, items, entities, or sounds.
    LevelEvent {
        /// Gameplay event code.
        event: i32,
        /// Event block position.
        pos: BlockPos,
        /// Event-specific data. Event `2001` carries [`LevelEventData::BlockState`]
        /// so its state-id source survives until a version-aware consumer or a
        /// generated-model boundary can resolve it.
        data: LevelEventData,
        /// Whether the event is global rather than distance-limited.
        global: bool,
    },
    /// An explosion occurred.
    ///
    /// One variant carries two wire shapes deliberately. Protocols 5 through
    /// 766 send the removed-block offsets on the explosion packet. Protocols
    /// 774 and 776 send only a block count for cosmetic scaling; their actual
    /// removals arrive as ordinary block updates. See
    /// [`Self::affected_blocks`] for how consumers distinguish those shapes.
    Explosion {
        /// World-space explosion centre.
        pos: Vec3,
        /// Blast radius, in blocks.
        radius: f32,
        /// Blocks the explosion removed, as integer offsets from `pos`
        /// (`pos.floor() + offset` is the removed block's position) —
        /// carried directly by protocols 5 through 766.
        ///
        /// **Always empty on protocol 774 or 776.** Their explosion packets
        /// carry only a count, with no positions, because the removals arrive
        /// as separate block-update events. A fold reading this field for
        /// "which blocks did this explosion remove" must treat an empty list
        /// as "not given by this packet", not as "the explosion removed
        /// nothing" — the two are indistinguishable from this field alone,
        /// which is why this variant exists rather than pretending 26.2 has
        /// the same fidelity.
        affected_blocks: Vec<[i8; 3]>,
        /// This client's own knockback impulse from the blast, if any — an
        /// additive velocity delta, not an absolute velocity.
        /// Protocols 5 through 766 carry the three components unconditionally,
        /// using zeroes outside the blast. Protocols 774 and 776 carry a real
        /// optional vector, which maps to `None` one-for-one.
        knockback: Option<Vec3>,
    },
    /// Particles should spawn.
    Particles {
        /// Canonical particle type key.
        particle: ResourceKey,
        /// Whether the particles should be visible at long distance.
        long_distance: bool,
        /// Whether the particles survive the **Minimal** particle setting —
        /// the level particles packet's own "always show" flag, which
        /// the client's particle-level calculation turns into a one-in-ten
        /// reprieve rather than an exemption.
        ///
        /// Distinct from `long_distance`, which is the *distance* cutoff, and
        /// the two are independent on the wire. **`false` on every legacy
        /// family**, and honestly so rather than by omission: the field does
        /// not exist on the pre-26.2 particle packets at all (1.12's
        /// particle packet carries only the distance flag and nothing else), so
        /// there is no value to carry and `false` is what the corresponding
        /// unconditional-particle call passes.
        always_show: bool,
        /// Particle origin.
        pos: Vec3,
        /// Randomized offset bounds.
        offset: Vec3f,
        /// Particle speed parameter.
        max_speed: f32,
        /// Number of particles to spawn.
        count: i32,
        /// The particle type's own extra payload, if it carries one. See
        /// [`ParticleOptions`].
        options: ParticleOptions,
    },
    /// A container's full content changed.
    ContainerContent {
        /// Window/container id.
        window_id: i32,
        /// Container synchronization state id.
        state_id: ContainerStateId,
        /// Slot contents in container order.
        items: Vec<Option<ItemStack>>,
        /// Item carried by the cursor.
        carried_item: Option<ItemStack>,
    },
    /// A single container slot changed.
    ContainerSlot {
        /// Window/container id.
        window_id: i32,
        /// Container synchronization state id.
        state_id: ContainerStateId,
        /// Slot index.
        slot: i32,
        /// New slot contents.
        item: Option<ItemStack>,
    },
    /// A container/menu property changed.
    ///
    /// These property ids are menu-local channels such as furnace progress,
    /// brewing progress, or enchantment costs. They are not registry ids.
    ContainerData {
        /// Window/container id.
        window_id: i32,
        /// Menu-local property id.
        property: i32,
        /// New property value.
        value: i32,
    },
    /// The server closed a container/menu screen.
    ScreenClosed {
        /// Window/container id.
        window_id: i32,
    },
    /// A container/menu screen opened.
    ScreenOpened {
        /// Window/container id.
        window_id: i32,
        /// Canonical menu type key.
        menu_type: ResourceKey,
        /// Screen title.
        title: Text,
    },
    /// A scoreboard objective was added, removed, or changed.
    ObjectiveUpdate {
        /// Objective name.
        name: String,
        /// Update mode.
        mode: ObjectiveMode,
        /// Display name for add/change; absent for remove.
        display_name: Option<Text>,
        /// Render type for add/change; absent for remove.
        render_type: Option<ObjectiveRenderType>,
        /// Objective default number format for add/change.
        number_format: Option<NumberFormat>,
    },
    /// A scoreboard display slot changed.
    DisplayObjective {
        /// Display slot being assigned.
        slot: DisplaySlot,
        /// Objective name, or `None` to clear the slot.
        objective: Option<String>,
    },
    /// A score was added or changed.
    ScoreUpdate {
        /// Score holder name.
        holder: String,
        /// Objective name.
        objective: String,
        /// Score value.
        value: i32,
        /// Optional display override for the holder.
        display: Option<Text>,
        /// Optional per-score number format.
        number_format: Option<NumberFormat>,
    },
    /// A score was reset.
    ScoreReset {
        /// Score holder name.
        holder: String,
        /// Objective to reset, or `None` to reset all objectives for the holder.
        objective: Option<String>,
    },
    /// A team was created, removed, changed, or had membership changed.
    TeamUpdate {
        /// Team name.
        name: String,
        /// Team action.
        action: TeamAction,
    },
    /// A boss bar was added, removed, or changed.
    BossBarUpdate {
        /// Boss bar id.
        id: Uuid,
        /// Boss bar action.
        action: BossAction,
    },
    /// The player list changed.
    PlayerListUpdate {
        /// Updated player entries.
        entries: Vec<PlayerListEntry>,
    },
    /// A chunk's data at `pos` became available or was replaced.
    ///
    /// This is a lightweight *notification*, not a data carrier. The adapter
    /// applies the fully decoded, version-free chunk (block-state and biome
    /// sections, light, heightmaps, block entities) directly into the
    /// client-owned [`World`](lodestone_world::World) as it decodes the packet;
    /// consumers read that data by querying the world, keyed by `pos`.
    ///
    /// Deliberately carrying only the position keeps this event cheap and, more
    /// importantly, keeps world correctness independent of consumer liveness:
    /// the event travels a bounded channel, so a payload here could be dropped
    /// under backpressure, and a dropped `ChunkLoaded` would be an unrecoverable
    /// hole. As a bare signal it is idempotent and safe to coalesce — treat it
    /// as "the region at `pos` is dirty; re-read or re-mesh it."
    ChunkLoaded {
        /// Chunk position; look the data up in the world by this key.
        pos: ChunkPos,
    },
    /// A chunk became unavailable. The adapter has already removed it from the
    /// client-owned world; this notifies consumers to drop anything derived
    /// from `pos` (a mesh, a collision cache).
    ChunkUnloaded {
        /// Chunk position.
        pos: ChunkPos,
    },
    /// One or more blocks changed inside an already-loaded section, and the
    /// adapter has already applied them to the client-owned
    /// [`World`](lodestone_world::World).
    ///
    /// Like [`ClientEvent::ChunkLoaded`] this is a **dirty-region signal**, not
    /// a data carrier — read the new states from the world. It exists
    /// separately from `ChunkLoaded` because the region is far smaller: a
    /// consumer that re-derives geometry needs to redo one section and only the
    /// neighbours the changed cells actually touch, where a chunk arrival
    /// invalidates a whole column and its horizontal seams. Overloading
    /// `ChunkLoaded` for block updates forces the consumer to conflate the two
    /// and pay the column-sized cost for every redstone tick.
    ///
    /// `section` is in section coordinates (block >> 4 on every axis).
    /// `blocks` lists the section-relative `(x, y, z)` of each changed cell so a
    /// consumer can tell an interior edit — which cannot affect a neighbouring
    /// section — from one on a boundary, which can.
    SectionBlocksChanged {
        /// Section coordinates of the section that changed.
        section: SectionPos,
        /// Section-relative `(x, y, z)`, each `0..16`, of every changed cell.
        blocks: Vec<[u8; 3]>,
    },
    /// A block-triggering "block event" (e.g. a note block playing, a piston
    /// starting to move, a chest lid animating) occurred.
    ///
    /// `b0`/`b1` are opaque per-block-type parameters; their meaning depends on
    /// `block` and is a rendering/audio concern for the consumer, not something
    /// the adapter interprets.
    BlockEvent {
        /// Block position.
        pos: BlockPos,
        /// First event parameter, meaning depends on `block`.
        b0: u8,
        /// Second event parameter, meaning depends on `block`.
        b1: u8,
        /// Canonical block type key.
        block: ResourceKey,
    },
    /// A block's break-progress overlay changed.
    ///
    /// `progress` is the raw wire byte (vanilla uses `0..=9`ish for visible
    /// stages and other values to clear the overlay); the adapter does not
    /// reinterpret it.
    BlockDestruction {
        /// Id of the entity breaking the block (usually a player).
        entity_id: i32,
        /// Block position.
        pos: BlockPos,
        /// Raw break-stage byte.
        progress: u8,
    },
    /// The server acknowledged a client-predicted block change up to
    /// `sequence`; predictions at or before it can be reconciled/discarded.
    BlockChangedAck {
        /// Acknowledged sequence number.
        sequence: PredictionSequence,
    },
    /// The chunk-loading center moved (usually following the player).
    ChunkCacheCenterChanged {
        /// New center chunk X.
        x: i32,
        /// New center chunk Z.
        z: i32,
    },
    /// The server's view/loading radius changed.
    ChunkCacheRadiusChanged {
        /// New radius, in chunks.
        radius: i32,
    },
    /// The simulation (entity-ticking) distance changed.
    SimulationDistanceChanged {
        /// New simulation distance, in chunks.
        distance: i32,
    },
    /// An entity-specific status/animation code was triggered.
    ///
    /// `status` is Mojang's raw per-entity-type event byte (e.g. spawn
    /// particles, play a sound, alter behavior); its meaning depends on the
    /// entity's type and is a consumer-side concern.
    EntityStatus {
        /// Entity id.
        entity_id: i32,
        /// Raw status/event byte.
        status: u8,
    },
    /// An entity's head yaw changed independently of its body rotation.
    EntityHeadRotation {
        /// Entity id.
        entity_id: i32,
        /// New head yaw, in degrees.
        head_yaw: f32,
    },
    /// An entity's passenger list changed.
    EntityPassengersChanged {
        /// Vehicle entity id.
        vehicle_id: i32,
        /// Passenger entity ids, in mounting order.
        passenger_ids: Vec<i32>,
    },
    /// An entity's leash holder changed.
    EntityLeashed {
        /// Leashed entity id.
        entity_id: i32,
        /// Holder entity id, or `None` if the leash was removed.
        holder_id: Option<i32>,
    },
    /// An item entity was picked up (visually flies to the collector before
    /// despawning).
    ItemPickup {
        /// Item entity id.
        item_entity_id: i32,
        /// Collecting entity id (usually a player).
        player_id: i32,
        /// Stack size collected.
        amount: i32,
    },
    /// An entity took damage.
    ///
    /// `damage_type_id` is the raw `minecraft:damage_type` registry network id.
    /// Unlike other registries this adapter resolves to canonical keys,
    /// `minecraft:damage_type` is a purely data-driven registry with no default
    /// protocol ids: its network id is assigned per-connection by the order the
    /// server's registry-sync configuration packets listed entries, which this
    /// adapter does not currently track. Carrying the raw id here is honest
    /// about that gap rather than guessing a mapping.
    EntityDamaged {
        /// Damaged entity id.
        entity_id: i32,
        /// Raw `minecraft:damage_type` registry network id (unresolved; see above).
        damage_type_id: i32,
        /// Entity id that caused the damage (e.g. an arrow's shooter), when known.
        cause_id: Option<i32>,
        /// Entity id that directly dealt the damage (e.g. the arrow itself), when known.
        direct_id: Option<i32>,
        /// World-space damage source position, when the damage had no direct entity source.
        source_pos: Option<Vec3>,
    },
    /// An entity played its hurt animation without necessarily taking damage
    /// (e.g. a client-side prediction correction).
    EntityHurtAnimation {
        /// Entity id.
        entity_id: i32,
        /// Yaw the hurt animation should play at, in degrees.
        yaw: f32,
    },
    /// An entity played a hand-swing or hit-effect animation.
    EntityAnimation {
        /// Entity id.
        entity_id: i32,
        /// Animation kind.
        action: AnimationAction,
    },
    /// A mob effect (potion effect) was applied to or refreshed on an entity.
    MobEffectApplied {
        /// Entity id.
        entity_id: i32,
        /// Canonical mob effect key.
        effect: ResourceKey,
        /// Effect amplifier (0 = level I).
        amplifier: i32,
        /// Remaining duration, in ticks.
        duration_ticks: i32,
        /// Whether the effect originated from ambient sources (e.g. a beacon).
        ambient: bool,
        /// Whether particles are shown.
        visible: bool,
        /// Whether the effect icon is shown in the HUD.
        show_icon: bool,
        /// Whether the effect blends its particle color with others.
        blend: bool,
    },
    /// A mob effect was removed from an entity.
    MobEffectRemoved {
        /// Entity id.
        entity_id: i32,
        /// Canonical mob effect key.
        effect: ResourceKey,
    },
    /// The vehicle the player is riding moved to an absolute position.
    VehicleMoved {
        /// New absolute position.
        pos: Vec3,
        /// New yaw, in degrees.
        yaw: f32,
        /// New pitch, in degrees.
        pitch: f32,
    },
    /// The local player's selected hotbar slot changed.
    HeldSlotChanged {
        /// New selected hotbar slot (`0..9`).
        slot: i32,
    },
    /// The local player's experience bar or level changed.
    ExperienceChanged {
        /// Progress toward the next level, in `0.0..1.0`.
        progress: f32,
        /// Current experience level.
        level: i32,
        /// Total accumulated experience points.
        total: i32,
    },
    /// The item held by the cursor (dragged item) changed.
    CursorItemChanged {
        /// New cursor item, or `None` if empty.
        item: Option<ItemStack>,
    },
    /// A slot in the local player's own inventory changed outside of an open
    /// container screen.
    InventorySlotChanged {
        /// Inventory slot index.
        slot: i32,
        /// New slot contents.
        item: Option<ItemStack>,
    },
    /// One or more entries were removed from the player list.
    PlayerListRemove {
        /// Removed player profile ids.
        profile_ids: Vec<Uuid>,
    },
    /// One or more name-keyed entries were removed from the player list.
    ///
    /// Protocol 5 has no profile UUID in either its add or remove shape, so its
    /// display name is the only identity available for correlating the pair.
    PlayerListRemoveByName {
        /// Removed player display names, exactly as received on the wire.
        profile_names: Vec<String>,
    },
    /// The main title text changed.
    TitleText {
        /// New title text.
        text: Text,
    },
    /// The subtitle text changed.
    SubtitleText {
        /// New subtitle text.
        text: Text,
    },
    /// Titles were cleared/hidden.
    TitlesCleared {
        /// Whether the fade/stay/fade-out timings should also reset to defaults.
        reset_times: bool,
    },
    /// The title fade-in/stay/fade-out timings changed.
    TitlesAnimation {
        /// Fade-in duration, in ticks.
        fade_in: i32,
        /// Stay duration, in ticks.
        stay: i32,
        /// Fade-out duration, in ticks.
        fade_out: i32,
    },
    /// An item (or shared cooldown group) started its use cooldown.
    ItemCooldown {
        /// Cooldown group identifier (an item id or a shared group name).
        group: ResourceKey,
        /// Cooldown duration, in ticks.
        duration_ticks: i32,
    },
    /// The world's difficulty (and whether it is locked) changed.
    DifficultyChanged {
        /// New difficulty.
        difficulty: Difficulty,
        /// Whether the difficulty is locked from further changes in the UI.
        locked: bool,
    },
    /// The server instructed the local player to rotate to (or by) a specific
    /// yaw/pitch, from the player rotation packet.
    PlayerRotationSet {
        /// New (or delta) body yaw, in degrees.
        y_rot: f32,
        /// Whether `y_rot` is relative to the current yaw rather than absolute.
        relative_y: bool,
        /// New (or delta) pitch, in degrees.
        x_rot: f32,
        /// Whether `x_rot` is relative to the current pitch rather than absolute.
        relative_x: bool,
    },
    /// The client's camera was attached to (or detached from) an entity, from
    /// the set camera packet.
    CameraSet {
        /// The entity id the camera now follows. Vanilla sends the local
        /// player's own id to reset the camera to the first-person view.
        entity_id: i32,
    },
    /// A written book screen should open, from the open book packet.
    BookOpened {
        /// `true` for the main hand, `false` for the off hand.
        main_hand: bool,
    },
    /// A sound (or sounds) should stop playing, from
    /// the stop sound packet. Absent fields are wildcards: `sound: None`
    /// stops every sound in `category` (or all sounds if `category` is also
    /// `None`), not "no sound".
    SoundStopped {
        /// Sound to stop, or `None` to match any sound.
        sound: Option<ResourceKey>,
        /// Category to restrict the stop to, or `None` to match any category.
        category: Option<SoundCategory>,
    },
    /// The player list header/footer text changed, from
    /// the tab list packet.
    TabListChanged {
        /// Header text shown above the player list.
        header: Text,
        /// Footer text shown below the player list.
        footer: Text,
    },
    /// The server's stored per-book recipe-book UI state, from
    /// the recipe-book-settings packet.
    ///
    /// Four books in the game's own fixed order, each carrying two booleans — the
    /// wire form is exactly eight bytes with no length prefix and no discriminator.
    /// Named fields rather than a `Vec`
    /// deliberately: the shape is fixed, so a collection would admit a length this
    /// packet cannot have.
    ///
    /// This is the *inbound* half of a round trip whose outbound half already
    /// existed — [`crate::action::ClientAction::SetRecipeBookSettings`] has been
    /// encoded by the adapters for some time, so the client could tell the server
    /// its book state and could never be told the state back.
    RecipeBookSettingsChanged {
        /// The crafting-table book.
        crafting: RecipeBookTypeSettings,
        /// The furnace book.
        furnace: RecipeBookTypeSettings,
        /// The blast-furnace book.
        blast_furnace: RecipeBookTypeSettings,
        /// The smoker book.
        smoker: RecipeBookTypeSettings,
    },
    /// The world border's center moved, from
    /// the set border center packet.
    WorldBorderCenterChanged {
        /// New center X coordinate.
        x: f64,
        /// New center Z coordinate.
        z: f64,
    },
    /// The world border began (or continued) smoothly resizing, from
    /// the set border lerp size packet.
    WorldBorderSizeLerping {
        /// Size (diameter, in blocks) the border is resizing from.
        old_size: f64,
        /// Size (diameter, in blocks) the border is resizing to.
        new_size: f64,
        /// Duration of the resize, in milliseconds.
        lerp_time_ms: i64,
    },
    /// The world border's size changed instantly (no interpolation), from
    /// the set border size packet.
    WorldBorderSizeChanged {
        /// New size (diameter, in blocks).
        size: f64,
    },
    /// The world border's warning delay changed, from
    /// the set border warning delay packet.
    WorldBorderWarningDelayChanged {
        /// New warning delay, in seconds, before the border starts closing in.
        warning_time: i32,
    },
    /// The world border's warning distance changed, from
    /// the set border warning distance packet.
    WorldBorderWarningDistanceChanged {
        /// New distance, in blocks, at which the warning effect appears.
        warning_blocks: i32,
    },
    /// The world border was fully (re)initialized, from
    /// the initialize border packet — sent on join/respawn instead of
    /// the incremental variants above.
    WorldBorderInitialized {
        /// New center X coordinate.
        x: f64,
        /// New center Z coordinate.
        z: f64,
        /// Size (diameter, in blocks) the border is resizing from.
        old_size: f64,
        /// Size (diameter, in blocks) the border is resizing to.
        new_size: f64,
        /// Duration of the resize, in milliseconds.
        lerp_time_ms: i64,
        /// Absolute maximum size the border can ever reach.
        absolute_max_size: i32,
        /// Distance, in blocks, at which the warning effect appears.
        warning_blocks: i32,
        /// Warning delay, in seconds, before the border starts closing in.
        warning_time: i32,
    },
    /// Combat tracking began for the local player, from
    /// the player combat enter packet (no payload).
    PlayerCombatEntered,
    /// Combat tracking ended for the local player, from
    /// the player combat end packet.
    PlayerCombatEnded {
        /// Duration of the combat encounter, in ticks.
        duration_ticks: i32,
    },
    /// The server opened a sign-editing UI, from
    /// the open sign editor packet.
    SignEditorOpened {
        /// Block position of the sign.
        pos: BlockPos,
        /// Whether the front (vs. back) text is being edited.
        is_front_text: bool,
    },
    /// The advancements screen should switch to a given tab, from
    /// the select advancements tab packet.
    AdvancementsTabSelected {
        /// Tab identifier, or `None` to close/deselect the tab.
        tab: Option<Identifier>,
    },
    /// A direction-accelerating projectile's power changed after a deflection,
    /// from the projectile power packet.
    ProjectilePowerChanged {
        /// Projectile entity id.
        entity_id: i32,
        /// New acceleration power.
        acceleration_power: f64,
    },
    /// A ridden entity's (e.g. horse, llama) inventory screen was opened,
    /// from the mount screen open packet.
    MountScreenOpened {
        /// Window/container id.
        container_id: i32,
        /// Number of inventory columns (varies by the ridden entity's
        /// carrying capacity).
        inventory_columns: i32,
        /// Ridden entity id.
        entity_id: i32,
    },
    /// The server's game rule values, from
    /// the game rule values packet.
    GameRulesChanged {
        /// Game rule identifier and its raw string value, in wire order.
        values: Vec<(Identifier, String)>,
    },
    /// The server asked the client to reconnect to a different address, from
    /// the transfer packet.
    TransferRequested {
        /// Target server host.
        host: String,
        /// Target server port.
        port: i32,
    },
    /// The server requested a previously stored cookie, from
    /// the cookie request packet.
    CookieRequested {
        /// Cookie key.
        key: Identifier,
    },
    /// The server asked the client to persist an opaque cookie, from
    /// the store cookie packet.
    CookieStored {
        /// Cookie key.
        key: Identifier,
        /// Opaque payload (at most 5120 bytes).
        payload: Vec<u8>,
    },
    /// The server offered a resource pack, from
    /// the resource pack push packet.
    ResourcePackPushed {
        /// Pack id, echoed back in the client's accept/decline response.
        id: Uuid,
        /// Download URL.
        url: String,
        /// SHA-1 hash of the pack (hex; may be empty if not provided).
        hash: String,
        /// Whether declining or failing to download disconnects the client.
        required: bool,
        /// Optional prompt message shown to the user.
        prompt: Option<Text>,
    },
    /// The server withdrew a previously pushed resource pack, from
    /// the resource pack pop packet.
    ResourcePackPopped {
        /// Pack id to remove, or `None` to remove all packs.
        id: Option<Uuid>,
    },
    /// A plugin (custom payload) message arrived, from
    /// the custom payload packet.
    ///
    /// `data` is the raw payload bytes for `channel`, undecoded: only
    /// `minecraft:brand` is specially typed by vanilla (as a single UTF-8
    /// string) and every other channel is opaque plugin data, so this
    /// adapter carries the bytes as-is rather than guessing a shape.
    CustomPayload {
        /// Channel identifier.
        channel: Identifier,
        /// Raw payload bytes.
        data: Vec<u8>,
    },
    /// Public server metadata pushed proactively during play, from
    /// the server data packet.
    ServerDataReceived {
        /// Message of the day.
        motd: Text,
        /// Favicon PNG bytes, if the server sent one.
        icon: Option<Vec<u8>>,
    },
    /// A play-state pong echo, from the pong response packet (distinct
    /// from the keep-alive-like `Ping`/`ClientAction::PongResponse` pair).
    PongReceived {
        /// Echoed time value.
        time: i64,
    },
    /// A previously sent chat message was deleted/withdrawn, from
    /// the delete chat packet.
    ChatMessageDeleted {
        /// The message's signature; the adapter resolves wire-level cache
        /// references to the full 256 bytes before emitting, so this is
        /// normally [`PackedMessageSignature::Full`].
        signature: PackedMessageSignature,
    },
    /// The local player should look toward a fixed point or another entity,
    /// from the player look at packet.
    PlayerLookAt {
        /// Anchor point on the local player to rotate from.
        from_anchor: LookAnchor,
        /// Target position (already resolved by the server for the entity
        /// case).
        target: Vec3,
        /// If set, the target was an entity at send time; carries its id and
        /// the anchor point used on it.
        at_entity: Option<PlayerLookAtEntity>,
    },
    /// The local player changed dimension (portal travel) or respawned after
    /// death, from the respawn packet.
    Respawned {
        /// New dimension.
        dimension: DimensionId,
        /// New game mode.
        game_mode: GameMode,
        /// Game mode before this respawn, if the server reported one.
        previous_game_mode: Option<GameMode>,
        /// Last death location, if the server tracks one for this dimension.
        last_death_location: Option<DeathLocation>,
    },
    /// The dimension **type** the local player is in changed, resolved against
    /// the Configuration `registry_data`.
    ///
    /// Emitted alongside [`Self::Login`] and [`Self::Respawned`] — the two
    /// packets that carry a dimension-type holder id — and always *before* them,
    /// so a consumer folding both sees the geometry before the level name that
    /// depends on it.
    ///
    /// # Why `dimension_type` is an `Option`
    ///
    /// It is `None` when the id could not be resolved: no `registry_data` was
    /// received (an older server, or a protocol family that does not send it),
    /// or the entry's contents were elided or malformed. That is deliberately
    /// **not** the same as "the overworld" — a consumer must fall back
    /// explicitly rather than inherit a plausible default, which is a shape
    /// that has been gotten wrong before. `holder_id` is always present, so a consumer can log
    /// exactly which id failed to resolve.
    DimensionTypeChanged {
        /// The `minecraft:dimension_type` holder id the server sent.
        holder_id: i32,
        /// The resolved dimension type, or `None` — see above.
        dimension_type: Option<DimensionTypeInfo>,
        /// Whether the level uses the **flat** world generator — the login and
        /// respawn packets' own `is_flat` boolean.
        ///
        /// # Its provenance is not the other two fields'
        ///
        /// `holder_id` and `dimension_type` come from the registry; this comes
        /// straight off the packet, and there is nothing in the
        /// `minecraft:dimension_type` registry that could supply it. It rides
        /// this event only because this event is emitted from exactly the two
        /// packets that carry it, and because every consumer that wants one
        /// wants the other: vanilla keeps both in its own client-level-data side by
        /// side, where its own void-darkness-onset-range query reads its own
        /// "is flat" flag and
        /// its own min-Y query reads the dimension type.
        ///
        /// It is deliberately **not** a field of [`DimensionTypeInfo`], which
        /// is a decode of one registry entry and must stay so — a struct with
        /// two sources is how a field ends up populated on one path and
        /// defaulted on another.
        ///
        /// `false` when the sending family has no such field (only `v26-2`
        /// emits this event today), which is also the non-flat answer, so a
        /// legacy session behaves exactly as it did.
        is_flat: bool,
    },
    /// The per-biome visual attributes the server declared in the Configuration
    /// `registry_data`, **indexed by biome holder id**.
    ///
    /// Emitted alongside [`Self::Login`], for the same reason and in the same
    /// position as [`Self::DimensionTypeChanged`]: re-entering Configuration
    /// resends the whole registry set and is always followed by a fresh `Login`,
    /// so `Login` is the one point at which the registries are known to be
    /// complete and current.
    ///
    /// # Why this carries colours and not names
    ///
    /// The biome registry is a **data-pack** registry: a pack can reorder it,
    /// rename an entry, or change a colour, so nothing about the mapping can be
    /// hardcoded and every hop has to be resolved off what the server sent. The
    /// obvious shape — ship the ordered names, look the colour up in a table
    /// derived from our jar — is wrong on all three counts *and* needs a table
    /// re-derived every version. Shipping the value at the holder id needs no
    /// table at all, and the consumer indexes it with exactly the integer a
    /// chunk section's biome palette already stores.
    BiomeVisuals {
        /// Each biome's `minecraft:visual/sky_color`, packed `0x00RR_GGBB` in
        /// **sRGB bytes**, at its holder id.
        ///
        /// `None` where the biome declares none — 10 of 26.2's 66, exactly the
        /// Nether and End biomes, whose dimensions draw no sky disc — or where
        /// the entry could not be parsed. A `None` still occupies its index: the
        /// position *is* the holder id, so dropping one would shift every later
        /// biome's colour by a slot.
        sky_colors: Vec<Option<u32>>,
    },
    /// The per-biome **climate** the server declared in the same Configuration
    /// `registry_data` [`Self::BiomeVisuals`] reads, **indexed by biome holder id** exactly as
    /// [`Self::BiomeVisuals::sky_colors`] is.
    ///
    /// A *separate* variant rather than two more fields on [`Self::BiomeVisuals`]
    /// on purpose: [`Self::BiomeVisuals`] already has a non-`ecs` consumer that
    /// destructures it by name with no `..`, so adding fields there is a
    /// breaking change to a file this session could not touch to fix in the
    /// same commit. Emitted at the same point as [`Self::BiomeVisuals`] (see its
    /// doc for why `Login` is the right moment), so the two always agree on
    /// which registry generation they describe.
    BiomeClimates {
        /// Each biome's declared (not height-adjusted) `temperature`, at its
        /// holder id. `None` where the entry could not be parsed — every real
        /// 26.2 biome declares one, so unlike `sky_colors` a `None` here should
        /// only ever mean "malformed or elided", never "this biome has none".
        temperatures: Vec<Option<f32>>,
        /// Each biome's `downfall`, at its holder id. Feeds the grass/foliage
        /// colormap sample alongside `temperature`; not itself part of the
        /// rain/snow decision.
        downfall: Vec<Option<f32>>,
        /// Each biome's `has_precipitation`, at its holder id. `false` means
        /// the biome never rains or snows regardless of temperature (deserts,
        /// most Nether/End biomes).
        has_precipitation: Vec<Option<bool>>,
    },
    /// The ordered entry names of the `minecraft:worldgen/biome` registry the
    /// server declared in the Configuration `registry_data`,
    /// **indexed by holder id** exactly as
    /// [`Self::BiomeVisuals::sky_colors`] and [`Self::BiomeClimates`] are.
    ///
    /// Emitted at the same point (`Login`) and for the same reason as those
    /// two — see [`Self::BiomeVisuals`]'s doc — so all three always describe
    /// the same registry generation.
    ///
    /// # Why this exists as a third variant rather than a name column on the other two
    ///
    /// [`Self::BiomeVisuals`] and [`Self::BiomeClimates`] carry colour/climate
    /// *values*; this carries the *identity* of the biome at each holder id,
    /// which is a different consumer (id → name resolution for a tint lookup,
    /// not a value the mesher shades with directly) and a different lifetime
    /// concern — see the `shell`-side [`Route`] arm and
    /// `crates/lodestone-shell/src/mesher.rs`'s `biome_name_at`.
    ///
    /// # Why this closes a real (not hypothetical) correctness gap
    ///
    /// Before this variant, `crates/lodestone-shell/src/mesher.rs` resolved a
    /// chunk section's biome holder id to a name through a hardcoded,
    /// alphabetical `FALLBACK_BIOME_NAMES` table — correct only against this
    /// project's own server, which derives the same alphabetical order. A
    /// real vanilla server (or one with a data pack that reorders, adds, or
    /// removes a biome) sends its **own** registry order, and nothing told
    /// the mesher what that order was: `ClientRegistries::entry_names` (the
    /// v26-2 adapter's already-correct decode of it) never left the version
    /// crate. Joining a third-party server could therefore paint the wrong
    /// grass/foliage/water colour with no error anywhere — the id was valid,
    /// just resolved through the wrong table.
    BiomeRegistryNames {
        /// Each biome's registry entry name (e.g. `minecraft:swamp`), at its
        /// holder id. Every synchronized registry entry carries a name (it is
        /// the wire key, never optional), unlike [`Self::BiomeVisuals::sky_colors`]
        /// or [`Self::BiomeClimates`]'s per-field `Option`s.
        names: Vec<String>,
    },
    /// The server's `minecraft:enchantment` registry order, emitted at `Login`
    /// alongside [`Self::BiomeRegistryNames`].
    ///
    /// # The latent bug this exists to remove
    ///
    /// Exactly [`Self::BiomeRegistryNames`]'s story, one registry over. The table
    /// was **already decoded** — `ClientRegistries::entry_names` has had it all
    /// along — and was simply never handed past the version crate, so
    /// `Sim::riptide_level` resolved `minecraft:riptide` through a **hardcoded
    /// holder id of 32**, derived from `riptide` being the 33rd of 26.2's 43
    /// built-in enchantments in resource-location-sorted order.
    ///
    /// That id is correct against a vanilla 26.2 server and wrong against any data
    /// pack that adds, removes or reorders an enchantment sorting before
    /// `riptide` — and it is wrong *silently*, because the id is still valid and
    /// still resolves to *an* enchantment. Same failure shape as the mesher's
    /// `FALLBACK_BIOME_NAMES`: the wrong table, not a missing one.
    ///
    /// A second consumer is already waiting on it — the enchantment-level gap in
    /// `crates/lodestone-shell/src/entities.rs`.
    EnchantmentRegistryNames {
        /// Each enchantment's registry entry name (e.g. `minecraft:riptide`), at
        /// its holder id. Empty when the server sent no `minecraft:enchantment`
        /// registry, which a consumer must treat as "fall back", not as "no
        /// enchantments exist".
        names: Vec<String>,
    },
    /// A win condition was signalled by the server: the game-event packet's
    /// `WIN_GAME` event (code `4`), sent when the local player exits the End
    /// through the exit portal after defeating the ender dragon.
    ///
    /// Carries no data: the real handler ignores the packet's `param` for
    /// this event and always opens the credits screen with the "show the poem"
    /// flag set `true`, so
    /// there is nothing version-free left to carry — this is a pure signal,
    /// like [`Self::Respawned`] is for a plain "you are alive again" with no
    /// win-specific payload.
    WinGame,
    /// The server's whole Brigadier command tree (`minecraft:commands`,
    /// clientbound id 16), sent once after login (and again if the tree
    /// changes, e.g. a permission level change). Boxed for the same reason
    /// `TitleText`/`UpdateName` box a [`Text`]: a full tree is the largest
    /// payload any `ClientEvent` carries, and every other variant pays that
    /// size in the enum's own stack footprint if it isn't indirected.
    ///
    /// Same shape as [`Self::BiomeRegistryNames`]: a registry-generation
    /// table with a single, obvious consumer (the chat box's tab completion
    /// and syntax highlighting) and no per-entity or per-session scalar to
    /// fold — see [`route`]'s `SHELL` arm for both.
    CommandTreeUpdated {
        /// The decoded tree.
        tree: Box<CommandTree>,
    },
    /// A reply to a serverbound `command_suggestion` request
    /// (`minecraft:command_suggestions`, clientbound id 15):
    /// `ClientboundCommandSuggestionsPacket(int id, int start, int length,
    /// List<Entry>)`. The transaction id lets the chat box discard a stale
    /// reply to a request it has since superseded, matching vanilla's own
    /// `ClientSuggestionProvider::completeCustomSuggestions` id check.
    CommandSuggestionsReceived {
        /// Transaction id, echoing the request's.
        id: i32,
        /// Start of the input-text byte range these suggestions replace.
        start: i32,
        /// Length of that byte range.
        length: i32,
        /// The suggested replacement strings.
        suggestions: Vec<CommandSuggestionEntry>,
    },
    /// A filled map's contents changed, from the map item data packet.
    ///
    /// Keyed on the **map id**, not on an entity: one map item can be held by
    /// several players and hung in several item frames at once, so this is
    /// session-scoped map state rather than per-entity state.
    ///
    /// Both payload halves are genuinely optional and independently so
    /// (`Optional<List<MapDecoration>>` and `Optional<MapPatch>`): a decoration-only
    /// update carries no pixels, and a pixel-only update carries no icons. `None`
    /// means "unchanged", never "empty" — clearing the decorations is
    /// `Some(vec![])`.
    MapItemData {
        /// The map's id (`MapId`), which is what the `minecraft:map_id` item
        /// component on a filled-map stack points at.
        map_id: i32,
        /// Zoom level, 0 (1 pixel per block) to 4 (16 blocks per pixel).
        scale: i8,
        /// Whether the map has been locked with a cartography table.
        locked: bool,
        /// Icons to draw over the map, replacing the previous set, or `None` if
        /// this update does not touch them.
        decorations: Option<Vec<MapDecoration>>,
        /// The changed sub-rectangle of the 128×128 colour grid, or `None`.
        color_patch: Option<MapPatch>,
    },
    /// The advancement tree and/or the local player's progress on it changed,
    /// from the update advancements packet.
    AdvancementsUpdated {
        /// `true` on the server's first packet: discard all known advancements
        /// and progress and treat `added` as the whole tree.
        reset: bool,
        /// Advancements added or redefined.
        added: Vec<AdvancementEntry>,
        /// Advancement ids that are no longer visible.
        removed: Vec<Identifier>,
        /// Per-advancement criterion progress, as
        /// `(advancement id, [(criterion, obtained epoch-millis)])`. A criterion
        /// present with `None` is known but not obtained.
        progress: Vec<(Identifier, Vec<(String, Option<i64>)>)>,
        /// Vanilla's own "show advancements" flag — whether completions announce.
        show_advancements: bool,
    },

    // ---- the remaining clientbound packets ----------------------------------
    //
    // Every variant below routes to `session`, which is deliberate and is the
    // fork `route`'s doc says has cost work twice. None of them is per-entity
    // state (`DebugEntityValue` is *about* an entity but is a debug feed keyed by
    // subscription, not a component hanging off one) and none is block/world
    // state travelling the shell's own stream — they are session-scoped tables,
    // the same shape as the scoreboard and the tab list. That also means none of
    // them needs an arm in `lodestone_shell::net::forward`, so this whole block
    // lands without a shell edit; a screen reads the session component when
    // someone builds one.
    /// The server awarded or resynchronised statistics
    /// (the award stats packet).
    ///
    /// Sent in full on request (vanilla's `/stats`-equivalent screen opening) and
    /// incrementally as counters move, so a fold must **overwrite per key**
    /// rather than accumulate: the wire value is the absolute count.
    StatisticsAwarded {
        /// One entry per statistic the server reported.
        stats: Vec<StatAward>,
    },
    /// The server changed the extra names offered in chat tab-completion
    /// (the custom chat completions packet).
    ChatCompletionsChanged {
        /// Whether to add to, remove from, or replace the current set.
        action: ChatCompletionsAction,
        /// The names this update concerns.
        entries: Vec<String>,
    },
    /// A per-block debug feed value (the debug block value packet).
    ///
    /// The server sends nothing on any debug feed until the client asks with
    /// [`crate::ClientAction::SubscribeDebug`] — this and its three siblings are
    /// the *response* half of that request, which is why neither half is useful
    /// alone.
    DebugBlockValue {
        /// The block this value is about.
        pos: BlockPos,
        /// The `minecraft:debug_subscription` this value belongs to.
        subscription: Identifier,
        /// The feed's own payload bytes, or `None` when the server is clearing
        /// this key. **Opaque**: the value codec is per-subscription and the
        /// seventeen registered ones share no shape (one has a `null` codec), so
        /// modelling them here would be seventeen decoders for a debug overlay.
        value: Option<Vec<u8>>,
    },
    /// A per-chunk debug feed value (the debug chunk value packet).
    DebugChunkValue {
        /// The chunk this value is about.
        chunk: ChunkPos,
        /// The `minecraft:debug_subscription` this value belongs to.
        subscription: Identifier,
        /// Opaque per-subscription payload; see [`Self::DebugBlockValue::value`].
        value: Option<Vec<u8>>,
    },
    /// A per-entity debug feed value (the debug entity value packet).
    ///
    /// Routed to `session`, not `ingest`, even though it names an entity: it is a
    /// debug overlay keyed by subscription with no lifetime tied to the entity's
    /// ECS row, and folding it as a component would resurrect entities the client
    /// has already forgotten.
    DebugEntityValue {
        /// Network id of the entity this value is about.
        entity_id: i32,
        /// The `minecraft:debug_subscription` this value belongs to.
        subscription: Identifier,
        /// Opaque per-subscription payload; see [`Self::DebugBlockValue::value`].
        value: Option<Vec<u8>>,
    },
    /// A one-shot debug feed event (the debug event packet).
    ///
    /// Unlike the three `*Value` packets this carries the payload **without** an
    /// optional wrapper — an event is always present — so there is no "clear this
    /// key" form.
    DebugEvent {
        /// The `minecraft:debug_subscription` this event belongs to.
        subscription: Identifier,
        /// Opaque per-subscription payload; see [`Self::DebugBlockValue::value`].
        value: Vec<u8>,
    },
    /// A batch of server performance samples (the debug sample packet).
    DebugSample {
        /// The samples, in nanoseconds for the tick-time kind.
        sample: Vec<i64>,
        /// Which sample series this batch belongs to.
        kind: DebugSampleKind,
    },
    /// The server asked the client to highlight a game-test position
    /// (the game test highlight pos packet).
    GameTestHighlightPos {
        /// Absolute world position.
        absolute: BlockPos,
        /// Position relative to the test's own origin.
        relative: BlockPos,
    },
    /// The server is running low on disk space
    /// (the low disk space warning packet).
    ///
    /// A zero-byte packet — `StreamCodec.unit` — so this variant carries nothing,
    /// like [`Self::WinGame`].
    LowDiskSpaceWarning,
    /// The server sent crash/report metadata for the client to attach to a report
    /// (the custom report details packet).
    CustomReportDetails {
        /// `(title, description)` pairs, at most 32 entries.
        details: Vec<(String, String)>,
    },
    /// The server advertised its links (the server links packet).
    ///
    /// Vanilla shows these on the pause and disconnect screens. Every entry is
    /// **untrusted** — the label may be an arbitrary server-authored component
    /// and the URL an arbitrary string — which is why nothing here resolves or
    /// validates either.
    ServerLinksReceived {
        /// The advertised links, in the order sent.
        links: Vec<ServerLink>,
    },
    /// A tracked waypoint was added, updated or removed
    /// (the tracked waypoint packet).
    WaypointUpdated {
        /// Whether this is a track, untrack or update.
        operation: WaypointOperation,
        /// The waypoint.
        waypoint: TrackedWaypoint,
    },
    /// A reply to a serverbound NBT query (the tag query packet).
    ///
    /// The transaction id echoes
    /// [`crate::ClientAction::QueryEntityTag`]/[`crate::ClientAction::QueryBlockEntityTag`],
    /// so a consumer can match a reply to its own request and drop a stale one.
    /// `tag` is `None` when the server had nothing (or refused): the wire carries
    /// a nullable compound, not an error.
    TagQueryResponse {
        /// Transaction id echoed from the request.
        transaction_id: i32,
        /// The queried NBT as raw network-NBT bytes, or `None`.
        tag: Option<Vec<u8>>,
    },
    /// The world's tick rate or freeze state changed
    /// (the ticking state packet) — vanilla's `/tick rate` and
    /// `/tick freeze`.
    TickingStateChanged {
        /// Ticks per second the server is targeting.
        tick_rate: f32,
        /// Whether the world is frozen.
        frozen: bool,
    },
    /// The server is stepping a frozen world forward
    /// (the ticking step packet) — vanilla's `/tick step`.
    TickingStepped {
        /// How many ticks remain to run while frozen.
        tick_steps: i32,
    },
    /// A test instance block reported its status
    /// (the test-instance-block-status packet).
    TestInstanceBlockStatus {
        /// Human-readable status line.
        status: Text,
        /// Detected region size, when the server has one.
        size: Option<(i32, i32, i32)>,
    },
    /// The server asked the client to open a dialog
    /// (the show dialog packet).
    ///
    /// The wire is a `Holder<Dialog>`: either a registry id, or an inline dialog
    /// as a network-NBT blob. `Dialog` is an NBT `Codec` union of six types with
    /// nested body/input/action trees — a *schema*, not a `StreamCodec` — so the
    /// inline form is carried here as raw NBT bytes. A screen that renders
    /// dialogs parses them; nothing before that point needs to.
    DialogShown {
        /// The registry id of a known dialog, when the server referenced one.
        registry_id: Option<i32>,
        /// The inline dialog as raw network-NBT bytes, when the server sent one.
        /// Exactly one of this and `registry_id` is `Some`.
        inline: Option<Vec<u8>>,
    },
    /// The server closed any open dialog (the clear dialog packet).
    ///
    /// Another zero-byte `StreamCodec.unit` packet.
    DialogCleared,

    // ---- the recipe/trade tranche --------------------------------------------
    //
    // These five needed `SlotDisplay` — a *recursive* registry-dispatched union
    // of eleven variants with no length prefix anywhere — so none of them could
    // land before the walker existed and all five landed together.
    //
    // Each carries **result item ids** rather than a modelled display tree. A
    // recipe panel and a toast both key on the result; the ingredient slots are
    // walked only because they must be consumed to reach it. Modelling the whole
    // tree would be a second recipe representation next to
    // `lodestone_game::recipe`, which already has one.
    /// The server unlocked recipes (the recipe book add packet).
    ///
    /// `replace` is the server's first-sync flag: discard the known set and treat
    /// `entries` as the whole book. **It sits after the entry list on the wire**,
    /// which is why the list cannot be carried as opaque trailing bytes.
    RecipeBookAdded {
        /// The unlocked recipes.
        entries: Vec<RecipeBookEntry>,
        /// Whether this replaces the known set rather than adding to it.
        replace: bool,
    },
    /// The server un-learned recipes (the recipe book remove packet),
    /// e.g. after a datapack reload.
    RecipeBookRemoved {
        /// `RecipeDisplayId`s to forget.
        display_ids: Vec<i32>,
    },
    /// The server is showing a ghost recipe in an open crafting grid
    /// (the place ghost recipe packet) — the faded preview after clicking a
    /// recipe in the book.
    GhostRecipeShown {
        /// The container the ghost belongs to.
        window_id: i32,
        /// Item ids the ghost's result slot can display, retaining registry
        /// provenance when a synchronized registry contains dynamic entries.
        result_items: Vec<ItemId>,
    },
    /// The server's recipe *property sets* changed
    /// (the update recipes packet).
    ///
    /// Not the recipe corpus: these are the "which items are valid in this slot"
    /// sets vanilla's screens use to grey out an input (fuel, smithing template,
    /// and so on), plus the stonecutter's own input→result list.
    RecipePropertySetsUpdated {
        /// `(property set key, valid item ids with registry provenance)`.
        item_sets: Vec<(Identifier, Vec<ItemId>)>,
        /// One entry per stonecutter recipe: `(input item ids, result item
        /// ids)`, each retaining registry provenance. The input is the ingredient a stonecutter's
        /// input slot must hold for this entry's results to be offered; without
        /// it a consumer cannot compute the subset of results reachable from
        /// whatever the slot currently holds.
        stonecutter_results: Vec<(Vec<ItemId>, Vec<ItemId>)>,
    },
    /// A villager or wandering trader opened its trade list
    /// (the merchant offers packet).
    MerchantOffersReceived {
        /// The trade container's window id.
        window_id: i32,
        /// The offers, in the order shown.
        offers: Vec<MerchantOffer>,
        /// The villager's level, 1–5.
        villager_level: i32,
        /// The villager's experience toward its next level.
        villager_xp: i32,
        /// Whether the level/xp bar should be shown.
        show_progress: bool,
        /// Whether this merchant restocks (false for a wandering trader).
        can_restock: bool,
    },
}

