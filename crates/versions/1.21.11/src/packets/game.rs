//! Play-state packets for this era (protocol 774).

use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};
use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use super::common::NetworkNbt;
use super::position::{GlobalPos, Position};

/// The spawn description the join and respawn packets share.
///
/// # The field that decides how a column is framed
///
/// `dimension` is a **registry index**, not a name. The names arrive earlier,
/// in the configuration phase's `minecraft:dimension_type` registry, and this
/// varint indexes into that registry's own delivery order. Reading it as a
/// string desynchronises immediately; reading it and then ignoring the
/// registry silently frames every column with the wrong section count. See
/// [`ChunkShape::from_dimension_index`](crate::packets::chunk::ChunkShape::from_dimension_index).
///
/// The trailing `sea_level` varint is absent from the 1.20.6 era's version of
/// this block, and it sits *after* the portal cooldown — so a decoder carried
/// forward from there stops one varint short and then reads the join packet's
/// own trailing secure-chat flag as a continuation byte.
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
    /// World-`y` the level treats as sea level.
    #[mc(varint)]
    pub sea_level: i32,
}

/// Clientbound `minecraft:login` (game-join) packet for this era.
///
/// Almost everything an older join packet carried about the *world* arrives
/// before play begins instead: the dimension registry, the chat-type registry
/// and the biome registry all come as `registry_data` while the connection is
/// still configuring. What is left here is the session — an entity id, the
/// world list, the distances — plus a [`SpawnInfo`] naming the dimension by
/// index.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login", state = Play, bound = Client, protocols = "774..=774")]
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

/// Clientbound `minecraft:respawn` packet.
///
/// Two fields: the same [`SpawnInfo`] the join packet carries, and a
/// **bitmask** of what to keep across the respawn — bit `0x01` keeps
/// attributes and `0x02` keeps metadata, so a boolean read of the byte reports
/// "keep nothing" for a metadata-only respawn.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:respawn", state = Play, bound = Client, protocols = "774..=774")]
pub struct Respawn {
    /// The dimension, level and spawn description respawned into.
    pub world_state: SpawnInfo,
    /// Retention bitmask: `0x01` attributes, `0x02` metadata.
    pub data_kept: u8,
}

/// Clientbound `minecraft:player_position` — a teleport that can be absolute
/// or relative per axis, and that carries a velocity as well as a position.
///
/// # This is not the 1.20.6 shape rearranged
///
/// The era below carries `x y z yaw pitch`, a single-byte relative-flag set,
/// and the teleport id **last**. Here the teleport id comes **first**, a
/// `(dx, dy, dz)` velocity sits between the position and the rotation, and the
/// flag set is a 32-bit word with nine assigned bits. Nothing about the two
/// layouts overlaps beyond the first eight bytes, so a stale decoder reads a
/// teleport id as the high half of an `x` coordinate and puts the player
/// somewhere astronomically far away.
///
/// Bit assignment, low bit first: `0x001` x, `0x002` y, `0x004` z, `0x008`
/// yaw, `0x010` pitch, `0x020` dx, `0x040` dy, `0x080` dz, `0x100` rotate the
/// velocity by the yaw delta. A set bit means that component is *relative* to
/// the player's current value.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_position", state = Play, bound = Client, protocols = "774..=774")]
pub struct ClientboundPlayerPosition {
    /// Teleport id the client must echo back in an accept-teleportation
    /// packet.
    #[mc(varint)]
    pub teleport_id: i32,
    /// X coordinate (absolute or relative per `flags`).
    pub x: f64,
    /// Y coordinate (absolute or relative per `flags`).
    pub y: f64,
    /// Z coordinate (absolute or relative per `flags`).
    pub z: f64,
    /// X velocity (absolute or relative per `flags`).
    pub dx: f64,
    /// Y velocity (absolute or relative per `flags`).
    pub dy: f64,
    /// Z velocity (absolute or relative per `flags`).
    pub dz: f64,
    /// Yaw in degrees (absolute or relative per `flags`).
    pub yaw: f32,
    /// Pitch in degrees (absolute or relative per `flags`).
    pub pitch: f32,
    /// Relative-component bitmask — see the type docs for the bit meanings.
    pub flags: i32,
}

/// Clientbound `minecraft:player_rotation` — a rotation-only correction, with
/// no counterpart in the 1.20.6 era.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_rotation", state = Play, bound = Client, protocols = "774..=774")]
pub struct ClientboundPlayerRotation {
    /// Yaw in degrees.
    pub yaw: f32,
    /// Whether `yaw` is relative to the player's current yaw.
    pub relative_yaw: bool,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether `pitch` is relative to the player's current pitch.
    pub relative_pitch: bool,
}

/// Serverbound `minecraft:accept_teleportation` — echo the teleport id a
/// clientbound position packet assigned, or the server rubber-bands the player
/// back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:accept_teleportation", state = Play, bound = Server, protocols = "774..=774")]
pub struct AcceptTeleportation {
    /// The id from the position packet being confirmed.
    #[mc(varint)]
    pub teleport_id: i32,
}

/// Clientbound `minecraft:set_default_spawn_position` — the client's compass
/// target.
///
/// It names the **level** as well as the position, where the 1.20.6 era's
/// version carries a bare position, and it carries a pitch as well as a yaw.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_default_spawn_position", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetDefaultSpawnPosition {
    /// Compass target, with the level it is in.
    pub location: GlobalPos,
    /// Yaw the compass points from, in degrees.
    pub yaw: f32,
    /// Pitch the compass points from, in degrees.
    pub pitch: f32,
}

/// Clientbound `minecraft:set_health`.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_health", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetHealth {
    /// Current health (`0.0`..=`20.0`).
    pub health: f32,
    /// Current food level.
    #[mc(varint)]
    pub food: i32,
    /// Current food saturation.
    pub food_saturation: f32,
}

/// Clientbound `minecraft:set_experience`.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_experience", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetExperience {
    /// Progress through the current level, `0.0`..=`1.0`.
    pub experience_bar: f32,
    /// Current experience level.
    #[mc(varint)]
    pub level: i32,
    /// Lifetime experience total.
    #[mc(varint)]
    pub total_experience: i32,
}

/// Clientbound `minecraft:disconnect` sent during play.
///
/// The reason is a **component in anonymous NBT**, where login-state
/// disconnect at this same protocol sends a JSON string. The two are not
/// interchangeable, and reading the play one as JSON takes a tag byte for a
/// length prefix.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:disconnect", state = Play, bound = Client, protocols = "774..=774")]
pub struct PlayDisconnect {
    /// Disconnect reason component.
    pub reason: NetworkNbt,
}

/// Clientbound `minecraft:player_abilities` — the server setting the local
/// player's flight and build permissions.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_abilities", state = Play, bound = Client, protocols = "774..=774")]
pub struct ClientboundAbilities {
    /// Bitset: `0x01` invulnerable, `0x02` flying, `0x04` may fly, `0x08`
    /// instant build.
    pub flags: i8,
    /// Flying speed multiplier.
    pub flying_speed: f32,
    /// Field-of-view modifier derived from walking speed.
    pub walking_speed: f32,
}

/// Clientbound `minecraft:game_event` — a one-byte reason plus a float
/// argument.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:game_event", state = Play, bound = Client, protocols = "774..=774")]
pub struct GameEvent {
    /// Reason id; `3` is a game-mode change, whose new mode is `value`.
    pub reason: u8,
    /// Reason-dependent argument.
    pub value: f32,
}

/// Clientbound `minecraft:start_configuration` — the server pulls a playing
/// connection back into the configuration phase.
///
/// A server may re-configure a live session (a resource-pack change, a
/// datapack reload), and a client that treats configuration as something it
/// left behind reads the next `registry_data` as a play packet. The client
/// answers with `configuration_acknowledged` and re-enters the phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Packet)]
#[mc(name = "minecraft:start_configuration", state = Play, bound = Client, protocols = "774..=774")]
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

/// Serverbound `minecraft:configuration_acknowledged` — the reply to
/// [`StartConfiguration`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Packet)]
#[mc(
    name = "minecraft:configuration_acknowledged",
    state = Play,
    bound = Server,
    protocols = "774..=774"
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

/// Clientbound `minecraft:chunk_batch_start` — opens a batch of columns whose
/// delivery the server wants paced by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Packet)]
#[mc(name = "minecraft:chunk_batch_start", state = Play, bound = Client, protocols = "774..=774")]
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

/// Clientbound `minecraft:chunk_batch_finished` — closes the batch and says
/// how many columns it held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:chunk_batch_finished",
    state = Play,
    bound = Client,
    protocols = "774..=774"
)]
pub struct ChunkBatchFinished {
    /// Number of columns in the batch just delivered.
    #[mc(varint)]
    pub batch_size: i32,
}

/// Serverbound `minecraft:chunk_batch_received` — the client's pacing reply.
///
/// The float is a *rate*: columns per tick the client is willing to accept. A
/// server that gets no reply throttles chunk delivery to a trickle, so this is
/// not optional politeness — it is what keeps the world loading.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:chunk_batch_received",
    state = Play,
    bound = Server,
    protocols = "774..=774"
)]
pub struct ChunkBatchReceived {
    /// Desired columns per tick.
    pub chunks_per_tick: f32,
}

/// Serverbound `minecraft:player_action` — start, cancel, or finish breaking a
/// block, plus the drop / release / swap-hands status codes.
///
/// Wire layout: varint status, packed position, signed-byte face, varint
/// block-prediction `sequence`. The sequence is load-bearing: omit it and the
/// server reads a VarInt off the next packet's length prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_action", state = Play, bound = Server, protocols = "774..=774")]
pub struct PlayerAction {
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

/// Serverbound `minecraft:use_item_on` — place a block or use an item against
/// one.
///
/// No inline item stack: the server resolves the held item from its own
/// inventory view. Using an item **in the air** is the separate [`UseItem`]
/// packet.
///
/// The `world_border_hit` flag between `inside_block` and `sequence` is absent
/// from the 1.20.6 era's version, so a decoder carried forward from there
/// reads the flag byte as the sequence varint and the sequence as a trailing
/// byte the server rejects.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_item_on", state = Play, bound = Server, protocols = "774..=774")]
pub struct UseItemOn {
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
    /// Whether the interaction ray crossed the world border.
    pub world_border_hit: bool,
    /// Block-prediction sequence id — see [`PlayerAction`].
    #[mc(varint)]
    pub sequence: i32,
}

/// Serverbound `minecraft:use_item` — use the held item in the air.
///
/// It carries the player's own look direction, which the 1.20.6 era's version
/// does not.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_item", state = Play, bound = Server, protocols = "774..=774")]
pub struct UseItem {
    /// Hand used (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
    /// Block-prediction sequence id — see [`PlayerAction`].
    #[mc(varint)]
    pub sequence: i32,
    /// Yaw in degrees at the moment of use.
    pub yaw: f32,
    /// Pitch in degrees at the moment of use.
    pub pitch: f32,
}

/// Serverbound `minecraft:interact` for an **attack** (mouse `1`): no hand, no
/// hit location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:interact", state = Play, bound = Server, protocols = "774..=774")]
pub struct Interact {
    /// Target entity id.
    #[mc(varint)]
    pub target: i32,
    /// Interaction kind (always `1`).
    #[mc(varint)]
    pub mouse: i32,
    /// Whether the player was sneaking.
    pub sneaking: bool,
}

/// Serverbound `minecraft:interact` for a plain interact (mouse `0`), which
/// carries the hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:interact", state = Play, bound = Server, protocols = "774..=774")]
pub struct InteractHand {
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

/// Serverbound `minecraft:interact` with a precise hit location (mouse `2`).
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:interact", state = Play, bound = Server, protocols = "774..=774")]
pub struct InteractAt {
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

/// Serverbound `minecraft:player_command` — the player-command channel
/// (sneak, sprint, elytra, horse jump).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_command", state = Play, bound = Server, protocols = "774..=774")]
pub struct PlayerCommandPacket {
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

/// Serverbound `minecraft:client_command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:client_command", state = Play, bound = Server, protocols = "774..=774")]
pub struct ClientCommand {
    /// Action id (`0` = perform respawn, `1` = request stats).
    #[mc(varint)]
    pub action: i32,
}

/// Serverbound `minecraft:teleport_to_entity` — teleport to (or follow)
/// another entity by uuid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:teleport_to_entity", state = Play, bound = Server, protocols = "774..=774")]
pub struct TeleportToEntity {
    /// Uuid of the entity to teleport to.
    pub target: Uuid,
}

/// Serverbound `minecraft:swing` — swing a hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:swing", state = Play, bound = Server, protocols = "774..=774")]
pub struct Swing {
    /// Hand swung (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
}

/// The movement-status flag set every serverbound movement packet ends with.
///
/// The 1.20.6 era ends each movement packet with a bare `on_ground` boolean.
/// Here it is a flag byte whose bit `0x01` is on-ground and bit `0x02` says
/// the client hit a wall this tick. The two agree byte-for-byte while the
/// client is not colliding, which is why a stale encoder passes every idle test
/// and reports the wrong thing exactly when the server is deciding whether to
/// trust a movement.
pub mod movement_flags {
    /// The player is standing on ground.
    pub const ON_GROUND: u8 = 0x01;
    /// The player collided horizontally during the tick being reported.
    pub const HORIZONTAL_COLLISION: u8 = 0x02;

    /// Packs the two booleans into the wire flag byte.
    #[must_use]
    pub const fn pack(on_ground: bool, horizontal_collision: bool) -> u8 {
        (if on_ground { ON_GROUND } else { 0 })
            | (if horizontal_collision {
                HORIZONTAL_COLLISION
            } else {
                0
            })
    }
}

/// Serverbound `minecraft:move_player_pos` — position without rotation.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_player_pos", state = Play, bound = Server, protocols = "774..=774")]
pub struct MovePlayerPos {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate (feet).
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// Movement-status flags — see [`movement_flags`].
    pub flags: u8,
}

/// Serverbound `minecraft:move_player_pos_rot` — position and rotation
/// together.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_player_pos_rot", state = Play, bound = Server, protocols = "774..=774")]
pub struct MovePlayerPosRot {
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
    /// Movement-status flags — see [`movement_flags`].
    pub flags: u8,
}

/// Serverbound `minecraft:move_player_rot` — rotation only.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_player_rot", state = Play, bound = Server, protocols = "774..=774")]
pub struct MovePlayerRot {
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Movement-status flags — see [`movement_flags`].
    pub flags: u8,
}

/// Serverbound `minecraft:move_player_status_only` — the flag byte alone, the
/// idle tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_player_status_only", state = Play, bound = Server, protocols = "774..=774")]
pub struct MovePlayerStatusOnly {
    /// Movement-status flags — see [`movement_flags`].
    pub flags: u8,
}

/// Serverbound `minecraft:player_loaded` — the client reporting that it has
/// finished loading in. Empty body, and absent from the 1.20.6 era entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Packet)]
#[mc(name = "minecraft:player_loaded", state = Play, bound = Server, protocols = "774..=774")]
pub struct PlayerLoaded;

impl Encode for PlayerLoaded {
    fn encode(&self, _w: &mut Writer, _ctx: Ctx) -> Result<()> {
        Ok(())
    }
}

impl Decode for PlayerLoaded {
    fn decode(_r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self)
    }
}

/// Serverbound `minecraft:client_tick_end` — closes the client's own tick.
/// Empty body, and absent from the 1.20.6 era entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Packet)]
#[mc(name = "minecraft:client_tick_end", state = Play, bound = Server, protocols = "774..=774")]
pub struct ClientTickEnd;

impl Encode for ClientTickEnd {
    fn encode(&self, _w: &mut Writer, _ctx: Ctx) -> Result<()> {
        Ok(())
    }
}

impl Decode for ClientTickEnd {
    fn decode(_r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self)
    }
}

/// Clientbound `minecraft:set_time`.
///
/// The trailing flag says whether the day-time counter advances; the 1.20.6
/// era's version stops after the two longs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_time", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetTime {
    /// Total world age, in ticks.
    pub age: i64,
    /// Current time of day, in ticks.
    pub time: i64,
    /// Whether the day-time counter is advancing.
    pub tick_day_time: bool,
}

/// Clientbound `minecraft:change_difficulty`.
///
/// The difficulty is a **varint** here and an unsigned byte in the 1.20.6 era.
/// The two agree for every value a server sends (`0..=3`) and disagree for a
/// malformed one, so this is a robustness difference rather than a live one —
/// stated because the shape measurement flags it and a reader should not have
/// to re-derive why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:change_difficulty", state = Play, bound = Client, protocols = "774..=774")]
pub struct ChangeDifficulty {
    /// Raw difficulty id (`0` peaceful .. `3` hard).
    #[mc(varint)]
    pub difficulty: i32,
    /// Whether the difficulty is locked from further changes in the UI.
    pub difficulty_locked: bool,
}

/// Clientbound `minecraft:tab_list` — the tab list's header and footer, both
/// components in anonymous NBT.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:tab_list", state = Play, bound = Client, protocols = "774..=774")]
pub struct TabList {
    /// Header component.
    pub header: NetworkNbt,
    /// Footer component.
    pub footer: NetworkNbt,
}

/// Clientbound `minecraft:open_sign_editor` — the server opened a sign editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:open_sign_editor", state = Play, bound = Client, protocols = "774..=774")]
pub struct OpenSignEditor {
    /// Block position of the sign.
    pub location: Position,
    /// Whether the front face is the one being edited.
    pub is_front_text: bool,
}

/// Clientbound `minecraft:update_mob_effect` — a status effect was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:update_mob_effect", state = Play, bound = Client, protocols = "774..=774")]
pub struct UpdateMobEffect {
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

/// Clientbound `minecraft:remove_mob_effect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:remove_mob_effect",
    state = Play,
    bound = Client,
    protocols = "774..=774"
)]
pub struct RemoveMobEffect {
    /// Target entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Potion-effect id.
    #[mc(varint)]
    pub effect_id: i32,
}

/// Clientbound `minecraft:block_update` — one block state changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_update", state = Play, bound = Client, protocols = "774..=774")]
pub struct BlockUpdate {
    /// The block's position.
    pub location: Position,
    /// The new flat wire state id, in this era's own numbering.
    #[mc(varint)]
    pub state_id: i32,
}

/// Clientbound `minecraft:move_vehicle` — the server correcting the position
/// of the vehicle the player is riding.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_vehicle", state = Play, bound = Client, protocols = "774..=774")]
pub struct MoveVehicle {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
}

/// Clientbound `minecraft:set_camera` — the entity the client renders from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_camera", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetCamera {
    /// Entity id to view from; the player's own id restores the normal view.
    #[mc(varint)]
    pub camera_id: i32,
}

/// Clientbound `minecraft:set_chunk_cache_center` — the column the server
/// centres the client's view on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_chunk_cache_center", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetChunkCacheCenter {
    /// Centre column x, in chunks.
    #[mc(varint)]
    pub chunk_x: i32,
    /// Centre column z, in chunks.
    #[mc(varint)]
    pub chunk_z: i32,
}

/// Clientbound `minecraft:set_chunk_cache_radius` — the server's view
/// distance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_chunk_cache_radius", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetChunkCacheRadius {
    /// View distance in chunks.
    #[mc(varint)]
    pub view_distance: i32,
}

/// Clientbound `minecraft:set_simulation_distance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_simulation_distance", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetSimulationDistance {
    /// Simulation distance in chunks.
    #[mc(varint)]
    pub distance: i32,
}

/// Clientbound `minecraft:set_titles_animation` — fade-in, hold and fade-out
/// times, in ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_titles_animation", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetTitlesAnimation {
    /// Fade-in duration, in ticks.
    pub fade_in: i32,
    /// Hold duration, in ticks.
    pub stay: i32,
    /// Fade-out duration, in ticks.
    pub fade_out: i32,
}

/// Clientbound `minecraft:clear_titles`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:clear_titles", state = Play, bound = Client, protocols = "774..=774")]
pub struct ClearTitles {
    /// Whether to reset the fade timings as well as clearing the text.
    pub reset: bool,
}

/// Clientbound `minecraft:ticking_state` — the server's tick rate and freeze
/// flag.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:ticking_state", state = Play, bound = Client, protocols = "774..=774")]
pub struct TickingState {
    /// Ticks per second the server intends to run at.
    pub tick_rate: f32,
    /// Whether the server's tick loop is frozen.
    pub is_frozen: bool,
}

/// Clientbound `minecraft:ticking_step` — a frozen server stepping forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:ticking_step", state = Play, bound = Client, protocols = "774..=774")]
pub struct TickingStep {
    /// Number of ticks stepped.
    #[mc(varint)]
    pub tick_steps: i32,
}

/// Serverbound `minecraft:recipe_book_change_settings` — toggle a recipe
/// book's open/filtering state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:recipe_book_change_settings", state = Play, bound = Server, protocols = "774..=774")]
pub struct RecipeBookChangeSettings {
    /// Recipe-book type ordinal (`0` crafting, `1` furnace, `2` blast
    /// furnace, `3` smoker).
    #[mc(varint)]
    pub book_id: i32,
    /// Whether the book is open.
    pub book_open: bool,
    /// Whether the "only craftable" filter is active.
    pub filter_active: bool,
}

/// Serverbound `minecraft:player_input` — the movement keys held this tick, as
/// a bitfield.
///
/// The server drives vehicles from this rather than from the rider's own
/// position packets, so a client that never sends it cannot steer a boat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_input", state = Play, bound = Server, protocols = "774..=774")]
pub struct PlayerInputPacket {
    /// Bitfield — see [`player_input_flags`].
    pub inputs: u8,
}

/// Bit assignments for [`PlayerInputPacket`]'s single byte.
pub mod player_input_flags {
    /// Forward key held.
    pub const FORWARD: u8 = 0x01;
    /// Backward key held.
    pub const BACKWARD: u8 = 0x02;
    /// Left strafe key held.
    pub const LEFT: u8 = 0x04;
    /// Right strafe key held.
    pub const RIGHT: u8 = 0x08;
    /// Jump key held.
    pub const JUMP: u8 = 0x10;
    /// Sneak key held.
    pub const SHIFT: u8 = 0x20;
    /// Sprint key held.
    pub const SPRINT: u8 = 0x40;
}

/// Clientbound `minecraft:section_blocks_update` — many block changes inside a
/// single 16×16×16 section, in one packet.
///
/// The section is a packed long: 22 bits of section x, 22 of section z, 20 of
/// section y, signed, most-significant first. Each record is a varint (or
/// longer) whose low 12 bits are the section-relative position packed
/// `x << 8 | z << 4 | y` and whose remaining high bits are the block state.
/// Both packings are little-endian in *bit* order rather than byte order,
/// which is why they are decoded arithmetically here instead of through the
/// derive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionBlocksUpdate {
    /// Section x, in sections.
    pub section_x: i32,
    /// Section y, in sections (may be negative).
    pub section_y: i32,
    /// Section z, in sections.
    pub section_z: i32,
    /// Section-relative `(x, y, z)` and the new state id, per changed block.
    pub blocks: Vec<([u8; 3], i32)>,
}

impl Decode for SectionBlocksUpdate {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let packed = r.i64()?;
        // Sign-extend each field out of its own bit width by shifting it to
        // the top of an i64 and back down.
        let section_x = ((packed >> 42) << 42 >> 42) as i32;
        let section_y = ((packed << 44) >> 44) as i32;
        let section_z = ((packed << 22) >> 42) as i32;
        let count = r.var_i32()?;
        if count < 0 {
            return Err(lodestone_core::Error::NegativeLength(count));
        }
        let mut blocks = Vec::with_capacity((count as usize).min(r.remaining()));
        for _ in 0..count {
            let record = r.var_i64()?;
            let local = (record & 0xfff) as u16;
            let state = (record >> 12) as i32;
            blocks.push((
                [
                    ((local >> 8) & 0xf) as u8,
                    (local & 0xf) as u8,
                    ((local >> 4) & 0xf) as u8,
                ],
                state,
            ));
        }
        Ok(Self {
            section_x,
            section_y,
            section_z,
            blocks,
        })
    }
}
