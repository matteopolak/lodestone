//! Play-state packets for protocol 776.

use lodestone_macros::{Decode, Encode, Packet};

/// Clientbound `login` (game-join) packet.
///
/// Only the prefix needed to surface a canonical login event is modelled; the
/// trailing fields (previous game type, debug/flat flags, last death location,
/// portal cooldown, sea level, and the secure-chat booleans) are swallowed by
/// the final [`rest`](GameLogin::rest) field since they are not needed yet.
///
/// Modelled wire layout: signed int entity id, boolean hardcore, a
/// varint-prefixed list of dimension names, varint max players, varint view
/// distance, varint simulation distance, boolean reduced debug info, boolean
/// show death screen, boolean limited crafting, then the spawn info prefix of
/// varint dimension-type holder id, string dimension name, big-endian 64-bit
/// hashed seed, and unsigned byte game type.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login", state = Play, bound = Client)]
pub struct GameLogin {
    /// Local player entity id.
    pub entity_id: i32,
    /// Whether the world is hardcore.
    pub hardcore: bool,
    /// Dimension names available on the server.
    pub levels: Vec<String>,
    /// Maximum player count (legacy, unused by the client).
    #[mc(varint)]
    pub max_players: i32,
    /// Server view distance in chunks.
    #[mc(varint)]
    pub view_distance: i32,
    /// Server simulation distance in chunks.
    #[mc(varint)]
    pub simulation_distance: i32,
    /// Whether reduced debug info is in effect.
    pub reduced_debug_info: bool,
    /// Whether the death screen is shown on death.
    pub show_death_screen: bool,
    /// Whether recipe-limited crafting is enabled.
    pub do_limited_crafting: bool,
    /// Registry holder id of the current dimension type.
    #[mc(varint)]
    pub dimension_type: i32,
    /// Identifier of the current dimension, such as `minecraft:overworld`.
    pub dimension: String,
    /// Low 64 bits of the hashed world seed.
    pub seed: i64,
    /// Game mode (`0` survival, `1` creative, `2` adventure, `3` spectator).
    pub game_type: u8,
    /// Remaining spawn-info bytes that are not modelled yet.
    #[mc(remaining)]
    pub rest: Vec<u8>,
}

/// A 256-byte Ed25519-style chat message signature.
///
/// Only ever encoded as absent (`None`) by phase 1, but modelled faithfully so
/// the [`ChatMessage`] body is fully derive-generated.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct MessageSignature(#[mc(fixed = 256)] pub [u8; 256]);

/// Serverbound `chat` packet.
///
/// Wire layout: string message (max 256 chars), big-endian 64-bit timestamp
/// (epoch millis), big-endian 64-bit salt, an optional message signature
/// (boolean presence flag then a 256-byte signature when present), then the
/// last-seen acknowledgement update: a varint offset, a fixed 3-byte (20-bit)
/// acknowledged bit set with no length prefix, and a signed checksum byte
/// (`0` means "ignore checksum").
///
/// Phase 1 sends unsigned chat: zero timestamp and salt, no signature, and an
/// empty acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat", state = Play, bound = Server)]
pub struct ChatMessage {
    /// Message text.
    #[mc(max = 256)]
    pub message: String,
    /// Client timestamp in epoch milliseconds.
    pub timestamp: i64,
    /// Random salt used for signing.
    pub salt: i64,
    /// Optional message signature (absent for unsigned chat).
    pub signature: Option<MessageSignature>,
    /// Offset of the last-seen acknowledgement window.
    #[mc(varint)]
    pub last_seen_offset: i32,
    /// Fixed 20-bit acknowledged bit set, packed into 3 bytes.
    #[mc(fixed = 3)]
    pub acknowledged: [u8; 3],
    /// Acknowledgement checksum (`0` to ignore).
    pub checksum: i8,
}

impl ChatMessage {
    /// Builds an unsigned chat message with an empty acknowledgement window.
    #[must_use]
    pub fn unsigned(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            timestamp: 0,
            salt: 0,
            signature: None,
            last_seen_offset: 0,
            acknowledged: [0; 3],
            checksum: 0,
        }
    }
}

/// Serverbound `chat_command` packet.
///
/// Wire layout: a single string carrying the command without its leading slash.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat_command", state = Play, bound = Server)]
pub struct ChatCommand {
    /// Command text without the leading `/`.
    pub command: String,
}

/// Serverbound `chat_ack` packet.
///
/// Wire layout: a single VarInt `offset` acknowledging that many additional
/// pending signed messages. The server decrements its unacknowledged count by
/// this offset; without it the pending list grows until the 4096-message cap
/// forces a disconnect, so this is the standalone drain the client sends when it
/// has seen messages but is not sending chat of its own.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat_ack", state = Play, bound = Server)]
pub struct ChatAck {
    /// Number of newly-acknowledged pending signed messages.
    #[mc(varint)]
    pub offset: i32,
}

/// Clientbound `set_health` packet.
///
/// Wire layout: big-endian `f32` health, a VarInt food level, and a big-endian
/// `f32` saturation. A health of `0.0` means the player has died and the server
/// will hold them on the death screen (streaming no chunks) until it receives a
/// respawn request.
#[derive(Debug, Clone, PartialEq, Decode, Encode, Packet)]
#[mc(name = "minecraft:set_health", state = Play, bound = Client)]
pub struct SetHealth {
    /// Current health (`0.0` .. `20.0`); `0.0` means dead.
    pub health: f32,
    /// Current food level (`0` .. `20`).
    #[mc(varint)]
    pub food: i32,
    /// Current food saturation.
    pub saturation: f32,
}

/// Clientbound `initialize_border` packet (wire id 43 in 26.2) — the world
/// border's full state, sent on join by `PlayerList.sendLevelInfo` before the
/// time sync and spawn-position packets.
///
/// Wire layout mirrors `ClientboundInitializeBorderPacket.write`'s field order
/// exactly: two big-endian `f64` centre coordinates, `old_size` and `new_size`
/// f64s, a VarLong lerp time, then three VarInts for `absolute_max_size`,
/// `warning_blocks` and `warning_time`.
///
/// **One deliberate divergence: the lerp time is in milliseconds, not vanilla's
/// ticks.** Vanilla writes `border.getLerpTime()` — the extent's remaining
/// *server ticks* — directly (`ClientboundInitializeBorderPacket.java`),
/// and the vanilla client re-runs that count as its own tick count. This
/// crate's client instead decodes the field as `lerp_time_ms` and interpolates
/// on
/// wall-clock (`lodestone-game::worldborder`'s `BorderExtent::Moving`), so the
/// server encoder converts ticks → ms (`* 50`) before writing. A vanilla client
/// served by this crate would lerp 50× too fast; a vanilla *server* joining
/// this crate's client already shows the same mismatch on this field.
///
/// `old_size`/`new_size` are the extent's `getSize()` and `getLerpTarget()`
/// (`ClientboundInitializeBorderPacket.java`) — for a static border they
/// are equal and the lerp time is 0.
#[derive(Debug, Clone, PartialEq, Decode, Encode, Packet)]
#[mc(name = "minecraft:initialize_border", state = Play, bound = Client)]
pub struct InitializeBorder {
    /// Border centre X.
    pub center_x: f64,
    /// Border centre Z.
    pub center_z: f64,
    /// Size at the start of the current lerp (`getSize()`).
    pub old_size: f64,
    /// Size at the end of the current lerp (`getLerpTarget()`).
    pub new_size: f64,
    /// Remaining lerp time in **milliseconds** (0 for a static border) — see
    /// the packet doc for why this crate diverges from vanilla's ticks.
    #[mc(varlong)]
    pub lerp_time: i64,
    /// The border's absolute maximum size (`WorldBorder.getAbsoluteMaxSize`).
    #[mc(varint)]
    pub absolute_max_size: i32,
    /// Warning distance in blocks.
    #[mc(varint)]
    pub warning_blocks: i32,
    /// Warning delay in seconds.
    #[mc(varint)]
    pub warning_time: i32,
}

/// Clientbound `set_border_center` packet (wire id 88 in 26.2) — moves the
/// border without changing its size.
///
/// Wire layout: two big-endian `f64` coordinates
/// (`ClientboundSetBorderCenterPacket`).
#[derive(Debug, Clone, PartialEq, Decode, Encode, Packet)]
#[mc(name = "minecraft:set_border_center", state = Play, bound = Client)]
pub struct SetBorderCenter {
    /// New border centre X.
    pub center_x: f64,
    /// New border centre Z.
    pub center_z: f64,
}

/// Clientbound `set_border_lerp_size` packet (wire id 89 in 26.2) — the live
/// resize delta a border shrink/grow broadcasts.
///
/// Wire layout: `old_size` and `new_size` big-endian f64s, then a VarLong
/// lerp time in **milliseconds** — the same deliberate divergence from the
/// vanilla wire as [`InitializeBorder`]'s lerp time (see that packet's doc).
/// Vanilla writes `border.getLerpTime()` — remaining *server ticks* — directly
/// in both packets (`ClientboundSetBorderLerpSizePacket.java`, no ×50); this
/// crate's client decodes the field as `lerp_time_ms` and interpolates on
/// wall-clock, so the server converts ticks → ms before it reaches this
/// encoder, which writes the ms value verbatim. A vanilla client served by
/// this crate would lerp 50× too fast.
#[derive(Debug, Clone, PartialEq, Decode, Encode, Packet)]
#[mc(name = "minecraft:set_border_lerp_size", state = Play, bound = Client)]
pub struct SetBorderLerpSize {
    /// Size at the start of the lerp.
    pub old_size: f64,
    /// Size at the end of the lerp.
    pub new_size: f64,
    /// Lerp duration in milliseconds.
    #[mc(varlong)]
    pub lerp_time_ms: i64,
}

/// Clientbound `set_border_size` packet (wire id 90 in 26.2) — the instant
/// snap a `WorldBorder.setSize` broadcasts.
///
/// Wire layout: a single big-endian `f64` size
/// (`ClientboundSetBorderSizePacket`).
#[derive(Debug, Clone, PartialEq, Decode, Encode, Packet)]
#[mc(name = "minecraft:set_border_size", state = Play, bound = Client)]
pub struct SetBorderSize {
    /// New border size.
    pub size: f64,
}

/// Clientbound `set_border_warning_delay` packet (wire id 91 in 26.2) — the
/// warning *time* delta.
///
/// Wire layout: a single VarInt seconds value
/// (`ClientboundSetBorderWarningDelayPacket`).
#[derive(Debug, Clone, PartialEq, Decode, Encode, Packet)]
#[mc(name = "minecraft:set_border_warning_delay", state = Play, bound = Client)]
pub struct SetBorderWarningDelay {
    /// Warning delay in seconds.
    #[mc(varint)]
    pub warning_time: i32,
}

/// Clientbound `set_border_warning_distance` packet (wire id 92 in 26.2) —
/// the warning *blocks* delta.
///
/// Wire layout: a single VarInt blocks value
/// (`ClientboundSetBorderWarningDistancePacket`).
#[derive(Debug, Clone, PartialEq, Decode, Encode, Packet)]
#[mc(name = "minecraft:set_border_warning_distance", state = Play, bound = Client)]
pub struct SetBorderWarningDistance {
    /// Warning distance in blocks.
    #[mc(varint)]
    pub warning_blocks: i32,
}

/// A dimension-qualified block position (`GlobalPos`).
///
/// Wire layout: an identifier string naming the dimension, then a single
/// big-endian 64-bit packed block position (`x` in the high 26 bits, `z` in the
/// middle 26 bits, `y` in the low 12 bits — the vanilla `BlockPos.asLong`
/// packing). Only decoded as part of the optional last-death-location field of
/// the [`Respawn`] spawn info; the packed position is kept verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Decode, Encode)]
pub struct GlobalPos {
    /// Identifier of the dimension the position belongs to.
    pub dimension: String,
    /// Packed block position (`BlockPos.asLong`).
    pub position: i64,
}

/// Clientbound `respawn` packet.
///
/// Sent when the player changes dimension (portal travel) or respawns after
/// death. It carries the same `CommonPlayerSpawnInfo` prefix as the game-join
/// `login` packet followed by a single `data_to_keep` bit mask byte.
///
/// The adapter decodes this in full (rather than swallowing the tail with
/// `remaining`) so the trailing zero-length check can catch a misaligned parse,
/// and uses [`dimension`](Respawn::dimension) to update the per-connection
/// chunk shape: the new dimension's build-height window governs how subsequent
/// `level_chunk_with_light` packets frame their section data.
///
/// Wire layout: VarInt dimension-type holder id, identifier dimension name,
/// big-endian 64-bit hashed seed, unsigned byte game type, signed byte previous
/// game type (`-1` for none), boolean is-debug, boolean is-flat, an optional
/// [`GlobalPos`] last death location (boolean presence flag then the value),
/// VarInt portal cooldown, VarInt sea level, and finally an unsigned byte of
/// data-to-keep flags.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:respawn", state = Play, bound = Client)]
pub struct Respawn {
    /// Registry holder id of the new dimension type.
    #[mc(varint)]
    pub dimension_type: i32,
    /// Identifier of the new dimension, such as `minecraft:the_nether`.
    pub dimension: String,
    /// Low 64 bits of the hashed world seed.
    pub seed: i64,
    /// Game mode (`0` survival, `1` creative, `2` adventure, `3` spectator).
    pub game_type: u8,
    /// Previous game mode, or `-1` when there was none.
    pub previous_game_type: i8,
    /// Whether the new dimension is a debug world.
    pub is_debug: bool,
    /// Whether the new dimension uses the flat-world generator.
    pub is_flat: bool,
    /// Last death location, if the server tracks one.
    pub last_death_location: Option<GlobalPos>,
    /// Remaining portal cooldown in ticks.
    #[mc(varint)]
    pub portal_cooldown: i32,
    /// Sea level of the new dimension.
    #[mc(varint)]
    pub sea_level: i32,
    /// Bit mask of player data to retain across the respawn.
    pub data_to_keep: u8,
}

/// Serverbound `client_command` packet.
///
/// Wire layout: a single VarInt enum ordinal. `0` is `perform_respawn` (leave
/// the death screen), `1` is `request_stats`, `2` is `request_gamerule_values`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:client_command", state = Play, bound = Server)]
pub struct ClientCommand {
    /// Action ordinal; `0` = perform respawn.
    #[mc(varint)]
    pub action: i32,
}

/// Flag bit set when the player is standing on the ground in a movement packet.
pub const MOVE_FLAG_ON_GROUND: u8 = 0x01;
/// Flag bit set when the player collided horizontally in a movement packet.
pub const MOVE_FLAG_HORIZONTAL_COLLISION: u8 = 0x02;

/// Serverbound `move_player_pos_rot` packet.
///
/// Sent when both position and rotation change (vanilla's
/// `LocalPlayer.sendPosition` when position and look are both dirty). This is
/// the packet a player controller drives to actually move in the world.
///
/// Wire layout: big-endian `f64` x, y, z, big-endian `f32` yaw and pitch, then
/// a single flags byte — bit 0 ([`MOVE_FLAG_ON_GROUND`]) and bit 1
/// ([`MOVE_FLAG_HORIZONTAL_COLLISION`]), matching vanilla's `packFlags`.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_player_pos_rot", state = Play, bound = Server)]
pub struct MovePlayerPosRot {
    /// Absolute x position.
    pub x: f64,
    /// Absolute y position (feet).
    pub y: f64,
    /// Absolute z position.
    pub z: f64,
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Movement flags (see [`MOVE_FLAG_ON_GROUND`] /
    /// [`MOVE_FLAG_HORIZONTAL_COLLISION`]).
    pub flags: u8,
}

/// Serverbound `move_player_pos` packet.
///
/// Sent when position moved but rotation did not (vanilla's
/// `LocalPlayer.sendPosition` when only position is dirty).
///
/// Wire layout: big-endian `f64` x, y, z, then a single flags byte — bit 0
/// ([`MOVE_FLAG_ON_GROUND`]) and bit 1 ([`MOVE_FLAG_HORIZONTAL_COLLISION`]),
/// matching vanilla's `packFlags`. No rotation fields at all.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_player_pos", state = Play, bound = Server)]
pub struct MovePlayerPos {
    /// Absolute x position.
    pub x: f64,
    /// Absolute y position (feet).
    pub y: f64,
    /// Absolute z position.
    pub z: f64,
    /// Movement flags (see [`MOVE_FLAG_ON_GROUND`] /
    /// [`MOVE_FLAG_HORIZONTAL_COLLISION`]).
    pub flags: u8,
}

/// Serverbound `move_player_rot` packet.
///
/// Sent when rotation changed but position did not (vanilla's
/// `LocalPlayer.sendPosition` when only look is dirty).
///
/// Wire layout: big-endian `f32` yaw and pitch, then a single flags byte —
/// bit 0 ([`MOVE_FLAG_ON_GROUND`]) and bit 1
/// ([`MOVE_FLAG_HORIZONTAL_COLLISION`]), matching vanilla's `packFlags`. No
/// position fields at all.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_player_rot", state = Play, bound = Server)]
pub struct MovePlayerRot {
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Movement flags (see [`MOVE_FLAG_ON_GROUND`] /
    /// [`MOVE_FLAG_HORIZONTAL_COLLISION`]).
    pub flags: u8,
}

/// Serverbound `move_player_status_only` packet.
///
/// Sent when neither position nor rotation changed enough to be "dirty", but
/// on-ground or horizontal-collision status flipped since the last tick
/// (vanilla's `LocalPlayer.sendPosition` fallback branch). Carries no pose
/// data at all — just the flags byte.
///
/// Wire layout: a single flags byte — bit 0 ([`MOVE_FLAG_ON_GROUND`]) and bit
/// 1 ([`MOVE_FLAG_HORIZONTAL_COLLISION`]), matching vanilla's `packFlags`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_player_status_only", state = Play, bound = Server)]
pub struct MovePlayerStatusOnly {
    /// Movement flags (see [`MOVE_FLAG_ON_GROUND`] /
    /// [`MOVE_FLAG_HORIZONTAL_COLLISION`]).
    pub flags: u8,
}

/// Serverbound `move_vehicle` packet.
///
/// Sent once per tick while riding a vehicle (`ServerboundMoveVehiclePacket`).
/// Unlike player movement, there is no dirty-tracking selection and no
/// horizontal-collision flag: vanilla always sends the full shape and packs
/// only `onGround` as a plain trailing boolean, not a bitset.
///
/// Wire layout: `Vec3.STREAM_CODEC` (big-endian `f64` x, y, z), big-endian
/// `f32` yaw then pitch, then a single boolean byte for `onGround`.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_vehicle", state = Play, bound = Server)]
pub struct MoveVehicle {
    /// Vehicle's absolute x position.
    pub x: f64,
    /// Vehicle's absolute y position.
    pub y: f64,
    /// Vehicle's absolute z position.
    pub z: f64,
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the vehicle is on the ground.
    pub on_ground: bool,
}

/// Serverbound `paddle_boat` packet (boat paddle animation input).
///
/// Wire layout: two plain booleans, `left` then `right`
/// (`ServerboundPaddleBoatPacket`'s `writeBoolean` pair).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:paddle_boat", state = Play, bound = Server)]
pub struct PaddleBoat {
    /// Whether the left paddle is in use.
    pub left: bool,
    /// Whether the right paddle is in use.
    pub right: bool,
}

/// Serverbound `player_loaded` packet with an empty body
/// (`ServerboundPlayerLoadedPacket`, `StreamCodec.unit`).
///
/// Zeroes the server's `clientLoadedTimeoutTimer` early so movement is
/// validated immediately instead of being silently ignored for the first
/// ~60 ticks after join/respawn.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_loaded", state = Play, bound = Server)]
pub struct PlayerLoaded;

/// Serverbound `command_suggestion` packet (tab-completion request).
///
/// Wire layout: a VarInt transaction id, then a VarInt-length-prefixed UTF-8
/// command string (`ServerboundCommandSuggestionPacket`'s `readUtf(32500)` /
/// `writeUtf`).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:command_suggestion", state = Play, bound = Server)]
pub struct CommandSuggestion {
    /// Transaction id echoed back in the server's suggestions response.
    #[mc(varint)]
    pub id: i32,
    /// The command text typed so far, including the leading slash.
    pub command: String,
}

/// Serverbound `swing` packet (arm-swing animation).
///
/// Wire layout: a single VarInt interaction-hand ordinal — `0` main hand, `1`
/// off hand — written as `FriendlyByteBuf.writeEnum` does for `InteractionHand`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:swing", state = Play, bound = Server)]
pub struct Swing {
    /// Interaction-hand ordinal; `0` = main hand, `1` = off hand.
    #[mc(varint)]
    pub hand: i32,
}

/// Serverbound `select_bundle_item` packet.
///
/// Highlights which stack inside a bundle's tooltip preview is selected
/// (`ServerboundSelectBundleItemPacket`). Wire layout: VarInt slot id, then
/// VarInt selected item index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:select_bundle_item", state = Play, bound = Server)]
pub struct SelectBundleItem {
    /// Slot id holding the bundle.
    #[mc(varint)]
    pub slot_id: i32,
    /// Highlighted stack's index within the bundle, or `-1` for none.
    #[mc(varint)]
    pub selected_item_index: i32,
}

/// Serverbound `container_slot_state_changed` packet.
///
/// Toggles a container slot's enabled state, e.g. a crafter's per-slot
/// disable toggle (`ServerboundContainerSlotStateChangedPacket`). Wire
/// layout: VarInt slot id, VarInt container id, then a trailing boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:container_slot_state_changed", state = Play, bound = Server)]
pub struct ContainerSlotStateChanged {
    /// Slot index within the container.
    #[mc(varint)]
    pub slot_id: i32,
    /// Open container id.
    #[mc(varint)]
    pub container_id: i32,
    /// New enabled/disabled state.
    pub new_state: bool,
}

/// Serverbound `recipe_book_change_settings` packet.
///
/// Wire layout: VarInt `RecipeBookType` ordinal (`writeEnum`), then two
/// trailing booleans, open then filtering
/// (`ServerboundRecipeBookChangeSettingsPacket`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:recipe_book_change_settings", state = Play, bound = Server)]
pub struct RecipeBookChangeSettings {
    /// `RecipeBookType` ordinal: 0 crafting, 1 furnace, 2 blast furnace, 3
    /// smoker.
    #[mc(varint)]
    pub book_type: i32,
    /// Whether the book is open.
    pub is_open: bool,
    /// Whether the "only craftable" filter is active.
    pub is_filtering: bool,
}

/// Serverbound `recipe_book_seen_recipe` packet.
///
/// Marks a recipe as seen, clearing its "new" highlight
/// (`ServerboundRecipeBookSeenRecipePacket`). Wire layout: a single VarInt
/// `RecipeDisplayId` index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:recipe_book_seen_recipe", state = Play, bound = Server)]
pub struct RecipeBookSeenRecipe {
    /// The recipe's display index.
    #[mc(varint)]
    pub recipe: i32,
}

/// Serverbound `place_recipe` packet.
///
/// Auto-places a recipe book entry's ingredients into an open crafting
/// container (`ServerboundPlaceRecipePacket`). Wire layout: VarInt container
/// id, VarInt `RecipeDisplayId` index, then a trailing boolean for
/// "use max items".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:place_recipe", state = Play, bound = Server)]
pub struct PlaceRecipe {
    /// Open container id.
    #[mc(varint)]
    pub container_id: i32,
    /// The recipe's display index.
    #[mc(varint)]
    pub recipe: i32,
    /// Whether to place the maximum possible quantity rather than one set of
    /// ingredients.
    pub use_max_items: bool,
}

/// Serverbound `change_game_mode` packet.
///
/// Sent by the singleplayer/LAN cheats-enabled F4 game-mode switcher
/// (`ServerboundChangeGameModePacket`). Wire layout: a single VarInt
/// `GameType` id via `ByteBufCodecs.idMapper` (`0` survival, `1` creative,
/// `2` adventure, `3` spectator) — the server remains authoritative and may
/// ignore this if the requester lacks permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:change_game_mode", state = Play, bound = Server)]
pub struct ChangeGameMode {
    /// `GameType` id.
    #[mc(varint)]
    pub mode: i32,
}

/// Serverbound `accept_teleportation` packet.
///
/// Sent in reply to a clientbound `player_position`, echoing its teleport id.
/// A client that never confirms is repeatedly re-corrected by the server, so
/// this must be emitted for every teleport the client accepts.
///
/// Wire layout: a single VarInt teleport id.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:accept_teleportation", state = Play, bound = Server)]
pub struct AcceptTeleportation {
    /// Teleport id echoed back from the `player_position` packet.
    #[mc(varint)]
    pub id: i32,
}

/// Clientbound `game_event` packet.
///
/// A catch-all for small world-state changes keyed by an event code. Vanilla
/// uses it for start/stop raining (`1`/`2`), a game-mode change (`3`), rain and
/// thunder intensity (`7`/`8`), and several gameplay one-shots the adapter does
/// not surface.
///
/// Wire layout: an unsigned byte event code followed by a big-endian `f32`
/// parameter whose meaning depends on the code.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub struct GameEvent {
    /// Event code (see the vanilla `ClientboundGameEventPacket` type table).
    pub event: u8,
    /// Event parameter; interpretation depends on [`event`](GameEvent::event).
    pub param: f32,
}

/// Clientbound `set_default_spawn_position` packet.
///
/// Sets the world's compass target / default respawn anchor. Reshaped in 1.21.9
/// to carry a full `RespawnData` (a dimension-qualified [`GlobalPos`] plus a yaw
/// and pitch) rather than the older bare block position and single angle.
///
/// Wire layout: a [`GlobalPos`] (identifier dimension then a packed 64-bit block
/// position) followed by big-endian `f32` yaw and pitch.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub struct SetDefaultSpawnPosition {
    /// Dimension-qualified spawn block position.
    pub location: GlobalPos,
    /// Spawn yaw in degrees.
    pub yaw: f32,
    /// Spawn pitch in degrees.
    pub pitch: f32,
}

/// Flag bit set when the player is invulnerable in a `player_abilities` packet.
pub const ABILITY_FLAG_INVULNERABLE: u8 = 0x01;
/// Flag bit set when the player is currently flying.
pub const ABILITY_FLAG_FLYING: u8 = 0x02;
/// Flag bit set when the player is allowed to fly.
pub const ABILITY_FLAG_CAN_FLY: u8 = 0x04;
/// Flag bit set when the player can instantly break blocks (creative build).
pub const ABILITY_FLAG_INSTABUILD: u8 = 0x08;

/// Clientbound `player_abilities` packet.
///
/// Tells the client which movement abilities are active and the player's flight
/// and walk speeds. The server sends it on join, on game-mode change, and when
/// flight is toggled.
///
/// Wire layout: a single flags byte (see the `ABILITY_FLAG_*` constants) then
/// big-endian `f32` flying speed and walking speed.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub struct PlayerAbilities {
    /// Packed ability flags (see the `ABILITY_FLAG_*` constants).
    pub flags: u8,
    /// Flying speed multiplier.
    pub flying_speed: f32,
    /// Field-of-view / walking speed multiplier.
    pub walking_speed: f32,
}

/// Clientbound `level_event` packet.
///
/// A positioned gameplay effect keyed by an event code — block-break effects,
/// door/piston sounds, portal ambience, and similar. The code is Mojang's
/// level-event code, not a registry id.
///
/// Wire layout: big-endian `i32` event code, a packed 64-bit block position
/// (`BlockPos.asLong`), a big-endian `i32` data value, then a boolean marking
/// the event as global rather than distance-limited.
#[derive(Debug, Clone, PartialEq, Eq, Decode)]
pub struct LevelEvent {
    /// Gameplay-level event code.
    pub event: i32,
    /// Packed event block position (`BlockPos.asLong`).
    pub position: i64,
    /// Event-specific data value.
    pub data: i32,
    /// Whether the event is global rather than distance-limited.
    pub global: bool,
}

/// Clientbound `level_particles` packet.
///
/// Spawns particles at a position with a randomized spread. The trailing
/// particle field is a `minecraft:particle_type` registry id followed by any
/// per-type option payload; because it is the final field, the options are
/// captured verbatim into [`options`](LevelParticles::options) rather than
/// decoded per type (the canonical model does not carry them), while the packed
/// prefix still decodes to fixed widths so a misparse is caught before the
/// particle id.
///
/// Wire layout: boolean override-limiter, boolean always-show, big-endian `f64`
/// x/y/z, big-endian `f32` x/y/z spread, big-endian `f32` max speed, big-endian
/// `i32` count, a VarInt particle-type registry id, then the type-specific
/// option bytes to end of packet.
#[derive(Debug, Clone, PartialEq, Decode)]
pub struct LevelParticles {
    /// Whether the particle bypasses the client's particle limiter.
    pub override_limiter: bool,
    /// Whether the particle is shown even with minimal particle settings.
    pub always_show: bool,
    /// Particle origin x.
    pub x: f64,
    /// Particle origin y.
    pub y: f64,
    /// Particle origin z.
    pub z: f64,
    /// Randomized x spread bound.
    pub x_dist: f32,
    /// Randomized y spread bound.
    pub y_dist: f32,
    /// Randomized z spread bound.
    pub z_dist: f32,
    /// Particle speed parameter.
    pub max_speed: f32,
    /// Number of particles to spawn.
    pub count: i32,
    /// Particle-type registry id.
    #[mc(varint)]
    pub particle_id: i32,
    /// Type-specific option bytes, consumed to end of packet.
    #[mc(remaining)]
    pub options: Vec<u8>,
}

/// Serverbound `attack` packet (26.2 split this out of the old interact packet).
///
/// Wire layout: a single VarInt target entity id. Unlike the interact packet,
/// it carries no hand, location, or secondary-action flag.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:attack", state = Play, bound = Server)]
pub struct Attack {
    /// Target entity id.
    #[mc(varint)]
    pub entity_id: i32,
}

/// Serverbound `player_action` packet.
///
/// Covers block-breaking phases and the item actions (drop, release, swap,
/// stab). Wire layout: VarInt action ordinal, packed `BlockPos` long, a single
/// direction byte (`Direction.get3DDataValue`), then a VarInt prediction
/// sequence. Item actions send a zeroed position and a `down` direction.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_action", state = Play, bound = Server)]
pub struct PlayerAction {
    /// Action ordinal (`0` start, `1` abort, `2` stop destroy; `3`–`7` item actions).
    #[mc(varint)]
    pub action: i32,
    /// Packed `BlockPos` long.
    pub pos: i64,
    /// Face as `Direction.get3DDataValue` (`0` down … `5` east).
    pub direction: u8,
    /// Client block-prediction sequence number.
    #[mc(varint)]
    pub sequence: i32,
}

/// Serverbound `use_item_on` packet (right-click a block face).
///
/// Wire layout: VarInt hand, then an inlined `BlockHitResult` — packed
/// `BlockPos` long, VarInt face ordinal, three `f32` cursor components relative
/// to the block, and an `inside_block` bool — then a VarInt prediction sequence.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_item_on", state = Play, bound = Server)]
pub struct UseItemOn {
    /// Interaction-hand ordinal; `0` = main, `1` = off.
    #[mc(varint)]
    pub hand: i32,
    /// Packed `BlockPos` long of the targeted block.
    pub pos: i64,
    /// Face ordinal (`Direction.get3DDataValue`) written as a VarInt.
    #[mc(varint)]
    pub face: i32,
    /// Cursor x within the block face, `0.0`–`1.0`.
    pub cursor_x: f32,
    /// Cursor y within the block face, `0.0`–`1.0`.
    pub cursor_y: f32,
    /// Cursor z within the block face, `0.0`–`1.0`.
    pub cursor_z: f32,
    /// Whether the player's head is inside the target block.
    pub inside_block: bool,
    /// Whether the hit collided with the world border.
    pub world_border_hit: bool,
    /// Client block-prediction sequence number.
    #[mc(varint)]
    pub sequence: i32,
}

/// Serverbound `use_item` packet (right-click in air).
///
/// Wire layout: VarInt hand, VarInt prediction sequence, then the yaw and pitch
/// the item was used at as two `f32`s.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_item", state = Play, bound = Server)]
pub struct UseItem {
    /// Interaction-hand ordinal; `0` = main, `1` = off.
    #[mc(varint)]
    pub hand: i32,
    /// Client block-prediction sequence number.
    #[mc(varint)]
    pub sequence: i32,
    /// Yaw the item was used at.
    pub yaw: f32,
    /// Pitch the item was used at.
    pub pitch: f32,
}

/// Serverbound `container_close` packet.
///
/// Wire layout: a single container id, written as a VarInt (`writeContainerId`).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:container_close", state = Play, bound = Server)]
pub struct ContainerClose {
    /// Container (window) id being closed.
    #[mc(varint)]
    pub window_id: i32,
}

/// Serverbound `set_carried_item` packet (hotbar selection change).
///
/// Wire layout: a single big-endian `short` hotbar slot index (`0`–`8`).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_carried_item", state = Play, bound = Server)]
pub struct SetCarriedItem {
    /// Selected hotbar slot index.
    pub slot: i16,
}

/// Serverbound `change_difficulty` packet.
///
/// Wire layout: a single VarInt difficulty ordinal
/// (`ServerboundChangeDifficultyPacket`, `Difficulty.STREAM_CODEC` —
/// `ByteBufCodecs.idMapper`, a raw VarInt of the ordinal, not the
/// `#[mc(varint)]`-tagged *typed* int this crate elsewhere reserves for
/// packet-prediction sequence numbers; same wire shape either way — see
/// `.cache/mc/26.2/src/net/minecraft/world/Difficulty.java`: `0` peaceful,
/// `1` easy, `2` normal, `3` hard).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:change_difficulty", state = Play, bound = Server)]
pub struct ChangeDifficultyServerbound {
    /// Difficulty ordinal, `0..=3`.
    #[mc(varint)]
    pub difficulty: i32,
}

/// Clientbound `change_difficulty` packet — the confirmation
/// [`crate::server_protocol::V770ServerProtocol::encode_change_difficulty`]
/// sends back.
///
/// Wire layout: VarInt difficulty ordinal, then a `locked` bool
/// (`ClientboundChangeDifficultyPacket`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:change_difficulty", state = Play, bound = Client)]
pub struct ChangeDifficultyClientbound {
    /// Difficulty ordinal, `0..=3`.
    #[mc(varint)]
    pub difficulty: i32,
    /// Whether difficulty is locked (further client-side UI affordance;
    /// vanilla does not reject a `ChangeDifficultyServerbound` while locked
    /// at the packet layer).
    pub locked: bool,
}

/// Serverbound `lock_difficulty` packet.
///
/// Wire layout: a single bool (`ServerboundLockDifficultyPacket`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:lock_difficulty", state = Play, bound = Server)]
pub struct LockDifficulty {
    /// Whether difficulty should now be locked.
    pub locked: bool,
}

/// One `(rule key, raw string value)` pair, shared by [`SetGameRule`]
/// (serverbound) and [`GameRuleValues`] (clientbound) — both wire formats are
/// a VarInt-prefixed list of this same pair shape
/// (`ServerboundSetGameRulePacket.Entry`'s `(ResourceKey<GameRule<?>>,
/// String)`, whose `ResourceKey` stream codec is `Identifier.STREAM_CODEC` —
/// a plain `namespace:path` string, the same wire shape `GameLogin::levels`
/// already uses for dimension keys — so `key` here is that string verbatim,
/// not a registry sync id).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct GameRuleEntry {
    /// The rule's identifier, e.g. `"minecraft:doDaylightCycle"`.
    pub key: String,
    /// The rule's raw string value, e.g. `"false"` or `"64"` — unparsed
    /// against any rule-specific type (this crate has no `GameRules`
    /// registry to parse against; see `crate::server`'s consumer).
    pub value: String,
}

/// Serverbound `set_game_rule` packet.
///
/// Wire layout: a VarInt-prefixed list of [`GameRuleEntry`]
/// (`ServerboundSetGameRulePacket`).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_game_rule", state = Play, bound = Server)]
pub struct SetGameRule {
    /// The rule changes requested, in wire order.
    pub entries: Vec<GameRuleEntry>,
}

/// Clientbound `game_rule_values` packet — the confirmation
/// [`crate::server_protocol::V770ServerProtocol::encode_game_rule_values`]
/// sends back. Vanilla's own struct carries the *whole* current
/// rule table as a map (`ClientboundGameRuleValuesPacket`); this crate has no
/// default rule set to include the rest of, so
/// `V770ServerProtocol::encode_game_rule_values` only ever sends the entries
/// that were just changed — same wire shape (a VarInt-prefixed list of
/// key/value pairs reads identically whether or not it happens to be the
/// full table), just a documented narrower *content*.
///
/// Wire layout: a VarInt-prefixed list of [`GameRuleEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:game_rule_values", state = Play, bound = Client)]
pub struct GameRuleValues {
    /// The rule entries being confirmed.
    pub entries: Vec<GameRuleEntry>,
}

/// Serverbound `player_input` packet (movement-input bitset).
///
/// Wire layout: a single flag byte — bit `1` forward, `2` backward, `4` left,
/// `8` right, `16` jump, `32` shift, `64` sprint.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_input", state = Play, bound = Server)]
pub struct PlayerInput {
    /// Packed movement-input flags.
    pub flags: u8,
}

/// Serverbound `player_command` packet (discrete player/vehicle commands).
///
/// Wire layout: VarInt entity id, VarInt action ordinal, VarInt data (the jump
/// boost for `start_riding_jump`, otherwise `0`).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_command", state = Play, bound = Server)]
pub struct PlayerCommand {
    /// Entity id the command applies to.
    #[mc(varint)]
    pub entity_id: i32,
    /// Command action ordinal.
    #[mc(varint)]
    pub action: i32,
    /// Command data (jump boost for `start_riding_jump`, else `0`).
    #[mc(varint)]
    pub data: i32,
}

/// Clientbound `chunk_batch_finished` packet.
///
/// Marks the end of a run of chunk packets and reports how many chunks it
/// carried. The client must reply with `chunk_batch_received` or the server
/// stops streaming chunks after ten unacknowledged batches.
#[derive(Debug, Clone, PartialEq, Eq, Decode, Encode, Packet)]
#[mc(name = "minecraft:chunk_batch_finished", state = Play, bound = Client)]
pub struct ChunkBatchFinished {
    /// Number of chunks in the batch just finished.
    #[mc(varint)]
    pub batch_size: i32,
}

/// Serverbound `chunk_batch_received` packet.
///
/// Acknowledges a finished chunk batch and reports the client's desired chunk
/// delivery rate, which the server clamps to `[0.01, 64.0]` chunks per tick.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chunk_batch_received", state = Play, bound = Server)]
pub struct ChunkBatchReceived {
    /// Desired chunks-per-tick delivery rate.
    pub desired_chunks_per_tick: f32,
}

/// Serverbound `configuration_acknowledged` packet with an empty body.
///
/// Sent in reply to a clientbound `start_configuration` to confirm the client
/// is leaving play and re-entering the configuration state (a mid-session
/// reconfigure — resource-pack/datapack reloads and `transfer` flows use it).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:configuration_acknowledged", state = Play, bound = Server)]
pub struct ConfigurationAcknowledged;

/// Serverbound `container_button_click` packet.
///
/// Wire layout: VarInt container id, VarInt button id.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:container_button_click", state = Play, bound = Server)]
pub struct ContainerButtonClick {
    /// Open container/window id.
    #[mc(varint)]
    pub window_id: i32,
    /// Button id defined by the open menu type.
    #[mc(varint)]
    pub button_id: i32,
}

/// Serverbound `pick_item_from_block` packet (middle-click a block).
///
/// Wire layout: packed `BlockPos` long, then a boolean requesting the block
/// entity's data be copied onto the picked stack.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:pick_item_from_block", state = Play, bound = Server)]
pub struct PickItemFromBlock {
    /// Packed `BlockPos` long of the targeted block.
    pub pos: i64,
    /// Whether to include the block entity's data.
    pub include_data: bool,
}

/// Serverbound `pick_item_from_entity` packet (middle-click an entity).
///
/// Wire layout: VarInt target entity id, then a boolean requesting the
/// entity's data be copied onto the picked stack.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:pick_item_from_entity", state = Play, bound = Server)]
pub struct PickItemFromEntity {
    /// Targeted entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Whether to include the entity's data.
    pub include_data: bool,
}

/// Clientbound `set_held_slot` packet — the server's answer to a middle-click
/// pick, resynchronising the client's selected hotbar slot after
/// `ServerGamePacketListenerImpl::tryPickItem`. Also the packet a real
/// vanilla server sends for any other server-initiated selection change; this
/// crate's client already decodes it into `ClientEvent::HeldSlotChanged`
/// (`adapter::player::handle_play_player`) and had no server-side encoder.
///
/// Wire layout: a single VarInt hotbar slot index (`0`-`8`).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_held_slot", state = Play, bound = Client)]
pub struct SetHeldSlot {
    /// Selected hotbar slot index.
    #[mc(varint)]
    pub slot: i32,
}

/// Serverbound `rename_item` packet (anvil name field).
///
/// Wire layout: a single UTF string, the new item name.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:rename_item", state = Play, bound = Server)]
pub struct RenameItem {
    /// New item name.
    pub name: String,
}

/// Serverbound `select_trade` packet (merchant trade-offer selection).
///
/// Wire layout: a single VarInt trade-offer index.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:select_trade", state = Play, bound = Server)]
pub struct SelectTrade {
    /// Index into the open merchant's offer list.
    #[mc(varint)]
    pub index: i32,
}

/// Serverbound `edit_book` packet.
///
/// Wire layout: VarInt slot, a VarInt-counted list of UTF page strings (each
/// max 1024 chars, list max 100 entries), then an optional UTF title (max 32
/// chars) present only when the player is signing rather than saving a draft.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:edit_book", state = Play, bound = Server)]
pub struct EditBook {
    /// Slot holding the book being edited.
    #[mc(varint)]
    pub slot: i32,
    /// Page contents, in order.
    #[mc(max = 100)]
    pub pages: Vec<String>,
    /// Title to publish under, present only when signing.
    pub title: Option<String>,
}

/// Serverbound `sign_update` packet.
///
/// Wire layout: packed `BlockPos` long, a boolean selecting the front (vs.
/// back) text, then the sign's four text lines as unconditional UTF strings
/// (max 384 chars each).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:sign_update", state = Play, bound = Server)]
pub struct SignUpdate {
    /// Packed `BlockPos` long of the target sign.
    pub pos: i64,
    /// Whether the front (vs. back) text is being edited.
    pub is_front_text: bool,
    /// First text line.
    pub line0: String,
    /// Second text line.
    pub line1: String,
    /// Third text line.
    pub line2: String,
    /// Fourth text line.
    pub line3: String,
}

/// Flag bit set when a command block's output line is tracked.
pub const COMMAND_BLOCK_FLAG_TRACK_OUTPUT: u8 = 0x01;
/// Flag bit set when a command block is conditional on the block behind it.
pub const COMMAND_BLOCK_FLAG_CONDITIONAL: u8 = 0x02;
/// Flag bit set when a command block runs automatically every tick.
pub const COMMAND_BLOCK_FLAG_AUTOMATIC: u8 = 0x04;

/// Serverbound `set_command_block` packet.
///
/// Wire layout: packed `BlockPos` long, UTF command string, VarInt
/// `CommandBlockEntity.Mode` ordinal (`0` sequence, `1` auto, `2` redstone),
/// then a single flags byte (see the `COMMAND_BLOCK_FLAG_*` constants).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_command_block", state = Play, bound = Server)]
pub struct SetCommandBlock {
    /// Packed `BlockPos` long of the target command block.
    pub pos: i64,
    /// Command text to run.
    pub command: String,
    /// Execution mode ordinal (`0` sequence, `1` auto, `2` redstone).
    #[mc(varint)]
    pub mode: i32,
    /// Packed output-tracking/conditional/automatic flags.
    pub flags: u8,
}

/// Flag bit set when the client reports it is currently flying, in the
/// serverbound `player_abilities` packet.
pub const SERVERBOUND_ABILITY_FLAG_FLYING: u8 = 0x02;

/// Serverbound `player_abilities` packet.
///
/// Wire layout: a single flags byte with only bit `1`
/// ([`SERVERBOUND_ABILITY_FLAG_FLYING`]) meaningful; all other bits are always
/// `0`. Unlike the clientbound packet of the same name, it carries no speed
/// fields.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:player_abilities", state = Play, bound = Server)]
pub struct ServerboundPlayerAbilities {
    /// Packed ability flags; only [`SERVERBOUND_ABILITY_FLAG_FLYING`] is used.
    pub flags: u8,
}

/// Serverbound `client_tick_end` packet with an empty body.
///
/// Sent once per client tick, after that tick's movement packet, so the server
/// can align world ticking with the client's tick boundary.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:client_tick_end", state = Play, bound = Server)]
pub struct ClientTickEnd;

/// Serverbound `set_command_minecart` packet (command-block minecart editor).
///
/// Wire layout: VarInt entity id, a UTF command string, then a trailing
/// boolean for output tracking (`ServerboundSetCommandMinecartPacket`) — no
/// mode/conditional/automatic flags byte, unlike [`SetCommandBlock`], since a
/// command-block minecart has neither a mode nor redstone/conditional
/// behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Decode)]
pub struct SetCommandMinecart {
    /// Target command-block-minecart entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Command text to run.
    pub command: String,
    /// Whether the output line is tracked.
    pub track_output: bool,
}

/// Serverbound `jigsaw_generate` packet (jigsaw-block "Generate" button).
///
/// Wire layout: packed `BlockPos` long, VarInt max depth ("levels"), then a
/// trailing boolean for "keep jigsaws" (`ServerboundJigsawGeneratePacket`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Decode)]
pub struct JigsawGenerate {
    /// Packed `BlockPos` long of the jigsaw block to generate from.
    pub pos: i64,
    /// Maximum structure-piece recursion depth.
    #[mc(varint)]
    pub levels: i32,
    /// Whether already-placed jigsaw blocks are kept rather than replaced.
    pub keep_jigsaws: bool,
}

/// Serverbound `set_jigsaw_block` packet (jigsaw-block editor screen).
///
/// Wire layout: packed `BlockPos` long, then five identifier/UTF strings —
/// name, target, pool, final-state block-state string, and the joint type's
/// serialized name (`"aligned"`/`"rollable"`, `JigsawBlockEntity.JointType`)
/// — then two VarInts, selection priority and placement priority
/// (`ServerboundSetJigsawBlockPacket`). Identifiers use the same
/// VarInt-length-prefixed UTF-8 wire shape as a plain string (see
/// [`BrandPayload`](crate::packets::common::BrandPayload)'s own doc comment),
/// so `String` decodes all five fields correctly regardless of which are
/// namespaced identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Decode)]
pub struct SetJigsawBlock {
    /// Packed `BlockPos` long of the target jigsaw block.
    pub pos: i64,
    /// The jigsaw's own name identifier.
    pub name: String,
    /// The target pool-element identifier this jigsaw attaches to.
    pub target: String,
    /// The structure-pool identifier to pull pieces from.
    pub pool: String,
    /// Block-state string applied once no further piece can attach.
    pub final_state: String,
    /// Serialized `JointType` name (`"aligned"` or `"rollable"`).
    pub joint: String,
    /// Selection priority among sibling jigsaws.
    #[mc(varint)]
    pub selection_priority: i32,
    /// Placement priority among sibling jigsaws.
    #[mc(varint)]
    pub placement_priority: i32,
}

/// Serverbound `set_structure_block` packet (structure-block editor screen).
///
/// Wire layout (`ServerboundSetStructureBlockPacket`): packed `BlockPos`
/// long; VarInt `StructureBlockEntity.UpdateType` ordinal and VarInt
/// `StructureMode` ordinal (both `FriendlyByteBuf.writeEnum`, i.e. a plain
/// VarInt of `Enum.ordinal()` — **not** the `ByteBufCodecs.idMapper` used
/// elsewhere in this file, though the two encode identically as a VarInt so
/// the wire bytes are the same either way); a UTF structure name; six signed
/// bytes (offset x/y/z then size x/y/z, each clamped `-48..=48`/`0..=48` by
/// the real packet before use — this decoder reads the raw byte
/// unclamped, since nothing here re-derives vanilla's clamp and the value is
/// discarded regardless); VarInt `Mirror` and `Rotation` ordinals (again
/// plain `writeEnum`); a UTF `data` string (mode/metadata); a big-endian
/// `f32` integrity; a VarLong seed; then a single flags byte packing
/// ignore-entities (`0x01`), show-air (`0x02`), show-bounding-box (`0x04`)
/// and strict (`0x08`).
#[derive(Debug, Clone, PartialEq, Decode)]
pub struct SetStructureBlock {
    /// Packed `BlockPos` long of the target structure block.
    pub pos: i64,
    /// `StructureBlockEntity.UpdateType` ordinal.
    #[mc(varint)]
    pub update_type: i32,
    /// `StructureMode` ordinal.
    #[mc(varint)]
    pub mode: i32,
    /// Structure name (save/load identifier).
    pub name: String,
    /// Offset x, unclamped raw byte.
    pub offset_x: i8,
    /// Offset y, unclamped raw byte.
    pub offset_y: i8,
    /// Offset z, unclamped raw byte.
    pub offset_z: i8,
    /// Size x, unclamped raw byte.
    pub size_x: i8,
    /// Size y, unclamped raw byte.
    pub size_y: i8,
    /// Size z, unclamped raw byte.
    pub size_z: i8,
    /// `Mirror` ordinal.
    #[mc(varint)]
    pub mirror: i32,
    /// `Rotation` ordinal.
    #[mc(varint)]
    pub rotation: i32,
    /// Mode-specific metadata string.
    pub data: String,
    /// Structure integrity (fraction of blocks placed on load), `0.0..=1.0`.
    pub integrity: f32,
    /// Structure-void random seed.
    #[mc(varlong)]
    pub seed: i64,
    /// Packed ignore-entities/show-air/show-bounding-box/strict flags.
    pub flags: u8,
}

/// Serverbound `set_test_block` packet (game-test test-block editor).
///
/// Wire layout: packed `BlockPos` long, VarInt `TestBlockMode` ordinal
/// (`ByteBufCodecs.idMapper`, same VarInt-of-ordinal wire shape as the plain
/// `writeEnum` fields above), then a plain UTF string message
/// (`ServerboundSetTestBlockPacket`).
#[derive(Debug, Clone, PartialEq, Eq, Decode)]
pub struct SetTestBlock {
    /// Packed `BlockPos` long of the target test block.
    pub pos: i64,
    /// `TestBlockMode` ordinal (`0` start, `1` log, `2` fail, `3` accept).
    #[mc(varint)]
    pub mode: i32,
    /// Free-form message shown by `log`/`fail`/`accept` modes.
    pub message: String,
}

/// Serverbound `entity_tag_query` packet (F3+I-style debug NBT inspection
/// request for an entity).
///
/// Wire layout: VarInt transaction id, then VarInt entity id
/// (`ServerboundEntityTagQueryPacket`, decompiled `.cache/mc/26.2/src`). No
/// clientbound `tag_query` encoder exists yet to answer it — see this
/// struct's decode-side consumer for why it is decoded but not acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct EntityTagQuery {
    /// Transaction id echoed back in the reply.
    #[mc(varint)]
    pub transaction_id: i32,
    /// Queried entity's network id.
    #[mc(varint)]
    pub entity_id: i32,
}

/// Serverbound `block_entity_tag_query` packet (F3+I-style debug NBT
/// inspection request for a block entity).
///
/// Wire layout: VarInt transaction id, then a packed `BlockPos` long
/// (`ServerboundBlockEntityTagQueryPacket`). Same "decoded, no reply
/// encoder yet" situation as [`EntityTagQuery`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct BlockEntityTagQuery {
    /// Transaction id echoed back in the reply.
    #[mc(varint)]
    pub transaction_id: i32,
    /// Packed `BlockPos` long of the queried block.
    pub pos: i64,
}
