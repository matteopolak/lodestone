//! Play-state packets for this era (protocol 766).

use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};
use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use super::common::NetworkNbt;
use super::position::Position;

/// The spawn description this era carries in place of the loose dimension
/// fields every older era spells out inline.
///
/// # Why this is a struct and not seven fields
///
/// [`JoinGame`] and [`Respawn`] carry the *same* description here, and 1.20.2
/// is where they stopped diverging: below this era the join packet carries a
/// dimension **codec** plus a name while respawn carries only names, so the
/// two packets need two different readers. At 766 both embed this block
/// verbatim, so there is one reader and one place the vertical window is
/// resolved from.
///
/// # The field that decides how a column is framed
///
/// `dimension` is a **registry index**, not a name. The names arrive earlier,
/// in the configuration phase's `minecraft:dimension_type` registry, and this
/// varint indexes into that registry's own delivery order. Reading it as a
/// string desynchronises immediately; reading it and then ignoring the
/// registry silently frames every column with the wrong section count. See
/// [`ChunkShape::from_dimension_index`](crate::packets::chunk::ChunkShape::from_dimension_index).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct SpawnInfo {
    /// Index into the configuration phase's dimension-type registry.
    #[mc(varint)]
    pub dimension: i32,
    /// Namespaced identifier of the **level** being joined or respawned into.
    pub world_name: String,
    /// Hashed world seed (for client-side biome noise).
    pub hashed_seed: i64,
    /// Game mode (`0` survival, `1` creative, `2` adventure, `3` spectator).
    pub game_mode: i8,
    /// Previous game mode, or `0xff` when there is none.
    pub previous_game_mode: u8,
    /// Whether this is a debug world.
    pub is_debug: bool,
    /// Whether this is a superflat world.
    pub is_flat: bool,
    /// Whether a death location follows.
    pub has_death_location: bool,
    /// Namespaced level the player last died in.
    #[mc(present_if = "has_death_location == true")]
    pub death_dimension: Option<String>,
    /// Block position the player last died at.
    #[mc(present_if = "has_death_location == true")]
    pub death_location: Option<Position>,
    /// Remaining portal cooldown, in ticks.
    #[mc(varint)]
    pub portal_cooldown: i32,
}

/// Clientbound `login` (game-join) packet for this era.
///
/// # What moved out of it
///
/// Almost everything an older join packet carried about the *world* is gone,
/// because 1.20.2 introduced a configuration phase that delivers it before
/// play begins: the dimension codec, the chat-type registry and the biome
/// registry all arrive as `registry_data` while the connection is still
/// configuring. What is left here is the session — an entity id, the world
/// list, the distances — plus a [`SpawnInfo`] naming the dimension by index.
///
/// The layout is checked against a real 766 join: entity id `1`, three world
/// names, max players `20`, view and simulation distance `10`, then a
/// [`SpawnInfo`] with dimension index `0`, and a trailing
/// `enforces_secure_chat` byte that consumes the packet exactly.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login", state = Play, bound = Client, protocols = "766..=766")]
pub struct JoinGame {
    /// Local player entity id.
    pub entity_id: i32,
    /// Whether the world is hardcore.
    pub is_hardcore: bool,
    /// Names of every world on the server (namespaced identifiers).
    pub world_names: Vec<String>,
    /// Legacy max-players hint.
    #[mc(varint)]
    pub max_players: i32,
    /// Server view distance in chunks.
    #[mc(varint)]
    pub view_distance: i32,
    /// Server simulation distance in chunks — how far entities tick.
    #[mc(varint)]
    pub simulation_distance: i32,
    /// Whether reduced debug info is in effect.
    pub reduced_debug_info: bool,
    /// Whether the respawn screen is shown on death.
    pub enable_respawn_screen: bool,
    /// Whether recipe unlocking gates crafting.
    pub do_limited_crafting: bool,
    /// The dimension, level and spawn description — see [`SpawnInfo`].
    pub world_state: SpawnInfo,
    /// Whether the server rejects unsigned chat.
    pub enforces_secure_chat: bool,
}

/// Clientbound `respawn` packet.
///
/// Two fields: the same [`SpawnInfo`] the join packet carries, and a
/// **bitmask** of what to keep across the respawn. The era below carries a
/// single `copy_metadata` boolean; here bit `0x01` keeps attributes and
/// `0x02` keeps metadata, so a boolean read of the byte reports "keep
/// nothing" for a metadata-only respawn.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:respawn", state = Play, bound = Client, protocols = "766..=766")]
pub struct Respawn {
    /// The dimension, level and spawn description respawned into.
    pub world_state: SpawnInfo,
    /// Retention bitmask: `0x01` attributes, `0x02` metadata.
    pub data_kept: u8,
}

/// Clientbound `position` (player position and look) packet.
///
/// Wire layout: f64 x/y/z, f32 yaw/pitch, unsigned-byte relative-coordinate
/// flags, varint teleport id. Bit `0x01` x, `0x02` y, `0x04` z, `0x08` yaw,
/// `0x10` pitch are relative when set.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:position", state = Play, bound = Client, protocols = "766..=766")]
pub struct ClientboundPositionLook {
    /// X coordinate (absolute or relative per `flags`).
    pub x: f64,
    /// Y coordinate (absolute or relative per `flags`).
    pub y: f64,
    /// Z coordinate (absolute or relative per `flags`).
    pub z: f64,
    /// Yaw in degrees (absolute or relative per `flags`).
    pub yaw: f32,
    /// Pitch in degrees (absolute or relative per `flags`).
    pub pitch: f32,
    /// Relative-coordinate bitmask.
    pub flags: i8,
    /// Teleport id the client must echo back in a `teleport_confirm` packet.
    #[mc(varint)]
    pub teleport_id: i32,
}

/// Serverbound `teleport_confirm` — echo the teleport id a clientbound
/// `position` assigned, or the server rubber-bands the player back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:teleport_confirm", state = Play, bound = Server, protocols = "766..=766")]
pub struct TeleportConfirm {
    /// The id from the position packet being confirmed.
    #[mc(varint)]
    pub teleport_id: i32,
}

/// Clientbound `spawn_position` — the client's compass target.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_position", state = Play, bound = Client, protocols = "766..=766")]
pub struct SpawnPosition {
    /// Compass target block position.
    pub location: Position,
    /// Angle the compass points from, in degrees.
    pub angle: f32,
}

/// Clientbound `update_health`.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:update_health", state = Play, bound = Client, protocols = "766..=766")]
pub struct UpdateHealth {
    /// Current health (`0.0`..=`20.0`).
    pub health: f32,
    /// Current food level.
    #[mc(varint)]
    pub food: i32,
    /// Current food saturation.
    pub food_saturation: f32,
}

/// Clientbound `kick_disconnect` sent during play.
///
/// The reason is a **component in anonymous NBT**, where login-state
/// disconnect at this same protocol still sends a JSON string. The two are
/// not interchangeable, and reading the play one as JSON takes a tag byte for
/// a length prefix.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:kick_disconnect", state = Play, bound = Client, protocols = "766..=766")]
pub struct KickDisconnect {
    /// Disconnect reason component.
    pub reason: NetworkNbt,
}

/// Clientbound `abilities` — the server setting the local player's flight and
/// build permissions.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:abilities", state = Play, bound = Client, protocols = "766..=766")]
pub struct ClientboundAbilities {
    /// Bitset: `0x01` invulnerable, `0x02` flying, `0x04` may fly,
    /// `0x08` instant build.
    pub flags: i8,
    /// Flying speed multiplier.
    pub flying_speed: f32,
    /// Field-of-view modifier derived from walking speed.
    pub walking_speed: f32,
}

/// Clientbound `game_state_change` — a one-byte reason plus a float argument.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:game_state_change", state = Play, bound = Client, protocols = "766..=766")]
pub struct GameStateChange {
    /// Reason id; `3` is a game-mode change, whose new mode is `value`.
    pub reason: u8,
    /// Reason-dependent argument.
    pub value: f32,
}

/// Clientbound `start_configuration` — the server pulls a playing connection
/// back into the configuration phase.
///
/// This packet is why the configuration phase is not a login-time detour:
/// from 1.20.2 a server may re-configure a live session (a resource-pack
/// change, a datapack reload), and a client that treats configuration as
/// something it left behind reads the next `registry_data` as a play packet.
/// The client answers with `configuration_acknowledged` and re-enters the
/// phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Packet)]
#[mc(name = "minecraft:start_configuration", state = Play, bound = Client, protocols = "766..=766")]
pub struct StartConfiguration;

impl Encode for StartConfiguration {
    fn encode(&self, _w: &mut Writer, _ctx: Ctx) -> Result<()> {
        Ok(())
    }
}

impl Decode for StartConfiguration {
    fn decode(_r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self)
    }
}

/// Serverbound `configuration_acknowledged` — the reply to
/// [`StartConfiguration`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Packet)]
#[mc(
    name = "minecraft:configuration_acknowledged",
    state = Play,
    bound = Server,
    protocols = "766..=766"
)]
pub struct ConfigurationAcknowledged;

impl Encode for ConfigurationAcknowledged {
    fn encode(&self, _w: &mut Writer, _ctx: Ctx) -> Result<()> {
        Ok(())
    }
}

impl Decode for ConfigurationAcknowledged {
    fn decode(_r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self)
    }
}

/// Clientbound `chunk_batch_start` — opens a batch of columns whose delivery
/// the server wants paced by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Packet)]
#[mc(name = "minecraft:chunk_batch_start", state = Play, bound = Client, protocols = "766..=766")]
pub struct ChunkBatchStart;

impl Encode for ChunkBatchStart {
    fn encode(&self, _w: &mut Writer, _ctx: Ctx) -> Result<()> {
        Ok(())
    }
}

impl Decode for ChunkBatchStart {
    fn decode(_r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self)
    }
}

/// Clientbound `chunk_batch_finished` — closes the batch and says how many
/// columns it held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:chunk_batch_finished",
    state = Play,
    bound = Client,
    protocols = "766..=766"
)]
pub struct ChunkBatchFinished {
    /// Number of columns in the batch just delivered.
    #[mc(varint)]
    pub batch_size: i32,
}

/// Serverbound `chunk_batch_received` — the client's pacing reply.
///
/// The float is a *rate*: columns per tick the client is willing to accept.
/// A server that gets no reply throttles chunk delivery to a trickle, so this
/// is not optional politeness — it is what keeps the world loading.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:chunk_batch_received",
    state = Play,
    bound = Server,
    protocols = "766..=766"
)]
pub struct ChunkBatchReceived {
    /// Desired columns per tick.
    pub chunks_per_tick: f32,
}

/// Serverbound `block_dig` — start, cancel, or finish breaking a block, plus
/// the drop / release / swap-hands status codes.
///
/// Wire layout: varint status, packed position, signed-byte face, varint
/// block-prediction `sequence`. The sequence is load-bearing: omit it and the
/// server reads a VarInt off the next packet's length prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_dig", state = Play, bound = Server, protocols = "766..=766")]
pub struct BlockDig {
    /// Digging status code.
    #[mc(varint)]
    pub status: i32,
    /// Target block position.
    pub location: Position,
    /// Face being mined (`0..=5`).
    pub face: i8,
    /// Block-prediction sequence id the server echoes back.
    #[mc(varint)]
    pub sequence: i32,
}

/// Serverbound `block_place` — place a block or use an item against one.
///
/// No inline item stack: the server resolves the held item from its own
/// inventory view, so placement needs no item registry. Using an item **in
/// the air** is the separate [`UseItem`] packet.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_place", state = Play, bound = Server, protocols = "766..=766")]
pub struct BlockPlace {
    /// Hand used (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
    /// Target block position.
    pub location: Position,
    /// Face being placed against (`0..=5`).
    #[mc(varint)]
    pub direction: i32,
    /// Cursor X within the face (`0.0..=1.0`).
    pub cursor_x: f32,
    /// Cursor Y within the face (`0.0..=1.0`).
    pub cursor_y: f32,
    /// Cursor Z within the face (`0.0..=1.0`).
    pub cursor_z: f32,
    /// Whether the player's head is inside the targeted block.
    pub inside_block: bool,
    /// Block-prediction sequence id — see [`BlockDig`].
    #[mc(varint)]
    pub sequence: i32,
}

/// Serverbound `use_item` — use the held item in the air.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_item", state = Play, bound = Server, protocols = "766..=766")]
pub struct UseItem {
    /// Hand used (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
    /// Block-prediction sequence id — see [`BlockDig`].
    #[mc(varint)]
    pub sequence: i32,
}

/// Serverbound `use_entity` for an **attack** (mouse `1`): no hand, no hit
/// location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_entity", state = Play, bound = Server, protocols = "766..=766")]
pub struct UseEntity {
    /// Target entity id.
    #[mc(varint)]
    pub target: i32,
    /// Interaction kind (always `1`).
    #[mc(varint)]
    pub mouse: i32,
    /// Whether the player was sneaking.
    pub sneaking: bool,
}

/// Serverbound `use_entity` for a plain **interact** (mouse `0`), which
/// carries the hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_entity", state = Play, bound = Server, protocols = "766..=766")]
pub struct UseEntityInteract {
    /// Target entity id.
    #[mc(varint)]
    pub target: i32,
    /// Interaction kind (always `0`).
    #[mc(varint)]
    pub mouse: i32,
    /// Hand used (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
    /// Whether the player was sneaking.
    pub sneaking: bool,
}

/// Serverbound `use_entity` with a precise hit location (mouse `2`).
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_entity", state = Play, bound = Server, protocols = "766..=766")]
pub struct UseEntityAt {
    /// Target entity id.
    #[mc(varint)]
    pub target: i32,
    /// Interaction kind (always `2`).
    #[mc(varint)]
    pub mouse: i32,
    /// Entity-local hit X.
    pub x: f32,
    /// Entity-local hit Y.
    pub y: f32,
    /// Entity-local hit Z.
    pub z: f32,
    /// Hand used (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
    /// Whether the player was sneaking.
    pub sneaking: bool,
}

/// Serverbound `entity_action` — the player-command channel (sneak, sprint,
/// elytra, horse jump).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_action", state = Play, bound = Server, protocols = "766..=766")]
pub struct EntityAction {
    /// The player's own entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Action ordinal.
    #[mc(varint)]
    pub action_id: i32,
    /// Horse-jump charge, `0` for every other action.
    #[mc(varint)]
    pub jump_boost: i32,
}

/// Serverbound `client_command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:client_command", state = Play, bound = Server, protocols = "766..=766")]
pub struct ClientCommand {
    /// Action id (`0` = perform respawn, `1` = request stats).
    #[mc(varint)]
    pub action: i32,
}

/// Serverbound `spectate` — teleport to (or follow) another entity by uuid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spectate", state = Play, bound = Server, protocols = "766..=766")]
pub struct Spectate {
    /// Uuid of the entity to spectate.
    pub target: Uuid,
}

/// Serverbound `arm_animation` — swing a hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:arm_animation", state = Play, bound = Server, protocols = "766..=766")]
pub struct ServerboundArmAnimation {
    /// Hand swung (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
}

/// Serverbound `position` — position without rotation.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:position", state = Play, bound = Server, protocols = "766..=766")]
pub struct ServerboundPosition {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate (feet).
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// Whether the player is standing on ground.
    pub on_ground: bool,
}

/// Serverbound `position_look` — position and rotation together.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:position_look", state = Play, bound = Server, protocols = "766..=766")]
pub struct ServerboundPositionLook {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate (feet).
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the player is standing on ground.
    pub on_ground: bool,
}

/// Serverbound `look` — rotation only.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:look", state = Play, bound = Server, protocols = "766..=766")]
pub struct ServerboundLook {
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the player is standing on ground.
    pub on_ground: bool,
}

/// Serverbound `flying` — the on-ground flag alone, the idle tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:flying", state = Play, bound = Server, protocols = "766..=766")]
pub struct ServerboundFlying {
    /// Whether the player is standing on ground.
    pub on_ground: bool,
}

/// Clientbound `update_time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:update_time", state = Play, bound = Client, protocols = "766..=766")]
pub struct UpdateTime {
    /// Total world age, in ticks.
    pub age: i64,
    /// Current time of day, in ticks.
    pub time: i64,
}

/// Clientbound `difficulty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:difficulty", state = Play, bound = Client, protocols = "766..=766")]
pub struct DifficultyPacket {
    /// Raw difficulty id (`0` peaceful .. `3` hard).
    pub difficulty: u8,
    /// Whether the difficulty is locked from further changes in the UI.
    pub difficulty_locked: bool,
}

/// Clientbound `playerlist_header` — the tab list's header and footer, both
/// components in anonymous NBT rather than the JSON strings the era below
/// sends.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:playerlist_header", state = Play, bound = Client, protocols = "766..=766")]
pub struct PlayerlistHeader {
    /// Header component.
    pub header: NetworkNbt,
    /// Footer component.
    pub footer: NetworkNbt,
}

/// Clientbound `open_sign_entity` — the server opened a sign editor.
///
/// The trailing boolean selects which of the sign's two text faces is being
/// edited; signs became two-sided in 1.20, so the field exists throughout
/// this era.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:open_sign_entity", state = Play, bound = Client, protocols = "766..=766")]
pub struct OpenSignEntity {
    /// Block position of the sign.
    pub location: Position,
    /// Whether the front face is the one being edited.
    pub is_front_text: bool,
}

/// Clientbound `entity_effect` — a status effect was applied.
///
/// Two differences from the era below, both invisible to a decoder that keeps
/// the old shape: the amplifier is a **varint** rather than a signed byte,
/// and the optional trailing factor-data NBT blob is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_effect", state = Play, bound = Client, protocols = "766..=766")]
pub struct EntityEffect {
    /// Target entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Potion-effect id.
    #[mc(varint)]
    pub effect_id: i32,
    /// Effect amplifier (`0` = level I).
    #[mc(varint)]
    pub amplifier: i32,
    /// Remaining duration, in ticks.
    #[mc(varint)]
    pub duration: i32,
    /// Flags byte: bit `0x01` ambient, `0x02` show particles, `0x04` show
    /// icon, `0x08` blend.
    pub flags: u8,
}

/// Clientbound `remove_entity_effect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:remove_entity_effect",
    state = Play,
    bound = Client,
    protocols = "766..=766"
)]
pub struct RemoveEntityEffect {
    /// Target entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Potion-effect id.
    #[mc(varint)]
    pub effect_id: i32,
}

/// Serverbound `recipe_book` — toggle a recipe book's open/filtering state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:recipe_book", state = Play, bound = Server, protocols = "766..=766")]
pub struct RecipeBook {
    /// Recipe-book type ordinal (`0` crafting, `1` furnace, `2` blast
    /// furnace, `3` smoker).
    #[mc(varint)]
    pub book_id: i32,
    /// Whether the book is open.
    pub book_open: bool,
    /// Whether the "only craftable" filter is active.
    pub filter_active: bool,
}
