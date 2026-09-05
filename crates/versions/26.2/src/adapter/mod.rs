//! [`VersionAdapter`] implementation driving the protocol 776 join flow.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer, read_network_nbt};
// The wire-shaped, decode-target command tree. Deliberately *not*
// `lodestone-command`'s arena/`dyn ArgumentType` construction API — see
// `lodestone_model::command_tree`'s module doc for why the two stay separate.
use lodestone_model::command_tree::{
    ArgumentParser, CommandSuggestionEntry, CommandSuggestionsResponse, CommandTree, NodeKind,
    RawCommandNode, StringKind,
};
use lodestone_model::{
    AdapterError, AdvancementDisplay, AdvancementEntry, AdvancementFrame, AnimationAction,
    ArmorTrim, BannerPatternLayer, BlockAabb, BlockActionKind, BlockFace, BlockHardness,
    BlockPos,
    BossAction,
    BossColor,
    BossOverlay, ChatAckInfo, ChatCompletionsAction, ChatKind, ChatMode, ChunkPos, ClientAction,
    ClientEvent,
    ClientSettings, CollisionRule, CommandBlockMode, ConnectionState, ContainerClickType,
    ContainerSlotChange, DeathLocation, DebugSampleKind, Difficulty, DimensionTypeInfo, Directive,
    DisplaySlot,
    DisplayedSkinParts,
    EntityBaseDimensions,
    EntityEquipment,
    EntityFacts,
    EntityInteraction, EntityMetadataUpdate, EntityMovement, EntityVariant, EquipmentSlot,
    GameMode, Hand, ItemComponents,
    ItemEnchantment, ItemPrototype, ItemProfile, ItemStack, ItemTool, JigsawJoint, LoginProfile,
    LookAnchor, MainHand, MapDecoration, MapPatch, MerchantOffer as ModelMerchantOffer,
    NumberFormat, ObjectiveMode, ObjectiveRenderType, PackedMessageSignature,
    ParticleOptions, ParticleStatus, PlayerCommand, PlayerInput, PlayerListEntry,
    PlayerLookAtEntity,
    PotDecorations, ProfileProperty as ModelProfileProperty,
    RecipeBookEntry, RecipeBookType,
    RecipeBookTypeSettings,
    ResourceKey, ResourcePackResponseKind, Rotation, SectionPos, ServerAddress, ServerLink,
    ServerLinkKind, SoundCategory, StatAward,
    StructureBlockMode, StructureBlockUpdateType, StructureMirror, StructureRotation, TeamAction,
    TeamColor, TeamParameters, TeleportFlags, TestBlockMode as ModelTestBlockMode,
    TestInstanceAction, TestInstanceData, TestInstanceStatus, Text, TextColor, ToolBlocks,
    ToolMining, ToolPatch, ToolRule, TrackedWaypoint, Vec3, Vec3f, VersionAdapter, Visibility,
    WaypointId, WaypointOperation, WaypointPosition, WorldSink, WrittenBookContent,
};
use lodestone_world::{
    BiomePatch, ChunkPos as WorldChunkPos, LightPatch, LoadedChunk, NibbleArray, PalettedContainer,
};
use lodestone_game::chat_ack::{MessageSignature, MessageSignatureCache};

use lodestone_data::block_entity_types::block_entity_type;
use lodestone_data::block_states::block_type_name;
use crate::chunk_batch::ChunkBatchSizeCalculator;
use lodestone_data::data_component_types::component_type_name;
use lodestone_data::entity_types::entity_type_name;
use lodestone_data::items::{item_id, item_name};
use lodestone_data::menus::menu_name;
use lodestone_data::mob_effects::{mob_effect_id, mob_effect_name};
use crate::packet_ids::{configuration, handshaking, login, play};
use crate::packets::chunk::{ChunkShape, LevelChunkWithLight};
use crate::packets::common::{
    BrandPayload, ClientInformation, CookieResponse, KeepAlive, PingRequest, Pong,
    ResourcePackResponse, TeleportToEntity,
};
use crate::packets::configuration::{
    AcceptCodeOfConduct, FinishConfiguration, ServerboundKnownPacks,
};
use crate::packets::entity::{read_lp_vec3, unpack_degrees};
use crate::packets::game::{
    ABILITY_FLAG_CAN_FLY, ABILITY_FLAG_FLYING, ABILITY_FLAG_INSTABUILD, ABILITY_FLAG_INVULNERABLE,
    AcceptTeleportation, Attack, BlockEntityTagQuery, COMMAND_BLOCK_FLAG_AUTOMATIC,
    COMMAND_BLOCK_FLAG_CONDITIONAL,
    COMMAND_BLOCK_FLAG_TRACK_OUTPUT, ChangeGameMode, ChatAck, ChatCommand, ChatMessage,
    ChatSessionUpdate,
    ChunkBatchFinished, ChunkBatchReceived, ClientCommand, ClientTickEnd, CommandSuggestion,
    ConfigurationAcknowledged, ContainerButtonClick, ContainerClose, ContainerSlotStateChanged,
    EditBook, EntityTagQuery, GameEvent, GameLogin, LevelEvent, LevelParticles,
    MOVE_FLAG_HORIZONTAL_COLLISION,
    MOVE_FLAG_ON_GROUND, MovePlayerPos, MovePlayerPosRot, MovePlayerRot, MovePlayerStatusOnly,
    MoveVehicle, PaddleBoat, PickItemFromBlock, PickItemFromEntity, PlaceRecipe, PlayerAbilities,
    PlayerAction, PlayerCommand as PlayerCommandPacket, PlayerInput as PlayerInputPacket,
    PlayerLoaded, RecipeBookChangeSettings, RecipeBookSeenRecipe, RenameItem, Respawn,
    SERVERBOUND_ABILITY_FLAG_FLYING, SelectBundleItem, SelectTrade, ServerboundPlayerAbilities,
    SetCarriedItem, SetCommandBlock, SetDefaultSpawnPosition, SetHealth, SignUpdate, Swing,
    UseItem, UseItemOn,
};
use crate::packets::handshake::Intention;
use crate::packets::login::{
    CustomQueryAnswer, EncryptionRequest, EncryptionResponse, LoginAcknowledged, LoginCompression,
    LoginDisconnect, LoginFinished,
};
use crate::packets::metadata::{
    MetadataClass, TrackedEntity, metadata_class, read_entity_metadata, read_update_attributes,
};
use crate::packets::player_info::{PlayerInfoRemove, PlayerInfoUpdate};
use crate::packets::registry::{ClientRegistries, DimensionType, RegistryData};
use crate::packets::scoreboard::{
    self as sb, BossEvent, ResetScore, SetDisplayObjective, SetObjective, SetPlayerTeam, SetScore,
};
use crate::packets::time::SetTime;
use lodestone_data::particle_types::particle_type_name;
use lodestone_data::sound_events::sound_event;

mod chat;
mod chunk;
mod connection;
mod entity;
mod inventory;
mod player;
mod scoreboard;
mod serverbound;
mod xfer;

// Re-exported so `crate::adapter::{game_mode_from_ordinal, game_mode_to_ordinal,
// DecodedStack, read_item_stack}` keep resolving after the split — `server_protocol.rs`
// and `packets/metadata.rs` depend on those exact paths.
pub(crate) use chunk::game_mode_from_ordinal;
pub(crate) use inventory::{DecodedStack, read_item_stack};
pub(crate) use serverbound::game_mode_to_ordinal;

/// Protocol version implemented by this adapter.
pub const PROTOCOL: i32 = 776;

/// Fixed decoding/encoding context for protocol 776.
const CTX: Ctx = Ctx { version: PROTOCOL };

/// Requested next-state value in the handshake for a login connection.
const NEXT_STATE_LOGIN: i32 = 2;

/// Version adapter for the 770-family protocols, implementing protocol 776
/// (Minecraft 26.2).
///
/// The adapter carries a small amount of per-connection state: the current
/// dimension's [`ChunkShape`], needed to decode `level_chunk_with_light`
/// (chunk framing depends on the dimension's build-height window, which is not
/// carried in the chunk packet itself). It is set from the `login` and
/// `respawn` packets and defaults to the overworld. One adapter instance is
/// created per connection and driven sequentially by the client, so the state
/// is guarded by a [`Mutex`] purely to satisfy `Sync`; there is no contention.
#[derive(Debug, Clone)]
pub struct V770Adapter {
    shape: Arc<Mutex<ChunkShape>>,
    batch: Arc<Mutex<ChunkBatchState>>,
    movement: Arc<Mutex<MovementSendState>>,
    /// Diagnostic only — `LOGIN` packets seen on this connection, so the
    /// `transfer` target can say whether a join is the first (a fresh session)
    /// or a later one (a proxy swapping the backend underneath one socket).
    /// See [`xfer`]'s module doc; nothing in the decode path reads it.
    logins_seen: Arc<AtomicU64>,
    /// Tracks the concrete type of spawned entities whose cosmetic variant lives
    /// at a metadata index that other mobs reuse (sheep wool @ 17, horse variant
    /// @ 18). Only these ambiguous classes are stored, bounding the map to the
    /// mobs actually present; self-identifying registry-holder variants need no
    /// entry. Populated on `add_entity`, cleared on `remove_entities`.
    variants: Arc<Mutex<HashMap<i32, TrackedEntity>>>,
    /// The overworld day clock, held across packets because `set_time` mostly
    /// does **not** carry it. See [`DayClock`].
    clock: Arc<Mutex<DayClock>>,
    /// Registries folded out of the Configuration `registry_data` stream.
    /// Empty until Configuration runs; every reader falls back
    /// explicitly, because a server that sends none must still play.
    registries: Arc<Mutex<ClientRegistries>>,
    /// Holder id of the `minecraft:world_clock` entry the **current dimension**
    /// follows, resolved at `login`/`respawn` from the dimension type's
    /// `default_clock`.
    ///
    /// `None` means "not resolved" — either no `registry_data` arrived, or the
    /// dimension type has no clock of its own (the Nether, which has fixed
    /// time). Both fall back to `SetTime::day_clock`'s lowest-holder-id pick;
    /// see the `set_time` arm.
    clock_holder: Arc<Mutex<Option<i32>>>,
    /// The client's 128-entry signed-chat signature cache. Packed
    /// ids in `PLAYER_CHAT`'s last-seen list and in `delete_chat` index into it;
    /// every received signed body is pushed back so future ids resolve. Guarded
    /// by a [`Mutex`] only to satisfy `Sync`, like the other per-connection
    /// state above.
    chat_cache: Arc<Mutex<MessageSignatureCache>>,
}

/// The client's copy of the server's overworld day clock.
///
/// # Why any state is needed here at all
///
/// 26.2's `set_time` is `(gameTime, Map<Holder<WorldClock>, ClockNetworkState>)`,
/// and the map is **empty in almost every packet**: the once-a-second
/// `MinecraftServer::forceGameTimeSynchronization` sends `an empty/literal map()`, while
/// `ServerClockManager::modifyClock` sends a one-entry map only when a clock
/// changes and `createFullSyncPacket` sends the full map once, at join. So a
/// stateless adapter has no day time to report for 19 packets out of 20, and the
/// previous code filled that hole with the monotonic world age — which pinned
/// `sky_darken_for_time_of_day` to one value for the whole session. See
/// [`SetTime::day_clock`] for the measurement.
///
/// Vanilla's client has the same problem and solves it the same way: it holds the
/// clock and advances it locally, the server only correcting it. Here the
/// correction *and* the elapsed-tick reference both ride on `set_time`'s own
/// `gameTime`, so no local tick loop is needed — `time_of_day` simply advances in
/// ~20-tick steps, one per sync. That granularity is invisible in the only
/// consumer (`sky_darken_for_time_of_day`, whose curve moves over thousands of
/// ticks).
///
/// # How to change it
///
/// If a second clock ever needs to be surfaced (the End clock, id `1`), widen
/// this to a map keyed by `holder_id` and pick per dimension; `ClientEvent`'s
/// single `time_of_day` field is the constraint, not this struct.
#[derive(Debug, Clone, Copy)]
struct DayClock {
    /// The clock's tick count at [`Self::at_game_time`].
    total_ticks: i64,
    /// Ticks of clock per tick of world age. **`0.0` means paused** — that is
    /// how `/gamerule advanceTime false` and a paused clock arrive on the wire.
    rate: f32,
    /// The `set_time.game_time` this anchor was taken at.
    at_game_time: i64,
    /// Whether a real clock update has ever been seen. Until it has, the anchor
    /// is seeded from the world age, reproducing the old behaviour exactly
    /// rather than reporting a confidently wrong `0`. In practice this window is
    /// closed by the join-time full sync before any gameplay packet arrives.
    synced: bool,
}

impl DayClock {
    /// The clock's tick count at world age `game_time`, extrapolated from the
    /// anchor at the server's own rate. Never runs backwards: a `game_time`
    /// behind the anchor (a re-anchor from a *later* packet, or a wrapped clock)
    /// contributes zero rather than a negative offset.
    fn time_of_day(&self, game_time: i64) -> i64 {
        let elapsed = game_time.saturating_sub(self.at_game_time).max(0);
        #[allow(clippy::cast_possible_truncation)]
        let advanced = (elapsed as f64 * f64::from(self.rate)) as i64;
        self.total_ticks.saturating_add(advanced)
    }
}

impl Default for DayClock {
    fn default() -> Self {
        Self {
            total_ticks: 0,
            rate: 1.0,
            at_game_time: 0,
            synced: false,
        }
    }
}

/// Per-connection chunk-batch flow-control state: the running rate estimator and
/// the start instant of the batch currently in flight. Guarded by a [`Mutex`]
/// only to satisfy `Sync`; a connection drives it sequentially.
#[derive(Debug)]
struct ChunkBatchState {
    calculator: ChunkBatchSizeCalculator,
    /// `None` on `wasm32`, which has no monotonic clock. Without a timing sample
    /// the calculator keeps its default desired rate, which is a throughput
    /// *hint* to the server rather than a correctness input — so declining to
    /// measure costs an adaptive batch size and nothing else.
    batch_start: Option<Instant>,
}

/// A monotonic reading where one exists, and `None` on `wasm32`.
///
/// `std::time::Instant::now()` **compiles** for `wasm32-unknown-unknown` and
/// then **panics at runtime** — "time not implemented on this platform". This
/// one call sat in `V770Adapter::new()`, so a browser session died at adapter
/// construction, before the first byte reached the wire, and the browser profile
/// is `panic = "abort"`: no unwind, no error path, just a dead session with
/// nothing in the log to name it.
///
/// Third instance of this defect in one day — the others were three join timers
/// in `lodestone-server`'s `server.rs` and `hold_read`/`hold_write` in
/// `lodestone-ecs` (`02d77f85`), the latter on the driver's ingest path, so the
/// join died on the first event after `Login`. **`tokio::time::Instant` is not
/// the fix**: rustc's own `help:` offers it, and it bottoms out in
/// `std::time::Instant::now()` and panics identically.
fn batch_now() -> Option<Instant> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        Some(Instant::now())
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// Per-connection movement-send tracking, mirroring the fields vanilla's
/// `LocalPlayer` uses to decide which `move_player_*` packet (if any) a tick
/// sends: `xLast`/`yLast`/`zLast`/`yRotLast`/`xRotLast`,
/// `lastOnGround`/`lastHorizontalCollision`, and `positionReminder`.
///
/// These track the last **sent** pose, not the last pose the client held —
/// position and rotation only advance when that axis actually sends (the
/// same hysteresis vanilla uses to avoid re-sending on float jitter), while
/// on-ground/horizontal-collision advance every tick regardless of whether
/// anything was sent. Zero-initialized to match Java's field defaults, so a
/// fresh connection's first `Move` reads as maximally "dirty" against the
/// all-zero baseline — exactly like a freshly constructed `LocalPlayer`.
#[derive(Debug, Clone, Copy)]
struct MovementSendState {
    last_pos: Vec3,
    last_yaw: f32,
    last_pitch: f32,
    last_on_ground: bool,
    last_horizontal_collision: bool,
    /// Ticks since the last full position update; forces one every 20 ticks
    /// even with zero movement, matching `positionReminder >= 20`.
    position_reminder: u32,
    /// The `transfer` tracing target's yardstick, and half of the staleness
    /// test [`V770Adapter::select_move_packet`] applies. Vanilla has no
    /// equivalent field; see [`xfer`]'s module doc for what it measures and
    /// why the answer cannot be read off the packets alone.
    last_teleport: Option<xfer::AcceptedTeleport>,
    /// Outbound movement packets emitted since `last_teleport` was recorded.
    /// Zero on the *first* move after a teleport, the only one that can have
    /// been overtaken by it.
    moves_since_teleport: u32,
}

impl Default for MovementSendState {
    fn default() -> Self {
        Self {
            last_pos: Vec3::new(0.0, 0.0, 0.0),
            last_yaw: 0.0,
            last_pitch: 0.0,
            last_on_ground: false,
            last_horizontal_collision: false,
            position_reminder: 0,
            last_teleport: None,
            moves_since_teleport: 0,
        }
    }
}

impl V770Adapter {
    /// Creates a new adapter with the overworld chunk shape as its default.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shape: Arc::new(Mutex::new(ChunkShape::overworld_1_21())),
            batch: Arc::new(Mutex::new(ChunkBatchState {
                calculator: ChunkBatchSizeCalculator::new(),
                batch_start: batch_now(),
            })),
            movement: Arc::new(Mutex::new(MovementSendState::default())),
            logins_seen: Arc::new(AtomicU64::new(0)),
            variants: Arc::new(Mutex::new(HashMap::new())),
            clock: Arc::new(Mutex::new(DayClock::default())),
            registries: Arc::new(Mutex::new(ClientRegistries::default())),
            clock_holder: Arc::new(Mutex::new(None)),
            chat_cache: Arc::new(Mutex::new(MessageSignatureCache::vanilla())),
        }
    }

    /// Records the start of a chunk batch so its duration can be measured when
    /// the matching `chunk_batch_finished` arrives.
    fn begin_chunk_batch(&self) {
        if let Ok(mut state) = self.batch.lock() {
            state.batch_start = batch_now();
        }
    }

    /// Folds the finished batch into the rate estimator and returns the desired
    /// chunks-per-tick rate to acknowledge with.
    fn finish_chunk_batch(&self, batch_size: i32) -> f32 {
        match self.batch.lock() {
            Ok(mut state) => {
                if let Some(start) = state.batch_start {
                    let duration_nanos = start.elapsed().as_nanos() as f64;
                    state
                        .calculator
                        .on_batch_finished(batch_size, duration_nanos);
                }
                state.calculator.desired_chunks_per_tick()
            }
            Err(_) => ChunkBatchSizeCalculator::new().desired_chunks_per_tick(),
        }
    }

    /// Records the chunk shape for `dimension` so subsequent chunk packets in
    /// that dimension decode against the correct build-height window.
    ///
    /// The name-matched fallback, used only when `registry_data` did not resolve
    /// the dimension type — see [`Self::enter_dimension`].
    fn set_dimension(&self, dimension: &str) {
        if let Ok(mut shape) = self.shape.lock() {
            *shape = ChunkShape::for_dimension(dimension);
        }
    }

    /// Folds one `registry_data` packet into this connection's registry store.
    fn apply_registry_data(&self, data: RegistryData) {
        if let Ok(mut registries) = self.registries.lock() {
            registries.apply(data);
        }
    }

    /// Resolves a `login`/`respawn` dimension-type holder id against the
    /// registries received during Configuration, and installs everything that
    /// depends on it: the chunk [`ChunkShape`] and the day-clock holder.
    ///
    /// Returns the [`DimensionTypeInfo`] to publish, or `None` when the id did
    /// not resolve — in which case the shape falls back to
    /// [`ChunkShape::for_dimension`]'s level-name match, exactly the old
    /// behaviour before registry-driven resolution existed. That fallback is
    /// *not* dead code: a protocol family or server
    /// that sends no `registry_data` still has to join, and the client must not
    /// disconnect over a registry it merely wanted.
    ///
    /// # Why the id and not the level name
    ///
    /// `login`/`respawn` carry both a level [`DimensionId`](lodestone_model::DimensionId)
    /// (`minecraft:the_nether`) and a bare `dimension_type` **holder id**. Only
    /// the latter is authoritative: a data pack can point a level called
    /// `mypack:mine` at the vanilla overworld type, or give
    /// `minecraft:overworld` a 1024-tall custom type, and a name match gets both
    /// wrong. `ChunkShape::for_dimension`'s own doc comment already admitted
    /// this; it is the height half of the same class of bug filed for sky light.
    fn enter_dimension(&self, holder_id: i32, level_name: &str) -> Option<DimensionTypeInfo> {
        let resolved = self
            .registries
            .lock()
            .ok()
            .and_then(|registries| {
                let (name, dimension_type) = registries.dimension_type(holder_id)?;
                // The clock holder must be resolved *while the lock is held*,
                // because `default_clock` names an entry in a second registry
                // living in the same store.
                let clock_holder = dimension_type
                    .default_clock
                    .as_deref()
                    .and_then(|clock| registries.world_clock_id(clock));
                Some((name.to_owned(), dimension_type.clone(), clock_holder))
            });

        let Some((name, dimension_type, clock_holder)) = resolved else {
            // Unresolved: keep the name match, and forget any clock holder the
            // previous dimension resolved — a stale holder is worse than none,
            // since `day_clock`'s fallback at least tracks a real clock.
            self.set_dimension(level_name);
            if let Ok(mut holder) = self.clock_holder.lock() {
                *holder = None;
            }
            return None;
        };

        if let Ok(mut shape) = self.shape.lock() {
            // Only the vertical window comes from the registry; the palette
            // framing and the air/biome ids are properties of the *protocol
            // family*, not of the dimension, so they keep the family's values.
            *shape = ChunkShape {
                min_y: dimension_type.min_y,
                section_count: dimension_type.section_count(),
                world_height: u32::try_from(dimension_type.height.max(0)).unwrap_or(0),
                ..ChunkShape::overworld_1_21()
            };
        }
        if let Ok(mut holder) = self.clock_holder.lock() {
            *holder = clock_holder;
        }
        dimension_type_info(&name, &dimension_type)
    }

    /// The day-clock holder id the current dimension follows, if resolved.
    fn current_clock_holder(&self) -> Option<i32> {
        self.clock_holder.lock().ok().and_then(|holder| *holder)
    }

    /// Returns the current dimension's chunk shape.
    fn current_shape(&self) -> ChunkShape {
        self.shape
            .lock()
            .map(|shape| shape.clone())
            .unwrap_or_else(|_| ChunkShape::overworld_1_21())
    }

    /// Chooses which `move_player_*` packet, if any, this tick's movement
    /// produces, and updates the send-tracking state accordingly.
    ///
    /// Mirrors vanilla's own client-side position-send tick exactly (see
    /// `ServerboundMovePlayerPacket` for the wire shapes it selects between):
    /// position is "dirty" when the squared distance from the last **sent**
    /// position exceeds `(2e-4)²`, or every 20 ticks regardless of movement
    /// (the periodic forced update); rotation is dirty on *any* nonzero yaw
    /// or pitch delta from the last sent rotation. Both-dirty sends
    /// `PosRot`; position-only sends `Pos`; rotation-only sends `Rot`;
    /// neither, but on-ground or horizontal-collision changed since last
    /// tick, sends `StatusOnly`; otherwise nothing is sent this tick — a
    /// deliberate, vanilla-faithful `None`, not a bug.
    ///
    /// `on_ground` and `horizontal_collision` are simulation outputs
    /// supplied by the caller (see [`ClientAction::Move`]); this method only
    /// ever compares and forwards them, never derives them.
    fn select_move_packet(
        &self,
        pos: Vec3,
        rotation: Rotation,
        on_ground: bool,
        horizontal_collision: bool,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        let mut state = self
            .movement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // **The first movement packet after a teleport must claim that
        // teleport's own target**, and this is the only place in the client
        // that can guarantee it. A vanilla client applies the pose, confirms
        // the teleport and sends its next movement packet on one thread, so a
        // claim built from the pre-teleport pose cannot exist there. Here the
        // shell's pose lives on the frame thread, three queues upstream of this
        // one, and the driver writes the confirmation the instant the packet
        // decodes — so a movement action that had already left the shell sits
        // in the driver's own queue and is encoded *after* the confirmation,
        // still claiming where the player used to be. Neither end of the
        // shell's channel can reach that action any more; this mutex, which the
        // confirmation itself is recorded under, is the last point that can.
        //
        // Measured against the survival oracle before this held: the server
        // answered such a claim with `moved too quickly!` and a corrective
        // teleport on roughly half of all teleports, reading a vertical term of
        // exactly the distance from the old pose to the target — the *speed*
        // rule, which unlike the positional-disagreement rule does not zero the
        // vertical component.
        //
        // Only the first move is rewritten, and only when it carries **both**
        // halves of the signature staleness actually has: it is more than
        // [`xfer::STALE_MOVE_BLOCKS`] from the teleport target (a producer that
        // had adopted the teleport could not have got that far in one tick),
        // *and* it is still within that same distance of the pose this adapter
        // last put on the wire — because a claim the teleport overtook was
        // built from the pre-teleport pose, which is that one.
        //
        // Distance from the target alone is not that signature, and reading it
        // as one silently swallows a caller's own deliberate long move. A
        // headless caller (`ClientHandle::move_to`/`set_position`/`walk_to`,
        // which run no physics and place the player wherever asked) routinely
        // makes its first move after a join placement hundreds of blocks away,
        // built long after that placement landed. Rewritten onto the target,
        // that move leaves the server believing the player never moved, with
        // no error on either end — and every consequence of moving is then
        // computed at the spawn: the streamed view never follows, no column is
        // ever forgotten, and a melee knockback direction measured from the
        // attacker's tracked position points from the wrong place.
        let stale_claim = state.moves_since_teleport == 0
            && state
                .last_teleport
                .is_some_and(|teleport| teleport.distance_to(pos) > xfer::STALE_MOVE_BLOCKS)
            && xfer::distance(pos, state.last_pos) <= xfer::STALE_MOVE_BLOCKS;
        let claimed = pos;
        let pos = match state.last_teleport {
            Some(teleport) if stale_claim => teleport.target,
            _ => pos,
        };
        // Vanilla's own post-teleport send passes `false` for both rather than
        // forwarding what the client last computed, and the flags below are
        // built from these two.
        let (on_ground, horizontal_collision) = if stale_claim {
            (false, false)
        } else {
            (on_ground, horizontal_collision)
        };

        let delta_x = pos.x - state.last_pos.x;
        let delta_y = pos.y - state.last_pos.y;
        let delta_z = pos.z - state.last_pos.z;
        let delta_yaw = f64::from(rotation.yaw) - f64::from(state.last_yaw);
        let delta_pitch = f64::from(rotation.pitch) - f64::from(state.last_pitch);

        state.position_reminder += 1;
        let distance_sq = delta_x * delta_x + delta_y * delta_y + delta_z * delta_z;
        let moved = distance_sq > 4.0e-8 || state.position_reminder >= 20;
        let rotated = delta_yaw != 0.0 || delta_pitch != 0.0;

        // A jump this large in one tick is a teleport (ours or the server's),
        // never real movement — sprint-jumping tops out well under one block
        // per tick. Logged so a build can show, in order: the placement
        // `TeleportPlayer` (`Driver::emit`'s `info` line), then this — the
        // first outbound movement packet *after* it, carrying the position it
        // actually claims. If this line is missing or still reports the old
        // world's coordinates after a transfer/reconfigure, the gap is here
        // (or upstream in whatever feeds `pos`) rather than in teleport
        // confirmation, which this crate already answers per-packet in
        // `handle_player_position`.
        if distance_sq > 64.0 {
            tracing::info!(
                target: "net",
                from_x = state.last_pos.x,
                from_y = state.last_pos.y,
                from_z = state.last_pos.z,
                to_x = pos.x,
                to_y = pos.y,
                to_z = pos.z,
                distance = distance_sq.sqrt(),
                "outbound movement jumped >8 blocks in one tick (teleport echo)"
            );
        }

        let flags = (if on_ground { MOVE_FLAG_ON_GROUND } else { 0 })
            | (if horizontal_collision {
                MOVE_FLAG_HORIZONTAL_COLLISION
            } else {
                0
            });

        let packet = if moved && rotated {
            let body = MovePlayerPosRot {
                x: pos.x,
                y: pos.y,
                z: pos.z,
                yaw: rotation.yaw,
                pitch: rotation.pitch,
                flags,
            };
            Some((play::serverbound::MOVE_PLAYER_POS_ROT, encode_body(&body)?))
        } else if moved {
            let body = MovePlayerPos {
                x: pos.x,
                y: pos.y,
                z: pos.z,
                flags,
            };
            Some((play::serverbound::MOVE_PLAYER_POS, encode_body(&body)?))
        } else if rotated {
            let body = MovePlayerRot {
                yaw: rotation.yaw,
                pitch: rotation.pitch,
                flags,
            };
            Some((play::serverbound::MOVE_PLAYER_ROT, encode_body(&body)?))
        } else if state.last_on_ground != on_ground
            || state.last_horizontal_collision != horizontal_collision
        {
            let body = MovePlayerStatusOnly { flags };
            Some((
                play::serverbound::MOVE_PLAYER_STATUS_ONLY,
                encode_body(&body)?,
            ))
        } else {
            None
        };

        // The `transfer` target's wire-side half. Emitted only when a packet is
        // actually produced, so the line count matches the packet count rather
        // than the tick count, and *before* the state updates below so
        // `moves_since_teleport` still reads zero on the first move after a
        // teleport — the only one whose distance from the target is diagnostic.
        // See `xfer`'s module doc for the race this is here to settle.
        if let Some((packet_id, _)) = packet.as_ref() {
            let teleport = state.last_teleport;
            let distance = teleport.map(|teleport| teleport.distance_to(pos));
            let seq = xfer::next_seq();
            if stale_claim {
                // `x`/`y`/`z` are what actually went on the wire, so they are
                // the teleport's own target and `dist_from_teleport` is zero;
                // the interesting number is `claimed`, the position the
                // simulation had built and this send replaced.
                tracing::warn!(
                    target: "transfer",
                    seq,
                    packet_id,
                    x = pos.x,
                    y = pos.y,
                    z = pos.z,
                    yaw = rotation.yaw,
                    pitch = rotation.pitch,
                    teleport_seq = teleport.map(|teleport| teleport.seq),
                    teleport_id = teleport.map(|teleport| teleport.id),
                    dist_from_teleport = distance,
                    claimed_x = claimed.x,
                    claimed_y = claimed.y,
                    claimed_z = claimed.z,
                    "xfer: move packet -- FIRST move after a teleport was built from a \
                     pre-teleport pose and has been rewritten to the teleport target; the \
                     server would have read the original as movement it did not authorise"
                );
            } else {
                tracing::debug!(
                    target: "transfer",
                    seq,
                    packet_id,
                    x = pos.x,
                    y = pos.y,
                    z = pos.z,
                    yaw = rotation.yaw,
                    pitch = rotation.pitch,
                    moves_since_teleport = state.moves_since_teleport,
                    teleport_seq = teleport.map(|teleport| teleport.seq),
                    teleport_id = teleport.map(|teleport| teleport.id),
                    dist_from_teleport = distance,
                    "xfer: move packet"
                );
            }
            state.moves_since_teleport = state.moves_since_teleport.saturating_add(1);
        }

        if moved {
            state.last_pos = pos;
            state.position_reminder = 0;
        }
        if rotated {
            state.last_yaw = rotation.yaw;
            state.last_pitch = rotation.pitch;
        }
        state.last_on_ground = on_ground;
        state.last_horizontal_collision = horizontal_collision;

        Ok(packet)
    }

    /// Counts a `LOGIN` packet and returns its ordinal on this connection.
    ///
    /// `1` is a fresh join. Anything higher is a **second login on one socket**
    /// — the shape a Velocity/BungeeCord backend switch takes, as distinct from
    /// the `minecraft:transfer` packet, which asks the client to reconnect to a
    /// new address and so starts a whole new connection (and a whole new
    /// adapter, whose count would be `1` again). Diagnostic only.
    pub(super) fn note_login(&self) -> u64 {
        self.logins_seen.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Records the teleport `handle_player_position` just answered with
    /// `ACCEPT_TELEPORTATION`, and emits the `transfer` target's inbound line.
    ///
    /// `absolute_target` is `Some` only when every positional component of the
    /// packet's `relatives` mask was absolute; a relative teleport **clears**
    /// the yardstick rather than inventing one, because this adapter holds no
    /// player position to resolve a delta against. See [`xfer`]'s module doc.
    ///
    /// Clearing, not keeping: a relative teleport has moved the player away
    /// from the previous absolute target, so measuring the next claim against
    /// that target answers a question nobody asked. That was merely misleading
    /// while this state only chose a log level; it would be wrong now that
    /// [`V770Adapter::select_move_packet`] rewrites a first post-teleport claim
    /// onto the target it finds here.
    pub(super) fn note_accepted_teleport(
        &self,
        id: i32,
        absolute_target: Option<Vec3>,
        rotation: Rotation,
        relatives: i32,
    ) {
        let seq = xfer::next_seq();
        tracing::debug!(
            target: "transfer",
            seq,
            teleport_id = id,
            x = absolute_target.map(|target| target.x),
            y = absolute_target.map(|target| target.y),
            z = absolute_target.map(|target| target.z),
            yaw = rotation.yaw,
            pitch = rotation.pitch,
            relatives,
            "xfer: PLAYER_POSITION received; ACCEPT_TELEPORTATION echoed with the same id"
        );
        let mut state = self
            .movement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.moves_since_teleport = 0;
        state.last_teleport =
            absolute_target.map(|target| xfer::AcceptedTeleport { seq, id, target });
    }
}

impl Default for V770Adapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns a protocol 776 version adapter.
///
/// This free function is the crate's canonical constructor entry point; the
/// client boxes the returned concrete type as a `dyn VersionAdapter`.
#[must_use]
pub fn adapter() -> V770Adapter {
    V770Adapter::new()
}

/// Encodes a packet body into a fresh byte buffer.
///
/// Thin wrapper over the version-free [`lodestone_core::encode_body`], which
/// returns a stringified error because `AdapterError` lives in
/// `lodestone-model` and `lodestone-core` cannot depend on it.
fn encode_body<T: Encode>(packet: &T) -> Result<Vec<u8>, AdapterError> {
    lodestone_core::encode_body(packet, CTX).map_err(AdapterError::Encode)
}

/// Decodes a packet body from raw bytes.
fn decode_body<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    lodestone_core::decode_body(payload, CTX).map_err(AdapterError::Decode)
}

/// Decodes a packet body and asserts the payload was consumed to the last byte.
///
/// The trailing zero-length check is the cheapest misparse detector available:
/// a wrong field width or a skipped conditional almost always leaves the reader
/// misaligned, which surfaces here as a decode error instead of silently
/// corrupting downstream state.
fn decode_full<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX).map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(value)
}

/// Unpacks vanilla's own packed block-position long value into canonical
/// block coordinates.
///
/// The packing places `x` in the high 26 bits, `z` in the middle 26 bits, and
/// `y` in the low 12 bits, each stored as a two's-complement signed field.
fn unpack_block_pos(packed: i64) -> BlockPos {
    let x = (packed >> 38) as i32;
    let y = ((packed << 52) >> 52) as i32;
    let z = ((packed << 26) >> 38) as i32;
    BlockPos { x, y, z }
}

/// Parses a `minecraft:*` identifier into a canonical [`ResourceKey`],
/// attributing a decode error to `what` on failure.
fn parse_key(name: &str, what: &str) -> Result<ResourceKey, AdapterError> {
    name.parse()
        .map_err(|_| AdapterError::Decode(format!("invalid {what} key {name}")))
}

/// Builds a [`Directive::Send`] from a packet id and an encodable body.
fn send<T: Encode>(packet_id: i32, packet: &T) -> Result<Directive, AdapterError> {
    Ok(Directive::Send {
        packet_id,
        payload: encode_body(packet)?,
    })
}

/// Maps a decode error to the adapter's decode-error variant.
fn dec_err(err: impl std::fmt::Display) -> AdapterError {
    AdapterError::Decode(err.to_string())
}

/// Lifts a wire [`DimensionType`] into the version-free [`DimensionTypeInfo`].
///
/// Returns `None` only when the registry entry's own id is not a valid
/// identifier — a server sending one has bigger problems, and reporting "not
/// resolved" is safer than inventing a key. The vertical window has already been
/// installed by the caller at that point, so a malformed *name* does not cost us
/// the correct chunk shape.
fn dimension_type_info(name: &str, value: &DimensionType) -> Option<DimensionTypeInfo> {
    Some(DimensionTypeInfo {
        name: name.parse::<ResourceKey>().ok()?,
        has_skylight: value.has_skylight,
        has_ceiling: value.has_ceiling,
        has_fixed_time: value.has_fixed_time,
        coordinate_scale: value.coordinate_scale,
        min_y: value.min_y,
        height: value.height,
        logical_height: value.logical_height,
        ambient_light: value.ambient_light,
        ambient_light_color: value.ambient_light_color,
    })
}

/// Dispatches a clientbound Play-state packet by trying each domain in turn.
///
/// Exactly one `handle_play_*` below ever recognises a given `packet_id` (the
/// wire ids are disjoint by construction), so every non-matching domain falls
/// through to its own empty `Ok(Vec::new())` and contributes nothing —
/// concatenating all seven results is equivalent to the single monolithic
/// if-chain this replaced, without needing an early return.
impl V770Adapter {
    fn handle_play(
        &self,
        world: &mut dyn WorldSink,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut directives = self.handle_play_connection(packet_id, payload)?;
        directives.extend(self.handle_play_chat(packet_id, payload)?);
        directives.extend(self.handle_play_chunk(world, packet_id, payload)?);
        directives.extend(self.handle_play_entity(packet_id, payload)?);
        directives.extend(self.handle_play_player(packet_id, payload)?);
        directives.extend(self.handle_play_inventory(packet_id, payload)?);
        directives.extend(self.handle_play_scoreboard(packet_id, payload)?);
        Ok(directives)
    }
}

impl VersionAdapter for V770Adapter {
    fn protocol_version(&self) -> i32 {
        PROTOCOL
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["26.2"]
    }

    fn supports(&self, protocol: i32) -> bool {
        protocol == PROTOCOL
    }

    fn begin_login(
        &self,
        profile: &LoginProfile,
        server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        let intention = Intention {
            protocol_version: PROTOCOL,
            host: server.host.clone(),
            port: server.port,
            next_state: NEXT_STATE_LOGIN,
        };
        let hello = crate::packets::login::LoginHello {
            name: profile.username.clone(),
            profile_id: profile.uuid,
        };
        Ok(vec![
            send(handshaking::serverbound::INTENTION, &intention)?,
            Directive::SetState(ConnectionState::Login),
            send(login::serverbound::HELLO, &hello)?,
        ])
    }

    fn handle_packet(
        &self,
        world: &mut dyn WorldSink,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        match state {
            ConnectionState::Login => self.handle_login(packet_id, payload),
            ConnectionState::Configuration => self.handle_configuration(packet_id, payload),
            ConnectionState::Play => self.handle_play(world, packet_id, payload),
            ConnectionState::Handshaking | ConnectionState::Status => {
                Err(AdapterError::UnsupportedPacketState { state })
            }
        }
    }

    fn encode_action(
        &self,
        state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        self.encode_client_action(state, action)
    }
    fn build_encryption_response(
        &self,
        encrypted_secret: &[u8],
        encrypted_token: &[u8],
    ) -> Result<Directive, AdapterError> {
        // Both inputs are already RSA ciphertext from the driver; we only frame
        // them as the version's two-byte-array `key` packet.
        send(
            login::serverbound::KEY,
            &EncryptionResponse {
                shared_secret: encrypted_secret.to_vec(),
                verify_token: encrypted_token.to_vec(),
            },
        )
    }

    fn entity_dimensions(&self, entity_type_id: i32) -> Option<EntityBaseDimensions> {
        // The base hitbox census is 26.2 game data homed in `lodestone-data`;
        // the registry seam reaches it through here so a
        // version-free consumer never names v26-2 or the data crate directly.
        // Base dims only — the caller folds SCALE/STEP_HEIGHT from the
        // entity's attribute map.
        lodestone_data::entity_dimensions::base_dimensions(entity_type_id)
    }

    fn entity_facts(&self, entity_type: &ResourceKey) -> Option<EntityFacts> {
        // The same two censuses `entity_dimensions` and `entity_census` expose,
        // read by resource key instead of wire id — which is the only identity a
        // consumer downstream of ingest still holds. Both lookups are indexed by
        // the same id, so resolving the key once serves both, and a type outside
        // either census misses whole rather than half-answering.
        let id = lodestone_data::entity_types::entity_type_id_parts(
            entity_type.namespace(),
            entity_type.path(),
        )?;
        Some(EntityFacts {
            dimensions: lodestone_data::entity_dimensions::base_dimensions(id)?,
            pushes_players: lodestone_data::entity_census::pushes_players(id)?,
            collidable: lodestone_data::entity_census::can_be_collided_with(id)?,
        })
    }

    fn block_hardness(&self, state_id: u32) -> Option<BlockHardness> {
        // The per-block-state hardness census is 26.2 game data homed in
        // `lodestone-data`; the registry seam reaches it through
        // here so a version-free consumer never names v26-2 or the data crate
        // directly. `requires_correct_tool` is the *block's* requirement, not
        // the player's tool match — see `BlockHardness`.
        let state_id = lodestone_data::block_states::StateId::new(state_id)?;
        let entry = lodestone_data::hardness::hardness(state_id);
        Some(BlockHardness {
            hardness: entry.hardness,
            requires_correct_tool: entry.requires_correct_tool,
        })
    }

    fn tool_mining(&self, held: Option<&ItemStack>, state_id: u32) -> Option<ToolMining> {
        // The `minecraft:tool` census — item prototypes, block tag membership,
        // and the block-state→block-registry map — is 26.2 game data homed in
        // `lodestone-data`; the registry seam reaches it through
        // here so a version-free consumer never names v26-2 or the data crate
        // directly. The returned `correct_tool` is already the equivalent of
        // vanilla's own correct-tool-for-drops check, block requirement
        // folded in, so the caller has nothing left to invert.
        let state_id = lodestone_data::block_states::StateId::new(state_id)?;
        Some(lodestone_data::tool::mining(held, state_id))
    }

    fn block_collision(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
        // The per-block-state collision census is 26.2 game data homed in
        // `lodestone-data` (dumped from the real 26.2 server's own
        // block-state registry); the registry seam reaches it through
        // here so a version-free consumer never names v26-2 or the data crate
        // directly. Zero-copy: `collision_shapes::Aabb` *is* `BlockAabb`, so
        // this hands back the rodata slice itself.
        lodestone_data::block_states::StateId::new(state_id)
            .map(lodestone_data::collision_shapes::collision_boxes)
    }

    fn block_name(&self, state_id: u32) -> Option<&'static str> {
        // Block *name* for a block-*state* id, from the same generated table the
        // asset baker resolves properties through. `&'static str` out of rodata,
        // O(1), no instance and no allocation — the physics seam calls this for
        // the block under the player every tick.
        lodestone_data::block_states::block_name(state_id)
    }

    fn block_outline(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
        // Vanilla's own block-state outline shape — the shape its entity-pick
        // routine clips against, and a third thing beside collision and fluid
        // presence. 26.2 game data
        // homed in `lodestone-data`; zero-copy out of rodata. See
        // `lodestone_data::outline_shapes` for why half of all states disagree
        // with `block_collision`.
        lodestone_data::block_states::StateId::new(state_id)
            .map(lodestone_data::outline_shapes::outline_boxes)
    }

    fn block_interaction(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
        // Vanilla's own block-state interaction shape — empty for all but four
        // block families, and a *face* refinement on top of the outline hit
        // rather than a clip target of its own.
        lodestone_data::block_states::StateId::new(state_id)
            .map(lodestone_data::outline_shapes::interaction_boxes)
    }

    fn item_prototype(&self, item: &str) -> Option<ItemPrototype> {
        // The item-prototype census (`minecraft:max_stack_size`,
        // `minecraft:max_damage`, `minecraft:equippable`) is 26.2 game data
        // homed in `lodestone-data`, because a clientbound stack
        // carries only the *patch* against it and so none of the three is
        // ever on the wire. Stacks decoded
        // by this adapter already have these folded into
        // `ItemComponents`' effective fields; this seam is for callers with no
        // stack in hand.
        lodestone_data::item_prototypes::model_prototype(item)
    }

    fn block_blocks_motion(&self, state_id: u32) -> Option<bool> {
        // Vanilla's own block-state motion-blocking flag, dumped per state
        // rather than derived from `block_collision`: vanilla's own
        // solidity-calculation routine's first three branches
        // (a forced-solid override on 237 blocks, a forced-non-solid override
        // on 8, and a null shape cache on the 23 dynamic-shape blocks) are
        // invisible to any shape table, and skipping them is wrong for 2,618
        // of 32,366 states. One bit
        // out of rodata. See `lodestone_data::block_solidity`.
        lodestone_data::block_states::StateId::new(state_id)
            .map(lodestone_data::block_solidity::blocks_motion)
    }

    fn block_bubble_column_drag(&self, state_id: u32) -> Option<bool> {
        // Read straight off the generated state table rather than through a bespoke
        // census: `drag` is a real blockstate property that Mojang's own
        // `blocks.json` carries, so `properties()` already has it and there is
        // nothing to dump. The two states are 15294 (`drag=true`, the block's
        // default) and 15295 (`drag=false`) in the 26.2 palette.
        //
        // The name is checked first because `drag` is not guaranteed unique to this
        // block across versions, and matching on the property alone would silently
        // widen if another block ever gained one.
        if lodestone_data::block_states::block_name(state_id)? != "minecraft:bubble_column" {
            return None;
        }
        lodestone_data::block_states::properties(state_id)?
            .iter()
            .find(|(name, _)| *name == "drag")
            .map(|(_, value)| *value == "true")
    }
}

#[cfg(test)]
mod tests {
    use lodestone_client::VersionAdapter;

    use super::adapter;

    #[test]
    fn tool_mining_validates_raw_state_ids_at_the_adapter_boundary() {
        let adapter = adapter();

        assert!(adapter.tool_mining(None, 0).is_some());
        assert!(
            adapter
                .tool_mining(None, lodestone_data::block_states::STATE_COUNT)
                .is_none()
        );
        assert!(adapter.tool_mining(None, u32::MAX).is_none());
    }
}
