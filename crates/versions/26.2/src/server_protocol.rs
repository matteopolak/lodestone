//! [`V770ServerProtocol`]: the server-side mirror of [`V770Adapter`].
//!
//! Where [`V770Adapter`] lifts clientbound protocol-776 packets into the
//! version-free client model, this type does the opposite side of the same
//! wire format: it *encodes* the clientbound packets a real vanilla 26.2
//! client expects and *decodes* the serverbound packets it sends, so
//! `lodestone-server`'s [`serve_connection`](lodestone_server::serve_connection)
//! loop can drive a real `lodestone-client` end to end over the in-memory
//! transport, with no fake wire format standing in.
//!
//! # Scope
//!
//! This implements the minimum sequence needed for a client to reach
//! [`State::Play`] and receive a rendered view: handshake, login, the
//! configuration phase (`select_known_packs`, all 29 synchronized registries,
//! and `update_tags` via [`ServerProtocol::encode_registry_data`], then the
//! finish signal), the
//! play join sequence (join game, default spawn, initial teleport,
//! chunk-cache center), `level_chunk_with_light`
//! for every column in the initial view, a post-join welcome chat, entity
//! spawn/update/remove for the mob simulation, server-initiated keep-alive
//! with a disconnect-on-timeout, time-of-day, and view streaming
//! (chunk-cache-center / forget / send) as the player moves between chunk
//! columns — the scheduling for all three lives in `lodestone-server`'s
//! `serve_play`; this module only supplies their encoders (and, for
//! keep-alive and movement, decoders) — and, since `docs/block-edit.md`,
//! decoders for the two serverbound editing packets (`player_action`'s three
//! destroy phases, `use_item_on`'s placement) plus the `block_update`
//! encoder that confirms an edit back to the acting client. See that doc for
//! what block editing does and does not cover; the wire layout here is a
//! faithful decode/encode of the real packets regardless of scope.
//!
//! # Why hand-written encoding is correct, not just convenient
//!
//! Every struct this module constructs and calls `.encode()` on already
//! derives `Decode` and is asserted against real bytes elsewhere in this
//! crate (`tests/join_flow.rs`'s golden vectors, `tests/live_chunk.rs`'s live
//! server capture). Deriving `Encode` on the same struct definition — rather
//! than hand-rolling a mirror-image encoder — is what keeps the two
//! directions from drifting apart: a field added to one is added to both.
//! The handful of packets with no existing struct (the `player_position`
//! teleport, `set_chunk_cache_center`) are written directly against
//! [`V770Adapter`]'s own decode logic for those same packets, which is the
//! best available specification for their wire layout.

/// The chunk-encode boundary's byte-identity gate (`DESIGN.md` §12.131): the
/// string path [`build_world_column`] used to be, kept as a control and asserted
/// to encode byte-identical payloads. A submodule rather than lines in this
/// file's own `mod tests` because it needs the pre-change body verbatim and this
/// file is already 5,000 lines that several agents edit concurrently. The
/// instructions-retired half is `tests/chunk_encode_cycles.rs` — it needs
/// `proc_pid_rusage`, and this crate is `#![forbid(unsafe_code)]`.
#[cfg(test)]
mod chunk_encode_identity;

use lodestone_core::{
    Ctx, Decode, Encode, Nbt, NbtTag, Reader, Writer, read_network_nbt, write_network_nbt,
};
// The command tree's *encode* side. `CommandTree` is aliased because this module
// already deals in `lodestone_world`/`lodestone_server` column types with short
// names and an unqualified `CommandTree` here would read as a server-side
// Brigadier tree, which is a different type in a different crate.
use lodestone_model::command_tree::{
    ArgumentParser, CommandSuggestionsResponse, CommandTree as WireCommandTree, NodeKind,
    RawCommandNode, StringKind,
};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, Difficulty, EntityAttributeSnapshot, GameMode,
    ItemComponents, ItemStack, RecipeBookType, ResourceKey, ResourcePackResponseKind, Rotation,
    SoundCategory, Text, TextContent, Vec3, Vec3f, WrittenBookContent,
};
use lodestone_server::{
    Abilities, ChunkColumn as ServerChunkColumn, ChunkEncoder, EntitySnapshot, HOTBAR_SIZE,
    MOTION_BLOCKING_HEIGHTMAP_TYPE_ID, MerchantOfferOut, MetadataField, PlayerListing,
    ResourcePackPush, ServerBound, ServerDirective, ServerProtocol, WorldBorder, WorldgenScope,
};
// Test-only: `encode_initialize_border_wire_layout` asserts the wire byte
// against this constant. Not imported above because the lib-only build (no
// `#[cfg(test)]`) never references it, and `cargo clippy -- -D warnings`
// treats that as an unused import.
#[cfg(test)]
use lodestone_server::ABSOLUTE_MAX_SIZE;
use lodestone_server::{AdvancementUpdate, StatKey, StatType};
use lodestone_server::crafting::{
    RecipeBookEntry as ServerRecipeBookEntry, RecipeDisplay as ServerRecipeDisplay,
    SlotDisplay as ServerSlotDisplay,
};
use lodestone_world::{
    ChunkColumn as WorldChunkColumn, ChunkSection, ColumnLight, Heightmap, Heightmaps,
    LightProperties, compute_column_light,
};
use lodestone_data::block::Block;
use uuid::Uuid;

// Test-only since the string→id resolver moved into `lodestone-data`
// (`block_states::state_id`): this module's production code no longer reads the
// forward table at all — `resolve_state_id` and `air_id` are one-line wrappers —
// while `stone_id` and the resolver's own gates below still walk it by name
// rather than trusting a literal id.
#[cfg(test)]
use lodestone_data::block_states::{block_name, properties};
use lodestone_data::entity_types::entity_type_id;
use lodestone_data::items::{item_id, item_name};
use lodestone_data::menus::{MenuId, menu_id};
use lodestone_data::mob_effects::{mob_effect_id, mob_effect_name};
use crate::entity_variants;
use crate::packet_ids::{MINECRAFT_VERSION, configuration, handshaking, login, play, status};
use crate::packets::chunk::ChunkShape;
use crate::packets::common::{
    ClientInformation, KeepAlive, PingRequest, Pong, ResourcePackResponse, TeleportToEntity,
};
use crate::packets::configuration::FinishConfiguration;
use crate::packets::entity::{pack_degrees, read_lp_vec3, write_lp_vec3};
use crate::packets::metadata::write_update_attributes;
use crate::packets::game::{
    AcceptTeleportation, Attack, BlockEntityTagQuery, ChangeDifficultyClientbound,
    ChangeDifficultyServerbound, ChangeGameMode, ChatAck, ChatCommand, ChatCommandSigned, ChatMessage,
    ChatSessionUpdate, ChunkBatchReceived,
    ClientCommand, ClientTickEnd, CommandSuggestion,
    ConfigurationAcknowledged, ContainerButtonClick, ContainerSlotStateChanged, EditBook,
    ABILITY_FLAG_CAN_FLY, ABILITY_FLAG_FLYING, ABILITY_FLAG_INSTABUILD,
    ABILITY_FLAG_INVULNERABLE, EntityTagQuery, GameEvent, GameLogin, GameRuleEntry, GameRuleValues,
    GlobalPos, InitializeBorder, JigsawGenerate,
    LockDifficulty, MOVE_FLAG_ON_GROUND, MovePlayerPos, MovePlayerPosRot, MovePlayerRot,
    MovePlayerStatusOnly, MoveVehicle, PaddleBoat, PickItemFromBlock, PickItemFromEntity,
    PlaceRecipe, PlayerAbilities, PlayerAction, PlayerCommand, PlayerLoaded, RecipeBookChangeSettings,
    RecipeBookSeenRecipe, RenameItem, Respawn, SERVERBOUND_ABILITY_FLAG_FLYING, SelectBundleItem,
    SelectTrade, ServerboundPlayerAbilities, SetBorderCenter, SetBorderLerpSize,
    SetBorderSize, SetBorderWarningDelay, SetBorderWarningDistance, SetCarriedItem,
    COMMAND_BLOCK_FLAG_AUTOMATIC, COMMAND_BLOCK_FLAG_CONDITIONAL, COMMAND_BLOCK_FLAG_TRACK_OUTPUT,
    SetCommandBlock, SetCommandMinecart, SetDefaultSpawnPosition, SetGameRule, SetHealth,
    SetHeldSlot, SetJigsawBlock, SetStructureBlock, SetTestBlock, SignUpdate, Swing, UseItem,
    UseItemOn,
};
use crate::packets::handshake::Intention;
use crate::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginCompression, LoginDisconnect, LoginFinished,
    LoginHello,
};

/// The `sea_level` field both the join `login` packet and the post-death
/// `respawn` packet carry.
///
/// Named rather than written twice because the two packets frame the *same*
/// dimension and a client that is told two different sea levels for one world has
/// no way to reconcile them. `63` is the value this crate has always sent at join
/// (`encode_game_login_rest`); it is one above the overworld generator's water
/// surface of 62, matching vanilla's own off-by-one convention for the field
/// (`ClientboundLoginPacket`'s `seaLevel` is `level.getSeaLevel()`, which is
/// vanilla's own noise-generator settings's own sea level() + 1` for the purposes this client uses it
/// for). Kept as the pre-existing constant rather than "corrected" here: changing
/// what the join packet says is a separate, wider change than adding a respawn.
const OVERWORLD_SEA_LEVEL: i32 = 63;

/// The local player's fixed network entity id, matching
/// [`begin_play`](V770ServerProtocol::begin_play)'s `GameLogin { entity_id:
/// LOCAL_PLAYER_ENTITY_ID, .. }` below — the id a real client latches onto as
/// "self" from the join packet, and therefore also the id
/// `encode_air_supply_update` must tag its metadata update with for a client
/// to apply it to its own local-player state rather than treating it as some
/// other entity's cosmetic bubble state.
const LOCAL_PLAYER_ENTITY_ID: i32 = 1;

/// The base entity class's own air-supply metadata index (confirmed
/// against the decompiled base entity source,
/// verified index `1` — see `crates/versions/26.2/src/packets/metadata.rs`'s
/// `IDX_AIR_SUPPLY` doc comment) and the `INT` serializer it is registered
/// under
/// (vanilla's own metadata-serializer registration order; that module's `SER_INT`).
/// Both constants are private to that module, so this hand-encoder restates
/// their values rather than importing them — the same "no existing struct to
/// reuse `Encode` from" situation `encode_chunk_cache_center_body` and
/// friends above are already in, and for the same reason: nothing on the
/// server side has ever needed to *write* a metadata list before this.
const METADATA_IDX_AIR_SUPPLY: u8 = 1;
const METADATA_SER_INT: i32 = 1;
/// Sentinel terminating a metadata list (mirrors `metadata.rs`'s private
/// `EOF_MARKER`).
const METADATA_EOF: u8 = 0xFF;

/// The creeper class's own swell-direction and ignited metadata
/// accessors (confirmed against the decompiled creeper source) plus the `BOOLEAN` serializer id,
/// restated for the same reason [`METADATA_IDX_AIR_SUPPLY`] restates
/// `IDX_AIR_SUPPLY`: `crates/versions/26.2/src/packets/metadata.rs`'s own
/// `IDX_CREEPER_SWELL_DIR`/`IDX_CREEPER_IGNITED`/`SER_BOOLEAN` are private to
/// that module. **Not hand-counted** — verified against the
/// `EntityDataIndexOracle` dump already in the tree
/// (`crates/versions/26.2/tests/support/entity_data_index_jvm.txt`:
/// `16 the creeper class's own swell-dir accessor 1 INT`; also `18 the creeper class's own is-ignited accessor 8
/// BOOLEAN`), the same dump that module's own decode-side constants cite and
/// whose doc comment records the two shipped off-by-one bugs
/// (the sheep class's own wool accessor, the horse class's own type-variant accessor) hand-counting produced
/// before it existed.
///
/// Index 16 also collides with the display class's own brightness-override accessor,
/// the ender-dragon class's own phase accessor and the warden class's own client-anger-level accessor (all `INT`), and
/// index 18 with several unrelated `BOOLEAN`/other-typed fields on other
/// mobs (see that same file's own doc comment for the full list) — but
/// unlike `metadata.rs`'s decode side, this *encoder* never needs a class
/// guard for that collision: [`SimMob::snapshot`](lodestone_server::SimMob)
/// only ever produces a [`MetadataField::CreeperSwellDir`]/
/// [`MetadataField::CreeperIgnited`] for a `SimMob` it already knows is a
/// creeper (`self.entity_type.path() == "creeper"`), so the guard's job is
/// done by construction at the one call site that builds the field list,
/// not by re-checking the species here.
const METADATA_IDX_CREEPER_SWELL_DIR: u8 = 16;
const METADATA_IDX_CREEPER_IGNITED: u8 = 18;
const METADATA_SER_BOOLEAN: i32 = 8;

/// the item-entity class's own item accessor's metadata index and the `ITEM_STACK` serializer id
/// it is registered under.
///
/// **Not hand-counted.** Both numbers are read straight off the
/// `EntityDataIndexOracle` dump in the tree
/// (`crates/versions/26.2/tests/support/entity_data_index_jvm.txt`:
/// `8 the item-entity class's own item accessor 7 ITEM_STACK`), and the same two bytes appear in a
/// packet captured off a real vanilla 26.2 server
/// (`tests/fixtures/item_entity_metadata_diamond.hex`: `08 07 …`), so there are
/// two independent outside sources agreeing.
///
/// # The index-8 collision, and why the separating column is neither `is_living`
/// nor `is_mob`
///
/// Index 8 is the single most crowded index in the dump — **nineteen** claimants,
/// including the living-entity class's own living-entity-flags accessor (`BYTE`),
/// the abstract-arrow class's own flags accessor (`BYTE`), the experience-orb class's own value accessor (`INT`),
/// the primed-tnt class's own fuse accessor (`INT`) and six other `ITEM_STACK` fields
/// (`EyeOfEnder`, `Fireball`, `FireworkRocketEntity`, `OminousItemSpawner`,
/// `ThrowableItemProjectile`, plus `ItemEntity` itself). CLAUDE.md's rule is that
/// the census column you need depends on which classes actually collide, and
/// **an item entity is neither living nor a mob**, so both of the columns the two
/// previously-recorded collisions used (`entity_census::is_living` for index 8's
/// living-vs-arrow split, `is_mob` for index 15's mob-vs-armour-stand split) are
/// the wrong instrument here: `is_living` and `is_mob` both report *false* for
/// `minecraft:item`, which does not distinguish it from `AbstractArrow` or
/// `PrimedTnt`.
///
/// This *encoder* needs no census column at all, and the reason is structural
/// rather than lucky. The decode side needs one because it is handed a byte with
/// no idea what entity it belongs to; here the field list is built by
/// [`MobSim::snapshots`](lodestone_server::MobSim)'s **item** loop, which
/// iterates the item-entity registry, so every [`MetadataField::Item`] that
/// reaches this arm belongs to an entity whose `entity_type` is `minecraft:item`
/// by construction — the same argument (and the same one call site) that
/// [`METADATA_IDX_CREEPER_SWELL_DIR`] records for the creeper fields. The guard
/// to keep is therefore on the *producer*: never push a `MetadataField::Item`
/// for anything but an item entity.
const METADATA_IDX_ITEM_ENTITY_ITEM: u8 = 8;
const METADATA_SER_ITEM_STACK: i32 = 7;

/// the experience-orb class's own value accessor's metadata index — **also 8**, with the `INT` serializer
/// [`METADATA_SER_INT`] already names.
///
/// Read off the same dump line-for-line as [`METADATA_IDX_ITEM_ENTITY_ITEM`]
/// (`tests/support/entity_data_index_jvm.txt`: `8 the experience-orb class's own value accessor 1 INT`), and
/// deliberately a *separate constant* with the same value rather than a reuse of that
/// one: they are two different fields that happen to collide, and a single shared
/// constant would make a future change to either silently move the other.
///
/// The producer-side guard is identical and is the only thing that separates them:
/// [`MobSim::snapshots`](lodestone_server::MobSim) builds
/// [`MetadataField::ExperienceOrbValue`] in its orb loop alone.
const METADATA_IDX_EXPERIENCE_ORB_VALUE: u8 = 8;

/// the tameable-animal class's own flags accessor's metadata index, and the `BYTE` serializer id.
///
/// Read off `tests/support/entity_data_index_jvm.txt`
/// (`18 the tameable-animal class's own flags accessor 0 BYTE`). **Index 18 is the most crowded index
/// in the game** — 37 claimants in that dump, four of them `BYTE`:
/// the tameable-animal class's own flags accessor, the abstract-horse class's own flags accessor,
/// the sheep class's own wool accessor and the shulker class's own color accessor. It is also
/// [`METADATA_IDX_CREEPER_IGNITED`]'s index under the `BOOLEAN` serializer.
///
/// Nothing on the wire distinguishes them, and no `entity_census` column separates
/// the four `BYTE` ones, so the guard is entirely on the *producer*:
/// `MobSim::snapshot` switches on the species. See
/// `lodestone_server::MetadataField::TamableFlags`.
const METADATA_IDX_TAMABLE_FLAGS: u8 = 18;
const METADATA_SER_BYTE: i32 = 0;

/// the abstract-horse class's own flags accessor's metadata index — **also 18**, also `BYTE`.
///
/// A separate constant with the same value rather than a reuse of
/// [`METADATA_IDX_TAMABLE_FLAGS`], for the reason
/// [`METADATA_IDX_EXPERIENCE_ORB_VALUE`] gives: two different fields that happen to
/// collide, and one shared constant would make a change to either silently move the
/// other. The **bit layouts differ** (`FLAG_TAME` is `0x02` here against the
/// tamable's `0x04`), which is what makes them genuinely different fields rather
/// than one field with two names.
const METADATA_IDX_HORSE_FLAGS: u8 = 18;

/// the ageable-mob class's own baby accessor, index 16 — a `BOOLEAN`. Matches the decode
/// side's `IDX_BABY` in `crates/versions/26.2/src/packets/metadata.rs`.
const METADATA_IDX_BABY: u8 = 16;
/// the villager class's own villager-data accessor — index 19, serializer `VILLAGER_DATA` (18).
/// Both numbers are off the committed jar dump
/// (`tests/support/entity_data_index_jvm.txt`: `19 the villager class's own villager-data accessor
/// 18 VILLAGER_DATA`), matching `crates/versions/26.2/src/packets/metadata.rs`'s
/// decode-side `SER_VILLAGER_DATA` constant exactly — this is the same field,
/// the other direction.
const METADATA_IDX_VILLAGER_DATA: u8 = 19;
const METADATA_SER_VILLAGER_DATA: i32 = 18;

/// the primed-tnt class's own fuse accessor — index 8, serializer `INT` (1). Off the same jar
/// dump line the decode side's `IDX_EXPERIENCE_ORB_VALUE` doc cites
/// (`tests/support/entity_data_index_jvm.txt`: `8 the primed-tnt class's own fuse accessor 1
/// INT`), one of index 8's five `INT`/`ITEM_STACK` claimants — see
/// `MetadataField::TntFuse`'s own doc for the full list.
const METADATA_IDX_TNT_FUSE: u8 = 8;

/// the furnace-minecart class's own fuel accessor — index 13, serializer `BOOLEAN` (8). The
/// jar dump's other index-13 claimant, `MinecartCommandBlock
/// .DATA_ID_COMMAND_NAME`, is a `STRING`; see `MetadataField::MinecartFuel`'s
/// own doc for why the producer alone disambiguates them.
const METADATA_IDX_MINECART_FUEL: u8 = 13;

/// the abstract-boat class's own paddle-left accessor — index 11, serializer `BOOLEAN` (8).
/// The jar dump's other index-11 claimants (`tests/support/entity_data_index_jvm.txt`)
/// are the abstract-minecart class's own custom-display-block accessor (`OPTIONAL_BLOCK_STATE`),
/// the arrow class's own effect-color accessor (`INT`), the display class's own translation accessor (`VECTOR3`)
/// and the thrown-trident class's own loyalty accessor (`BYTE`) — none share the `BOOLEAN`
/// serializer except the living-entity class's own effect-ambience accessor; see
/// `MetadataField::BoatPaddles`'s own doc for why the producer alone
/// disambiguates the two.
const METADATA_IDX_BOAT_PADDLE_LEFT: u8 = 11;

/// the abstract-boat class's own paddle-right accessor — index 12, serializer `BOOLEAN` (8).
/// The jar dump's other index-12 `BOOLEAN` claimant is the thrown-trident class's own foil accessor;
/// see [`METADATA_IDX_BOAT_PADDLE_LEFT`].
const METADATA_IDX_BOAT_PADDLE_RIGHT: u8 = 12;

/// the vehicle-entity class's own hurt accessor/`DATA_ID_HURTDIR`/`DATA_ID_DAMAGE` — indices 8,
/// 9 and 10, serializers `INT` (1), `INT` (1) and `FLOAT` (3). Read off the jar
/// dump (`tests/support/entity_data_index_jvm.txt`), which lists five `INT`
/// claimants at index 8 and two at index 9, none of them a `LivingEntity`; see
/// `MetadataField::VehicleHurt`'s own doc for why the producer alone
/// disambiguates them. Index 10's `FLOAT` has this as its only claimant.
const METADATA_IDX_VEHICLE_HURT_TIME: u8 = 8;
/// See [`METADATA_IDX_VEHICLE_HURT_TIME`].
const METADATA_IDX_VEHICLE_HURT_DIR: u8 = 9;
/// See [`METADATA_IDX_VEHICLE_HURT_TIME`].
const METADATA_IDX_VEHICLE_DAMAGE: u8 = 10;
/// vanilla's own metadata-serializer registry's own float accessor's registration id, restated here for
/// [`METADATA_IDX_AIR_SUPPLY`]'s stated reason.
const METADATA_SER_FLOAT: i32 = 3;

/// the ender-dragon class's own phase accessor — index 16, serializer `INT` (1). Off the jar
/// dump (`tests/support/entity_data_index_jvm.txt`: `16 the ender-dragon class's own phase accessor
/// 1 INT`), one of six `INT` claimants at index 16 alongside
/// [`METADATA_IDX_BABY`]'s `BOOLEAN` neighbours — see
/// `MetadataField::DragonPhase`'s own doc for the full list. The producer
/// (`MobSim::push_dragon_snapshots`, the sole caller) disambiguates.
const METADATA_IDX_DRAGON_PHASE: u8 = 16;

/// the end-crystal class's own beam-target accessor — index 8, serializer `OPTIONAL_BLOCK_POS`
/// (11). Off the jar dump (`8 the end-crystal class's own beam-target accessor 11
/// OPTIONAL_BLOCK_POS`) — the only index-8 claimant with this serializer, so
/// no producer guard is needed the way [`METADATA_IDX_TNT_FUSE`]'s `INT`
/// siblings need one.
const METADATA_IDX_CRYSTAL_BEAM_TARGET: u8 = 8;
const METADATA_SER_OPTIONAL_BLOCK_POS: i32 = 11;

/// the end-crystal class's own show-bottom accessor — index 9, serializer `BOOLEAN` (8). Off the
/// jar dump (`9 the end-crystal class's own show-bottom accessor 8 BOOLEAN`), one of three
/// `BOOLEAN` claimants at index 9 — see `MetadataField::CrystalShowBottom`'s
/// own doc for the other two. The producer
/// (`MobSim::push_end_crystal_snapshots`, the sole caller) disambiguates.
const METADATA_IDX_CRYSTAL_SHOW_BOTTOM: u8 = 9;

/// the base entity class's own pose accessor — index 6, serializer `POSE` (20). Off the jar dump
/// (`tests/support/entity_data_index_jvm.txt`: `6 the base entity class's own pose accessor 20
/// POSE`), the **only** claimant at this index — see
/// `MetadataField::Pose`'s own doc for why that means no species switch is
/// needed here, unlike every other index in this file. `METADATA_SER_POSE`
/// matches `crates/versions/26.2/src/packets/metadata.rs`'s own `SER_POSE`
/// decode-side constant, so a raw pose id round-trips byte-for-byte.
const METADATA_IDX_POSE: u8 = 6;
const METADATA_SER_POSE: i32 = 20;

/// the wither-boss class's own inv accessor — index 19, serializer `INT` (1). Off the jar
/// dump (`tests/support/entity_data_index_jvm.txt`: `19 the wither-boss class's own inv accessor
/// 1 INT`), one of six `INT` claimants at index 19 — see
/// `MetadataField::WitherInvulnerableTicks`'s own doc for the full list. The
/// producer (`MobSim::push_wither_snapshots`, the sole caller) disambiguates,
/// exactly as [`METADATA_IDX_DRAGON_PHASE`] does for its own index.
const METADATA_IDX_WITHER_INVULNERABLE_TICKS: u8 = 19;

/// the goat class's own has-left-horn accessor — index 19, serializer `BOOLEAN` (8). Off the
/// jar dump (`tests/support/entity_data_index_jvm.txt`: `19
/// the goat class's own has-left-horn accessor 8 BOOLEAN`) — see `MetadataField::GoatHorns`'s own
/// doc for the full claimant list at this index. The producer
/// (`SimMob::snapshot`'s `"goat"` arm, the sole caller) disambiguates,
/// exactly as [`METADATA_IDX_WITHER_INVULNERABLE_TICKS`] does for its own
/// index.
const METADATA_IDX_GOAT_HAS_LEFT_HORN: u8 = 19;

/// the goat class's own has-right-horn accessor — index 20, serializer `BOOLEAN` (8). Off the
/// jar dump (`tests/support/entity_data_index_jvm.txt`: `20
/// the goat class's own has-right-horn accessor 8 BOOLEAN`). See
/// [`METADATA_IDX_GOAT_HAS_LEFT_HORN`]'s own doc.
const METADATA_IDX_GOAT_HAS_RIGHT_HORN: u8 = 20;

/// the axolotl class's own playing-dead accessor — index 19, serializer `BOOLEAN` (8). Off the
/// jar dump (`tests/support/entity_data_index_jvm.txt`: `19
/// the axolotl class's own playing-dead accessor 8 BOOLEAN`) — one of the `BOOLEAN` claimants
/// [`METADATA_IDX_GOAT_HAS_LEFT_HORN`]'s own doc already names at this
/// index. The producer (`SimMob::snapshot`'s `"axolotl"` arm, the sole
/// caller) disambiguates, exactly as that constant's own doc describes for
/// its pair.
const METADATA_IDX_AXOLOTL_PLAYING_DEAD: u8 = 19;

/// the camel class's own dash accessor — index 19, serializer `BOOLEAN` (8). Off the jar dump
/// (`tests/support/entity_data_index_jvm.txt`: `19 the camel class's own dash accessor 8 BOOLEAN`) —
/// one of the `BOOLEAN` claimants [`METADATA_IDX_GOAT_HAS_LEFT_HORN`]'s own
/// doc already names at this index. The producer (`SimMob::snapshot`'s
/// `"camel"` arm, the sole caller) disambiguates, exactly as that constant's
/// own doc describes for its pair.
const METADATA_IDX_CAMEL_DASH: u8 = 19;

/// the sniffer class's own state accessor — index 18, serializer `SNIFFER_STATE` (35). Off the
/// jar dump (`tests/support/entity_data_index_jvm.txt`: `18 the sniffer class's own state accessor
/// 35 SNIFFER_STATE`). Unlike every other `MetadataField` index constant in
/// this file, `35` is not a reused generic serializer — it is a real, distinct
/// `EntityDataSerializer` (vanilla's own metadata-serializer registry's own sniffer-state accessor, id 35 in
/// the jar's own registration order), so the wire value is a plain VarInt
/// enum ordinal, the same shape [`METADATA_SER_POSE`] already uses. The
/// producer (`SimMob::snapshot`'s `"sniffer"` arm, the sole caller)
/// disambiguates index 18 from the armadillo class's own armadillo-state accessor's own claim on
/// the same index (serializer 36, a different type — the wire's own
/// serializer-id field is what actually separates the two, not species
/// alone).
const METADATA_IDX_SNIFFER_STATE: u8 = 18;
const METADATA_SER_SNIFFER_STATE: i32 = 35;

/// The overworld world-clock's registry holder id
/// (`WorldClocks::bootstrap` registers `minecraft:overworld` first,
/// `minecraft:the_end` second — see `packets::time::ClockUpdate::holder_id`'s
/// doc comment). The only clock this crate ever anchors: the integrated
/// server always joins into the overworld (this type's own doc comment).
const OVERWORLD_CLOCK_HOLDER_ID: i32 = 0;

/// Fixed decoding/encoding context for protocol 776 (mirrors [`crate::adapter`]'s
/// own `CTX`; kept private to this module since only this file names raw
/// packet ids on the server side).
const CTX: Ctx = Ctx { version: 776 };

/// The block-state id for `minecraft:stone`, resolved by name so a change to
/// the generated table cannot silently desync this from the real registry
/// id. Test-only now: `build_world_column` used to write this
/// as its solid-block fallback (before it carried real per-block state) and
/// `encode_chunk`'s own call site is where that literal lived; now the only
/// remaining reference is `encode_block_update_wire_layout`'s pinning
/// assertion below, which still writes literal `"minecraft:stone"` through
/// [`resolve_state_id`] and checks the id lands here.
#[cfg(test)]
fn stone_id() -> u32 {
    // Registry id `1` is asserted to be `minecraft:stone` by
    // `tests/block_states.rs`; re-deriving it by name here (rather than the
    // bare literal) means a regenerated table that ever renumbered stone
    // would fail loudly at the lookup below instead of silently sending the
    // wrong block.
    (0..).find(|&id| block_name(id) == Some("minecraft:stone")).expect(
        "generated block-state table has no `minecraft:stone` entry — regenerate or fix the table",
    )
}

/// Fallback: the block-state id for `minecraft:air`, resolved by name for the
/// same reason [`stone_id`] is rather than hardcoded as registry id `0`. Used
/// both as [`resolve_state_id`]'s no-match fallback and, indirectly, wherever
/// this module needs air's id.
///
/// Delegates to [`lodestone_data::block_states::air_state_id`], which caches it.
/// This used to be a 32,366-row scan **per call**, and one of those calls is on
/// the per-column encode path.
fn air_id() -> u32 {
    lodestone_data::block_states::air_state_id()
}

/// vanilla's own particle-type registry's own explosion-emitter accessor's network registry id, restated for the
/// same reason [`METADATA_IDX_AIR_SUPPLY`] restates its decode-side sibling:
/// `crate::adapter`'s own `PARTICLE_ID_EXPLOSION_EMITTER` is private to that
/// module. Every real vanilla explosion source (the creeper class's own explode creeper,
/// TNT, beds, respawn anchors) sends this id, never the plain `EXPLOSION`
/// id `decode_explode` also accepts as a simpler-to-decode alternative.
const PARTICLE_ID_EXPLOSION_EMITTER: i32 = 29;

/// The `EnumSet<vanilla's own clientbound player-info-update packet's own action>` bit set
/// [`V770ServerProtocol::encode_player_info_add`] sends: `ADD_PLAYER` (ordinal
/// 0), `UPDATE_GAME_MODE` (2), `UPDATE_LISTED` (3), `UPDATE_LATENCY` (4).
///
/// `1 | 4 | 8 | 16 = 29`. Written as the shifted ordinals rather than the
/// literal so it cannot drift from the ordinals the entry body below writes
/// fields for, in that order — a mask and a body that disagree produce a
/// misparse the client reports as trailing bytes, not as a missing field. The
/// ordinals themselves match `crate::packets::player_info`'s own `action`
/// module, the decode-side statement of the same table.
const PLAYER_INFO_ADD_ACTIONS: u8 = (1 << 0) | (1 << 2) | (1 << 3) | (1 << 4);

/// The game mode a tab-list entry reports, restated from
/// [`V770ServerProtocol::begin_play`]'s own `game_type: 0` (survival) — see
/// [`V770ServerProtocol::encode_player_info_add`]'s doc comment for why this is
/// a restatement rather than a read.
const JOIN_GAME_MODE: i32 = 0;

/// The `minecraft:sound_event` registry id for
/// `minecraft:entity.generic.explode` (vanilla's own sound-events registry's own generic-explode accessor),
/// resolved by name the same way [`stone_id`]/[`air_id`] resolve block
/// states — bounded by [`SOUND_EVENT_COUNT`] so a name this table has never
/// had (a stale or ahead-of-version generated table) fails loudly here
/// rather than scanning forever. Used by [`V770ServerProtocol::encode_explode`]
/// to build the `Holder<SoundEvent>` **registry-reference** encoding a real
/// vanilla server sends for this sound — see that method's own doc comment
/// for why that is the byte-accurate choice, verified against
/// vanilla's own codec library's own holder's decompiled encode arm, not the decoder's own
/// (weaker) direct-literal-name path.
fn explosion_sound_registry_id() -> i32 {
    (0..lodestone_data::sound_events::SOUND_EVENT_COUNT as i32)
        .find(|&id| lodestone_data::sound_events::sound_event_name(id) == Some("minecraft:entity.generic.explode"))
        .expect(
            "generated sound-event table has no `minecraft:entity.generic.explode` entry — \
             regenerate or fix the table",
        )
}

/// The fixed-point scale for `sound` packet positions: coordinates go on the
/// wire as `(int)(block * 8)`, so each unit is `1/8` of a block. Vanilla's
/// vanilla's own clientbound sound packet's own location-accuracy accessor; restated here for the same reason
/// [`PARTICLE_ID_EXPLOSION_EMITTER`] is — [`crate::adapter`]'s own copy is
/// private to that module.
const SOUND_POSITION_SCALE: f64 = 8.0;

/// The `minecraft:sound_event` registry id for `name`, or `None` if
/// 26.2 has no such sound.
///
/// Indexed once into a `name -> id` map rather than scanned per call: a busy tick
/// can carry several sounds and the table is ~1,500 entries. The `None` is
/// load-bearing — see [`V770ServerProtocol::encode_sound`].
fn sound_event_registry_id(name: &str) -> Option<i32> {
    static INDEX: std::sync::OnceLock<std::collections::HashMap<&'static str, i32>> =
        std::sync::OnceLock::new();
    INDEX
        .get_or_init(|| {
            (0..lodestone_data::sound_events::SOUND_EVENT_COUNT as i32)
                .filter_map(|id| {
                    lodestone_data::sound_events::sound_event_name(id).map(|name| (name, id))
                })
                .collect()
        })
        .get(name)
        .copied()
}

/// The `minecraft:particle_type` registry id for `name`, or `None`
/// for an unknown one.
///
/// Named "simple" as a warning rather than a filter: this crate has no census of
/// *which* particle types carry option bytes, so the id it returns is only safe
/// to send for an argument-less `SimpleParticleType`. Every producer in
/// `lodestone_server::effects` is one; a future option-carrying particle needs
/// the options written too, not just this id.
fn simple_particle_registry_id(name: &str) -> Option<i32> {
    static INDEX: std::sync::OnceLock<std::collections::HashMap<&'static str, i32>> =
        std::sync::OnceLock::new();
    INDEX
        .get_or_init(|| {
            (0..)
                .map_while(|id| {
                    lodestone_data::particle_types::particle_type_name(id).map(|name| (name, id))
                })
                .collect()
        })
        .get(name)
        .copied()
}

/// This port's own biome registry id space — index in this
/// **sorted** array is the wire id [`resolve_biome_id`] uses. Regenerable
/// with `awk '/^row\./{print $2}' scripts/worldgen-oracle/biome_java.txt |
/// sort -u`, the exact set `lodestone-worldgen`'s embedded overworld
/// parameter table can ever resolve a column to.
///
/// # Why "sorted by name" and not vanilla's own biome registry order
///
/// Real vanilla assigns biome wire ids by **registration order** in a
/// `minecraft:worldgen/biome` dynamic-registry sync sent during the
/// configuration phase. This server sends that sync too, now
/// (relayed as captured vanilla bytes — see `registry_data_fixtures`'s module
/// docs), but it is still relayed **opaquely**: nothing in this crate parses
/// entries back out of it, and nothing on the client reads a biome by wire id
/// today — `lodestone-shell` still has no `impl BiomeTint`; checked directly,
/// zero implementors in `crates/lodestone-shell/src`. So there is still no
/// *biome* id space this table needs to agree with, and no consumer on the
/// client side to agree with it either: the `ChunkSection::biomes()`
/// container this now populates reaches the wire and nothing downstream
/// reads it back into a name. Any
/// stable, reproducible convention is therefore safe **for now**; alphabetical
/// is the simplest one that needs no extra bookkeeping. **This is provisional**
/// — once a real `worldgen/biome` sync exists (or a render-layers agent needs
/// a name for a wire id), replace this table with that sync's actual order,
/// do not assume this one is a substitute for it.
const BIOME_NAMES: &[&str] = &[
    "minecraft:badlands",
    "minecraft:bamboo_jungle",
    "minecraft:beach",
    "minecraft:birch_forest",
    "minecraft:cherry_grove",
    "minecraft:cold_ocean",
    "minecraft:dark_forest",
    "minecraft:deep_cold_ocean",
    "minecraft:deep_dark",
    "minecraft:deep_frozen_ocean",
    "minecraft:deep_lukewarm_ocean",
    "minecraft:deep_ocean",
    "minecraft:desert",
    "minecraft:dripstone_caves",
    "minecraft:eroded_badlands",
    "minecraft:flower_forest",
    "minecraft:forest",
    "minecraft:frozen_ocean",
    "minecraft:frozen_peaks",
    "minecraft:frozen_river",
    "minecraft:grove",
    "minecraft:ice_spikes",
    "minecraft:jagged_peaks",
    "minecraft:jungle",
    "minecraft:lukewarm_ocean",
    "minecraft:lush_caves",
    "minecraft:mangrove_swamp",
    "minecraft:meadow",
    "minecraft:mushroom_fields",
    "minecraft:ocean",
    "minecraft:old_growth_birch_forest",
    "minecraft:old_growth_pine_taiga",
    "minecraft:old_growth_spruce_taiga",
    "minecraft:pale_garden",
    "minecraft:plains",
    "minecraft:river",
    "minecraft:savanna",
    "minecraft:savanna_plateau",
    "minecraft:snowy_beach",
    "minecraft:snowy_plains",
    "minecraft:snowy_slopes",
    "minecraft:snowy_taiga",
    "minecraft:sparse_jungle",
    "minecraft:stony_peaks",
    "minecraft:stony_shore",
    "minecraft:sulfur_caves",
    "minecraft:sunflower_plains",
    "minecraft:swamp",
    "minecraft:taiga",
    "minecraft:warm_ocean",
    "minecraft:windswept_forest",
    "minecraft:windswept_gravelly_hills",
    "minecraft:windswept_hills",
    "minecraft:windswept_savanna",
    "minecraft:wooded_badlands",
];

/// Resolves a biome id string ([`ServerChunkColumn::biome_state`]'s
/// vocabulary) to this port's [`BIOME_NAMES`] index — see that constant's
/// doc comment for the id-space caveat. Falls back to `minecraft:plains`'s
/// index for any name outside the known set (never panics on unexpected
/// data, mirroring [`resolve_state_id`]'s tiered-fallback posture).
///
/// # Panics
/// Panics if `BIOME_NAMES` has no `"minecraft:plains"` entry (a corrupt
/// table, not a runtime condition).
fn resolve_biome_id(name: &str) -> u32 {
    BIOME_NAMES.iter().position(|&n| n == name).unwrap_or_else(|| {
        BIOME_NAMES
            .iter()
            .position(|&n| n == "minecraft:plains")
            .expect("BIOME_NAMES missing minecraft:plains")
    }) as u32
}

/// vanilla's own clientbound game-event packet's own change-game-mode accessor's own event code.
const GAME_EVENT_CHANGE_GAME_MODE: u8 = 3;

/// Resolves a canonical block-state string ([`ServerChunkColumn`]'s own
/// vocabulary, e.g. `"minecraft:water[level=0]"`, `"minecraft:stone"`) to its
/// protocol-776 registry id, falling back to air for a block name this table
/// does not carry.
///
/// **The resolution itself now lives in
/// [`lodestone_data::block_states::state_id`]** — the three-tier
/// exact/default-plus-overrides/default algorithm, its synthetic-property drop
/// and the reason the default state is not the lowest id are all documented
/// there, and so is the index that makes it `O(log 1196)` plus one scan of *that
/// block's* states rather than the 32,366-row scan with a string compare per row
/// this function used to be. This wrapper is the air fallback and nothing else.
///
/// Moving it was a performance change with a correctness dividend: `lodestone-server`'s
/// [`ServerChunkColumn`] resolves its own block palette through the *same*
/// function now (`palette_state_ids`), so [`build_world_column`] indexes integers
/// instead of hashing 98,304 strings per column, and the two paths cannot drift
/// into two different understandings of what a bare block name means. Both
/// remaining string callers ([`V770ServerProtocol::encode_block_update`] and
/// `encode_block_update_body`) are per-*edit*, not per-block.
///
/// A block-update confirmation is best-effort feedback (see
/// `docs/block-edit.md`), not the server's authoritative state — that stays
/// in [`ServerChunkColumn`]'s own string form, which this function only
/// reads. The air fallback exists so a state string this version's table cannot
/// parse back at all degrades to a visibly-wrong confirmation rather than a
/// panic or a corrupted wire id.
fn resolve_state_id(state: &str) -> u32 {
    lodestone_data::block_states::state_id(state).unwrap_or_else(air_id)
}

/// Unpacks vanilla's vanilla's own block-position type's own as long form (the inverse of
/// [`pack_block_pos`]): `x` in the high 26 bits, `z` in the middle 26 bits,
/// `y` in the low 12 bits, each sign-extended back out via a
/// left-then-arithmetic-right shift pair. Mirrors `V770Adapter`'s own private
/// `unpack_block_pos` exactly (kept as a local duplicate here — this module
/// already keeps its own hand-written mirrors of the adapter's `pack`/encode
/// helpers rather than sharing them across the decode/encode boundary, per
/// this file's own module doc).
fn unpack_block_pos(packed: i64) -> BlockPos {
    let x = (packed >> 38) as i32;
    let y = ((packed << 52) >> 52) as i32;
    let z = ((packed << 26) >> 38) as i32;
    BlockPos::new(x, y, z)
}

/// Maps vanilla's own direction enum's own get3 d data value (`0` down … `5` east) back to a
/// [`BlockFace`] — the inverse of `V770Adapter`'s own `face_ordinal`. Any
/// value outside `0..=5` (a malformed packet) falls back to `East` rather
/// than panicking; the resulting `ServerBound` still carries a valid
/// position, so the worst case is a break/place computed against the wrong
/// face, not a dropped connection.
fn face_from_ordinal(ordinal: i32) -> BlockFace {
    match ordinal {
        0 => BlockFace::Down,
        1 => BlockFace::Up,
        2 => BlockFace::North,
        3 => BlockFace::South,
        4 => BlockFace::West,
        _ => BlockFace::East,
    }
}

/// Maps a wire difficulty ordinal (`0` peaceful … `3` hard,
/// vanilla's own difficulty enum's own stream codec) to [`Difficulty`], mirroring `V770Adapter`'s
/// own `CHANGE_DIFFICULTY` decode (`adapter/player.rs`, the clientbound direction of
/// the same wire concept): an out-of-range id decodes to `None` rather than
/// vanilla's vanilla's own id-map helper's own out of bounds strategy::WRAP` silently aliasing it to a
/// different difficulty — a malformed packet drops (`ServerBound::Ignored`),
/// it does not misreport.
fn difficulty_from_ordinal(ordinal: i32) -> Option<Difficulty> {
    match ordinal {
        0 => Some(Difficulty::Peaceful),
        1 => Some(Difficulty::Easy),
        2 => Some(Difficulty::Normal),
        3 => Some(Difficulty::Hard),
        _ => None,
    }
}

/// The inverse of [`difficulty_from_ordinal`], for encoding a confirmation
/// back out.
fn difficulty_to_ordinal(difficulty: Difficulty) -> i32 {
    match difficulty {
        Difficulty::Peaceful => 0,
        Difficulty::Easy => 1,
        Difficulty::Normal => 2,
        Difficulty::Hard => 3,
    }
}

/// Encodes a packet body into a fresh byte buffer.
fn encode_body<T: Encode>(packet: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    packet
        .encode(&mut writer, CTX)
        .expect("encoding a well-formed struct into a `Vec<u8>` writer cannot fail");
    writer.into_vec()
}

/// Builds a [`ServerDirective::Send`] from a packet id and an encodable body.
fn send<T: Encode>(packet_id: i32, packet: &T) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet),
    }
}

/// `minecraft:custom_stat` registry paths by numeric id, from
/// `.cache/mc/26.2/generated/reports/registries.json`. A built-in registry, so
/// these ids are the jar's and not synced.
///
/// Note the 26.2 names: `play_time`, not the older `play_one_minute`.
const CUSTOM_STAT_IDS: &[&str] = &[
    "leave_game",
    "play_time",
    "total_world_time",
    "time_since_death",
    "time_since_rest",
    "sneak_time",
    "walk_one_cm",
    "crouch_one_cm",
    "sprint_one_cm",
    "walk_on_water_one_cm",
    "fall_one_cm",
    "climb_one_cm",
    "fly_one_cm",
    "walk_under_water_one_cm",
    "minecart_one_cm",
    "boat_one_cm",
    "pig_one_cm",
    "happy_ghast_one_cm",
    "horse_one_cm",
    "aviate_one_cm",
    "swim_one_cm",
    "strider_one_cm",
    "nautilus_one_cm",
    "jump",
    "drop",
    "damage_dealt",
    "damage_dealt_absorbed",
    "damage_dealt_resisted",
    "damage_taken",
    "damage_blocked_by_shield",
    "damage_absorbed",
    "damage_resisted",
    "deaths",
    "mob_kills",
    "animals_bred",
    "player_kills",
    "fish_caught",
    "talked_to_villager",
    "traded_with_villager",
    "eat_cake_slice",
    "fill_cauldron",
    "use_cauldron",
    "clean_armor",
    "clean_banner",
    "clean_shulker_box",
    "interact_with_brewingstand",
    "interact_with_beacon",
    "inspect_dropper",
    "inspect_hopper",
    "inspect_dispenser",
    "play_noteblock",
    "tune_noteblock",
    "pot_flower",
    "trigger_trapped_chest",
    "open_enderchest",
    "enchant_item",
    "play_record",
    "interact_with_furnace",
    "interact_with_crafting_table",
    "open_chest",
    "sleep_in_bed",
    "open_shulker_box",
    "open_barrel",
    "interact_with_blast_furnace",
    "interact_with_smoker",
    "interact_with_lectern",
    "interact_with_campfire",
    "interact_with_cartography_table",
    "interact_with_loom",
    "interact_with_stonecutter",
    "bell_ring",
    "raid_trigger",
    "raid_win",
    "interact_with_anvil",
    "interact_with_grindstone",
    "target_hit",
    "interact_with_smithing_table",
];

/// The `minecraft:block` **registry** id (registration order) for a block name.
///
/// A linear scan over [`Block`] rather than a reverse map: statistics are a
/// request/response batch of at most a few hundred entries, sent when a player
/// opens one screen, so a table would cost more to keep than the scan does to
/// run. Note this is the registry id space, **not** the block-state id space a
/// chunk palette uses.
fn block_registry_id_by_name(name: &str) -> Option<i32> {
    Block::all()
        .find(|block| block.name() == name)
        .map(|block| i32::from(block.registry_id()))
}

/// Resolves a [`StatKey`] to the pair of VarInts vanilla's own stat stream
/// codec writes: the
/// `minecraft:stat_type` registry id, then the value's id in whichever registry
/// that type dispatches on.
///
/// The four value registries come straight from vanilla's own stats source: `mined` is
/// `BLOCK`, the five item counters are `ITEM`, the two kill counters are
/// `ENTITY_TYPE`, and `custom` is `CUSTOM_STAT`. Getting that mapping wrong is
/// invisible — every id resolves to *something* in the wrong registry, and the
/// client draws a plausible line about the wrong block.
fn stat_wire_ids(key: &StatKey) -> Option<(i32, i32)> {
    let type_id = match key.kind {
        StatType::Mined => 0,
        StatType::Crafted => 1,
        StatType::Used => 2,
        StatType::Broken => 3,
        StatType::PickedUp => 4,
        StatType::Dropped => 5,
        StatType::Killed => 6,
        StatType::KilledBy => 7,
        StatType::Custom => 8,
    };
    let value = key.value.as_str();
    let value_id = match key.kind {
        StatType::Mined => block_registry_id_by_name(value)?,
        StatType::Crafted
        | StatType::Used
        | StatType::Broken
        | StatType::PickedUp
        | StatType::Dropped => item_id(value)?,
        StatType::Killed | StatType::KilledBy => entity_type_id(value)?,
        StatType::Custom => {
            // Custom stats are conventionally written bare (`play_time`) but the
            // registry key is namespaced, so accept either spelling.
            let path = value.strip_prefix("minecraft:").unwrap_or(value);
            let index = CUSTOM_STAT_IDS.iter().position(|name| *name == path)?;
            i32::try_from(index).ok()?
        }
    };
    Some((type_id, value_id))
}

/// `minecraft:slot_display` registry ids, in vanilla's own slot-displays registration's own bootstrap's
/// registration order — the dispatch key vanilla's own slot-display type's own stream codec writes before
/// each variant's own body.
///
/// Registration order **is** the id assignment for a `registerSimple` registry, so
/// this list is the record, not a guess. Only the five a crafting recipe reaches
/// are named; the six unnamed ids (2 `with_any_potion`, 3
/// `only_with_component`, 7 `dyed`, 8 `smithing_trim`, 9 `with_remainder`, 1
/// `any_fuel`) belong to furnace/brewing/smithing displays.
mod slot_display {
    pub const EMPTY: i32 = 0;
    pub const ITEM: i32 = 4;
    pub const ITEM_STACK: i32 = 5;
    pub const TAG: i32 = 6;
    pub const COMPOSITE: i32 = 10;
}

/// `minecraft:recipe_display` registry ids, in vanilla's own recipe-displays
/// datagen bootstrap routine's order.
mod recipe_display {
    pub const CRAFTING_SHAPELESS: i32 = 0;
    pub const CRAFTING_SHAPED: i32 = 1;
}

/// `minecraft:recipe_book_category` ids, in vanilla's own recipe-book-categories
/// registration order. Only the crafting book's four are reachable from the
/// bundled corpus; the furnace/stonecutter/smithing entries are listed so the
/// numbering is checkable against the source rather than trusted.
const RECIPE_BOOK_CATEGORIES: &[&str] = &[
    "crafting_building_blocks",
    "crafting_redstone",
    "crafting_equipment",
    "crafting_misc",
    "furnace_food",
    "furnace_blocks",
    "furnace_misc",
    "blast_furnace_blocks",
    "blast_furnace_misc",
    "smoker_food",
    "stonecutter",
    "smithing",
    "campfire",
];

/// Writes one vanilla's own slot-display type's own stream codec value: the registry dispatch id, then the
/// variant body.
///
/// An `item`/`item_stack` naming an id the 26.2 item census does not know degrades
/// to `empty` rather than writing a wrong id — the same choice
/// [`write_optional_item_stack`] makes, and the only one that keeps the rest of the
/// packet parseable.
fn write_slot_display(w: &mut Writer, display: &ServerSlotDisplay) {
    match display {
        ServerSlotDisplay::Empty => w.var_i32(slot_display::EMPTY),
        ServerSlotDisplay::Item(item) => match item_id(&item.to_string()) {
            Some(id) => {
                w.var_i32(slot_display::ITEM);
                w.var_i32(id);
            }
            None => w.var_i32(slot_display::EMPTY),
        },
        ServerSlotDisplay::Stack { item, count } => match item_id(&item.to_string()) {
            Some(id) => {
                w.var_i32(slot_display::ITEM_STACK);
                // vanilla's own item-stack-template codec's own stream codec is item, **then** count, then
                // the component patch — the opposite field order from
                // vanilla's own item-stack type's own optional-stream-codec accessor, which leads with the count.
                // Transcribing one from the other is the mistake to avoid here.
                w.var_i32(id);
                w.var_i32(*count);
                w.var_i32(0); // added components
                w.var_i32(0); // removed components
            }
            None => w.var_i32(slot_display::EMPTY),
        },
        ServerSlotDisplay::Tag(tag) => {
            w.var_i32(slot_display::TAG);
            w.string(&tag.to_string());
        }
        ServerSlotDisplay::Composite(contents) => {
            w.var_i32(slot_display::COMPOSITE);
            w.var_i32(i32::try_from(contents.len()).unwrap_or(i32::MAX));
            for entry in contents {
                write_slot_display(w, entry);
            }
        }
    }
}

/// Writes one vanilla's own recipe-display type's own stream codec value: dispatch id, the type's own
/// fields, then `result` and `craftingStation` (in that order, for every type).
fn write_recipe_display(w: &mut Writer, display: &ServerRecipeDisplay) {
    let station = ServerSlotDisplay::Item(
        "minecraft:crafting_table"
            .parse()
            .expect("static item id is valid"),
    );
    match display {
        ServerRecipeDisplay::Shaped {
            width,
            height,
            ingredients,
            result,
        } => {
            w.var_i32(recipe_display::CRAFTING_SHAPED);
            w.var_i32(*width);
            w.var_i32(*height);
            w.var_i32(i32::try_from(ingredients.len()).unwrap_or(i32::MAX));
            for ingredient in ingredients {
                write_slot_display(w, ingredient);
            }
            write_slot_display(w, result);
            write_slot_display(w, &station);
        }
        ServerRecipeDisplay::Shapeless {
            ingredients,
            result,
        } => {
            w.var_i32(recipe_display::CRAFTING_SHAPELESS);
            w.var_i32(i32::try_from(ingredients.len()).unwrap_or(i32::MAX));
            for ingredient in ingredients {
                write_slot_display(w, ingredient);
            }
            write_slot_display(w, result);
            write_slot_display(w, &station);
        }
    }
}

/// Body of `ClientboundRecipeBookAddPacket`: a list of
/// `(RecipeDisplayEntry, flags)` pairs, then the `replace` bool.
///
/// `RecipeDisplayEntry` is `id`, `display`, `OptionalInt group`,
/// `recipe_book_category` registry id, and `Optional<List<Ingredient>>` where an
/// `Ingredient` is a `HolderSet<Item>`.
///
/// **The `HolderSet` encoding is the subtle part.** vanilla's own codec library's own holder set
/// writes a VarInt that is `0` for "a tag follows" and `n + 1` for "a list of `n`
/// direct entries follows". We always write the direct-list form (the ingredient
/// items are already resolved server-side), so every count here is `len + 1` — an
/// off-by-one that is *not* an off-by-one.
///
/// Bit 0 remains clear because a join-time book is not a discovery toast. Bit
/// 1 comes from the server's per-connection seen state: a fresh entry is
/// highlighted until the client reports `recipe_book_seen_recipe` for its id.
fn encode_recipe_book_add_body(entries: &[ServerRecipeBookEntry], replace: bool) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(i32::try_from(entries.len()).unwrap_or(i32::MAX));
    for entry in entries {
        w.var_i32(entry.id);
        write_recipe_display(&mut w, &entry.display);
        // The group is an offset VarInt, **not** a bool-prefixed optional: `0`
        // is absent and a present value is written one higher. A bool-prefixed
        // encoding happens to agree on the absent case (a `false` byte and a
        // zero VarInt are both `0x00`) and mis-frames every following field
        // whenever a group is present, which is why the client-side decoder in
        // `adapter::inventory`'s `decode_recipe_book_add` reads it this way.
        match entry.group {
            Some(group) => w.var_i32(group.saturating_add(1)),
            None => w.var_i32(0),
        }
        let category = RECIPE_BOOK_CATEGORIES
            .iter()
            .position(|name| *name == entry.category)
            .and_then(|i| i32::try_from(i).ok())
            // `crafting_misc`, the tab vanilla's own JSON default lands in.
            .unwrap_or(3);
        w.var_i32(category);
        if entry.crafting_requirements.is_empty() {
            w.bool(false);
        } else {
            w.bool(true);
            w.var_i32(i32::try_from(entry.crafting_requirements.len()).unwrap_or(i32::MAX));
            for ingredient in &entry.crafting_requirements {
                let ids: Vec<i32> = ingredient
                    .iter()
                    .filter_map(|item| item_id(&item.to_string()))
                    .collect();
                // See this function's doc: `n + 1`, because `0` means "a tag
                // reference follows instead".
                w.var_i32(i32::try_from(ids.len() + 1).unwrap_or(i32::MAX));
                for id in ids {
                    w.var_i32(id);
                }
            }
        }
        w.u8(if entry.highlight { 0x02 } else { 0x00 }); // no notification; optional highlight
    }
    w.bool(replace);
    w.into_vec()
}

/// Body of `ClientboundUpdateAdvancementsPacket` (see the trait method for the
/// field-by-field wire notes).
fn encode_update_advancements_body(update: &AdvancementUpdate) -> Vec<u8> {
    let mut w = Writer::default();
    w.bool(update.reset);
    w.var_i32(i32::try_from(update.added.len()).unwrap_or(i32::MAX));
    for advancement in &update.added {
        w.string(&advancement.id);
        match &advancement.parent {
            Some(parent) => {
                w.bool(true);
                w.string(parent);
            }
            None => w.bool(false),
        }
        // No display: `lodestone_server::advancements::Advancement` deliberately
        // carries no presentation (it has no component model), and vanilla's own
        // reader treats the optional as absent-and-hidden rather than erroring.
        // A client with its own advancement table (ours does) keys on the id and
        // draws its own icon; the progress below is the part that was missing.
        w.bool(false);
        w.var_i32(i32::try_from(advancement.requirements.len()).unwrap_or(i32::MAX));
        for group in &advancement.requirements {
            w.var_i32(i32::try_from(group.len()).unwrap_or(i32::MAX));
            for criterion in group {
                w.string(criterion);
            }
        }
        w.bool(advancement.sends_telemetry_event);
    }
    w.var_i32(i32::try_from(update.removed.len()).unwrap_or(i32::MAX));
    for id in &update.removed {
        w.string(id);
    }
    w.var_i32(i32::try_from(update.progress.len()).unwrap_or(i32::MAX));
    for entry in &update.progress {
        w.string(&entry.id);
        w.var_i32(i32::try_from(entry.criteria.len()).unwrap_or(i32::MAX));
        for (name, obtained) in &entry.criteria {
            w.string(name);
            // `CriterionProgress` is a nullable `Instant`: presence bool then
            // epoch millis as a big-endian long.
            match obtained {
                Some(millis) => {
                    w.bool(true);
                    w.i64(*millis);
                }
                None => w.bool(false),
            }
        }
    }
    w.bool(update.show_advancements);
    w.into_vec()
}

/// Body of `ClientboundAwardStatsPacket`: a VarInt-counted map of
/// `(stat type id, value id) -> count`.
fn encode_award_stats_body(stats: &[(StatKey, i32)]) -> Vec<u8> {
    let resolved: Vec<((i32, i32), i32)> = stats
        .iter()
        .filter_map(|(key, count)| stat_wire_ids(key).map(|ids| (ids, *count)))
        .collect();
    let mut w = Writer::default();
    w.var_i32(i32::try_from(resolved.len()).unwrap_or(i32::MAX));
    for ((type_id, value_id), count) in resolved {
        w.var_i32(type_id);
        w.var_i32(value_id);
        w.var_i32(count);
    }
    w.into_vec()
}

/// Decodes a packet body, asserting the payload was consumed to the last
/// byte. Returns `None` on any decode error or trailing bytes rather than
/// panicking: a malformed packet from the wire should drop that packet, not
/// take down the connection.
fn decode_full<T: Decode>(payload: &[u8]) -> Option<T> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX).ok()?;
    reader.ensure_empty().ok()?;
    Some(value)
}

/// Decodes a `ServerboundCustomPayloadPacket`: a length-prefixed
/// channel identifier (`string(32767)`, the same bound the clientbound
/// direction encodes under in `adapter/connection.rs`), then the channel-specific payload
/// as the remaining bytes verbatim.
///
/// Every channel is lifted into [`ServerBound::CustomPayload`] unchanged, where
/// this crate used to model only `minecraft:brand` and drop everything else.
/// The channel registry and the register/unregister interpretation now live in
/// the version-free server (`lodestone-server`'s `plugin_channels` module); an
/// unregistered channel is dropped there, exactly vanilla's `DiscardedPayload`
/// fallback. `None` on a channel that fails to parse as a [`ResourceKey`] — a
/// malformed packet drops that packet, not the connection (the same convention
/// as [`decode_full`]).
fn decode_custom_payload(payload: &[u8]) -> Option<ServerBound> {
    let mut r = Reader::new(payload);
    let channel = r.string(32767).ok()?;
    let channel: ResourceKey = channel.parse().ok()?;
    Some(ServerBound::CustomPayload {
        channel,
        data: r.remaining_bytes().to_vec(),
    })
}

/// Decodes one serverbound container-click item written as a `HashedStack`
/// (vanilla's own codec library's own optional(vanilla's own hashed-stack shape's own actual item.STREAM_CODEC)`), the
/// inverse of the client-side encoder of the same name
/// (`crate::adapter::write_hashed_stack`): a bool presence flag, then, only
/// if present, the item registry id (VarInt), the count (VarInt), and two
/// VarInt component-patch entry counts (added, removed).
///
/// Our own client always writes `0`/`0` for the two patch counts — see
/// `write_hashed_stack`'s own doc comment, which notes creative slot-set with
/// custom components is out of scope for that encoder too. A **nonzero**
/// count here is therefore either a future client carrying real
/// component-patch entries this decoder has no byte-accurate per-entry
/// layout for, or a malformed packet; either way the safest response is to
/// fail the whole decode rather than guess a skip length and misalign every
/// byte that follows (the same "malformed packet drops the packet, not the
/// connection" convention this module already follows elsewhere).
///
/// Returns `None` on any decode failure, `Some(None)` for an explicitly
/// empty slot, `Some(Some(stack))` for a resolved item. An item id with no
/// entry in the generated table, or a name the wire item-key vocabulary does
/// not accept, is treated as a decode failure for the same reason.
fn read_hashed_stack(r: &mut Reader) -> Option<Option<ItemStack>> {
    if !r.bool().ok()? {
        return Some(None);
    }
    let item_id = r.var_i32().ok()?;
    let count = r.var_i32().ok()?;
    let added = r.var_i32().ok()?;
    let removed = r.var_i32().ok()?;
    if added != 0 || removed != 0 {
        return None;
    }
    let name = item_name(item_id)?;
    let item = name.parse().ok()?;
    let count = u32::try_from(count).ok()?;
    Some(Some(ItemStack::new(item, count)))
}

/// Decodes the serverbound `container_click` packet body into
/// [`ServerBound::ContainerClicked`].
///
/// Wire layout (`ServerboundContainerClickPacket`, mirrors the client-side
/// encoder `crate::adapter::encode_container_click` exactly): VarInt
/// container id, VarInt state id, big-endian `short` slot, big-endian `byte`
/// button, `ContainerInput` ordinal (VarInt), a changed-slots map (VarInt
/// entry count, then per entry a big-endian `short` slot key and a
/// [`read_hashed_stack`] value), then the carried cursor stack, also a
/// [`read_hashed_stack`].
///
/// The clicked slot/button/click-type fields **are** carried into
/// [`ServerBound`]: `lodestone-server`'s `container_click::do_click` re-derives
/// the whole menu state from them, the way vanilla's own `doClick` does.
/// `changed_slots`/`carried_item` come along as the client's *prediction*, which
/// the consumer compares against and never stores — see that variant's own doc
/// comment.
fn decode_container_click(payload: &[u8]) -> Option<ServerBound> {
    let mut r = Reader::new(payload);
    let window_id = r.var_i32().ok()?;
    let state_id = r.var_i32().ok()?;
    let slot = i32::from(r.i16().ok()?);
    let button = r.i8().ok()?;
    let click_type = r.var_i32().ok()?;
    let count = r.var_i32().ok()?;
    let count = usize::try_from(count).ok()?;
    // No `Vec::with_capacity(count)`: `count` is attacker-controlled and
    // unrelated to `payload`'s actual length until each entry is read, so
    // pre-allocating it would let a short, malformed packet request an
    // enormous allocation before the first bounds check ever fails.
    let mut changed_slots = Vec::new();
    for _ in 0..count {
        let slot = i32::from(r.i16().ok()?);
        let item = read_hashed_stack(&mut r)?;
        changed_slots.push((slot, item));
    }
    let carried_item = read_hashed_stack(&mut r)?;
    r.ensure_empty().ok()?;
    Some(ServerBound::ContainerClicked {
        window_id,
        state_id,
        slot,
        button,
        click_type,
        changed_slots,
        carried_item,
    })
}

/// Reads a serverbound `set_creative_mode_slot` item
/// (vanilla's own item-stack type's own optional-untrusted-stream-codec accessor, the inverse of the
/// client-side encoder `crate::adapter::write_optional_item_stack`): a VarInt
/// count where `<= 0` means empty, otherwise the item registry id as a
/// VarInt, then an empty `DataComponentPatch` (two VarInt `0`s, added then
/// removed).
///
/// Deliberately **not** the same shape as [`read_hashed_stack`]: that one has
/// a leading presence bool and puts the item id before the count
/// (vanilla's own hashed-stack actual-item stream codec); this one has no
/// presence bool at
/// all — a `count` of zero or less *is* the absence marker
/// (vanilla's own optional-item-stack-codec factory, verified against
/// the decompiled 26.2 item-stack source) — and puts
/// the count first. Conflating the two would silently misalign every byte
/// that follows.
///
/// A nonzero component-patch count is treated as a decode failure for the
/// same reason [`read_hashed_stack`] does: this crate's canonical
/// [`ItemStack`] carries no components, so there is no way to apply a
/// nonempty patch, and guessing a skip length would misalign the rest of the
/// packet.
fn read_optional_item_stack(r: &mut Reader) -> Option<Option<ItemStack>> {
    let count = r.var_i32().ok()?;
    if count <= 0 {
        return Some(None);
    }
    let item_id = r.var_i32().ok()?;
    let added = r.var_i32().ok()?;
    let removed = r.var_i32().ok()?;
    if added != 0 || removed != 0 {
        return None;
    }
    let name = item_name(item_id)?;
    let item = name.parse().ok()?;
    let count = u32::try_from(count).ok()?;
    Some(Some(ItemStack::new(item, count)))
}

/// Reads one serverbound `set_beacon` mob-effect slot
/// (vanilla's own codec library's own optional(vanilla's own mob-effect type's own stream codec)`, the inverse of
/// `crate::adapter::write_optional_mob_effect`): a bool presence flag, then,
/// only if present, the effect's `minecraft:mob_effect` registry id as a
/// direct VarInt.
///
/// Returns the effect's canonical name on success — this module's own
/// `SET_BEACON` decode arm lifts both calls straight into a real
/// `ServerBound::SetBeacon`.
fn read_optional_mob_effect(r: &mut Reader) -> Option<Option<&'static str>> {
    if !r.bool().ok()? {
        return Some(None);
    }
    let id = r.var_i32().ok()?;
    Some(Some(mob_effect_name(id)?))
}

/// Packs a block position into vanilla's vanilla's own block-position type's own as long form: `x` in the
/// high 26 bits, `z` in the middle 26 bits, `y` in the low 12 bits.
fn pack_block_pos(x: i32, y: i32, z: i32) -> i64 {
    ((i64::from(x) & 0x3FF_FFFF) << 38)
        | ((i64::from(z) & 0x3FF_FFFF) << 12)
        | (i64::from(y) & 0xFFF)
}

/// Hand-written encoder for the clientbound `player_position` (teleport)
/// packet, which has no existing struct in `packets::game` because it is
/// currently only ever *decoded* (see `V770Adapter::handle_player_position`).
///
/// Wire layout (mirrors the decode side exactly): VarInt teleport id, position
/// `f64`×3, delta-movement `f64`×3 (zero — an absolute teleport carries no
/// velocity), yaw/pitch `f32`, then a big-endian `i32` relative-flags bit set
/// (`0` — every field here is absolute).
fn encode_player_position_teleport(
    id: i32,
    x: f64,
    y: f64,
    z: f64,
    yaw: f32,
    pitch: f32,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(id);
    w.f64(x);
    w.f64(y);
    w.f64(z);
    w.f64(0.0);
    w.f64(0.0);
    w.f64(0.0);
    w.f32(yaw);
    w.f32(pitch);
    w.i32(0);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `set_chunk_cache_center` packet:
/// two VarInt chunk coordinates, no other fields.
fn encode_chunk_cache_center_body(cx: i32, cz: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(cx);
    w.var_i32(cz);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `forget_level_chunk` packet: a
/// single packed `i64` — `x` in the low 32 bits, `z` in the high 32 — mirroring
/// vanilla's vanilla's own chunk-position type's own pack exactly as `V770Adapter::handle_play`'s
/// `FORGET_LEVEL_CHUNK` decode arm already reads it (`adapter/chunk.rs`, the
/// `packed as i32` / `(packed >> 32) as i32` pair).
fn encode_forget_chunk_body(cx: i32, cz: i32) -> Vec<u8> {
    let packed = (i64::from(cx) & 0xFFFF_FFFF) | ((i64::from(cz) & 0xFFFF_FFFF) << 32);
    let mut w = Writer::default();
    w.i64(packed);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `block_update` packet: a packed
/// `BlockPos` long ([`pack_block_pos`]) followed by a VarInt block-state
/// registry id — mirrors vanilla's own clientbound block-update packet's own stream codec
/// (vanilla's own block-position type's own stream codec composed with vanilla's own codec library's own id mapper(Block
/// .BLOCK_STATE_REGISTRY)`, vanilla's own clientbound block-update packet's own stream codec) and
/// this crate's own decode of the same packet in `V770Adapter::handle_play`'s
/// `BLOCK_UPDATE` arm (`adapter/chunk.rs`), which reads the identical
/// packed-i64-then-VarInt shape.
fn encode_block_update_body(x: i32, y: i32, z: i32, state_id: u32) -> Vec<u8> {
    let mut w = Writer::default();
    w.i64(pack_block_pos(x, y, z));
    w.var_i32(state_id as i32);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `game_event` packet (wire id 38),
/// the small keyed world-state channel vanilla uses for weather transitions.
/// Wire layout: an unsigned byte event id, then a big-endian `f32` param —
/// exactly `ClientboundGameEventPacket`'s `writeByte(event) + writeFloat(param)`
/// (vanilla's own clientbound game-event packet's own stream codec), and exactly the shape
/// `packets::game::GameEvent`'s `Decode` impl reads back on this crate's own
/// client side (`V770Adapter`'s `GAME_EVENT` arm, `adapter/chunk.rs`).
fn game_event_body(kind: u8, value: f32) -> Vec<u8> {
    let mut w = Writer::default();
    w.u8(kind);
    w.f32(value);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `set_time` packet, mirroring
/// `packets::time::SetTime`'s `Decode` impl exactly (that struct has no
/// `Encode` impl to reuse — the map-valued field it decodes cannot come from
/// an existing bidirectional struct the way this module's other reused types
/// do, so a hand-written mirror is the documented fallback here, same as
/// `encode_player_position_teleport`/`encode_chunk_cache_center_body` above).
///
/// Wire layout: `i64` `game_time`, then a VarInt-counted list of clock
/// updates. `day_time` of `None` sends an empty list (vanilla's
/// once-a-second `forceGameTimeSynchronization` broadcast, which
/// deliberately leaves the client's held day/night anchor untouched — see
/// `packets::time::SetTime::day_clock`'s doc comment). `day_time` of
/// `Some(total_ticks)` sends exactly one update, anchoring the overworld
/// clock (`OVERWORLD_CLOCK_HOLDER_ID`) to `total_ticks` at the normal 1:1
/// rate: a **plain** VarInt holder id (no `+1`/inline convention — see
/// `packets::time::ClockUpdate::holder_id`'s doc comment on why that
/// differs from the *other* holder codec), a VarLong tick count, then two
/// big-endian `f32`s (partial tick `0.0`, rate `1.0`).
fn encode_set_time_body(game_time: i64, day_time: Option<i64>) -> Vec<u8> {
    let mut w = Writer::default();
    w.i64(game_time);
    match day_time {
        Some(total_ticks) => {
            w.var_i32(1);
            w.var_i32(OVERWORLD_CLOCK_HOLDER_ID);
            w.var_i64(total_ticks);
            w.f32(0.0); // partial_tick
            w.f32(1.0); // rate: normal day/night speed, never paused
        }
        None => {
            w.var_i32(0);
        }
    }
    w.into_vec()
}

/// The `minecraft:command_argument_type` registry id of one parser, plus that
/// parser's own network payload, written to `w`.
///
/// # Where the ids come from
///
/// `.cache/mc/26.2/generated/reports/registries.json`'s
/// `minecraft:command_argument_type` block, whose `protocol_id`s are `0..=56` in
/// exactly this order. Mojang's generator, not a community table and not this
/// crate's own decoder — which matters, because writing the encoder from
/// `V770Adapter`'s `read_argument_parser` alone would inherit any mistake that
/// already had. The two agree; that agreement is now evidence rather than an
/// assumption.
///
/// # Payloads, each from its own `ArgumentTypeInfo::serializeToNetwork`
///
/// | parsers | payload |
/// |---|---|
/// | the four Brigadier numerics | vanilla's own argument-utils helper's own create number flags byte (bit 0 min, bit 1 max) then only the **present** bounds |
/// | `brigadier:string` | `writeEnum`, i.e. a VarInt `StringType` ordinal |
/// | `minecraft:entity` | one flags byte, bit 0 `single`, bit 1 `playersOnly` |
/// | `minecraft:score_holder` | one flags byte, bit 0 `multiple` |
/// | `minecraft:time` | a bare big-endian `int` minimum — **no** flags byte |
/// | the five `resource*` | `writeResourceKey` → `writeIdentifier`, one VarInt-length UTF-8 string |
/// | everything else | nothing at all (`SingletonArgumentInfo::serializeToNetwork` is empty) |
///
/// A bound is *absent* exactly when it equals its type's sentinel — vanilla's own
/// test is `template.min != the JDK's own integer type's own min-value accessor and, for the floating types,
/// `!= -the JDK's own float type's own max-value accessor / the JDK's own float type's own max-value accessor. So the flags byte is derived here
/// from the same comparison rather than from a separate "has bound" field, which
/// is what keeps it in step with the decoder's mirror-image reconstruction.
fn write_argument_parser(w: &mut Writer, parser: &ArgumentParser) {
    /// vanilla's own argument-utils helper's own number-flag-min accessor.
    const HAS_MIN: u8 = 1;
    /// vanilla's own argument-utils helper's own number-flag-max accessor.
    const HAS_MAX: u8 = 2;

    match parser {
        ArgumentParser::Bool => w.var_i32(0),
        ArgumentParser::Float { min, max } => {
            w.var_i32(1);
            let has_min = *min != -f32::MAX;
            let has_max = *max != f32::MAX;
            w.u8((u8::from(has_min) * HAS_MIN) | (u8::from(has_max) * HAS_MAX));
            if has_min {
                w.f32(*min);
            }
            if has_max {
                w.f32(*max);
            }
        }
        ArgumentParser::Double { min, max } => {
            w.var_i32(2);
            let has_min = *min != -f64::MAX;
            let has_max = *max != f64::MAX;
            w.u8((u8::from(has_min) * HAS_MIN) | (u8::from(has_max) * HAS_MAX));
            if has_min {
                w.f64(*min);
            }
            if has_max {
                w.f64(*max);
            }
        }
        ArgumentParser::Integer { min, max } => {
            w.var_i32(3);
            let has_min = *min != i32::MIN;
            let has_max = *max != i32::MAX;
            w.u8((u8::from(has_min) * HAS_MIN) | (u8::from(has_max) * HAS_MAX));
            if has_min {
                w.i32(*min);
            }
            if has_max {
                w.i32(*max);
            }
        }
        ArgumentParser::Long { min, max } => {
            w.var_i32(4);
            let has_min = *min != i64::MIN;
            let has_max = *max != i64::MAX;
            w.u8((u8::from(has_min) * HAS_MIN) | (u8::from(has_max) * HAS_MAX));
            if has_min {
                w.i64(*min);
            }
            if has_max {
                w.i64(*max);
            }
        }
        ArgumentParser::String(kind) => {
            w.var_i32(5);
            w.var_i32(match kind {
                StringKind::SingleWord => 0,
                StringKind::QuotablePhrase => 1,
                StringKind::GreedyPhrase => 2,
            });
        }
        ArgumentParser::Entity { single, players_only } => {
            w.var_i32(6);
            w.u8(u8::from(*single) | u8::from(*players_only) << 1);
        }
        ArgumentParser::GameProfile => w.var_i32(7),
        ArgumentParser::BlockPos => w.var_i32(8),
        ArgumentParser::ColumnPos => w.var_i32(9),
        ArgumentParser::Vec3 => w.var_i32(10),
        ArgumentParser::Vec2 => w.var_i32(11),
        ArgumentParser::BlockState => w.var_i32(12),
        ArgumentParser::BlockPredicate => w.var_i32(13),
        ArgumentParser::ItemStack => w.var_i32(14),
        ArgumentParser::ItemPredicate => w.var_i32(15),
        ArgumentParser::TeamColor => w.var_i32(16),
        ArgumentParser::HexColor => w.var_i32(17),
        ArgumentParser::Component => w.var_i32(18),
        ArgumentParser::Style => w.var_i32(19),
        ArgumentParser::Message => w.var_i32(20),
        ArgumentParser::NbtCompoundTag => w.var_i32(21),
        ArgumentParser::NbtTag => w.var_i32(22),
        ArgumentParser::NbtPath => w.var_i32(23),
        ArgumentParser::Objective => w.var_i32(24),
        ArgumentParser::ObjectiveCriteria => w.var_i32(25),
        ArgumentParser::Operation => w.var_i32(26),
        ArgumentParser::Particle => w.var_i32(27),
        ArgumentParser::Angle => w.var_i32(28),
        ArgumentParser::Rotation => w.var_i32(29),
        ArgumentParser::ScoreboardSlot => w.var_i32(30),
        ArgumentParser::ScoreHolder { multiple } => {
            w.var_i32(31);
            w.u8(u8::from(*multiple));
        }
        ArgumentParser::Swizzle => w.var_i32(32),
        ArgumentParser::Team => w.var_i32(33),
        ArgumentParser::ItemSlot => w.var_i32(34),
        ArgumentParser::ItemSlots => w.var_i32(35),
        ArgumentParser::ResourceLocation => w.var_i32(36),
        ArgumentParser::Function => w.var_i32(37),
        ArgumentParser::EntityAnchor => w.var_i32(38),
        ArgumentParser::IntRange => w.var_i32(39),
        ArgumentParser::FloatRange => w.var_i32(40),
        ArgumentParser::Dimension => w.var_i32(41),
        ArgumentParser::GameMode => w.var_i32(42),
        ArgumentParser::Time { min } => {
            w.var_i32(43);
            w.i32(*min);
        }
        ArgumentParser::ResourceOrTag { registry } => {
            w.var_i32(44);
            w.string(&registry.to_string());
        }
        ArgumentParser::ResourceOrTagKey { registry } => {
            w.var_i32(45);
            w.string(&registry.to_string());
        }
        ArgumentParser::Resource { registry } => {
            w.var_i32(46);
            w.string(&registry.to_string());
        }
        ArgumentParser::ResourceKeyArg { registry } => {
            w.var_i32(47);
            w.string(&registry.to_string());
        }
        ArgumentParser::ResourceSelector { registry } => {
            w.var_i32(48);
            w.string(&registry.to_string());
        }
        ArgumentParser::TemplateMirror => w.var_i32(49),
        ArgumentParser::TemplateRotation => w.var_i32(50),
        ArgumentParser::Heightmap => w.var_i32(51),
        ArgumentParser::LootTable => w.var_i32(52),
        ArgumentParser::LootPredicate => w.var_i32(53),
        ArgumentParser::LootModifier => w.var_i32(54),
        ArgumentParser::Dialog => w.var_i32(55),
        ArgumentParser::Uuid => w.var_i32(56),
        // A parser id this build does not model. Nothing but the raw id can be
        // written — the payload was never decoded, so there is none to reproduce —
        // and that is exactly what our own decoder assumes for an unknown id, so
        // the two ends stay aligned. Unreachable from a decode (an unmodeled id
        // becomes `NodeKind::Unrecognized`, handled by the node writer) and
        // unreachable from `lodestone-server`'s projection, which only ever names
        // parsers its own `McArg`s declare.
        ArgumentParser::Unknown(id) => w.var_i32(*id),
    }
}

/// Writes one vanilla's own clientbound commands packet's own entry: `Entry::write`'s exact order —
/// the flags byte, the child-index array (`writeVarIntArray`, so a VarInt count
/// then VarInt elements), the redirect index **only** when `FLAG_REDIRECT` is
/// set, then the type-dependent stub.
///
/// The stub order for an argument is `writeUtf(name)`, the parser id, the parser
/// payload, and only then the custom-suggestions identifier — the suggestions id
/// comes **after** the payload, which is the one field order here that cannot be
/// guessed from field names and which `ArgumentNodeStub::write` fixes.
///
/// A [`NodeKind::Unrecognized`] node is written as a **root-type** entry, keeping
/// its children, redirect and executable bit. That is not a fallback invented
/// here: it is what a client already does with such a node, since
/// vanilla's own clientbound commands packet's own read returns a null stub and vanilla's own command-node resolver's own resolve
/// builds a bare `RootCommandNode` for it. Re-encoding it as an argument is
/// impossible anyway — a node that failed to decode carries neither a name nor a
/// payload.
fn write_command_node(w: &mut Writer, node: &RawCommandNode) {
    /// `TYPE_ROOT`, and the type of an unrecognised node's degraded form.
    const TYPE_ROOT: u8 = 0;
    /// `TYPE_LITERAL`.
    const TYPE_LITERAL: u8 = 1;
    /// `TYPE_ARGUMENT`.
    const TYPE_ARGUMENT: u8 = 2;
    /// `FLAG_EXECUTABLE`.
    const EXECUTABLE: u8 = 4;
    /// `FLAG_REDIRECT`.
    const REDIRECT: u8 = 8;
    /// `FLAG_CUSTOM_SUGGESTIONS`.
    const CUSTOM_SUGGESTIONS: u8 = 16;
    /// `FLAG_RESTRICTED`.
    const RESTRICTED: u8 = 32;

    let mut flags = match &node.kind {
        NodeKind::Root | NodeKind::Unrecognized { .. } => TYPE_ROOT,
        NodeKind::Literal { .. } => TYPE_LITERAL,
        NodeKind::Argument { .. } => TYPE_ARGUMENT,
    };
    if node.executable {
        flags |= EXECUTABLE;
    }
    if node.redirect.is_some() {
        flags |= REDIRECT;
    }
    if node.restricted {
        flags |= RESTRICTED;
    }
    if let NodeKind::Argument { suggestions: Some(_), .. } = &node.kind {
        flags |= CUSTOM_SUGGESTIONS;
    }
    w.u8(flags);

    // `writeVarIntArray`: count then elements. The cast is checked against the
    // node count by the caller, which is the only place that knows it.
    w.var_i32(node.children.len() as i32);
    for &child in &node.children {
        w.var_i32(child as i32);
    }
    if let Some(redirect) = node.redirect {
        w.var_i32(redirect as i32);
    }
    match &node.kind {
        NodeKind::Root | NodeKind::Unrecognized { .. } => {}
        NodeKind::Literal { name } => w.string(name),
        NodeKind::Argument { name, parser, suggestions } => {
            w.string(name);
            write_argument_parser(w, parser);
            if let Some(provider) = suggestions {
                w.string(&provider.to_string());
            }
        }
    }
}

/// Encodes a whole `minecraft:commands` payload (clientbound id 16).
///
/// `ClientboundCommandsPacket::write` is `writeCollection(entries, …)` then
/// `writeVarInt(rootIndex)` — the node list **first**, the root index last, which
/// is the mirror of `V770Adapter`'s `decode_command_tree` and the ordering a
/// round-trip cannot catch you getting wrong if both ends agree wrongly. Read
/// against the vanilla record, not against the decoder.
fn encode_commands_body(tree: &WireCommandTree) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(tree.len() as i32);
    for index in 0..tree.len() {
        let node = tree.node(index).expect("index < len is always in range");
        write_command_node(&mut w, node);
    }
    w.var_i32(tree.root() as i32);
    w.into_vec()
}

/// Encodes a whole `minecraft:command_suggestions` payload (clientbound id 15).
///
/// vanilla's own clientbound command-suggestions packet's own stream codec (mirrored from the
/// decode side in `V770Adapter::decode_command_suggestions`, which this crate's
/// own client half uses to read a *real* server's reply): three VarInts (`id`,
/// `start`, `length`), then a list of `Entry(String text, Optional<Component>
/// tooltip)`. This server never attaches a tooltip to a suggestion — the
/// `false` presence byte matches `CommandSuggestionEntry::tooltip` being
/// `None` for every candidate `ServerCommands::suggest` produces — so the
/// `Some` arm below has no production caller today, but it encodes through
/// [`command_suggestion_tooltip_nbt`] rather than a bare `{"text": ...}`
/// compound precisely so a future caller that does attach a styled tooltip
/// does not silently lose it the way the client-side decode used to.
fn encode_command_suggestions_body(response: &CommandSuggestionsResponse) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(response.id);
    w.var_i32(response.start);
    w.var_i32(response.length);
    w.var_i32(response.suggestions.len() as i32);
    for entry in &response.suggestions {
        w.string(&entry.text);
        match &entry.tooltip {
            Some(tooltip) => {
                w.bool(true);
                write_network_nbt(&mut w, &command_suggestion_tooltip_nbt(tooltip))
                    .expect("a command-suggestion tooltip built from a `Text` always encodes");
            }
            None => w.bool(false),
        }
    }
    w.into_vec()
}

/// Lowers a [`Text`] to a network-NBT chat component for the
/// `minecraft:command_suggestions` tooltip field — the exact mirror of
/// `V770Adapter::decode_command_suggestions`'s read side (`Text::from_nbt`),
/// field for field: `text`/`translate`/`with`/`fallback`/`extra` plus every
/// [`TextStyle`] field `lodestone_model::text::nbt_style` reads back
/// (`color`, `bold`, `italic`, `underlined`, `strikethrough`, `obfuscated`,
/// `font`).
///
/// Deliberately **not** [`text_to_nbt`]: that function is scoped to the
/// disconnect-reason field alone and its own doc forbids reuse as a general
/// serializer, precisely because it drops style — the one thing a command
/// suggestion tooltip exists to carry (a hex colour has no legacy-code
/// fallback). Click/hover/insertion are still omitted, matching
/// [`text_to_nbt`]'s scope for the same reason: a tab-complete tooltip is a
/// hover-only informational popup with no interactivity of its own to carry.
fn command_suggestion_tooltip_nbt(text: &Text) -> Nbt {
    let mut fields: Vec<(String, Nbt)> = Vec::new();
    match &text.content {
        TextContent::Literal(literal) => {
            fields.push(("text".to_owned(), Nbt::String(literal.clone())));
        }
        TextContent::Translate {
            key,
            with,
            fallback,
        } => {
            fields.push(("translate".to_owned(), Nbt::String(key.clone())));
            if let Some(fallback) = fallback {
                fields.push(("fallback".to_owned(), Nbt::String(fallback.clone())));
            }
            if !with.is_empty() {
                fields.push((
                    "with".to_owned(),
                    Nbt::List {
                        element_type: NbtTag::Compound,
                        elements: with.iter().map(command_suggestion_tooltip_nbt).collect(),
                    },
                ));
            }
        }
    }
    let style = &text.style;
    if let Some(color) = style.color {
        fields.push(("color".to_owned(), Nbt::String(color.name())));
    }
    if let Some(bold) = style.bold {
        fields.push(("bold".to_owned(), Nbt::Byte(bold as i8)));
    }
    if let Some(italic) = style.italic {
        fields.push(("italic".to_owned(), Nbt::Byte(italic as i8)));
    }
    if let Some(underlined) = style.underlined {
        fields.push(("underlined".to_owned(), Nbt::Byte(underlined as i8)));
    }
    if let Some(strikethrough) = style.strikethrough {
        fields.push(("strikethrough".to_owned(), Nbt::Byte(strikethrough as i8)));
    }
    if let Some(obfuscated) = style.obfuscated {
        fields.push(("obfuscated".to_owned(), Nbt::Byte(obfuscated as i8)));
    }
    if let Some(font) = style.font {
        fields.push(("font".to_owned(), Nbt::String(font.name().to_owned())));
    }
    if !text.extra.is_empty() {
        fields.push((
            "extra".to_owned(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: text.extra.iter().map(command_suggestion_tooltip_nbt).collect(),
            },
        ));
    }
    Nbt::Compound(fields)
}

/// Hand-written encoder for the clientbound `system_chat` packet, which has no
/// existing struct because it is currently only ever *decoded* (see
/// `V770Adapter::handle_play`'s `SYSTEM_CHAT` arm). Wire layout (mirrors the
/// decode side exactly): a network-form NBT text component (root tag id +
/// payload, no root name — vanilla's vanilla's own component-serialization helper's own trusted-stream-codec accessor),
/// then a big-endian `bool` overlay flag (`false` selects normal chat history,
/// `true` the action-bar overlay).
fn encode_system_chat(message: &str, overlay: bool) -> Vec<u8> {
    let component = Nbt::Compound(vec![("text".to_owned(), Nbt::String(message.to_owned()))]);
    let mut w = Writer::default();
    write_network_nbt(&mut w, &component).expect("plain string NBT component always encodes");
    w.bool(overlay);
    w.into_vec()
}

/// Lowers a server→client plugin-channel payload,
/// `ClientboundCustomPayloadPacket`: a VarInt-prefixed channel identifier, then
/// the channel-specific payload verbatim. Hand-written, in the same "no
/// existing struct" style as [`encode_system_chat`] — the client side only
/// *decodes* this packet, and that decoder (`adapter/connection.rs`'s `decode_custom_payload`,
/// which reads exactly this shape) is the mirror-side specification. Both the
/// Configuration and Play clientbound ids share this body.
fn encode_custom_payload_body(channel: &ResourceKey, data: &[u8]) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(&channel.to_string());
    w.bytes(data);
    w.into_vec()
}

/// Lowers a [`Text`] to a network-NBT chat component, for the **disconnect
/// reason** field.
///
/// # Scope, stated because a partial serializer is a trap
///
/// This is **not** a general `Text` → NBT serializer, and must not be reused as
/// one. It writes exactly the three things a disconnect reason carries —
/// `text`, `translate` (with `fallback` and `with`), and `extra` — and
/// **deliberately drops style, click, hover and insertion**, because a
/// disconnect reason renders on the "connection lost" screen, which has no
/// interactivity and (in vanilla) applies its own styling. Passing a styled
/// component through here would silently lose the styling, which is why the
/// function is private and named for its one caller. A general serializer
/// belongs in `lodestone-model` next to `Text::from_nbt`, as its inverse.
///
/// The shape is pinned by the *decoder* on the other side of the same wire:
/// `V770Adapter`'s `nbt_reason_text` reads this with `read_network_nbt` +
/// `Text::from_nbt`, and that decoder has been validated against real servers'
/// disconnect packets. Field names follow vanilla's own component codecs —
/// a string field named `"translate"` and the optional `"fallback"` beside it
/// (confirmed against the decompiled translatable-contents source).
fn text_to_nbt(text: &Text) -> Nbt {
    let mut fields: Vec<(String, Nbt)> = Vec::new();
    match &text.content {
        TextContent::Literal(literal) => {
            fields.push(("text".to_owned(), Nbt::String(literal.clone())));
        }
        TextContent::Translate {
            key,
            with,
            fallback,
        } => {
            fields.push(("translate".to_owned(), Nbt::String(key.clone())));
            if let Some(fallback) = fallback {
                fields.push(("fallback".to_owned(), Nbt::String(fallback.clone())));
            }
            if !with.is_empty() {
                fields.push(("with".to_owned(), component_list(with)));
            }
        }
    }
    if !text.extra.is_empty() {
        fields.push(("extra".to_owned(), component_list(&text.extra)));
    }
    Nbt::Compound(fields)
}

/// An NBT list of chat components — every element a `TAG_Compound`, which is the
/// `element_type` a wire NBT list carries in its header. Only reached for a
/// non-empty slice: an *empty* NBT list would need an element tag with no element
/// to derive it from, and both callers guard on `is_empty` for that reason.
fn component_list(texts: &[Text]) -> Nbt {
    Nbt::List {
        element_type: NbtTag::Compound,
        elements: texts.iter().map(text_to_nbt).collect(),
    }
}

/// Serializes a disconnect reason into the raw network-NBT payload the
/// Configuration- and Play-phase `ClientboundDisconnectPacket` carries: the
/// component alone, with no wrapper fields, which is why there is no struct to
/// derive `Encode` from. Same `write_network_nbt` path `encode_system_chat` uses.
fn encode_component_nbt(text: &Text) -> Vec<u8> {
    let mut w = Writer::default();
    write_network_nbt(&mut w, &text_to_nbt(text))
        .expect("a chat component built from a `Text` always encodes into a `Vec<u8>` writer");
    w.into_vec()
}

/// The JSON twin of [`text_to_nbt`], for the **login**-phase disconnect only.
///
/// The login phase predates NBT components on the wire, so
/// vanilla's own clientbound login-disconnect packet still carries its
/// reason as a
/// length-prefixed JSON string (its own lenient-JSON stream codec, capped at
/// 262144) while the Configuration and
/// Play clientbound disconnect packet carries NBT. Writing NBT in the login phase
/// produces a packet a real client cannot parse, which is the single easiest
/// mistake to make here — hence two functions rather than one, with the same
/// field names and the same deliberate omissions (see [`text_to_nbt`]'s scope
/// note).
fn text_to_json(text: &Text) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    match &text.content {
        TextContent::Literal(literal) => {
            object.insert(
                "text".to_owned(),
                serde_json::Value::String(literal.clone()),
            );
        }
        TextContent::Translate {
            key,
            with,
            fallback,
        } => {
            object.insert("translate".to_owned(), serde_json::Value::String(key.clone()));
            if let Some(fallback) = fallback {
                object.insert(
                    "fallback".to_owned(),
                    serde_json::Value::String(fallback.clone()),
                );
            }
            if !with.is_empty() {
                object.insert(
                    "with".to_owned(),
                    serde_json::Value::Array(with.iter().map(text_to_json).collect()),
                );
            }
        }
    }
    if !text.extra.is_empty() {
        object.insert(
            "extra".to_owned(),
            serde_json::Value::Array(text.extra.iter().map(text_to_json).collect()),
        );
    }
    serde_json::Value::Object(object)
}

/// Base64-encodes `bytes` with the standard RFC 4648 alphabet and `=`
/// padding — the exact inverse of `lodestone_net::status::decode_base64`, which
/// this crate's *client* half already uses to read a real server's favicon.
///
/// Hand-rolled for the same reason that decoder is: it is a dozen lines, and
/// vanilla's favicon field is the only thing in this file that needs base64 at
/// all (vanilla's own server-status favicon codec is literally a standard
/// base64 encode behind a fixed prefix, confirmed against the decompiled
/// 26.2 source). Standard alphabet, not base64url: vanilla
/// uses the JDK's standard encoder, which is the `+`/`/` variant.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        // Pack the (1..=3) input bytes left-aligned into 24 bits, then peel
        // off four 6-bit groups, emitting `=` for any group with no input
        // bits behind it at all.
        let mut buf = [0u8; 3];
        buf[..chunk.len()].copy_from_slice(chunk);
        let packed = (u32::from(buf[0]) << 16) | (u32::from(buf[1]) << 8) | u32::from(buf[2]);
        for group in 0..4 {
            if group <= chunk.len() {
                let index = (packed >> (18 - 6 * group)) & 0x3f;
                out.push(char::from(ALPHABET[index as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Serializes vanilla's own server-status document
/// (confirmed against the decompiled 26.2 source) into the JSON body of a
/// `status_response` packet.
///
/// Field-by-field against that record's codec, in vanilla's own declaration
/// order:
///
/// | JSON key | vanilla source | notes |
/// |---|---|---|
/// | `description` | vanilla's own component-serialization helper's own codec accessor | written as `{"text": …}` |
/// | `players` | vanilla's own status-response players record's own codec accessor (`:53-60`) | `max`, `online`, `sample` |
/// | `version` | vanilla's own status-response version record's own codec accessor (`:64-69`) | `name`, `protocol` |
/// | `favicon` | vanilla's own favicon codec holder's own codec accessor (`:37-49`) | `data:image/png;base64,…` |
/// | `enforcesSecureChat` | vanilla's own codec type's own bool accessor (`:30`) | omitted when `false` |
///
/// Two deliberate choices about *omission*, both licensed by that codec rather
/// than guessed. `players`, `version`, `favicon` and `enforcesSecureChat` are
/// each `lenientOptionalFieldOf`, so a missing key is legal — but `players` and
/// `version` are what a client's server-list row actually renders, so they are
/// always written. `favicon` is omitted entirely when there is no icon (an
/// empty-string favicon is *not* legal: vanilla's own favicon codec holder's own codec accessor errors with
/// `"Unknown format"` on anything lacking the prefix, `:38-40`), and
/// `enforcesSecureChat` is omitted when `false` because that is its declared
/// default (`:30`) and vanilla's own encoder drops defaulted optional fields.
///
/// `description` is written as a `{"text": …}` object rather than a bare JSON
/// string. **A live 26.2 server emits the bare-string form** for a MOTD set in
/// `server.properties` — captured, not assumed; see
/// `tests/fixtures/vanilla_status_response_26_2.json`, whose `description` is
/// the string `"Lodestone survival test world"` with no wrapper. `Component`'s
/// serializer collapses a plain literal that way. This function deliberately
/// does *not* match that, because both forms decode
/// (vanilla's own component-serialization helper's own codec accessor accepts either, and our own client-side
/// `lodestone_net::status::parse_status_json` has gates for both) and the object
/// form is unambiguous for a MOTD that happens to look like a number, `true`, or
/// `null` — which the bare-string form would still encode correctly but which is
/// one fewer thing to reason about. If a future gate ever needs byte-identity
/// with vanilla's own output, this is the field that will differ, and this
/// paragraph is why.
fn encode_status_response_body(
    description: &str,
    players_online: i32,
    players_max: i32,
    sample: &[(Uuid, String)],
    favicon_png: Option<&[u8]>,
    enforces_secure_chat: bool,
) -> Vec<u8> {
    let mut document = serde_json::Map::new();
    document.insert(
        "description".to_owned(),
        serde_json::json!({ "text": description }),
    );
    document.insert(
        "players".to_owned(),
        serde_json::json!({
            "max": players_max,
            "online": players_online,
            // Vanilla's own name-and-id codec keys these `id` and `name`, and writes the
            // uuid through its own string-form UUID codec — the hyphenated string
            // form, not the two-longs array a *packet* field would use
            // (confirmed against the decompiled name-and-id record source).
            "sample": sample
                .iter()
                .map(|(id, name)| serde_json::json!({ "id": id.to_string(), "name": name }))
                .collect::<Vec<_>>(),
        }),
    );
    document.insert(
        "version".to_owned(),
        serde_json::json!({ "name": MINECRAFT_VERSION, "protocol": crate::PROTOCOL }),
    );
    if let Some(png) = favicon_png {
        document.insert(
            "favicon".to_owned(),
            serde_json::Value::String(format!("data:image/png;base64,{}", base64_encode(png))),
        );
    }
    if enforces_secure_chat {
        document.insert(
            "enforcesSecureChat".to_owned(),
            serde_json::Value::Bool(true),
        );
    }

    let json = serde_json::Value::Object(document).to_string();
    let mut w = Writer::default();
    w.string(&json);
    w.into_vec()
}

/// Writes one vanilla's own item-stack type's own optional-stream-codec accessor value (used by both
/// `container_set_content`'s list/carried entries and `container_set_slot`'s
/// single item): a VarInt count (`<= 0` is the empty stack), then, only if
/// non-empty, the item registry id as a VarInt and an empty
/// `DataComponentPatch` (VarInt `0` added, VarInt `0` removed).
///
/// This is the clientbound twin of `adapter::serverbound::write_optional_item_stack`
/// (the serverbound `set_creative_mode_slot` encoder), restated here rather
/// than imported: that function is private to its own module, and there is
/// no shared `pub(crate)` export for it. Both directions genuinely share the same
/// wire shape (vanilla's own item-stack type's own optional-stream-codec accessor is the same stream codec
/// constant either way), so this restatement is the same "no existing struct
/// to derive `Encode` from" situation `encode_system_chat` is already in, not
/// a new inconsistency. An item whose canonical key has no entry in the
/// generated registry table (should not happen for anything this crate's own
/// block-entity/inventory models can produce) degrades to writing an empty
/// stack rather than panicking or corrupting the rest of the packet.
/// Resolves a `minecraft:*` key to its wire *holder* value (`id + 1`, `0` if
/// unresolved) for one of `entity_variants`'s id-to-name tables
/// (`villager_type`/`villager_profession`), searching by name rather than
/// duplicating either table here — both are `pub fn`s in
/// `crate::entity_variants`, which this crate owns, so this stays a single
/// small hunk rather than a second copy of either list to drift from the
/// first. `32` covers both tables with room to spare (7 villager types, 15
/// professions in the 26.2 jar).
fn villager_registry_wire_id(lookup: fn(i32) -> Option<&'static str>, key: &str) -> i32 {
    (0..32)
        .find(|&id| lookup(id) == Some(key))
        .map_or(0, |id| id + 1)
}

fn write_optional_item_stack(w: &mut Writer, item: Option<&ItemStack>) {
    match item.filter(|stack| stack.count > 0) {
        None => w.var_i32(0),
        Some(stack) => match item_id(&stack.item.to_string()) {
            Some(id) => {
                w.var_i32(i32::try_from(stack.count).unwrap_or(i32::MAX));
                w.var_i32(id);
                write_item_component_patch(w, &stack.components);
            }
            None => w.var_i32(0),
        },
    }
}

/// Reverse of [`lodestone_data::data_component_types::component_type_name`]:
/// finds the numeric `minecraft:data_component_type` registry id for a name
/// by linear scan over the same table, the identical "no reverse export,
/// search the forward one" shape [`villager_registry_wire_id`] above already
/// uses and for the same reason.
fn component_type_id(name: &str) -> Option<i32> {
    (0..lodestone_data::data_component_types::DATA_COMPONENT_TYPE_COUNT as i32)
        .find(|&id| lodestone_data::data_component_types::component_type_name(id) == Some(name))
}

/// Writes an item stack's outbound component patch for
/// `container_set_slot`/`container_set_content`/`merchant_offers`: a VarInt
/// added-component count, a VarInt removed-component count, then the added
/// `(type id, payload)` entries.
///
/// **Scope.** The top-level `custom_data` component and the two book
/// components used by the book-edit path (`writable_book_content`/
/// `written_book_content`) are written here. Custom data is emitted only when
/// it is one complete compound-root network-NBT value; malformed values are
/// omitted without changing the valid book entries that follow.
/// `removed` is always `0` because this crate only adds components to stacks
/// it produces; it never removes one from a stack already held by a client.
/// Every other modeled [`ItemComponents`] field (`custom_name`,
/// `enchantments`, `dyed_color`, `trim`, …) remains an empty patch until its
/// outbound stream-codec writer is implemented and checked against the
/// protocol's reference bytes.
fn write_item_component_patch(w: &mut Writer, components: &ItemComponents) {
    let custom_data = components
        .custom_data
        .as_deref()
        .filter(|bytes| valid_custom_data(bytes));
    let count = i32::from(custom_data.is_some())
        + i32::from(components.writable_book_content.is_some())
        + i32::from(components.written_book_content.is_some());
    // The wire format writes both counts up front: the added-component count
    // followed by the removed-component count, before any entry. This order
    // is pinned by `book_content_wiring.rs` through the independently-written
    // client decoder; placing the removed count after the entries would make
    // the payload incompatible even though a symmetric local round trip could
    // appear to succeed.
    w.var_i32(count);
    w.var_i32(0); // removed components: this crate never sends a removal.
    if let Some(bytes) = custom_data {
        w.var_i32(0); // minecraft:custom_data is the first component registry entry.
        w.bytes(bytes);
    }
    if let Some(pages) = &components.writable_book_content {
        write_writable_book_content_entry(w, pages);
    }
    if let Some(content) = &components.written_book_content {
        write_written_book_content_entry(w, content);
    }
}

/// Accepts only one complete compound-root network-NBT value. Component
/// payloads are not length-prefixed, so emitting a malformed value would make
/// the client consume the following component entries as part of this one.
fn valid_custom_data(bytes: &[u8]) -> bool {
    let mut reader = Reader::new(bytes);
    matches!(read_network_nbt(&mut reader), Ok(Nbt::Compound(_)))
        && reader.ensure_empty().is_ok()
}

/// One added `minecraft:writable_book_content` entry: the component type id,
/// then vanilla's own writable-book-content type's own stream codec's payload — a VarInt page count,
/// then per page a `Filterable<String>` (the raw string, then `false` for
/// "no filtered alternate"; this crate runs no chat-filtering service, the
/// same call the decode-side reader in `adapter/inventory.rs` makes for the
/// reverse direction).
fn write_writable_book_content_entry(w: &mut Writer, pages: &[String]) {
    w.var_i32(component_type_id("minecraft:writable_book_content").unwrap_or(0));
    w.var_i32(i32::try_from(pages.len()).unwrap_or(i32::MAX));
    for page in pages {
        w.string(page);
        w.bool(false);
    }
}

/// One added `minecraft:written_book_content` entry:
/// vanilla's own written-book-content type's own stream codec's composite order exactly — title as a
/// `Filterable<String>`, plain `author` string, VarInt `generation`, a
/// VarInt-counted list of `Filterable<Component>` pages (each
/// [`written_book_page_nbt`] then a `false` filtered-alternate flag), then
/// the `resolved` bool.
fn write_written_book_content_entry(w: &mut Writer, content: &WrittenBookContent) {
    w.var_i32(component_type_id("minecraft:written_book_content").unwrap_or(0));
    w.string(&content.title);
    w.bool(false); // no filtered alternate
    w.string(&content.author);
    w.var_i32(i32::from(content.generation));
    w.var_i32(i32::try_from(content.pages.len()).unwrap_or(i32::MAX));
    for page in &content.pages {
        write_network_nbt(w, &written_book_page_nbt(page))
            .expect("a written-book page built from `Text::literal` always encodes");
        w.bool(false); // no filtered alternate
    }
    w.bool(content.resolved);
}

/// Serializes one written-book page to network-NBT. Deliberately narrower
/// than a general `Text` serializer would need to be, the same scope
/// discipline [`text_to_nbt`]'s own doc comment insists on for its one
/// caller: every page this crate itself signs is `Text::literal` with no
/// style, click, hover or insertion (`apply_edit_book`'s own
/// `Text::literal(page)` map in `lodestone-server`), so only the `Literal`
/// and `Translate` content shapes are handled — the only two [`TextContent`]
/// variants that exist — and neither ever carries style/click/hover/
/// insertion here, so nothing is silently dropped for a page this crate
/// produces. A page decoded from a real client's own written book (richer
/// than a literal) is not reachable through this encoder, because this
/// crate never re-serializes a stack it decoded — it only ever encodes
/// stacks it constructed itself.
fn written_book_page_nbt(text: &Text) -> Nbt {
    match &text.content {
        TextContent::Literal(literal) => {
            Nbt::Compound(vec![("text".to_owned(), Nbt::String(literal.clone()))])
        }
        TextContent::Translate { key, .. } => {
            Nbt::Compound(vec![("translate".to_owned(), Nbt::String(key.clone()))])
        }
    }
}

/// Hand-written encoder for the clientbound `open_screen` packet
/// (`ClientboundOpenScreenPacket`), which has no existing struct because it
/// is currently only ever *decoded* (see `V770Adapter::decode_open_screen`,
/// the exact mirror of this wire layout). Wire layout: VarInt container id
/// (vanilla's own codec library's own container accessor), VarInt `minecraft:menu` registry id
/// (vanilla's own codec library's own registry(vanilla's own registry-key holder's own menu accessor)` — a plain, non-holder registry
/// id, the same as `decode_open_screen`'s own `menu_name` lookup), then the
/// title as a network-form NBT text component — the identical plain-string
/// shape [`encode_system_chat`] already writes.
/// Writes one `ItemCost`: item registry id VarInt, count VarInt, an empty
/// `DataComponentExactPredicate` (VarInt `0`) — the exact mirror of
/// `crate::adapter::inventory::read_item_cost`'s decode side. An item this
/// crate cannot resolve to a wire id degrades to a zero-count cost rather
/// than writing a bad registry id that would desync everything after it.
fn write_item_cost(w: &mut Writer, cost: &(ResourceKey, i32)) {
    let (item, count) = cost;
    match item_id(&item.to_string()) {
        Some(id) => {
            w.var_i32(id);
            w.var_i32(*count);
            w.var_i32(0);
        }
        None => {
            w.var_i32(0);
            w.var_i32(0);
            w.var_i32(0);
        }
    }
}

/// Hand-written encoder for the clientbound `merchant_offers` packet. No
/// shared packet struct covers this direction; the decoder at
/// `crate::adapter::inventory::decode_merchant_offers` documents the same
/// wire layout.
///
/// Wire layout: VarInt window id, VarInt offer count, then per offer:
/// `cost_a` ([`write_item_cost`]), `result` as one
/// [`write_optional_item_stack`], a `bool` for whether `cost_b` follows (and
/// if so, one more [`write_item_cost`]), `out_of_stock` bool, then the five
/// **big-endian `i32`** fields `uses`/`max_uses`/`xp`/`special_price_diff`
/// (not VarInts — see `decode_merchant_offers`'s own doc for the trap), a
/// big-endian `f32` `price_multiplier`, a big-endian `i32` `demand` — and,
/// past every offer, the trailing VarInt `villager_level`, VarInt
/// `villager_xp`, `bool` `show_progress`, `bool` `can_restock`.
///
/// Every offer this crate generates is freshly created and unused:
/// `out_of_stock` is always `false`, `uses`/`special_price_diff`/`demand`
/// always `0`, and `price_multiplier` is the no-discount default (`0.05`).
/// This crate does not model villager reputation, so it has no other value to
/// derive here.
fn encode_merchant_offers_body(
    window_id: i32,
    offers: &[MerchantOfferOut],
    level: i32,
    xp: i32,
    show_progress: bool,
    can_restock: bool,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(window_id);
    w.var_i32(i32::try_from(offers.len()).unwrap_or(i32::MAX));
    for offer in offers {
        write_item_cost(&mut w, &offer.wants_a);
        let result = ItemStack::new(
            offer.gives.0.clone(),
            u32::try_from(offer.gives.1).unwrap_or(0),
        );
        write_optional_item_stack(&mut w, Some(&result));
        match &offer.wants_b {
            Some(cost_b) => {
                w.bool(true);
                write_item_cost(&mut w, cost_b);
            }
            None => w.bool(false),
        }
        w.bool(false); // out_of_stock: every generated offer starts fresh.
        w.i32(0); // uses
        w.i32(offer.max_uses);
        w.i32(offer.xp);
        w.i32(0); // special_price_diff: no reputation/demand pricing yet.
        w.f32(0.05); // price_multiplier: MerchantOffer's own no-discount default.
        w.i32(0); // demand
    }
    w.var_i32(level);
    w.var_i32(xp);
    w.bool(show_progress);
    w.bool(can_restock);
    w.into_vec()
}

fn encode_open_screen_body(window_id: i32, menu_registry_id: MenuId, title: &str) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(window_id);
    w.var_i32(menu_registry_id.raw());
    let component = Nbt::Compound(vec![("text".to_owned(), Nbt::String(title.to_owned()))]);
    write_network_nbt(&mut w, &component).expect("plain string NBT component always encodes");
    w.into_vec()
}

/// Hand-written encoder for the clientbound `container_set_content` packet
/// (`ClientboundContainerSetContentPacket`), which has no existing struct
/// because it is currently only ever *decoded* (see
/// `V770Adapter::handle_play`'s `CONTAINER_SET_CONTENT` arm, the exact mirror
/// of this wire layout). Wire layout: VarInt container id, VarInt state id,
/// then vanilla's own item-stack type's own optional-list-stream-codec accessor (a VarInt count followed by
/// that many [`write_optional_item_stack`] entries), then the carried/cursor
/// stack as one more [`write_optional_item_stack`].
fn encode_container_content_body(
    window_id: i32,
    state_id: i32,
    items: &[Option<ItemStack>],
    carried: Option<&ItemStack>,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(window_id);
    w.var_i32(state_id);
    w.var_i32(i32::try_from(items.len()).unwrap_or(i32::MAX));
    for item in items {
        write_optional_item_stack(&mut w, item.as_ref());
    }
    write_optional_item_stack(&mut w, carried);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `container_set_slot` packet
/// (`ClientboundContainerSetSlotPacket`), mirroring the decode side exactly
/// (`V770Adapter::handle_play`'s `CONTAINER_SET_SLOT` arm): VarInt container
/// id, VarInt state id, big-endian `short` slot, then one
/// [`write_optional_item_stack`].
fn encode_container_slot_body(
    window_id: i32,
    state_id: i32,
    slot: i32,
    item: Option<&ItemStack>,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(window_id);
    w.var_i32(state_id);
    w.i16(slot as i16);
    write_optional_item_stack(&mut w, item);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `container_set_data` packet
/// (vanilla's own clientbound container-set-data packet), mirroring the
/// decode side exactly
/// (`V770Adapter::handle_play`'s `CONTAINER_SET_DATA` arm): VarInt container
/// id, then the property index and its value as two big-endian `short`s
/// (vanilla's own container-id writer for the first field only — `id`/
/// `value` are plain `writeShort` calls, confirmed against the decompiled
/// 26.2 source).
fn encode_container_data_body(window_id: i32, property: i32, value: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(window_id);
    w.i16(property as i16);
    w.i16(value as i16);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `update_mob_effect` packet
/// (`ClientboundUpdateMobEffectPacket`), the exact mirror of
/// `V770Adapter::handle_play_entity`'s `UPDATE_MOB_EFFECT` decode arm
/// (`adapter/entity.rs`): VarInt entity id, VarInt `minecraft:mob_effect`
/// registry id, VarInt amplifier, VarInt duration (ticks), then one `u8`
/// bitset (`ambient` `0x1`, `visible` `0x2`, `show_icon` `0x4`, `blend`
/// `0x8`). An effect this crate cannot resolve to a registry id degrades to
/// writing nothing at all (`ServerDirective::None`) rather than a malformed
/// packet id — see this function's own caller.
#[allow(clippy::too_many_arguments)]
fn encode_update_mob_effect_body(
    entity_id: i32,
    effect_id: i32,
    amplifier: u32,
    duration_ticks: i32,
    ambient: bool,
    visible: bool,
    show_icon: bool,
    blend: bool,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(entity_id);
    w.var_i32(effect_id);
    w.var_i32(i32::try_from(amplifier).unwrap_or(i32::MAX));
    w.var_i32(duration_ticks);
    let mut flags = 0u8;
    if ambient {
        flags |= 0x1;
    }
    if visible {
        flags |= 0x2;
    }
    if show_icon {
        flags |= 0x4;
    }
    if blend {
        flags |= 0x8;
    }
    w.u8(flags);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `remove_mob_effect` packet
/// (`ClientboundRemoveMobEffectPacket`), the exact mirror of
/// `V770Adapter::handle_play_entity`'s `REMOVE_MOB_EFFECT` decode arm: VarInt
/// entity id, VarInt `minecraft:mob_effect` registry id.
fn encode_remove_mob_effect_body(entity_id: i32, effect_id: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(entity_id);
    w.var_i32(effect_id);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `add_entity` packet, which has no
/// existing struct because it is currently only ever *decoded* (see
/// `V770Adapter::handle_add_entity`, the exact mirror of this wire layout).
///
/// Wire layout: VarInt id, UUID, VarInt entity-type id, position `f64`×3,
/// low-precision velocity ([`write_lp_vec3`]), then three signed-byte angles
/// in **pitch, yaw, head_yaw** order (note: this order is reversed from
/// `move_entity`'s yaw-then-pitch), then a trailing VarInt **Object Data** field
/// from [`EntitySnapshot::object_data`] (`0` for ordinary mobs, and the block
/// state id for a `minecraft:falling_block` — see that field's own doc).
///
/// This field used to be a hardcoded `0`, which is correct for every entity kind
/// that does not override `getAddEntityPacket` and silently wrong for the one that
/// does: a falling block's imitated state travels here and nowhere else.
///
/// An `entity_type` with no match in this version's registry (a typo, or a
/// key from a version this table doesn't cover) falls back to network id `0`
/// rather than failing the whole spawn — a wrong model is recoverable, a
/// dropped connection is not.
fn encode_add_entity_body(entity: &EntitySnapshot) -> Vec<u8> {
    let type_id = entity_type_id(&entity.entity_type.to_string()).unwrap_or(0);
    let mut w = Writer::default();
    w.var_i32(entity.id);
    w.uuid(entity.uuid);
    w.var_i32(type_id);
    w.f64(entity.position.x);
    w.f64(entity.position.y);
    w.f64(entity.position.z);
    write_lp_vec3(
        &mut w,
        entity.velocity.x,
        entity.velocity.y,
        entity.velocity.z,
    );
    w.i8(pack_degrees(entity.rotation.pitch));
    w.i8(pack_degrees(entity.rotation.yaw));
    w.i8(pack_degrees(entity.head_yaw));
    w.var_i32(entity.object_data);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `teleport_entity` packet (the
/// entity-position analogue of [`encode_player_position_teleport`]), which has
/// no existing struct because it is currently only ever *decoded* (see
/// `V770Adapter::handle_entity_position`).
///
/// Wire layout: VarInt id, position `f64`×3, delta-movement `f64`×3 (zero —
/// an absolute update carries no velocity here; velocity travels separately
/// via `set_entity_motion`), yaw/pitch as **`f32`** (unlike `add_entity`'s
/// signed-byte angles), a trailing big-endian `i32` relative-flags bit set
/// (`0` — every field is absolute), then a `bool` on-ground flag. All mobs
/// the sim currently spawns are land-walkers (`MobShape::land`), so on-ground
/// is hardcoded `true`; `EntitySnapshot` carries no on-ground field yet to
/// derive this from.
fn encode_teleport_entity(entity: &EntitySnapshot) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(entity.id);
    w.f64(entity.position.x);
    w.f64(entity.position.y);
    w.f64(entity.position.z);
    w.f64(0.0);
    w.f64(0.0);
    w.f64(0.0);
    w.f32(entity.rotation.yaw);
    w.f32(entity.rotation.pitch);
    w.i32(0);
    w.bool(true);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `rotate_head` packet: VarInt id
/// then one signed-byte angle ([`pack_degrees`]), the exact mirror of the
/// inline `ROTATE_HEAD` decode arm in `V770Adapter::handle_play`.
fn encode_rotate_head(entity_id: i32, head_yaw: f32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(entity_id);
    w.i8(pack_degrees(head_yaw));
    w.into_vec()
}

/// Encodes the trailing `GameLogin::rest` bytes: the spawn-info fields not
/// modelled as named struct fields (see that struct's doc comment for why).
/// None of these are consumed by `V770Adapter::handle_play`'s `LOGIN` arm, so
/// their exact values only need to be well-formed, not vanilla-authentic.
fn encode_game_login_rest() -> Vec<u8> {
    let mut w = Writer::default();
    w.bool(false); // has_last_death_location
    w.var_i32(0); // portal_cooldown
    w.var_i32(OVERWORLD_SEA_LEVEL); // sea_level
    w.bool(false); // online_mode (no auth in the integrated server)
    w.bool(false); // enforces_secure_chat
    w.into_vec()
}

/// Converts one `lodestone-server` [`ServerChunkColumn`] into the
/// version-free [`WorldChunkColumn`] the wire codec speaks, carrying the
/// **real** per-block state the source already computed (grass, dirt,
/// deepslate, gravel, water, …) rather than a solid/air classification, and
/// the source's real per-quart
/// biome assignment rather than one constant id everywhere. Every block
/// cell is read as an **integer** via [`ServerChunkColumn::block_state_id`];
/// every biome **cell** via [`ServerChunkColumn::biome_cell_index`] through
/// [`resolve_biome_id`] — a real per-`y` grid, not one surface
/// sample broadcast down the column.
///
/// # This function does no string work at all, and that is recent
///
/// It used to read [`ServerChunkColumn::block_state`] — a `&str` — 98,304 times
/// per column, probe each through a per-column `HashMap<&str, u32>` (std's
/// SipHash), and resolve each *distinct* entry through what was then a
/// 32,366-row scan doing a string compare per row: order 10⁶ string comparisons
/// per served column, paid on every join and every view-tracker resend. It was
/// invisible to the 21-unit worldgen optimisation drive because the generation
/// cost metric excludes protocol encode by definition.
///
/// The resolution now happens **once per palette entry, on the server side**, at
/// column-adoption time (`ChunkColumn::palette_state_ids`), so the inner loop
/// here is a range check and two array indexes. `resolve_state_id` still exists
/// for the per-*edit* callers and is the same `lodestone_data` function the
/// palette resolves through, so the two cannot drift. `DESIGN.md` §12.131 has
/// the measurement.
///
/// [`resolve_biome_id`] is a 55-entry linear scan, and it is called once per
/// entry in the column's own biome *palette* (`biome_palette_ids` below) — a
/// handful, measured in single digits — never once per cell. That is what makes
/// a 1,536-cell 3-D grid cost strictly less than the 16 calls the old
/// vertically-broadcast surface array made. It is now the only string work left
/// in this function.
///
/// Iterates section-major (matching wire order) and skips sections that end
/// up entirely default (air-only, default biome), since
/// [`WorldChunkColumn::set_section`] already elides those.
fn build_world_column(shape: &ChunkShape, source: &ServerChunkColumn) -> WorldChunkColumn {
    let mut column = WorldChunkColumn::new(
        shape.min_y,
        shape.section_count,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );

    // This column's real 3-D biome grid. The column stores its
    // cells as indices into a small per-column palette — a handful of entries,
    // never the 1,536 cells — so resolving that palette once and indexing per
    // cell is *cheaper* than the 16 `resolve_biome_id` calls this replaced,
    // while carrying a per-`y` answer instead of one broadcast vertically.
    // Broadcasting was what erased `lush_caves`/`dripstone_caves`/`deep_dark`
    // from every column the server sent.
    let biome_palette_ids: Vec<u32> = source
        .biome_cell_palette()
        .iter()
        .map(|name| resolve_biome_id(name))
        .collect();

    for section_index in 0..shape.section_count {
        let base_y = shape.min_y + (section_index * ChunkSection::EDGE) as i32;
        let mut section = ChunkSection::new(
            shape.block_kind,
            shape.biome_kind,
            shape.air_id,
            shape.biome_id,
        );
        for ly in 0..ChunkSection::EDGE {
            let wy = base_y + ly as i32;
            for lz in 0..ChunkSection::EDGE {
                for lx in 0..ChunkSection::EDGE {
                    let id = source.block_state_id(lx as i32, wy, lz as i32);
                    // `air_id` is already the container's own default (see
                    // `ChunkSection::new`), so a cell resolving to it needs
                    // no explicit write — same short-circuit the old
                    // `is_solid` check gave air cells, just derived from the
                    // real state now.
                    if id != shape.air_id {
                        section.set_block(lx, ly, lz, id);
                    }
                }
            }
        }
        for qy in 0..4usize {
            let column_qy = section_index * 4 + qy;
            for qz in 0..4usize {
                for qx in 0..4usize {
                    let cell = source.biome_cell_index(qx, column_qy, qz) as usize;
                    section.set_biome(qx, qy, qz, biome_palette_ids[cell]);
                }
            }
        }
        if !section.is_empty(shape.biome_id) {
            column.set_section(section_index, Some(section));
        }
    }

    column
}

/// Encodes one [`WorldChunkColumn`] into the `level_chunk_with_light` body,
/// mirroring `LevelChunkWithLight`'s decode in `packets::chunk` exactly:
/// `x`, `z`, empty heightmaps, the length-prefixed section blob (per section
/// two leading shorts — non-air count then fluid count, always `0` — then the
/// block-state container then the biome container), the block-entity list
/// ([`encode_block_entities`]), then the trailing light payload.
///
/// Heightmaps now carry the generator's real `MOTION_BLOCKING` map when the
/// column has one — `Heightmap::new(world_height)` picks its own
/// 9-bit width from `height_bits`, so no width is chosen here. A column from
/// anywhere but the generator (`ChunkColumn::new`, a region-file load) still
/// sends the zero-entry NBT it always sent: valid and decodable, simply empty.
/// The other three sent maps (`WORLD_SURFACE`, `OCEAN_FLOOR`,
/// `MOTION_BLOCKING_NO_LEAVES`) are deliberately still absent — see
/// `docs/motion-blocking-heightmap.md` for why sending `NO_LEAVES` today would
/// send a knowingly wrong map.
///
/// **`light` is no longer all-`Missing`.** It is the caller's
/// computed [`ColumnLight`]; see [`compute_served_light`] for where it comes
/// from and what `Missing` used to mean on the client.
fn encode_column_body(
    cx: i32,
    cz: i32,
    shape: &ChunkShape,
    column: &WorldChunkColumn,
    light: &ColumnLight,
    source: &ServerChunkColumn,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.i32(cx);
    w.i32(cz);

    let mut heightmaps = Heightmaps::new();
    if let Some(stored) = source.motion_blocking() {
        let mut map = Heightmap::new(source.height as u32);
        for lz in 0..16usize {
            for lx in 0..16usize {
                map.set(lx, lz, u32::from(stored[lx + lz * 16]));
            }
        }
        heightmaps.insert(MOTION_BLOCKING_HEIGHTMAP_TYPE_ID, map);
    }
    heightmaps.encode(&mut w);

    let mut section_blob = Writer::default();
    for section_index in 0..shape.section_count {
        // A freshly synthesized empty section for indices the column elided
        // (all-air, default biome) — every section index still gets bytes on
        // the wire; there is no "skip empty section" shortcut.
        let synthesized;
        let section = match column.section(section_index) {
            Some(section) => section,
            None => {
                synthesized = ChunkSection::new(
                    shape.block_kind,
                    shape.biome_kind,
                    shape.air_id,
                    shape.biome_id,
                );
                &synthesized
            }
        };
        section_blob.i16(section.non_air_count() as i16);
        section_blob.i16(0); // fluid count: this pipeline models no fluids yet
        section.block_states().encode(&mut section_blob);
        section.biomes().encode(&mut section_blob);
    }
    let section_bytes = section_blob.into_vec();
    w.var_i32(section_bytes.len() as i32);
    w.bytes(&section_bytes);

    encode_block_entities(&mut w, source);

    debug_assert_eq!(
        light.light_section_count(),
        shape.section_count + 2,
        "light must span the shape's `section_count + 2` light sections"
    );
    light.encode(&mut w);

    w.into_vec()
}

/// Resolves a `minecraft:block_entity_type` registry key to its protocol-776
/// registry id, or `None` for a key this version does not have.
///
/// A 49-entry linear scan over [`lodestone_data::block_entity_types`]'s own
/// name table, in the same shape as [`resolve_biome_id`] — the table is indexed
/// *by* id, and there is no reverse map in `lodestone-data`. Called once per
/// block entity in a column, of which the overwhelming majority have zero, so a
/// map would cost more to build than the scans it saves.
fn resolve_block_entity_type_id(name: &str) -> Option<u32> {
    (0..lodestone_data::block_entity_types::TYPE_COUNT)
        .filter_map(lodestone_data::block_entity_types::BlockEntityType::new)
        .find(|&kind| lodestone_data::block_entity_types::block_entity_type_name(kind) == name)
        .map(lodestone_data::block_entity_types::BlockEntityType::raw)
}

/// Writes the chunk packet's block-entity array: a VarInt count
/// then, per entry, the section-relative XZ packed into one byte (`x << 4 | z`),
/// the **absolute** Y as a big-endian short, the block-entity type's registry id
/// as a VarInt, and the network-NBT payload. Exactly the layout
/// [`lodestone_world::BlockEntity::decode`] reads back.
///
/// This used to be a hardcoded `var_i32(0)` — every chunk claiming it held no
/// block entities at all, so a generated bee nest arrived as a decorative block
/// with nothing inside it and a chest loaded off disk arrived empty.
///
/// An entry is **skipped** — count included — when its type key does not resolve
/// in this version's registry, or when its NBT does not serialize. Both are
/// filtered *before* the count is written, which is the whole reason the payload
/// is built into a scratch buffer per entry rather than straight into `w`: a
/// wrong VarInt type id merely mis-draws one entity, while a count that does not
/// match the records that follow desynchronises the stream and takes the
/// connection down. An `Opaque` entity's tree comes from a region file we did
/// not write, so "this NBT does not encode" is a real input, not an invariant to
/// `expect` on.
fn encode_block_entities(w: &mut Writer, source: &ServerChunkColumn) {
    let entries: Vec<(lodestone_model::BlockPos, u32, Vec<u8>)> = source
        .block_entities()
        .iter()
        .filter_map(|(pos, entity)| {
            let type_id = resolve_block_entity_type_id(entity.type_id())?;
            let nbt = lodestone_server::chunk_nbt::block_entity_to_nbt(*pos, entity);
            let mut body = Writer::default();
            write_network_nbt(&mut body, &nbt).ok()?;
            Some((*pos, type_id, body.into_vec()))
        })
        .collect();

    w.var_i32(entries.len() as i32);
    for (pos, type_id, nbt) in entries {
        w.u8((((pos.x & 15) << 4) | (pos.z & 15)) as u8);
        w.i16(pos.y as i16);
        w.var_i32(type_id as i32);
        w.bytes(&nbt);
    }
}

/// The 26.2 [`LightProperties`] the served-chunk light engine runs against:
/// `lodestone-data`'s per-block-state dampening/emission census, read straight
/// out of rodata.
///
/// A zero-sized adapter rather than a table this crate builds, so there is no
/// per-column setup cost and nothing to cache. See
/// [`lodestone_data::light_props`] for the provenance argument — in particular
/// that every gap in it darkens rather than brightens.
struct V770LightProps;

impl LightProperties for V770LightProps {
    fn opacity(&self, state: u32) -> u8 {
        lodestone_data::block_states::StateId::new(state)
            .map_or(0, lodestone_data::light_props::dampening)
    }

    fn emission(&self, state: u32) -> u8 {
        lodestone_data::block_states::StateId::new(state)
            .map_or(0, lodestone_data::light_props::emission)
    }
}

/// Computes the sky and block light for one served column.
///
/// # Why this exists at all
///
/// Until this landed, every column the integrated server sent carried
/// `ColumnLight::new(section_count)` — all-`LightData::Missing`
/// (`lodestone_world::light`), for both layers, in every section. That is a
/// legal wire form, and it is *not* "no light": a client resolves an absent sky
/// section to its dimension default, which in the overworld is **full daylight**
/// (`lodestone_render::SkyDefault::Full`; vanilla's own client does the same
/// through `SkyLightSectionStorage`). So the symptom was a **fully bright**
/// world — caves and sealed rooms included — not a dark one. Anyone hunting this
/// bug by looking for blackness was looking for the wrong colour.
///
/// # Where light is computed, and what that costs
///
/// Here, at serve time, over the [`WorldChunkColumn`] `build_world_column` has
/// already materialised. That is not where it *belongs* — see the seam note
/// below — but it is the only place reachable from
/// [`ServerProtocol::encode_chunk`], whose signature carries one column and no
/// access to the [`lodestone_server::ChunkSource`] the neighbours live in.
///
/// The cost is bounded by construction: the flood is `O(cells)` over the
/// `(section_count + 2) * 4096` cells of one column with a 15-bucket queue, and
/// it runs on whatever thread already paid for `build_world_column`'s 98,304
/// `resolve_state_id` lookups — a far larger constant. `tests/server_light.rs`
/// measures the ratio of the two in one process, which is the only honest way to
/// state a cost on this machine (an absolute duration gets attributed to
/// concurrent load).
///
/// # The cross-chunk seam, and why this is the isolated compute
///
/// Sky and block light cross column boundaries, so the exact answer for a column
/// needs its eight neighbours — that is what
/// `lodestone_world::compute_column_light_with_neighbours` is for, and it is exact, because
/// light decays at least one level per block and `15 < 16` so no source beyond
/// the immediate neighbours can reach the centre.
///
/// This function cannot call it: `encode_chunk` is handed one column, and at
/// join the columns arrive in spiral ring order, so a column's *outward*
/// neighbours have not been generated yet when it is encoded. Consulting only
/// the neighbours seen so far would bias every column's outward edge dark —
/// worse than a symmetric residual.
///
/// So this is the isolated compute, and the residual is a **measured, gated
/// number**, not a caveat: `tests/server_light.rs`'s
/// `served_light_has_no_cross_chunk_seam_residual` recomputes the same terrain
/// with a full 3×3 neighbourhood and fails, printing a bounding box, if the two
/// disagree anywhere. The day the generator grows terrain whose light genuinely
/// spans a seam, that gate goes red and the fix is the brokered
/// `lodestone-server` patch recorded in `DESIGN.md` §12.117: compute light in the
/// chunk source, where the neighbourhood is already resident, and carry it on
/// [`ServerChunkColumn`].
fn compute_served_light(column: &WorldChunkColumn) -> ColumnLight {
    compute_column_light(column, &V770LightProps)
}

/// Server-side implementation of the protocol-776 (Minecraft 26.2) wire
/// format, driving `lodestone-server`'s [`ServerProtocol`] seam.
///
/// Holds no per-connection state: unlike [`V770Adapter`] (which tracks the
/// current dimension's [`ChunkShape`] across `login`/`respawn`), the server
/// always joins into the overworld today, so the shape is a constant rather
/// than connection state. A future respawn/dimension-change feature would
/// need to thread shape through here the same way the adapter does.
#[derive(Debug, Clone, Copy, Default)]
pub struct V770ServerProtocol;

// ---------------------------------------------------------------------------
// Configuration-phase `registry_data` payloads
// ---------------------------------------------------------------------------
//
// Each constant is the full serialized **network NBT** for one registry entry
// — root tag byte included — captured verbatim from a real vanilla 26.2 server
// during Configuration on the creative oracle. The capture lives in
// `tests/fixtures/registry_data_{dimension_type,world_clock}.hex`
// (`tests/live_registry_data.rs` wrote it; the `live-registry` feature re-captures
// and diffs it), and `tests/registry_data.rs` asserts the payloads emitted below
// are byte-identical to those fixtures. They are bytes, not values, precisely so
// the server ships vanilla's own wire format instead of a re-encoding of our own
// understanding of it.

const DIMENSION_TYPE_OVERWORLD_NBT: &[u8] = &[
    0x0a, 0x08, 0x00, 0x0d, 0x64, 0x65, 0x66, 0x61, 0x75, 0x6c, 0x74, 0x5f, 0x63, 0x6c, 0x6f, 0x63,
    0x6b, 0x00, 0x13, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x6f, 0x76, 0x65,
    0x72, 0x77, 0x6f, 0x72, 0x6c, 0x64, 0x01, 0x00, 0x16, 0x68, 0x61, 0x73, 0x5f, 0x65, 0x6e, 0x64,
    0x65, 0x72, 0x5f, 0x64, 0x72, 0x61, 0x67, 0x6f, 0x6e, 0x5f, 0x66, 0x69, 0x67, 0x68, 0x74, 0x00,
    0x05, 0x00, 0x0d, 0x61, 0x6d, 0x62, 0x69, 0x65, 0x6e, 0x74, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74,
    0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x1f, 0x6d, 0x6f, 0x6e, 0x73, 0x74, 0x65, 0x72, 0x5f, 0x73,
    0x70, 0x61, 0x77, 0x6e, 0x5f, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74,
    0x5f, 0x6c, 0x69, 0x6d, 0x69, 0x74, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x0a, 0x69, 0x6e, 0x66,
    0x69, 0x6e, 0x69, 0x62, 0x75, 0x72, 0x6e, 0x00, 0x1f, 0x23, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72,
    0x61, 0x66, 0x74, 0x3a, 0x69, 0x6e, 0x66, 0x69, 0x6e, 0x69, 0x62, 0x75, 0x72, 0x6e, 0x5f, 0x6f,
    0x76, 0x65, 0x72, 0x77, 0x6f, 0x72, 0x6c, 0x64, 0x01, 0x00, 0x0c, 0x68, 0x61, 0x73, 0x5f, 0x73,
    0x6b, 0x79, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x01, 0x08, 0x00, 0x09, 0x74, 0x69, 0x6d, 0x65, 0x6c,
    0x69, 0x6e, 0x65, 0x73, 0x00, 0x17, 0x23, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74,
    0x3a, 0x69, 0x6e, 0x5f, 0x6f, 0x76, 0x65, 0x72, 0x77, 0x6f, 0x72, 0x6c, 0x64, 0x06, 0x00, 0x10,
    0x63, 0x6f, 0x6f, 0x72, 0x64, 0x69, 0x6e, 0x61, 0x74, 0x65, 0x5f, 0x73, 0x63, 0x61, 0x6c, 0x65,
    0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x0e, 0x6c, 0x6f, 0x67, 0x69, 0x63,
    0x61, 0x6c, 0x5f, 0x68, 0x65, 0x69, 0x67, 0x68, 0x74, 0x00, 0x00, 0x01, 0x80, 0x0a, 0x00, 0x0a,
    0x61, 0x74, 0x74, 0x72, 0x69, 0x62, 0x75, 0x74, 0x65, 0x73, 0x0a, 0x00, 0x20, 0x6d, 0x69, 0x6e,
    0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x61, 0x75, 0x64, 0x69, 0x6f, 0x2f, 0x62, 0x61, 0x63,
    0x6b, 0x67, 0x72, 0x6f, 0x75, 0x6e, 0x64, 0x5f, 0x6d, 0x75, 0x73, 0x69, 0x63, 0x0a, 0x00, 0x07,
    0x64, 0x65, 0x66, 0x61, 0x75, 0x6c, 0x74, 0x03, 0x00, 0x09, 0x6d, 0x61, 0x78, 0x5f, 0x64, 0x65,
    0x6c, 0x61, 0x79, 0x00, 0x00, 0x5d, 0xc0, 0x08, 0x00, 0x05, 0x73, 0x6f, 0x75, 0x6e, 0x64, 0x00,
    0x14, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x6d, 0x75, 0x73, 0x69, 0x63,
    0x2e, 0x67, 0x61, 0x6d, 0x65, 0x03, 0x00, 0x09, 0x6d, 0x69, 0x6e, 0x5f, 0x64, 0x65, 0x6c, 0x61,
    0x79, 0x00, 0x00, 0x2e, 0xe0, 0x00, 0x0a, 0x00, 0x08, 0x63, 0x72, 0x65, 0x61, 0x74, 0x69, 0x76,
    0x65, 0x03, 0x00, 0x09, 0x6d, 0x61, 0x78, 0x5f, 0x64, 0x65, 0x6c, 0x61, 0x79, 0x00, 0x00, 0x5d,
    0xc0, 0x08, 0x00, 0x05, 0x73, 0x6f, 0x75, 0x6e, 0x64, 0x00, 0x18, 0x6d, 0x69, 0x6e, 0x65, 0x63,
    0x72, 0x61, 0x66, 0x74, 0x3a, 0x6d, 0x75, 0x73, 0x69, 0x63, 0x2e, 0x63, 0x72, 0x65, 0x61, 0x74,
    0x69, 0x76, 0x65, 0x03, 0x00, 0x09, 0x6d, 0x69, 0x6e, 0x5f, 0x64, 0x65, 0x6c, 0x61, 0x79, 0x00,
    0x00, 0x2e, 0xe0, 0x00, 0x00, 0x05, 0x00, 0x1d, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66,
    0x74, 0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x63, 0x6c, 0x6f, 0x75, 0x64, 0x5f, 0x68,
    0x65, 0x69, 0x67, 0x68, 0x74, 0x43, 0x40, 0x54, 0x7b, 0x08, 0x00, 0x1a, 0x6d, 0x69, 0x6e, 0x65,
    0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x66, 0x6f, 0x67,
    0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x07, 0x23, 0x63, 0x30, 0x64, 0x38, 0x66, 0x66, 0x08,
    0x00, 0x24, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75,
    0x61, 0x6c, 0x2f, 0x61, 0x6d, 0x62, 0x69, 0x65, 0x6e, 0x74, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74,
    0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x07, 0x23, 0x30, 0x61, 0x30, 0x61, 0x30, 0x61, 0x08,
    0x00, 0x1a, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75,
    0x61, 0x6c, 0x2f, 0x73, 0x6b, 0x79, 0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x07, 0x23, 0x37,
    0x38, 0x61, 0x37, 0x66, 0x66, 0x0a, 0x00, 0x1e, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66,
    0x74, 0x3a, 0x61, 0x75, 0x64, 0x69, 0x6f, 0x2f, 0x61, 0x6d, 0x62, 0x69, 0x65, 0x6e, 0x74, 0x5f,
    0x73, 0x6f, 0x75, 0x6e, 0x64, 0x73, 0x0a, 0x00, 0x04, 0x6d, 0x6f, 0x6f, 0x64, 0x03, 0x00, 0x0a,
    0x74, 0x69, 0x63, 0x6b, 0x5f, 0x64, 0x65, 0x6c, 0x61, 0x79, 0x00, 0x00, 0x17, 0x70, 0x06, 0x00,
    0x06, 0x6f, 0x66, 0x66, 0x73, 0x65, 0x74, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08,
    0x00, 0x05, 0x73, 0x6f, 0x75, 0x6e, 0x64, 0x00, 0x16, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61,
    0x66, 0x74, 0x3a, 0x61, 0x6d, 0x62, 0x69, 0x65, 0x6e, 0x74, 0x2e, 0x63, 0x61, 0x76, 0x65, 0x03,
    0x00, 0x13, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x5f, 0x73, 0x65, 0x61, 0x72, 0x63, 0x68, 0x5f, 0x65,
    0x78, 0x74, 0x65, 0x6e, 0x74, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x08, 0x00, 0x1c, 0x6d, 0x69,
    0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x63,
    0x6c, 0x6f, 0x75, 0x64, 0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x09, 0x23, 0x63, 0x63, 0x66,
    0x66, 0x66, 0x66, 0x66, 0x66, 0x00, 0x03, 0x00, 0x05, 0x6d, 0x69, 0x6e, 0x5f, 0x79, 0xff, 0xff,
    0xff, 0xc0, 0x0a, 0x00, 0x19, 0x6d, 0x6f, 0x6e, 0x73, 0x74, 0x65, 0x72, 0x5f, 0x73, 0x70, 0x61,
    0x77, 0x6e, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x5f, 0x6c, 0x65, 0x76, 0x65, 0x6c, 0x03, 0x00,
    0x0d, 0x6d, 0x69, 0x6e, 0x5f, 0x69, 0x6e, 0x63, 0x6c, 0x75, 0x73, 0x69, 0x76, 0x65, 0x00, 0x00,
    0x00, 0x00, 0x03, 0x00, 0x0d, 0x6d, 0x61, 0x78, 0x5f, 0x69, 0x6e, 0x63, 0x6c, 0x75, 0x73, 0x69,
    0x76, 0x65, 0x00, 0x00, 0x00, 0x07, 0x08, 0x00, 0x04, 0x74, 0x79, 0x70, 0x65, 0x00, 0x11, 0x6d,
    0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x75, 0x6e, 0x69, 0x66, 0x6f, 0x72, 0x6d,
    0x00, 0x01, 0x00, 0x0b, 0x68, 0x61, 0x73, 0x5f, 0x63, 0x65, 0x69, 0x6c, 0x69, 0x6e, 0x67, 0x00,
    0x03, 0x00, 0x06, 0x68, 0x65, 0x69, 0x67, 0x68, 0x74, 0x00, 0x00, 0x01, 0x80, 0x00,
];
const DIMENSION_TYPE_OVERWORLD_CAVES_NBT: &[u8] = &[
    0x0a, 0x08, 0x00, 0x0d, 0x64, 0x65, 0x66, 0x61, 0x75, 0x6c, 0x74, 0x5f, 0x63, 0x6c, 0x6f, 0x63,
    0x6b, 0x00, 0x13, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x6f, 0x76, 0x65,
    0x72, 0x77, 0x6f, 0x72, 0x6c, 0x64, 0x01, 0x00, 0x16, 0x68, 0x61, 0x73, 0x5f, 0x65, 0x6e, 0x64,
    0x65, 0x72, 0x5f, 0x64, 0x72, 0x61, 0x67, 0x6f, 0x6e, 0x5f, 0x66, 0x69, 0x67, 0x68, 0x74, 0x00,
    0x05, 0x00, 0x0d, 0x61, 0x6d, 0x62, 0x69, 0x65, 0x6e, 0x74, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74,
    0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x1f, 0x6d, 0x6f, 0x6e, 0x73, 0x74, 0x65, 0x72, 0x5f, 0x73,
    0x70, 0x61, 0x77, 0x6e, 0x5f, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74,
    0x5f, 0x6c, 0x69, 0x6d, 0x69, 0x74, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x0a, 0x69, 0x6e, 0x66,
    0x69, 0x6e, 0x69, 0x62, 0x75, 0x72, 0x6e, 0x00, 0x1f, 0x23, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72,
    0x61, 0x66, 0x74, 0x3a, 0x69, 0x6e, 0x66, 0x69, 0x6e, 0x69, 0x62, 0x75, 0x72, 0x6e, 0x5f, 0x6f,
    0x76, 0x65, 0x72, 0x77, 0x6f, 0x72, 0x6c, 0x64, 0x01, 0x00, 0x0c, 0x68, 0x61, 0x73, 0x5f, 0x73,
    0x6b, 0x79, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x01, 0x08, 0x00, 0x09, 0x74, 0x69, 0x6d, 0x65, 0x6c,
    0x69, 0x6e, 0x65, 0x73, 0x00, 0x17, 0x23, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74,
    0x3a, 0x69, 0x6e, 0x5f, 0x6f, 0x76, 0x65, 0x72, 0x77, 0x6f, 0x72, 0x6c, 0x64, 0x06, 0x00, 0x10,
    0x63, 0x6f, 0x6f, 0x72, 0x64, 0x69, 0x6e, 0x61, 0x74, 0x65, 0x5f, 0x73, 0x63, 0x61, 0x6c, 0x65,
    0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x0e, 0x6c, 0x6f, 0x67, 0x69, 0x63,
    0x61, 0x6c, 0x5f, 0x68, 0x65, 0x69, 0x67, 0x68, 0x74, 0x00, 0x00, 0x01, 0x80, 0x0a, 0x00, 0x0a,
    0x61, 0x74, 0x74, 0x72, 0x69, 0x62, 0x75, 0x74, 0x65, 0x73, 0x0a, 0x00, 0x20, 0x6d, 0x69, 0x6e,
    0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x61, 0x75, 0x64, 0x69, 0x6f, 0x2f, 0x62, 0x61, 0x63,
    0x6b, 0x67, 0x72, 0x6f, 0x75, 0x6e, 0x64, 0x5f, 0x6d, 0x75, 0x73, 0x69, 0x63, 0x0a, 0x00, 0x07,
    0x64, 0x65, 0x66, 0x61, 0x75, 0x6c, 0x74, 0x03, 0x00, 0x09, 0x6d, 0x61, 0x78, 0x5f, 0x64, 0x65,
    0x6c, 0x61, 0x79, 0x00, 0x00, 0x5d, 0xc0, 0x08, 0x00, 0x05, 0x73, 0x6f, 0x75, 0x6e, 0x64, 0x00,
    0x14, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x6d, 0x75, 0x73, 0x69, 0x63,
    0x2e, 0x67, 0x61, 0x6d, 0x65, 0x03, 0x00, 0x09, 0x6d, 0x69, 0x6e, 0x5f, 0x64, 0x65, 0x6c, 0x61,
    0x79, 0x00, 0x00, 0x2e, 0xe0, 0x00, 0x0a, 0x00, 0x08, 0x63, 0x72, 0x65, 0x61, 0x74, 0x69, 0x76,
    0x65, 0x03, 0x00, 0x09, 0x6d, 0x61, 0x78, 0x5f, 0x64, 0x65, 0x6c, 0x61, 0x79, 0x00, 0x00, 0x5d,
    0xc0, 0x08, 0x00, 0x05, 0x73, 0x6f, 0x75, 0x6e, 0x64, 0x00, 0x18, 0x6d, 0x69, 0x6e, 0x65, 0x63,
    0x72, 0x61, 0x66, 0x74, 0x3a, 0x6d, 0x75, 0x73, 0x69, 0x63, 0x2e, 0x63, 0x72, 0x65, 0x61, 0x74,
    0x69, 0x76, 0x65, 0x03, 0x00, 0x09, 0x6d, 0x69, 0x6e, 0x5f, 0x64, 0x65, 0x6c, 0x61, 0x79, 0x00,
    0x00, 0x2e, 0xe0, 0x00, 0x00, 0x05, 0x00, 0x1d, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66,
    0x74, 0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x63, 0x6c, 0x6f, 0x75, 0x64, 0x5f, 0x68,
    0x65, 0x69, 0x67, 0x68, 0x74, 0x43, 0x40, 0x54, 0x7b, 0x08, 0x00, 0x1a, 0x6d, 0x69, 0x6e, 0x65,
    0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x66, 0x6f, 0x67,
    0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x07, 0x23, 0x63, 0x30, 0x64, 0x38, 0x66, 0x66, 0x08,
    0x00, 0x24, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75,
    0x61, 0x6c, 0x2f, 0x61, 0x6d, 0x62, 0x69, 0x65, 0x6e, 0x74, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74,
    0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x07, 0x23, 0x30, 0x61, 0x30, 0x61, 0x30, 0x61, 0x08,
    0x00, 0x1a, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75,
    0x61, 0x6c, 0x2f, 0x73, 0x6b, 0x79, 0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x07, 0x23, 0x37,
    0x38, 0x61, 0x37, 0x66, 0x66, 0x0a, 0x00, 0x1e, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66,
    0x74, 0x3a, 0x61, 0x75, 0x64, 0x69, 0x6f, 0x2f, 0x61, 0x6d, 0x62, 0x69, 0x65, 0x6e, 0x74, 0x5f,
    0x73, 0x6f, 0x75, 0x6e, 0x64, 0x73, 0x0a, 0x00, 0x04, 0x6d, 0x6f, 0x6f, 0x64, 0x03, 0x00, 0x0a,
    0x74, 0x69, 0x63, 0x6b, 0x5f, 0x64, 0x65, 0x6c, 0x61, 0x79, 0x00, 0x00, 0x17, 0x70, 0x06, 0x00,
    0x06, 0x6f, 0x66, 0x66, 0x73, 0x65, 0x74, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08,
    0x00, 0x05, 0x73, 0x6f, 0x75, 0x6e, 0x64, 0x00, 0x16, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61,
    0x66, 0x74, 0x3a, 0x61, 0x6d, 0x62, 0x69, 0x65, 0x6e, 0x74, 0x2e, 0x63, 0x61, 0x76, 0x65, 0x03,
    0x00, 0x13, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x5f, 0x73, 0x65, 0x61, 0x72, 0x63, 0x68, 0x5f, 0x65,
    0x78, 0x74, 0x65, 0x6e, 0x74, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x08, 0x00, 0x1c, 0x6d, 0x69,
    0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x63,
    0x6c, 0x6f, 0x75, 0x64, 0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x09, 0x23, 0x63, 0x63, 0x66,
    0x66, 0x66, 0x66, 0x66, 0x66, 0x00, 0x03, 0x00, 0x05, 0x6d, 0x69, 0x6e, 0x5f, 0x79, 0xff, 0xff,
    0xff, 0xc0, 0x0a, 0x00, 0x19, 0x6d, 0x6f, 0x6e, 0x73, 0x74, 0x65, 0x72, 0x5f, 0x73, 0x70, 0x61,
    0x77, 0x6e, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x5f, 0x6c, 0x65, 0x76, 0x65, 0x6c, 0x03, 0x00,
    0x0d, 0x6d, 0x69, 0x6e, 0x5f, 0x69, 0x6e, 0x63, 0x6c, 0x75, 0x73, 0x69, 0x76, 0x65, 0x00, 0x00,
    0x00, 0x00, 0x03, 0x00, 0x0d, 0x6d, 0x61, 0x78, 0x5f, 0x69, 0x6e, 0x63, 0x6c, 0x75, 0x73, 0x69,
    0x76, 0x65, 0x00, 0x00, 0x00, 0x07, 0x08, 0x00, 0x04, 0x74, 0x79, 0x70, 0x65, 0x00, 0x11, 0x6d,
    0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x75, 0x6e, 0x69, 0x66, 0x6f, 0x72, 0x6d,
    0x00, 0x01, 0x00, 0x0b, 0x68, 0x61, 0x73, 0x5f, 0x63, 0x65, 0x69, 0x6c, 0x69, 0x6e, 0x67, 0x01,
    0x03, 0x00, 0x06, 0x68, 0x65, 0x69, 0x67, 0x68, 0x74, 0x00, 0x00, 0x01, 0x80, 0x00,
];
const DIMENSION_TYPE_END_NBT: &[u8] = &[
    0x0a, 0x08, 0x00, 0x0d, 0x64, 0x65, 0x66, 0x61, 0x75, 0x6c, 0x74, 0x5f, 0x63, 0x6c, 0x6f, 0x63,
    0x6b, 0x00, 0x11, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x74, 0x68, 0x65,
    0x5f, 0x65, 0x6e, 0x64, 0x01, 0x00, 0x16, 0x68, 0x61, 0x73, 0x5f, 0x65, 0x6e, 0x64, 0x65, 0x72,
    0x5f, 0x64, 0x72, 0x61, 0x67, 0x6f, 0x6e, 0x5f, 0x66, 0x69, 0x67, 0x68, 0x74, 0x01, 0x05, 0x00,
    0x0d, 0x61, 0x6d, 0x62, 0x69, 0x65, 0x6e, 0x74, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x3e, 0x80,
    0x00, 0x00, 0x03, 0x00, 0x1f, 0x6d, 0x6f, 0x6e, 0x73, 0x74, 0x65, 0x72, 0x5f, 0x73, 0x70, 0x61,
    0x77, 0x6e, 0x5f, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x5f, 0x6c,
    0x69, 0x6d, 0x69, 0x74, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x0e, 0x68, 0x61, 0x73, 0x5f, 0x66,
    0x69, 0x78, 0x65, 0x64, 0x5f, 0x74, 0x69, 0x6d, 0x65, 0x01, 0x08, 0x00, 0x0a, 0x69, 0x6e, 0x66,
    0x69, 0x6e, 0x69, 0x62, 0x75, 0x72, 0x6e, 0x00, 0x19, 0x23, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72,
    0x61, 0x66, 0x74, 0x3a, 0x69, 0x6e, 0x66, 0x69, 0x6e, 0x69, 0x62, 0x75, 0x72, 0x6e, 0x5f, 0x65,
    0x6e, 0x64, 0x01, 0x00, 0x0c, 0x68, 0x61, 0x73, 0x5f, 0x73, 0x6b, 0x79, 0x6c, 0x69, 0x67, 0x68,
    0x74, 0x01, 0x08, 0x00, 0x06, 0x73, 0x6b, 0x79, 0x62, 0x6f, 0x78, 0x00, 0x03, 0x65, 0x6e, 0x64,
    0x08, 0x00, 0x09, 0x74, 0x69, 0x6d, 0x65, 0x6c, 0x69, 0x6e, 0x65, 0x73, 0x00, 0x11, 0x23, 0x6d,
    0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x69, 0x6e, 0x5f, 0x65, 0x6e, 0x64, 0x06,
    0x00, 0x10, 0x63, 0x6f, 0x6f, 0x72, 0x64, 0x69, 0x6e, 0x61, 0x74, 0x65, 0x5f, 0x73, 0x63, 0x61,
    0x6c, 0x65, 0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x0e, 0x6c, 0x6f, 0x67,
    0x69, 0x63, 0x61, 0x6c, 0x5f, 0x68, 0x65, 0x69, 0x67, 0x68, 0x74, 0x00, 0x00, 0x01, 0x00, 0x0a,
    0x00, 0x0a, 0x61, 0x74, 0x74, 0x72, 0x69, 0x62, 0x75, 0x74, 0x65, 0x73, 0x0a, 0x00, 0x20, 0x6d,
    0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x61, 0x75, 0x64, 0x69, 0x6f, 0x2f, 0x62,
    0x61, 0x63, 0x6b, 0x67, 0x72, 0x6f, 0x75, 0x6e, 0x64, 0x5f, 0x6d, 0x75, 0x73, 0x69, 0x63, 0x0a,
    0x00, 0x07, 0x64, 0x65, 0x66, 0x61, 0x75, 0x6c, 0x74, 0x01, 0x00, 0x15, 0x72, 0x65, 0x70, 0x6c,
    0x61, 0x63, 0x65, 0x5f, 0x63, 0x75, 0x72, 0x72, 0x65, 0x6e, 0x74, 0x5f, 0x6d, 0x75, 0x73, 0x69,
    0x63, 0x01, 0x03, 0x00, 0x09, 0x6d, 0x61, 0x78, 0x5f, 0x64, 0x65, 0x6c, 0x61, 0x79, 0x00, 0x00,
    0x5d, 0xc0, 0x08, 0x00, 0x05, 0x73, 0x6f, 0x75, 0x6e, 0x64, 0x00, 0x13, 0x6d, 0x69, 0x6e, 0x65,
    0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x6d, 0x75, 0x73, 0x69, 0x63, 0x2e, 0x65, 0x6e, 0x64, 0x03,
    0x00, 0x09, 0x6d, 0x69, 0x6e, 0x5f, 0x64, 0x65, 0x6c, 0x61, 0x79, 0x00, 0x00, 0x17, 0x70, 0x00,
    0x00, 0x08, 0x00, 0x1a, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69,
    0x73, 0x75, 0x61, 0x6c, 0x2f, 0x66, 0x6f, 0x67, 0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x07,
    0x23, 0x31, 0x38, 0x31, 0x33, 0x31, 0x38, 0x08, 0x00, 0x24, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72,
    0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x61, 0x6d, 0x62, 0x69, 0x65,
    0x6e, 0x74, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x07,
    0x23, 0x33, 0x66, 0x34, 0x37, 0x33, 0x66, 0x08, 0x00, 0x1a, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72,
    0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x73, 0x6b, 0x79, 0x5f, 0x63,
    0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x07, 0x23, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x0a, 0x00, 0x1e,
    0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x61, 0x75, 0x64, 0x69, 0x6f, 0x2f,
    0x61, 0x6d, 0x62, 0x69, 0x65, 0x6e, 0x74, 0x5f, 0x73, 0x6f, 0x75, 0x6e, 0x64, 0x73, 0x0a, 0x00,
    0x04, 0x6d, 0x6f, 0x6f, 0x64, 0x03, 0x00, 0x0a, 0x74, 0x69, 0x63, 0x6b, 0x5f, 0x64, 0x65, 0x6c,
    0x61, 0x79, 0x00, 0x00, 0x17, 0x70, 0x06, 0x00, 0x06, 0x6f, 0x66, 0x66, 0x73, 0x65, 0x74, 0x40,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x05, 0x73, 0x6f, 0x75, 0x6e, 0x64, 0x00,
    0x16, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x61, 0x6d, 0x62, 0x69, 0x65,
    0x6e, 0x74, 0x2e, 0x63, 0x61, 0x76, 0x65, 0x03, 0x00, 0x13, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x5f,
    0x73, 0x65, 0x61, 0x72, 0x63, 0x68, 0x5f, 0x65, 0x78, 0x74, 0x65, 0x6e, 0x74, 0x00, 0x00, 0x00,
    0x08, 0x00, 0x00, 0x08, 0x00, 0x20, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a,
    0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x73, 0x6b, 0x79, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74,
    0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x07, 0x23, 0x61, 0x63, 0x36, 0x30, 0x63, 0x64, 0x05,
    0x00, 0x21, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75,
    0x61, 0x6c, 0x2f, 0x73, 0x6b, 0x79, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x5f, 0x66, 0x61, 0x63,
    0x74, 0x6f, 0x72, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x05, 0x6d, 0x69, 0x6e, 0x5f, 0x79,
    0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x19, 0x6d, 0x6f, 0x6e, 0x73, 0x74, 0x65, 0x72, 0x5f, 0x73,
    0x70, 0x61, 0x77, 0x6e, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x5f, 0x6c, 0x65, 0x76, 0x65, 0x6c,
    0x00, 0x00, 0x00, 0x0f, 0x01, 0x00, 0x0b, 0x68, 0x61, 0x73, 0x5f, 0x63, 0x65, 0x69, 0x6c, 0x69,
    0x6e, 0x67, 0x00, 0x03, 0x00, 0x06, 0x68, 0x65, 0x69, 0x67, 0x68, 0x74, 0x00, 0x00, 0x01, 0x00,
    0x00,
];
const DIMENSION_TYPE_NETHER_NBT: &[u8] = &[
    0x0a, 0x08, 0x00, 0x0e, 0x63, 0x61, 0x72, 0x64, 0x69, 0x6e, 0x61, 0x6c, 0x5f, 0x6c, 0x69, 0x67,
    0x68, 0x74, 0x00, 0x06, 0x6e, 0x65, 0x74, 0x68, 0x65, 0x72, 0x01, 0x00, 0x16, 0x68, 0x61, 0x73,
    0x5f, 0x65, 0x6e, 0x64, 0x65, 0x72, 0x5f, 0x64, 0x72, 0x61, 0x67, 0x6f, 0x6e, 0x5f, 0x66, 0x69,
    0x67, 0x68, 0x74, 0x00, 0x05, 0x00, 0x0d, 0x61, 0x6d, 0x62, 0x69, 0x65, 0x6e, 0x74, 0x5f, 0x6c,
    0x69, 0x67, 0x68, 0x74, 0x3d, 0xcc, 0xcc, 0xcd, 0x03, 0x00, 0x1f, 0x6d, 0x6f, 0x6e, 0x73, 0x74,
    0x65, 0x72, 0x5f, 0x73, 0x70, 0x61, 0x77, 0x6e, 0x5f, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x5f, 0x6c,
    0x69, 0x67, 0x68, 0x74, 0x5f, 0x6c, 0x69, 0x6d, 0x69, 0x74, 0x00, 0x00, 0x00, 0x0f, 0x01, 0x00,
    0x0e, 0x68, 0x61, 0x73, 0x5f, 0x66, 0x69, 0x78, 0x65, 0x64, 0x5f, 0x74, 0x69, 0x6d, 0x65, 0x01,
    0x08, 0x00, 0x0a, 0x69, 0x6e, 0x66, 0x69, 0x6e, 0x69, 0x62, 0x75, 0x72, 0x6e, 0x00, 0x1c, 0x23,
    0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x69, 0x6e, 0x66, 0x69, 0x6e, 0x69,
    0x62, 0x75, 0x72, 0x6e, 0x5f, 0x6e, 0x65, 0x74, 0x68, 0x65, 0x72, 0x01, 0x00, 0x0c, 0x68, 0x61,
    0x73, 0x5f, 0x73, 0x6b, 0x79, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x00, 0x08, 0x00, 0x06, 0x73, 0x6b,
    0x79, 0x62, 0x6f, 0x78, 0x00, 0x04, 0x6e, 0x6f, 0x6e, 0x65, 0x08, 0x00, 0x09, 0x74, 0x69, 0x6d,
    0x65, 0x6c, 0x69, 0x6e, 0x65, 0x73, 0x00, 0x14, 0x23, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61,
    0x66, 0x74, 0x3a, 0x69, 0x6e, 0x5f, 0x6e, 0x65, 0x74, 0x68, 0x65, 0x72, 0x06, 0x00, 0x10, 0x63,
    0x6f, 0x6f, 0x72, 0x64, 0x69, 0x6e, 0x61, 0x74, 0x65, 0x5f, 0x73, 0x63, 0x61, 0x6c, 0x65, 0x40,
    0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x0e, 0x6c, 0x6f, 0x67, 0x69, 0x63, 0x61,
    0x6c, 0x5f, 0x68, 0x65, 0x69, 0x67, 0x68, 0x74, 0x00, 0x00, 0x00, 0x80, 0x0a, 0x00, 0x0a, 0x61,
    0x74, 0x74, 0x72, 0x69, 0x62, 0x75, 0x74, 0x65, 0x73, 0x05, 0x00, 0x21, 0x6d, 0x69, 0x6e, 0x65,
    0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x66, 0x6f, 0x67,
    0x5f, 0x65, 0x6e, 0x64, 0x5f, 0x64, 0x69, 0x73, 0x74, 0x61, 0x6e, 0x63, 0x65, 0x42, 0xc0, 0x00,
    0x00, 0x0a, 0x00, 0x2b, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69,
    0x73, 0x75, 0x61, 0x6c, 0x2f, 0x64, 0x65, 0x66, 0x61, 0x75, 0x6c, 0x74, 0x5f, 0x64, 0x72, 0x69,
    0x70, 0x73, 0x74, 0x6f, 0x6e, 0x65, 0x5f, 0x70, 0x61, 0x72, 0x74, 0x69, 0x63, 0x6c, 0x65, 0x08,
    0x00, 0x04, 0x74, 0x79, 0x70, 0x65, 0x00, 0x21, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66,
    0x74, 0x3a, 0x64, 0x72, 0x69, 0x70, 0x70, 0x69, 0x6e, 0x67, 0x5f, 0x64, 0x72, 0x69, 0x70, 0x73,
    0x74, 0x6f, 0x6e, 0x65, 0x5f, 0x6c, 0x61, 0x76, 0x61, 0x00, 0x05, 0x00, 0x22, 0x6d, 0x69, 0x6e,
    0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x67, 0x61, 0x6d, 0x65, 0x70, 0x6c, 0x61, 0x79, 0x2f,
    0x73, 0x6b, 0x79, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x5f, 0x6c, 0x65, 0x76, 0x65, 0x6c, 0x40,
    0x80, 0x00, 0x00, 0x01, 0x00, 0x22, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a,
    0x67, 0x61, 0x6d, 0x65, 0x70, 0x6c, 0x61, 0x79, 0x2f, 0x70, 0x69, 0x67, 0x6c, 0x69, 0x6e, 0x73,
    0x5f, 0x7a, 0x6f, 0x6d, 0x62, 0x69, 0x66, 0x79, 0x00, 0x08, 0x00, 0x24, 0x6d, 0x69, 0x6e, 0x65,
    0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x61, 0x6d, 0x62,
    0x69, 0x65, 0x6e, 0x74, 0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72,
    0x00, 0x07, 0x23, 0x33, 0x30, 0x32, 0x38, 0x32, 0x31, 0x01, 0x00, 0x1c, 0x6d, 0x69, 0x6e, 0x65,
    0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x67, 0x61, 0x6d, 0x65, 0x70, 0x6c, 0x61, 0x79, 0x2f, 0x66,
    0x61, 0x73, 0x74, 0x5f, 0x6c, 0x61, 0x76, 0x61, 0x01, 0x08, 0x00, 0x20, 0x6d, 0x69, 0x6e, 0x65,
    0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x73, 0x6b, 0x79,
    0x5f, 0x6c, 0x69, 0x67, 0x68, 0x74, 0x5f, 0x63, 0x6f, 0x6c, 0x6f, 0x72, 0x00, 0x07, 0x23, 0x37,
    0x61, 0x37, 0x61, 0x66, 0x66, 0x05, 0x00, 0x21, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66,
    0x74, 0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x73, 0x6b, 0x79, 0x5f, 0x6c, 0x69, 0x67,
    0x68, 0x74, 0x5f, 0x66, 0x61, 0x63, 0x74, 0x6f, 0x72, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x23,
    0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74, 0x3a, 0x67, 0x61, 0x6d, 0x65, 0x70, 0x6c,
    0x61, 0x79, 0x2f, 0x77, 0x61, 0x74, 0x65, 0x72, 0x5f, 0x65, 0x76, 0x61, 0x70, 0x6f, 0x72, 0x61,
    0x74, 0x65, 0x73, 0x01, 0x05, 0x00, 0x23, 0x6d, 0x69, 0x6e, 0x65, 0x63, 0x72, 0x61, 0x66, 0x74,
    0x3a, 0x76, 0x69, 0x73, 0x75, 0x61, 0x6c, 0x2f, 0x66, 0x6f, 0x67, 0x5f, 0x73, 0x74, 0x61, 0x72,
    0x74, 0x5f, 0x64, 0x69, 0x73, 0x74, 0x61, 0x6e, 0x63, 0x65, 0x41, 0x20, 0x00, 0x00, 0x00, 0x03,
    0x00, 0x05, 0x6d, 0x69, 0x6e, 0x5f, 0x79, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x19, 0x6d, 0x6f,
    0x6e, 0x73, 0x74, 0x65, 0x72, 0x5f, 0x73, 0x70, 0x61, 0x77, 0x6e, 0x5f, 0x6c, 0x69, 0x67, 0x68,
    0x74, 0x5f, 0x6c, 0x65, 0x76, 0x65, 0x6c, 0x00, 0x00, 0x00, 0x07, 0x01, 0x00, 0x0b, 0x68, 0x61,
    0x73, 0x5f, 0x63, 0x65, 0x69, 0x6c, 0x69, 0x6e, 0x67, 0x01, 0x03, 0x00, 0x06, 0x68, 0x65, 0x69,
    0x67, 0x68, 0x74, 0x00, 0x00, 0x01, 0x00, 0x00,
];
const WORLD_CLOCK_OVERWORLD_NBT: &[u8] = &[0x0a, 0x00];
const WORLD_CLOCK_END_NBT: &[u8] = &[0x0a, 0x00];

/// Encodes one Configuration-phase `registry_data` packet from its registry
/// key and ordered entries, writing the wire format by hand: the registry
/// identifier, the entry count, then per entry its identifier, a `true` data
/// flag, and the entry's full serialized network NBT **including its root tag
/// byte**. Entry index *is* the holder id the rest of the wire uses, so order
/// here is load-bearing — and the bodies are captured vanilla bytes, so a real
/// client reads its own format rather than a re-encoding of our understanding.
fn encode_registry_data_packet(registry: &str, entries: &[(&str, &[u8])]) -> ServerDirective {
    let mut w = Writer::default();
    w.string(registry);
    w.var_i32(entries.len() as i32);
    for &(id, nbt) in entries {
        w.string(id);
        w.bool(true);
        w.bytes(nbt);
    }
    ServerDirective::Send {
        packet_id: configuration::clientbound::REGISTRY_DATA,
        payload: w.into_vec(),
    }
}

/// The `minecraft:dimension_type` registry this server publishes, **in wire
/// order**.
///
/// A holder id is an index into this list, so the order is the mapping — which is
/// why it is a named table read by both [`ServerProtocol::encode_registry_data`]
/// and [`dimension_type_holder_id`] rather than an inline literal at the one call
/// site. Two copies of an *order* is the shape that silently renumbers a dimension:
/// the registry would still be well-formed, the ids would still resolve, and every
/// Nether trip would frame its chunks against `the_end`.
const DIMENSION_TYPE_REGISTRY: [(&str, &[u8]); 4] = [
    ("minecraft:overworld", DIMENSION_TYPE_OVERWORLD_NBT),
    (
        "minecraft:overworld_caves",
        DIMENSION_TYPE_OVERWORLD_CAVES_NBT,
    ),
    ("minecraft:the_end", DIMENSION_TYPE_END_NBT),
    ("minecraft:the_nether", DIMENSION_TYPE_NETHER_NBT),
];

/// The `dimension_type` holder id for a level key, or `None` for a key this
/// server's registry does not publish.
///
/// Derived from [`DIMENSION_TYPE_REGISTRY`]'s order rather than written down, for
/// the reason that constant's doc gives.
fn dimension_type_holder_id(dimension: &str) -> Option<i32> {
    DIMENSION_TYPE_REGISTRY
        .iter()
        .position(|(id, _)| *id == dimension)
        .and_then(|index| i32::try_from(index).ok())
}

/// The Nether's `sea_level` — `noise_settings/nether.json`'s own value, and the
/// height its lava seas fill to. **Not** derivable from the overworld's 63.
const NETHER_SEA_LEVEL: i32 = 32;

/// The `sea_level` a `respawn` packet carries for a destination level.
///
/// The End has no sea and vanilla's `the_end` noise settings put `sea_level` at 0;
/// an unknown key gets the overworld's, which is the value this server sent for
/// every packet before dimensions existed.
fn sea_level_for_dimension(dimension: &str) -> i32 {
    match dimension {
        "minecraft:the_nether" => NETHER_SEA_LEVEL,
        "minecraft:the_end" => 0,
        _ => OVERWORLD_SEA_LEVEL,
    }
}

/// The [`ChunkShape`] a served column is framed against, taken from **the column
/// itself** rather than from a hardcoded overworld constant.
///
/// # Why the column and not the connection
///
/// A chunk packet's section count is a property of the *dimension*, and the client
/// derives its own from the `dimension_type` holder id `login`/`respawn` carried
/// (`V770Adapter::enter_dimension`). Once the server can host more than one
/// dimension, a constant here is wrong for one of them: a Nether column is
/// `min_y 0, height 256` (16 sections) against the overworld's `-64, 384` (24), and
/// serving one through the other's shape mis-slices every section — a decode error
/// on the client, not a cosmetic one.
///
/// `ServerChunkColumn` already carries `min_y` and `height`, and `lodestone-server`
/// builds every column with the dimension's own window (see that crate's
/// `NetherChunkSource::WINDOW_HEIGHT`, which is the dimension type's 256 and
/// deliberately not the generator's 128). So reading them off the column keeps the
/// wire framing and the terrain that fills it derived from **one** number, rather
/// than from two that must be kept in agreement by hand.
///
/// Only the vertical window comes from the column: the palette framing and the
/// air/biome ids are properties of the protocol family, exactly as
/// `V770Adapter::enter_dimension` documents for the receiving side.
///
/// # This recognises the two real windows and defaults to the overworld's
///
/// It is deliberately **not** `section_count = height / 16` for an arbitrary column.
/// A chunk's section count is a property of the *dimension the client resolved*, and
/// nothing else: a client framed against the overworld reads exactly 24 sections
/// whatever the server's column happens to contain.
///
/// That distinction was measured. Several of this crate's own loopback fixtures serve
/// deliberately tiny columns — `combat_live`'s `AirSource` is `ChunkColumn::new(0,
/// 16)`, one section — because the test is about combat and no block is ever read.
/// Framing that column against its own height emitted a one-section packet to a
/// 24-section client, and six live tests failed with *"initial column never
/// arrived"*: the client joined, spawned, and silently could not decode a single
/// chunk. The short column was always fine against the overworld window (the missing
/// rows are simply empty sections), and it still is.
///
/// So the mapping is from a *known dimension window* to that dimension's shape, and
/// anything unrecognised keeps the overworld's — the exact behaviour every caller had
/// before the Nether existed.
fn shape_for_column(column: &ServerChunkColumn) -> ChunkShape {
    let nether_or_end = ChunkShape::nether_or_end_1_21();
    if column.min_y == nether_or_end.min_y
        && column.height == nether_or_end.world_height as i32
    {
        return nether_or_end;
    }
    ChunkShape::overworld_1_21()
}

/// The single column-encode body in this crate.
///
/// It lives on the [`ChunkEncoder`] impl rather than on
/// [`ServerProtocol::encode_chunk`] because a `ChunkEncoder` is `'static` and
/// therefore movable into the `spawn_blocking` closure that generated the column
/// — which is where these 62 M instructions per column belong, and specifically
/// not on the connection task that owes the player a reply to their block break.
/// `ServerProtocol::encode_chunk` calls straight through, so the two cannot
/// drift.
impl ChunkEncoder for V770ServerProtocol {
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ServerChunkColumn) -> ServerDirective {
        let shape = shape_for_column(column);
        let world_column = build_world_column(&shape, column);
        let light = compute_served_light(&world_column);
        let payload = encode_column_body(cx, cz, &shape, &world_column, &light, column);
        ServerDirective::Send {
            packet_id: play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
            payload,
        }
    }
}

/// The zlib compression threshold this server enables during login,
/// matching vanilla's own default
/// (`network-compression-threshold=256` — measured identical across every
/// `server.properties` under `.cache/mc/`). Packets whose uncompressed body
/// is at least this many bytes are zlib-framed; smaller ones go out through
/// compressed framing uncompressed (`packets::login::LoginCompression`'s own
/// doc comment, `lodestone-net`'s `Codec`).
const COMPRESSION_THRESHOLD: i32 = 256;

impl ServerProtocol for V770ServerProtocol {
    fn decode(&self, state: lodestone_core::State, packet_id: i32, payload: &[u8]) -> ServerBound {
        use lodestone_core::State;

        match state {
            State::Handshaking if packet_id == handshaking::serverbound::INTENTION => {
                match decode_full::<Intention>(payload) {
                    Some(intention) => {
                        let next_state = if intention.next_state == 2 {
                            State::Login
                        } else {
                            State::Status
                        };
                        ServerBound::Handshake { next_state }
                    }
                    None => ServerBound::Ignored,
                }
            }
            // The Status phase. A handshake with `next_state == 1`
            // used to always *reach* `State::Status` here, but nothing answered
            // it, so our server was invisible in a real client's multiplayer
            // list — the client sends `status_request`, waits, and gives up.
            //
            // `ServerboundStatusRequestPacket` is vanilla's own stream-codec type's own unit(INSTANCE)`: the body is
            // genuinely empty, so an empty payload is the *correct* decode, not
            // a truncation. `decode_full` on a zero-field struct would be an
            // equivalent way to say this; the explicit emptiness check is
            // clearer and still rejects a payload carrying junk.
            State::Status if packet_id == status::serverbound::STATUS_REQUEST => {
                if payload.is_empty() {
                    ServerBound::StatusRequest
                } else {
                    ServerBound::Ignored
                }
            }
            // `ServerboundPingRequestPacket`: a single big-endian `long`.
            // The same struct
            // the Play-state arm below already decodes — vanilla shares one
            // packet class across both states, which is why
            // `packets::common::PingRequest` documents itself that way.
            State::Status if packet_id == status::serverbound::PING_REQUEST => {
                match decode_full::<PingRequest>(payload) {
                    Some(ping) => ServerBound::PingRequest { time: ping.time },
                    None => ServerBound::Ignored,
                }
            }
            State::Login if packet_id == login::serverbound::HELLO => {
                match decode_full::<LoginHello>(payload) {
                    Some(hello) => ServerBound::LoginStart {
                        username: hello.name,
                        uuid: hello.profile_id,
                    },
                    None => ServerBound::Ignored,
                }
            }
            State::Login if packet_id == login::serverbound::LOGIN_ACKNOWLEDGED => {
                ServerBound::LoginAcknowledged
            }
            // The client's answer to an online-mode
            // `EncryptionRequest`. Pure lift, no crypto — both fields are
            // still RSA ciphertext; `crate::server`'s connection loop owns
            // decrypting them.
            State::Login if packet_id == login::serverbound::KEY => {
                match decode_full::<EncryptionResponse>(payload) {
                    Some(key) => ServerBound::EncryptionResponse {
                        shared_secret: key.shared_secret,
                        verify_token: key.verify_token,
                    },
                    None => ServerBound::Ignored,
                }
            }
            State::Configuration
                if packet_id == configuration::serverbound::FINISH_CONFIGURATION =>
            {
                ServerBound::ConfigurationFinished
            }
            // A client announces the channels it supports during
            // Configuration, via `minecraft:register`/`minecraft:unregister`
            // custom payloads — the same wire packet as the Play-phase arm
            // below, same lift: every channel becomes `ServerBound::CustomPayload`
            // and the version-free server owns the interpretation.
            State::Configuration
                if packet_id == configuration::serverbound::CUSTOM_PAYLOAD =>
            {
                decode_custom_payload(payload).unwrap_or(ServerBound::Ignored)
            }
            State::Play if packet_id == play::serverbound::KEEP_ALIVE => {
                match decode_full::<KeepAlive>(payload) {
                    Some(keep_alive) => ServerBound::KeepAlive { id: keep_alive.id },
                    None => ServerBound::Ignored,
                }
            }
            // All four serverbound movement packets are lifted.
            // Vanilla's vanilla's own client-side local-player class's own send position sends exactly *one* of
            // them per tick, choosing on which of position/look is dirty, so
            // dropping any one of the four is not a redundancy — it is a
            // hole in a partition. `MOVE_PLAYER_POS_ROT` in particular used
            // to decode `yaw`/`pitch` and throw them away, which is why a
            // walking, turning player's avatar stood frozen at yaw 0 for
            // every other client.
            State::Play if packet_id == play::serverbound::MOVE_PLAYER_POS => {
                match decode_full::<MovePlayerPos>(payload) {
                    Some(m) => ServerBound::PlayerMoved {
                        x: m.x,
                        y: m.y,
                        z: m.z,
                        // Genuinely absent from this packet's wire body, not
                        // merely unread — see the variant's doc comment.
                        rotation: None,
                        on_ground: m.flags & MOVE_FLAG_ON_GROUND != 0,
                    },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::MOVE_PLAYER_POS_ROT => {
                match decode_full::<MovePlayerPosRot>(payload) {
                    Some(m) => ServerBound::PlayerMoved {
                        x: m.x,
                        y: m.y,
                        z: m.z,
                        rotation: Some(Rotation {
                            yaw: m.yaw,
                            pitch: m.pitch,
                        }),
                        on_ground: m.flags & MOVE_FLAG_ON_GROUND != 0,
                    },
                    None => ServerBound::Ignored,
                }
            }
            // Vanilla's own serverbound player-action packet's action enum,
            // read off the enum's own
            // declaration order in 26.2 (confirmed against the decompiled source)
            // rather than guessed: START_DESTROY_BLOCK, ABORT_DESTROY_BLOCK,
            // STOP_DESTROY_BLOCK, **DROP_ALL_ITEMS, DROP_ITEM**, RELEASE_USE_ITEM,
            // SWAP_ITEM_WITH_OFFHAND, STAB. Note 3 is the *whole stack* and 4 is
            // one item — the order reads backwards from the key bindings (`Q` is
            // one item, `Ctrl+Q` is the stack), and swapping them makes `Q` throw
            // the player's entire stack.
            //
            // 3 and 4 used to fall into the `_ => Ignored` arm below, so pressing
            // `Q` did nothing whatsoever; they now lift to
            // `ServerBound::ItemDropped`. 6 (SWAP_ITEM_WITH_OFFHAND) now lifts to
            // `ServerBound::SwapItemInHand`; 7 (STAB) still has no server-side
            // model.
            State::Play if packet_id == play::serverbound::PLAYER_ACTION => {
                match decode_full::<PlayerAction>(payload) {
                    Some(action) => {
                        let pos = unpack_block_pos(action.pos);
                        let face = face_from_ordinal(i32::from(action.direction));
                        match action.action {
                            0 => ServerBound::BlockAction {
                                action: BlockActionKind::StartDestroy,
                                pos,
                                face,
                                sequence: action.sequence,
                            },
                            1 => ServerBound::BlockAction {
                                action: BlockActionKind::AbortDestroy,
                                pos,
                                face,
                                sequence: action.sequence,
                            },
                            2 => ServerBound::BlockAction {
                                action: BlockActionKind::StopDestroy,
                                pos,
                                face,
                                sequence: action.sequence,
                            },
                            3 => ServerBound::ItemDropped { whole_stack: true },
                            4 => ServerBound::ItemDropped { whole_stack: false },
                            // The bow's release. This ordinal used to
                            // fall through to `Ignored`, which is why a player
                            // could draw a bow (the client animates locally) and
                            // never fire anything — the packet that ends the draw
                            // reached no server-side model at all.
                            5 => ServerBound::ReleaseUseItem,
                            // The `F`-key hand swap. STAB (7) is still
                            // genuinely unmodelled — see
                            // `ServerBound::SwapItemInHand`'s own doc comment
                            // for the consumer and why this and 7 stayed
                            // paired until now.
                            6 => ServerBound::SwapItemInHand,
                            _ => ServerBound::Ignored,
                        }
                    }
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::USE_ITEM_ON => {
                match decode_full::<UseItemOn>(payload) {
                    Some(use_item) => ServerBound::UseItemOn {
                        pos: unpack_block_pos(use_item.pos),
                        face: face_from_ordinal(use_item.face),
                        cursor: Vec3f {
                            x: use_item.cursor_x,
                            y: use_item.cursor_y,
                            z: use_item.cursor_z,
                        },
                        sequence: use_item.sequence,
                        // Same malformed-input convention as `USE_ITEM`'s hand just
                        // above: anything outside `0..=1` degrades to main hand
                        // rather than dropping the packet.
                        hand: u8::try_from(use_item.hand).unwrap_or(0),
                    },
                    None => ServerBound::Ignored,
                }
            }
            // Right-click-in-air, the trigger for every player-side
            // projectile launch. The yaw/pitch this packet carries is the reason
            // a throw has a direction at all — this crate tracks no per-connection
            // rotation, and the last `PlayerRotated` is not necessarily the facing
            // at the instant of the throw.
            State::Play if packet_id == play::serverbound::USE_ITEM => {
                match decode_full::<UseItem>(payload) {
                    Some(u) => ServerBound::UseItem {
                        // The wire field is a VarInt; anything outside `0..=1` is
                        // malformed and reads as the main hand rather than dropping
                        // the packet, matching this module's established
                        // "malformed input degrades the effect, not the connection"
                        // convention (`face_from_ordinal`).
                        hand: u8::try_from(u.hand).unwrap_or(0),
                        yaw: u.yaw,
                        pitch: u.pitch,
                    },
                    None => ServerBound::Ignored,
                }
            }
            // The `Attack` packet is the whole trigger for a
            // melee hit — see `ServerBound::Attack`'s own doc comment for why
            // the sibling `minecraft:interact` packet is deliberately left
            // undecoded (no interaction model to hand it to).
            State::Play if packet_id == play::serverbound::ATTACK => {
                match decode_full::<Attack>(payload) {
                    Some(a) => ServerBound::Attack {
                        entity_id: a.entity_id,
                    },
                    None => ServerBound::Ignored,
                }
            }
            // Vanilla's own serverbound player-input packet: a single flags byte
            // (its own `Input` stream codec, confirmed against the decompiled
            // source) — bit `0x40` is `sprint`,
            // bit `0x20` is `shift`, and bit `0x10` is `jump`, the three
            // flags `ServerBound::PlayerInput` carries (see its own doc
            // comment for why the rest are decoded off the wire here and
            // then dropped rather than threaded further). `jump` used to be
            // one of those dropped flags — the exact "a value the decoder
            // reads off the wire and discards at the decode site" shape —
            // until camel dash needed it: the camel class's own on player jump is this bit's
            // whole trigger.
            State::Play if packet_id == play::serverbound::PLAYER_INPUT => {
                let mut r = Reader::new(payload);
                match r.u8() {
                    Ok(flags) if r.ensure_empty().is_ok() => ServerBound::PlayerInput {
                        sprint: flags & 0x40 != 0,
                        shift: flags & 0x20 != 0,
                        jump: flags & 0x10 != 0,
                    },
                    _ => ServerBound::Ignored,
                }
            }
            // World/block-admin decode. `CHANGE_DIFFICULTY`,
            // `LOCK_DIFFICULTY` and `SET_GAME_RULE` are the three cheap,
            // observable packets among the thirteen operator/debug ones — see
            // `crate::server::apply_difficulty_change`/
            // `apply_game_rule_changed` for the consumer and
            // `WorldAdminState`'s doc comment for what is deliberately not
            // modelled (a `GameRules` registry, cross-connection broadcast).
            // The command/structure/jigsaw-block and test-only packets from
            // the same issue are deliberately not decoded here — see that
            // issue's tracker comment for why each is a deep feature rather
            // than a decode gap.
            State::Play if packet_id == play::serverbound::CHANGE_DIFFICULTY => {
                match decode_full::<ChangeDifficultyServerbound>(payload)
                    .and_then(|p| difficulty_from_ordinal(p.difficulty))
                {
                    Some(difficulty) => ServerBound::DifficultyChanged { difficulty },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::LOCK_DIFFICULTY => {
                match decode_full::<LockDifficulty>(payload) {
                    Some(p) => ServerBound::DifficultyLockChanged { locked: p.locked },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::SET_GAME_RULE => {
                match decode_full::<SetGameRule>(payload) {
                    Some(p) => ServerBound::GameRuleChanged {
                        entries: p.entries.into_iter().map(|e| (e.key, e.value)).collect(),
                    },
                    None => ServerBound::Ignored,
                }
            }
            // Server-authoritative inventory model: the prerequisite this
            // itself asked for, and the two packets that unblock it end to
            // end — see `lodestone_server::inventory`'s module doc comment.
            State::Play if packet_id == play::serverbound::SET_CARRIED_ITEM => {
                match decode_full::<SetCarriedItem>(payload).and_then(|p| u8::try_from(p.slot).ok())
                {
                    // Mirrors vanilla's own hotbar-slot check
                    // (confirmed against the decompiled inventory source) at the decode boundary, per
                    // `ServerBound::CarriedItemChanged`'s own doc comment.
                    Some(slot) if slot < HOTBAR_SIZE => ServerBound::CarriedItemChanged { slot },
                    _ => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::CONTAINER_CLICK => {
                decode_container_click(payload).unwrap_or(ServerBound::Ignored)
            }
            // `ServerboundContainerClosePacket`: a single VarInt container id
            // (vanilla's own buffer-writer helper's own write container id, the same plain-VarInt
            // vanilla's own codec library's own container accessor codec `decode_container_click`
            // already reads for its own window id). No existing struct to
            // decode through — this is the smallest possible packet, so a
            // hand-written read is simpler than adding a one-field struct.
            State::Play if packet_id == play::serverbound::CONTAINER_CLOSE => {
                let mut r = Reader::new(payload);
                match r.var_i32() {
                    Ok(window_id) if r.ensure_empty().is_ok() => {
                        ServerBound::ContainerClosed { window_id }
                    }
                    _ => ServerBound::Ignored,
                }
            }

            // Movement/player-state, remaining 6 of 11 —
            // `MOVE_PLAYER_ROT` and `MOVE_PLAYER_STATUS_ONLY` now lift into
            // their own variants just below, alongside the two position-
            // carrying siblings above. Every wire layout below is checked
            // directly against
            // `.cache/mc/26.2/src`'s `ServerboundMovePlayerPacket`/
            // `ServerboundPlayerAbilitiesPacket`/`ServerboundMoveVehiclePacket`/
            // etc. — not merely `decode(encode(x))` against this crate's own
            // client encoder, which already sends every one of these
            // (`crate::adapter`). The remaining markers without a server
            // consumer stay `Ignored`; readiness is the one marker that now
            // gates movement-dependent simulation in `lodestone-server`.
            State::Play if packet_id == play::serverbound::MOVE_PLAYER_ROT => {
                match decode_full::<MovePlayerRot>(payload) {
                    Some(m) => ServerBound::PlayerRotated {
                        yaw: m.yaw,
                        pitch: m.pitch,
                        on_ground: m.flags & MOVE_FLAG_ON_GROUND != 0,
                    },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::MOVE_PLAYER_STATUS_ONLY => {
                match decode_full::<MovePlayerStatusOnly>(payload) {
                    Some(m) => ServerBound::PlayerStatusOnly {
                        on_ground: m.flags & MOVE_FLAG_ON_GROUND != 0,
                    },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::PLAYER_ABILITIES => {
                match decode_full::<ServerboundPlayerAbilities>(payload) {
                    Some(p) => ServerBound::PlayerAbilitiesChanged {
                        flying: p.flags & SERVERBOUND_ABILITY_FLAG_FLYING != 0,
                    },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::PLAYER_LOADED => {
                match decode_full::<PlayerLoaded>(payload) {
                    Some(_) => ServerBound::PlayerLoaded,
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::ACCEPT_TELEPORTATION => {
                match decode_full::<AcceptTeleportation>(payload) {
                    Some(teleport) => ServerBound::TeleportationAccepted { id: teleport.id },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::CLIENT_TICK_END => {
                match decode_full::<ClientTickEnd>(payload) {
                    Some(_) => ServerBound::ClientTickEnded,
                    None => ServerBound::Ignored,
                }
            }
            // Vehicle movement lifts into a real variant consumed by the
            // server's vehicle registry. The client's authoritative boat
            // transform therefore reaches the simulation and can be observed
            // by other connected players.
            //
            // No entity id is present on the wire; the server associates this
            // transform with the player's root vehicle, so the variant carries
            // only position and orientation.
            State::Play if packet_id == play::serverbound::MOVE_VEHICLE => {
                match decode_full::<MoveVehicle>(payload) {
                    Some(m) => ServerBound::VehicleMoved {
                        position: Vec3::new(m.x, m.y, m.z),
                        yaw: m.yaw,
                        pitch: m.pitch,
                    },
                    None => ServerBound::Ignored,
                }
            }
            // `PADDLE_BOAT` carries the left/right paddle states to
            // `MobSim::apply_boat_paddle`. The effect is cosmetic, but the
            // server applies it so a second connected player sees another
            // player's boat animate its paddles.
            State::Play if packet_id == play::serverbound::PADDLE_BOAT => {
                match decode_full::<PaddleBoat>(payload) {
                    Some(PaddleBoat { left, right }) => ServerBound::PaddleBoat { left, right },
                    None => ServerBound::Ignored,
                }
            }

            // Entity actions/combat/interaction, remaining 6 of
            // 9 — `ATTACK`, `PLAYER_ACTION` and `USE_ITEM_ON` are already
            // decoded and applied above. All six below are field-verified
            // against `.cache/mc/26.2/src`'s decompiled packet classes.
            //
            // Vanilla's own interact packet: VarInt target entity id, VarInt
            // interaction-hand ordinal, a low-precision vector location (the
            // same codec [`read_lp_vec3`](crate::packets::entity::read_lp_vec3)
            // already decodes and unit-tests for entity velocity), then a
            // trailing boolean for the secondary-action (shift) modifier. 26.2
            // split the old combined interact/attack packet in two (see
            // `ServerBound::Attack`'s own doc comment); this is the right-click
            // half, and its consumer is
            // `lodestone_server::mobs::MobSim::interact`.
            //
            // The location is read and dropped rather than skipped: it is the only
            // way the `ensure_empty` below can still prove the frame was fully
            // consumed, which is what catches a field-order transposition.
            State::Play if packet_id == play::serverbound::INTERACT => {
                let mut r = Reader::new(payload);
                let decoded = (|| -> lodestone_core::Result<ServerBound> {
                    let entity_id = r.var_i32()?;
                    let hand = r.var_i32()?;
                    let _location = read_lp_vec3(&mut r)?;
                    let using_secondary_action = r.bool()?;
                    r.ensure_empty()?;
                    Ok(ServerBound::InteractEntity {
                        entity_id,
                        hand,
                        using_secondary_action,
                    })
                })();
                decoded.unwrap_or(ServerBound::Ignored)
            }
            // `ServerboundSwingPacket`: a single VarInt hand ordinal. See
            // `ServerBound::Swing`'s own doc comment for the consumer (a
            // broadcast to every other connected player) and the "malformed
            // input degrades rather than drops" convention this shares with
            // `USE_ITEM`/`USE_ITEM_ON` — anything outside `0..=1` reads as
            // the main hand.
            State::Play if packet_id == play::serverbound::SWING => {
                match decode_full::<Swing>(payload) {
                    Some(s) => ServerBound::Swing {
                        hand: u8::try_from(s.hand).unwrap_or(0),
                    },
                    None => ServerBound::Ignored,
                }
            }
            // `USE_ITEM` is decoded and connected above (the
            // right-click-in-air arm constructing `ServerBound::UseItem`) —
            // this used to be a second, shadowed stub that decoded to
            // `Ignored` and could never run because a `match` picks the
            // first satisfied guard.
            State::Play if packet_id == play::serverbound::PLAYER_COMMAND => {
                // Only the `STOP_SLEEPING` action (0) has a
                // server-side consumer — the "wake up" the client sends when
                // the player climbs out of bed or dies. The other actions
                // (sprinting/riding/jump states) decode to Ignored, exactly
                // like `BlockAction`'s unconsumed ordinals. Note the wire
                // `entityId` is always the sender's own local-player id (1)
                // and is deliberately dropped: who is waking up comes from the
                // connection's own player id, not the wire.
                match decode_full::<PlayerCommand>(payload) {
                    Some(PlayerCommand { action: 0, .. }) => {
                        ServerBound::PlayerCommand { action: 0 }
                    }
                    _ => ServerBound::Ignored,
                }
            }
            // `ServerboundSpectatorActionPacket`: a single VarInt using
            // vanilla's own codec library's own optional-var-int accessor's offset encoding (`0` = no
            // target, a present id `i` written as `i + 1`) — the exact
            // inverse of `crate::adapter::encode_spectator_action`, which
            // already documents why this must be hand-decoded rather than a
            // derived `Option<i32>` (a bool-prefixed optional would silently
            // misparse this packet).
            State::Play if packet_id == play::serverbound::SPECTATOR_ACTION => {
                let mut r = Reader::new(payload);
                let decoded = (|| -> lodestone_core::Result<Option<i32>> {
                    let raw = r.var_i32()?;
                    let target_entity_id = if raw == 0 { None } else { Some(raw - 1) };
                    r.ensure_empty()?;
                    Ok(target_entity_id)
                })();
                match decoded {
                    Ok(target_entity_id) => ServerBound::SpectatorAction { target_entity_id },
                    Err(_) => ServerBound::Ignored,
                }
            }
            // `ServerboundTeleportToEntityPacket`: a single uuid — the
            // spectator's chosen target from the tab list. See
            // `ServerBound::TeleportToEntity`'s own doc comment for the
            // consumer and its disclosed scope (connected players only).
            State::Play if packet_id == play::serverbound::TELEPORT_TO_ENTITY => {
                match decode_full::<TeleportToEntity>(payload) {
                    Some(t) => ServerBound::TeleportToEntity { uuid: t.uuid },
                    None => ServerBound::Ignored,
                }
            }

            // Inventory/container: remaining packets beyond the
            // three already decoded and applied above (`CONTAINER_CLICK`,
            // `CONTAINER_CLOSE`, `SET_CARRIED_ITEM`, into the real
            // `PlayerInventory` model). Every struct below
            // either already exists and is exercised by this crate's own
            // client encoder (`crate::adapter`, itself checked against
            // `docs/container-clicks.md` and `.cache/mc/26.2/src`), or is
            // hand-decoded against the same decompiled source directly. All
            // decode to `Ignored`: `PlayerInventory` only covers window 0's
            // 41 native slots via `ContainerClicked`/`CarriedItemChanged`
            // today — it has no recipe-book, beacon, anvil, bundle, book,
            // sign, or creative-slot state to receive any of these into yet.
            // `SET_CREATIVE_MODE_SLOT` is the one exception worth flagging:
            // unlike the rest of this family it writes into exactly the slot
            // space `PlayerInventory` already models (window 0), so wiring
            // it up is "add a `ServerBound::CreativeModeSlotSet { slot,
            // item }` variant and an arm that writes straight into
            // `PlayerInventory`, mirroring `ContainerClicked`'s own
            // consumer" rather than a new feature — the smallest next step
            // in this family, once someone can touch `lodestone-server`.
            // A follow-up fix: this used to decode-and-discard. The
            // enchanting table's "choose an offer" button is the only
            // consumer (`ServerBound::ContainerButtonClick`'s own doc
            // comment) — `crate::server`'s handler re-derives the cost from
            // the currently open table rather than trusting `button_id`
            // beyond "which of the three slots".
            State::Play if packet_id == play::serverbound::CONTAINER_BUTTON_CLICK => {
                match decode_full::<ContainerButtonClick>(payload) {
                    Some(ContainerButtonClick { window_id, button_id }) => {
                        ServerBound::ContainerButtonClick { window_id, button_id }
                    }
                    None => ServerBound::Ignored,
                }
            }
            // `ServerboundContainerSlotStateChangedPacket` — a crafter's
            // per-slot enable/disable toggle. `crate::server`'s consumer
            // checks the currently open menu is really a crafter before
            // touching `crate::block_entities::BlockEntity::Crafter`, the
            // same "don't trust the wire id alone" shape
            // `ContainerButtonClick`'s own handler already has for the
            // enchanting table.
            State::Play if packet_id == play::serverbound::CONTAINER_SLOT_STATE_CHANGED => {
                match decode_full::<ContainerSlotStateChanged>(payload) {
                    Some(ContainerSlotStateChanged { slot_id, container_id, new_state }) => {
                        ServerBound::ContainerSlotStateChanged {
                            window_id: container_id,
                            slot_id,
                            new_state,
                        }
                    }
                    None => ServerBound::Ignored,
                }
            }
            // Vanilla's own serverbound set-creative-mode-slot packet:
            // big-endian `i16` slot
            // (vanilla's own fixed-width `SHORT` codec), then an
            // [`read_optional_item_stack`]
            // item (vanilla's own untrusted-optional item-stack stream codec)
            // — see that
            // helper's doc comment for why it is not the same shape as
            // [`read_hashed_stack`]. Field order and both codecs read
            // straight off vanilla's own set-creative-mode-slot packet's
            // composite stream codec, not off our own encoder.
            //
            // This lifts into [`ServerBound::CreativeModeSlotSet`], whose
            // consumer (`apply_creative_mode_slot_set`) writes through
            // `PlayerInventory::apply_menu_slot_change`. Vanilla's own
            // valid-slot/drop split (confirmed against the decompiled server
            // packet-listener source,
            // `1..=45` accepted, `< 0` meaning "drop into the world") is left
            // to that consumer rather than filtered here, so the variant
            // carries the raw wire slot — see its doc comment.
            State::Play if packet_id == play::serverbound::SET_CREATIVE_MODE_SLOT => {
                let mut r = Reader::new(payload);
                // Qualified as `self::` (not a bare call) so
                // `cargo xtask connectedness`'s delegate-following classifier
                // doesn't try to recurse into a helper that returns
                // `Option<Option<ItemStack>>` rather than `ServerBound`.
                let decoded = (|| -> Option<(i16, Option<ItemStack>)> {
                    let slot = r.i16().ok()?;
                    let item = self::read_optional_item_stack(&mut r)?;
                    r.ensure_empty().ok()?;
                    Some((slot, item))
                })();
                match decoded {
                    Some((slot, item)) => ServerBound::CreativeModeSlotSet { slot, item },
                    None => ServerBound::Ignored,
                }
            }
            // `recipe` is a vanilla's own recipe-display-id type's own index — an opaque
            // position in the book the *server* handed out, not a recipe name; see
            // `ServerBound::RecipePlaced`'s own doc comment.
            State::Play if packet_id == play::serverbound::PLACE_RECIPE => {
                match decode_full::<PlaceRecipe>(payload) {
                    Some(p) => ServerBound::RecipePlaced {
                        window_id: p.container_id,
                        recipe_index: p.recipe,
                        use_max_items: p.use_max_items,
                    },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::RECIPE_BOOK_CHANGE_SETTINGS => {
                match decode_full::<RecipeBookChangeSettings>(payload).and_then(|p| {
                    let book_type = match p.book_type {
                        0 => RecipeBookType::Crafting,
                        1 => RecipeBookType::Furnace,
                        2 => RecipeBookType::BlastFurnace,
                        3 => RecipeBookType::Smoker,
                        _ => return None,
                    };
                    Some(ServerBound::RecipeBookSettingsChanged {
                        book_type,
                        open: p.is_open,
                        filtering: p.is_filtering,
                    })
                }) {
                    Some(update) => update,
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::RECIPE_BOOK_SEEN_RECIPE => {
                match decode_full::<RecipeBookSeenRecipe>(payload) {
                    Some(p) => ServerBound::RecipeBookRecipeSeen {
                        recipe_index: p.recipe,
                    },
                    None => ServerBound::Ignored,
                }
            }
            // `SELECT_TRADE` lifts into `ServerBound::SelectTrade`. The server
            // consumer resolves the villager from this connection's tracked
            // open-merchant state; the packet itself carries no window id.
            State::Play if packet_id == play::serverbound::SELECT_TRADE => {
                match decode_full::<SelectTrade>(payload) {
                    Some(p) => ServerBound::SelectTrade { index: p.index },
                    None => ServerBound::Ignored,
                }
            }
            // The beacon-setting packet carries two optional effect keys
            // (primary, then secondary), each read by
            // [`read_optional_mob_effect`], the inverse of
            // `crate::adapter::encode_set_beacon`. The decoded values lift
            // into `ServerBound::SetBeacon`; validation and application live
            // in `crate::beacon` and the server consumer.
            State::Play if packet_id == play::serverbound::SET_BEACON => {
                let mut r = Reader::new(payload);
                // See `SET_CREATIVE_MODE_SLOT`'s comment above for why these
                // are qualified as `self::` rather than bare calls.
                let decoded = (|| -> Option<(Option<&'static str>, Option<&'static str>)> {
                    let primary = self::read_optional_mob_effect(&mut r)?;
                    let secondary = self::read_optional_mob_effect(&mut r)?;
                    r.ensure_empty().ok()?;
                    Some((primary, secondary))
                })();
                match decoded {
                    Some((primary, secondary)) => ServerBound::SetBeacon {
                        primary: primary.map(str::to_owned),
                        secondary: secondary.map(str::to_owned),
                    },
                    None => ServerBound::Ignored,
                }
            }
            // The book-edit packet carries no `ItemStack`; the component-patch
            // decode path used by the item-carrying packets
            // (`CONTAINER_CLICK`/`SET_CREATIVE_MODE_SLOT`) does not apply.
            // The server consumer looks the book up in the tracked
            // `PlayerInventory` by `slot`.
            State::Play if packet_id == play::serverbound::EDIT_BOOK => {
                match decode_full::<EditBook>(payload) {
                    Some(EditBook { slot, pages, title }) => {
                        ServerBound::EditBook { slot, pages, title }
                    }
                    None => ServerBound::Ignored,
                }
            }
            // Block-entity text, not item state — arguably miscategorized
            // alongside the inventory-model packets above. Wire shape only,
            // matching every other packet-shaped `ServerBound` arm's own
            // convention — `crate::block_entities::apply_sign_update` (via
            // `crate::server`'s consumer) is where the ownership/waxed gate
            // and the actual text write happen.
            State::Play if packet_id == play::serverbound::SIGN_UPDATE => {
                match decode_full::<SignUpdate>(payload) {
                    Some(SignUpdate { pos, is_front_text, line0, line1, line2, line3 }) => {
                        ServerBound::SignUpdate {
                            pos: unpack_block_pos(pos),
                            is_front_text,
                            lines: [line0, line1, line2, line3],
                        }
                    }
                    None => ServerBound::Ignored,
                }
            }
            // A follow-up fix: this used to decode-and-discard. The
            // anvil's rename field is the only consumer
            // (`ServerBound::RenameItem`'s own doc comment) —
            // `crate::server`'s handler gates on an open `AnvilMenu` the same
            // way vanilla's own server-side rename-item handler does.
            State::Play if packet_id == play::serverbound::RENAME_ITEM => {
                match decode_full::<RenameItem>(payload) {
                    Some(RenameItem { name }) => ServerBound::RenameItem { name },
                    None => ServerBound::Ignored,
                }
            }
            // Middle-click pick. `crate::server`'s consumer runs
            // vanilla's `tryPickItem` three-way split (hotbar-select /
            // inventory-swap / creative-create); this arm is only the wire
            // shape, unpacking `pos` the same way `USE_ITEM_ON` above does.
            State::Play if packet_id == play::serverbound::PICK_ITEM_FROM_BLOCK => {
                match decode_full::<PickItemFromBlock>(payload) {
                    Some(PickItemFromBlock { pos, include_data }) => ServerBound::PickItemFromBlock {
                        pos: unpack_block_pos(pos),
                        include_data,
                    },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::PICK_ITEM_FROM_ENTITY => {
                match decode_full::<PickItemFromEntity>(payload) {
                    Some(PickItemFromEntity { entity_id, include_data }) => {
                        ServerBound::PickItemFromEntity { entity_id, include_data }
                    }
                    None => ServerBound::Ignored,
                }
            }
            // The selected-item packet is not represented in the clientbound
            // bundle-contents payload, whose selected-item marker is always
            // unset. The server consumer nevertheless stores the selected
            // slot so the next right-click extraction can remove the intended
            // item.
            State::Play if packet_id == play::serverbound::BUNDLE_ITEM_SELECTED => {
                match decode_full::<SelectBundleItem>(payload) {
                    Some(SelectBundleItem { slot_id, selected_item_index }) => {
                        ServerBound::SelectBundleItem { slot_id, selected_item_index }
                    }
                    None => ServerBound::Ignored,
                }
            }

            // World and block-administration packets beyond
            // `CHANGE_DIFFICULTY`/`LOCK_DIFFICULTY`/`SET_GAME_RULE` remain
            // ignored because this crate does not model jigsaw, structure,
            // or game-test state. Command-block updates are the exception:
            // they decode into `BlockEntity::CommandBlock` and are consumed by
            // `crate::server` through `crate::command_block`.
            State::Play if packet_id == play::serverbound::SET_COMMAND_BLOCK => {
                match decode_full::<SetCommandBlock>(payload) {
                    Some(SetCommandBlock { pos, command, mode, flags }) => ServerBound::SetCommandBlock {
                        pos: unpack_block_pos(pos),
                        command,
                        mode,
                        track_output: flags & COMMAND_BLOCK_FLAG_TRACK_OUTPUT != 0,
                        conditional: flags & COMMAND_BLOCK_FLAG_CONDITIONAL != 0,
                        automatic: flags & COMMAND_BLOCK_FLAG_AUTOMATIC != 0,
                    },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::SET_COMMAND_MINECART => {
                let _ = decode_full::<SetCommandMinecart>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::JIGSAW_GENERATE => {
                let _ = decode_full::<JigsawGenerate>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::SET_JIGSAW_BLOCK => {
                let _ = decode_full::<SetJigsawBlock>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::SET_STRUCTURE_BLOCK => {
                let _ = decode_full::<SetStructureBlock>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::SET_TEST_BLOCK => {
                let _ = decode_full::<SetTestBlock>(payload);
                ServerBound::Ignored
            }
            // `ServerboundCustomClickActionPacket`: an identifier, then a
            // length-prefixed optional NBT tag
            // (vanilla's own codec library's own length prefixed(65536)` wraps
            // `optionalTagCodec` with an outer VarInt byte-length) — the tag
            // contents are never interpreted server-side for any known
            // click-action id, so only the outer shape (identifier, VarInt
            // length, then that many bytes skipped) is verified here rather
            // than decoding the NBT itself.
            State::Play if packet_id == play::serverbound::CUSTOM_CLICK_ACTION => {
                let mut r = Reader::new(payload);
                let decoded = (|| -> lodestone_core::Result<()> {
                    let _id = r.string(32767)?;
                    let len = r.var_i32()?;
                    let len = usize::try_from(len)
                        .map_err(|_| lodestone_core::Error::UnexpectedEof)?;
                    let _tag_bytes = r.bytes(len)?;
                    r.ensure_empty()
                })();
                let _ = decoded;
                ServerBound::Ignored
            }
            // Also administration-adjacent, decoded for the same reason as
            // the rest of this issue's family even though neither is named
            // in its original packet-id list: `CHANGE_GAME_MODE` (F4
            // singleplayer/LAN cheat gamemode switch) and
            // `CONFIGURATION_ACKNOWLEDGED` (the reply to a clientbound
            // `start_configuration` mid-session reconfigure, which this
            // crate's join sequence never sends — see
            // `ServerBound::ConfigurationFinished`'s sibling handling for the
            // *initial* configuration handshake, which is a different wire
            // packet from this one).
            State::Play if packet_id == play::serverbound::CHANGE_GAME_MODE => {
                match decode_full::<ChangeGameMode>(payload)
                    .and_then(|p| crate::adapter::game_mode_from_ordinal(p.mode))
                {
                    Some(mode) => ServerBound::ChangeGameMode { mode },
                    // An id outside `0..=3` is malformed; dropped rather than
                    // guessed, and the server's authoritative echo then puts the
                    // client back where it was.
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::CONFIGURATION_ACKNOWLEDGED => {
                let _ = decode_full::<ConfigurationAcknowledged>(payload);
                ServerBound::Ignored
            }
            // Deliberately left undecoded (falls through to the wildcard
            // below), unlike the rest of this issue's family:
            // `TEST_INSTANCE_BLOCK_ACTION`'s body
            // (vanilla's own test-instance block-entity class's own data.STREAM_CODEC`) is a nested
            // `Optional<ResourceKey>`/`Vec3i`/`Rotation`/`Status`/
            // `Optional<...>` composite this crate has no codec support for
            // yet, and — like its sibling `SET_TEST_BLOCK` above — it
            // drives the game-test framework only, which this crate does
            // not implement at all. Left for whoever adds game-test
            // support, at which point the real `Data` type will exist to
            // decode into anyway.

            // Connection-lifecycle/system, remaining packets
            // beyond `KEEP_ALIVE` above. `PONG`/`PING_REQUEST` already have
            // structs exercised by this crate's client encoder; the rest
            // follow the same field-verified-against-decompiled-source
            // convention as the other four families above.
            //
            // `ServerboundPingRequestPacket` is the same struct the Status-state
            // arm above decodes (vanilla shares one packet class across both
            // states — see that arm's own comment), so this reuses
            // `ServerBound::PingRequest` rather than adding a second variant.
            // `dispatch_play_packet` answers it with `encode_pong_response`,
            // matching vanilla's own server-side ping-request handler
            // (it replies with the clientbound pong-response packet, echoing the time)
            // exactly — the Status arm additionally closes the connection, which
            // this one must not do.
            State::Play if packet_id == play::serverbound::PING_REQUEST => {
                match decode_full::<PingRequest>(payload) {
                    Some(ping) => ServerBound::PingRequest { time: ping.time },
                    None => ServerBound::Ignored,
                }
            }
            // The `pong` body is a raw big-endian `i32`, distinct from the
            // `i64` keep-alive echo. A valid reply is an accepted no-op: the
            // server has no `ping` producer or pending-id state to update.
            State::Play if packet_id == play::serverbound::PONG => {
                match decode_full::<Pong>(payload) {
                    Some(pong) => ServerBound::Pong { id: pong.id },
                    None => ServerBound::Ignored,
                }
            }
            // `ServerboundCustomPayloadPacket`: a channel
            // identifier then a channel-specific payload. Where this crate used
            // to model only the `minecraft:brand` channel and drop everything
            // else as vanilla's `DiscardedPayload`, it now lifts **every**
            // channel into `ServerBound::CustomPayload` unchanged — the
            // version-free server owns the register/unregister interpretation
            // and the registered-channel dispatch, and drops unregistered
            // traffic exactly like vanilla. See [`decode_custom_payload`].
            State::Play if packet_id == play::serverbound::CUSTOM_PAYLOAD => {
                decode_custom_payload(payload).unwrap_or(ServerBound::Ignored)
            }
            State::Play if packet_id == play::serverbound::RESOURCE_PACK => {
                match decode_full::<ResourcePackResponse>(payload).and_then(|packet| {
                    let response = match packet.action {
                        0 => ResourcePackResponseKind::SuccessfullyLoaded,
                        1 => ResourcePackResponseKind::Declined,
                        2 => ResourcePackResponseKind::FailedDownload,
                        3 => ResourcePackResponseKind::Accepted,
                        4 => ResourcePackResponseKind::Downloaded,
                        5 => ResourcePackResponseKind::InvalidUrl,
                        6 => ResourcePackResponseKind::FailedReload,
                        7 => ResourcePackResponseKind::Discarded,
                        _ => return None,
                    };
                    Some(ServerBound::ResourcePackResponse {
                        id: packet.id,
                        response,
                    })
                }) {
                    Some(response) => response,
                    None => ServerBound::Ignored,
                }
            }
            // A chunk-streaming regression investigation found this
            // arm and `CHUNK_BATCH_RECEIVED` below used to decode-then-drop
            // like every other packet in this `Ignored` family, from when
            // this crate had no consumer for either. A later fix added
            // `ServerBound::ClientInformationChanged`/`ChunkBatchAcknowledged`
            // and their consumers in `crate::server` (`ViewTracker::set_view_radius`
            // and the `awaiting_chunk_batch_ack` flow-control gate), but never
            // came back to update *this* decode arm — so both variants were
            // dead code, constructed nowhere, and every view-streaming batch
            // after the first queued behind a permanently-`true`
            // `awaiting_chunk_batch_ack` and was never flushed. Reproduced at
            // committed `main`: `cargo test -p lodestone-v26-2 --test block_edit
            // -- dig_and_place_persist_through_forget_and_reload` timed out
            // waiting for a forgotten chunk to be re-sent after walking back,
            // and eprintln probing confirmed zero `ChunkBatchAcknowledged`
            // packets ever reached this match in the whole run.
            State::Play if packet_id == play::serverbound::CLIENT_INFORMATION => {
                match decode_full::<ClientInformation>(payload) {
                    Some(info) => ServerBound::ClientInformationChanged {
                        view_distance: info.view_distance,
                    },
                    None => ServerBound::Ignored,
                }
            }
            // Vanilla's own serverbound client-command packet: a single
            // `readEnum` VarInt
            // ordinal over `Action { PERFORM_RESPAWN, REQUEST_STATS,
            // REQUEST_GAMERULE_VALUES }` —
            // its whole body, read
            // straight off the decompiled source rather than off our own
            // encoder. The ordinal is passed through unmapped; its consumer
            // (`apply_client_command`) mirrors
            // vanilla's own server-side client-command handler, including
            // that method's `getHealth() > 0.0F → return` respawn guard, and
            // treats `REQUEST_STATS` as a documented no-op.
            //
            // This arm returned `Ignored` while that consumer already
            // existed, so respawn was unreachable — the same dead-variant
            // shape found for `CLIENT_INFORMATION` and
            // `CHUNK_BATCH_RECEIVED`, and from the same commit (`c4ad474`),
            // which wired four consumers while only two decode arms were
            // ever updated.
            // `tests/serverbound_wiring.rs`'s
            // `every_serverbound_variant_is_constructed_by_decode` now fails
            // if any `ServerBound` variant stops being constructed here.
            State::Play if packet_id == play::serverbound::CLIENT_COMMAND => {
                match decode_full::<ClientCommand>(payload) {
                    Some(p) => ServerBound::ClientCommand { action: p.action },
                    None => ServerBound::Ignored,
                }
            }
            // `ServerboundChatCommandPacket` is a single
            // string carrying the command **without** its leading `/`; the
            // client-side encoder in this same crate
            // (`adapter/serverbound.rs`'s `ClientAction::SendCommand` arm) writes exactly
            // this struct to exactly this id, so decode and encode are pinned
            // to one another rather than to a hand-copied layout.
            //
            // `decode_full` (not a lenient partial read) because a trailing
            // byte here means we misread the packet, and a misread command is
            // worse than an ignored one: it would run *something*.
            //
            State::Play if packet_id == play::serverbound::CHAT_COMMAND => {
                match decode_full::<ChatCommand>(payload) {
                    Some(p) => ServerBound::ChatCommand { command: p.command },
                    None => ServerBound::Ignored,
                }
            }
            // `ChatCommandSigned` — sent instead of the plain `chat_command`
            // only when the client's command contains an argument the
            // server's `COMMANDS` tree declared signable
            // (vanilla's own argument-signatures helper's own sign command). This server never declares
            // any argument signable (`ServerBound::ChatCommand`'s own doc
            // comment), so no real client sends this form today, but it is
            // decoded and routed through the same
            // `ServerBound::ChatCommand` consumer rather than left `Ignored`:
            // the `command` text is well-formed and executable regardless of
            // whether its arguments carry a signature, and `ArgumentSignatures`
            // verifies individual *arguments* against a signable-argument
            // declaration this crate never makes — there is nothing for that
            // verification to gate here, unlike `minecraft:chat`'s
            // whole-message signature, which `crate::chat_session::decide`
            // does verify. `timestamp`/`salt`/`argument_signatures` and the
            // trailing acknowledgement block are decoded (to find the end of
            // the frame) and then dropped, the same convention `CHAT_ACK`
            // below uses.
            State::Play if packet_id == play::serverbound::CHAT_COMMAND_SIGNED => {
                match decode_full::<ChatCommandSigned>(payload) {
                    Some(p) => ServerBound::ChatCommand { command: p.command },
                    None => ServerBound::Ignored,
                }
            }
            // `ServerboundCommandSuggestionPacket` — a tab-completion request.
            // `CommandSuggestion` is the **same** struct
            // `adapter/serverbound.rs`'s `ClientAction::CommandSuggestion` arm
            // encodes, so decode and encode are pinned to one another exactly
            // as `CHAT_COMMAND` above is. Unlike `CHAT_COMMAND`, `command`
            // carries the **whole input line including the leading `/`** — see
            // `ServerBound::CommandSuggestion`'s own doc, and
            // `crate::server`'s consumer strips it before consulting
            // `ServerCommands::suggest`.
            State::Play if packet_id == play::serverbound::COMMAND_SUGGESTION => {
                match decode_full::<CommandSuggestion>(payload) {
                    Some(p) => ServerBound::CommandSuggestion { id: p.id, command: p.command },
                    None => ServerBound::Ignored,
                }
            }
            // A player typing a message. `ChatMessage` is the
            // **same** struct `adapter/serverbound.rs`'s `ClientAction::SendChat` arm
            // encodes, so decode and encode are pinned to one another exactly
            // as `CHAT_COMMAND` above is, rather than to a hand-copied layout.
            // Its field order matches `ServerboundChatPacket`'s own
            // constructor (26.2): `readUtf(256)`, `readInstant()`,
            // `readLong()` salt, `readNullable(MessageSignature::read)`, then
            // vanilla's own last-seen-messages record's own update (a VarInt offset, a fixed 20-bit bit
            // set in 3 bytes, and a checksum byte).
            //
            // `decode_full`, not a partial read: the trailing acknowledgement
            // block is the part most likely to be misread, and a frame we only
            // half-understand should be dropped rather than broadcast. The
            // acknowledgement fields (`last_seen_offset`/`acknowledged`/
            // `checksum`) are still discarded after that — see
            // `ServerBound::Chat`'s own doc for why — but `timestamp`/`salt`/
            // `signature` now survive, for `crate::chat_session::decide` to
            // verify against the sender's announced session, if any.
            State::Play if packet_id == play::serverbound::CHAT => {
                match decode_full::<ChatMessage>(payload) {
                    Some(p) => ServerBound::Chat {
                        message: p.message,
                        timestamp_millis: p.timestamp,
                        salt: p.salt,
                        signature: p.signature.map(|s| s.0),
                    },
                    None => ServerBound::Ignored,
                }
            }
            // `ServerboundChatSessionUpdatePacket` — a client announcing (or
            // re-announcing) its chat-signing session. `ChatSessionUpdate` is
            // the **same** struct the client-side encoder in this crate
            // produces for `ClientAction::AnnounceChatSession`
            // (`adapter/serverbound.rs`), so decode and encode are pinned to
            // one another exactly as `CHAT`/`CHAT_COMMAND` above are.
            State::Play if packet_id == play::serverbound::CHAT_SESSION_UPDATE => {
                match decode_full::<ChatSessionUpdate>(payload) {
                    Some(p) => ServerBound::ChatSessionAnnounced {
                        session_id: p.session_id,
                        expires_at_millis: p.expires_at,
                        public_key: p.public_key,
                        key_signature: p.key_signature,
                    },
                    None => ServerBound::Ignored,
                }
            }
            // `ServerboundChatAckPacket` — a single VarInt offset
            // acknowledging pending signed messages the client has seen.
            // Decoded so a well-formed frame's byte length is understood (an
            // unparsed trailing VarInt would otherwise desync the stream one
            // packet later), then discarded rather than surfaced as its own
            // `ServerBound` variant: this crate never sends a signed
            // `player_chat` (see `docs/player-chat.md`'s "signing decision"),
            // so a real client's own last-seen window — and therefore this
            // offset — stays permanently `0` regardless of how much chat
            // happens. There is nothing yet for it to acknowledge.
            State::Play if packet_id == play::serverbound::CHAT_ACK => {
                let _ = decode_full::<ChatAck>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::CHUNK_BATCH_RECEIVED => {
                match decode_full::<ChunkBatchReceived>(payload) {
                    Some(p) => ServerBound::ChunkBatchAcknowledged {
                        desired_chunks_per_tick: p.desired_chunks_per_tick,
                    },
                    None => ServerBound::Ignored,
                }
            }
            // `ServerboundSeenAdvancementsPacket`: a VarInt `Action` ordinal
            // (`0` opened-tab, `1` closed-screen, plain `writeEnum`), then an
            // identifier tab id present **only** when the action is
            // opened-tab — not a generic bool-prefixed optional, so this is
            // hand-decoded rather than a derived `Option<String>` field
            // (which would read a spurious extra bool/byte for the common
            // `closed_screen` case).
            State::Play if packet_id == play::serverbound::SEEN_ADVANCEMENTS => {
                let mut r = Reader::new(payload);
                let decoded = (|| -> lodestone_core::Result<ServerBound> {
                    let action = r.var_i32()?;
                    let tab = match action {
                        0 => Some(r.string(32767)?),
                        1 => None,
                        value => {
                            return Err(lodestone_core::Error::InvalidEnumVariant {
                                name: "seen advancements action",
                                value,
                            });
                        }
                    };
                    r.ensure_empty()
                        .map(|()| ServerBound::SeenAdvancements { tab })
                })();
                decoded.unwrap_or(ServerBound::Ignored)
            }
            State::Play if packet_id == play::serverbound::ENTITY_TAG_QUERY => {
                match decode_full::<EntityTagQuery>(payload) {
                    Some(query) => ServerBound::EntityTagQuery {
                        transaction_id: query.transaction_id,
                        entity_id: query.entity_id,
                    },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::BLOCK_ENTITY_TAG_QUERY => {
                match decode_full::<BlockEntityTagQuery>(payload) {
                    Some(query) => ServerBound::BlockEntityTagQuery {
                        transaction_id: query.transaction_id,
                        pos: unpack_block_pos(query.pos),
                    },
                    None => ServerBound::Ignored,
                }
            }
            // Deliberately left undecoded (fall through to the wildcard
            // below), unlike the rest of this issue's family:
            // - `COOKIE_RESPONSE`: this crate's client cannot send this
            //   either (see "Cookies and transfers are dead ends," the
            //   completeness epic) — there is no existing encoder to
            //   cross-check a hand-decode against, and no cookie this crate
            //   ever sets to receive a response about.
            // - `DEBUG_SUBSCRIPTION_REQUEST`: its body is a
            //   registry-keyed (vanilla's own registry-key holder's own debug-subscription accessor) set with no
            //   VarInt-id table in this crate to resolve against — an F3
            //   debug-sample-graph subscription with no gameplay effect,
            //   the same "low priority, file for completeness" packet this
            //   issue's own text already flags.
            _ => ServerBound::Ignored,
        }
    }

    // Mirrors vanilla's own
    // `this.connection.send(new ClientboundHelloPacket("", pubKey, challenge, true))`
    // (vanilla's own server-side login packet listener's own handle hello) exactly — empty server-id,
    // the caller's keypair/token, and `should_authenticate` fixed `true`
    // (vanilla never constructs this packet with `false`; encryption without
    // session-server verification is not a real wire state).
    fn encode_encryption_request(
        &self,
        public_key_der: &[u8],
        verify_token: &[u8],
    ) -> ServerDirective {
        send(
            login::clientbound::HELLO,
            &EncryptionRequest {
                server_id: String::new(),
                public_key: public_key_der.to_vec(),
                challenge: verify_token.to_vec(),
                should_authenticate: true,
            },
        )
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        let finished = LoginFinished {
            profile_id: uuid,
            name: username.to_string(),
            properties: Vec::new(),
            session_id: uuid,
        };
        // Enable packet compression before the login-success
        // reply. Vanilla's own default (`network-compression-threshold=256`
        // in every `server.properties` under `.cache/mc/`) — packets whose
        // *uncompressed* body is at least this many bytes get zlib framing;
        // smaller ones are sent through compressed framing uncompressed
        // (`LoginCompression`'s own doc comment).
        //
        // Ordering is load-bearing, mirroring
        // `Connection::enable_encryption`'s own doc comment for the same
        // hazard: `LOGIN_COMPRESSION` itself must go out **before**
        // compression is active (the client cannot decompress a packet that
        // tells it compression is starting), and every packet after —
        // starting with this very `LOGIN_FINISHED` — must go out
        // **compressed**, or the two sides frame disagreeing on which layer
        // came first. `crate::server`'s `apply` executes directives strictly
        // in order and each `Send` reads the codec's compression state at
        // the moment it writes, so `[Send(LOGIN_COMPRESSION),
        // SetCompression(threshold), Send(LOGIN_FINISHED)]` is the ordering
        // that gets this right — the same shape vanilla's own
        // `ServerLoginPacketListenerImpl` uses (send, then
        // `connection.setupCompression`).
        vec![
            send(
                login::clientbound::LOGIN_COMPRESSION,
                &LoginCompression {
                    threshold: COMPRESSION_THRESHOLD,
                },
            ),
            ServerDirective::SetCompression(COMPRESSION_THRESHOLD),
            send(login::clientbound::LOGIN_FINISHED, &finished),
        ]
    }

    fn encode_status_response(
        &self,
        description: &str,
        players_online: i32,
        players_max: i32,
        sample: &[(Uuid, String)],
        favicon_png: Option<&[u8]>,
        enforces_secure_chat: bool,
    ) -> ServerDirective {
        ServerDirective::Send {
            packet_id: status::clientbound::STATUS_RESPONSE,
            payload: encode_status_response_body(
                description,
                players_online,
                players_max,
                sample,
                favicon_png,
                enforces_secure_chat,
            ),
        }
    }

    fn encode_disconnect(&self, state: lodestone_core::State, reason: &Text) -> ServerDirective {
        use lodestone_core::State;

        match state {
            // JSON, not NBT — see `text_to_json`'s doc comment. `LoginDisconnect`
            // already derives `Encode`/`Decode` and its own doc comment records
            // the same asymmetry from the decode side.
            State::Login => send(
                login::clientbound::LOGIN_DISCONNECT,
                &LoginDisconnect {
                    reason: text_to_json(reason).to_string(),
                },
            ),
            // NBT, via the same `write_network_nbt` path `encode_system_chat`
            // uses. There is no `Disconnect` struct to derive `Encode` from
            // (the body is a bare component with no wrapper fields), so the
            // payload is the NBT alone.
            State::Configuration => ServerDirective::Send {
                packet_id: configuration::clientbound::DISCONNECT,
                payload: encode_component_nbt(reason),
            },
            State::Play => ServerDirective::Send {
                packet_id: play::clientbound::DISCONNECT,
                payload: encode_component_nbt(reason),
            },
            // Handshaking and Status have no disconnect packet in 26.2 — the
            // Status clientbound set is `status_response`/`pong_response` only,
            // and vanilla's `ServerStatusPacketListenerImpl` closes the channel
            // rather than sending anything. Emitting nothing is correct; the
            // caller still closes.
            State::Handshaking | State::Status => ServerDirective::None,
        }
    }

    fn encode_pong_response(&self, time: i64) -> ServerDirective {
        // Vanilla's own clientbound pong-response packet is a single
        // big-endian `long`
        // (confirmed against the decompiled 26.2 source), byte-identical to
        // the serverbound ping-request packet it answers — which is why the
        // client-side `PingRequest` struct is the right thing to encode here
        // rather than a second one-field mirror of it.
        send(status::clientbound::PONG_RESPONSE, &PingRequest { time })
    }

    fn encode_registry_data(&self) -> Vec<ServerDirective> {
        // The full Configuration-phase registry burst a real
        // vanilla client expects, in vanilla's own wire order
        // (`SynchronizeRegistriesTask`): `select_known_packs` (requesting
        // zero packs — this server ships no datapacks), then one
        // `registry_data` per synchronized registry (all 29 —
        // vanilla's own registry-data loader's own synchronized-registries accessor, read off the
        // decompiled source rather than `registries.json`, which omits
        // `dimension_type`/`world_clock` entirely because both are
        // data-pack-loaded), then `update_tags`. The server loop sends
        // `begin_configuration`'s `FINISH_CONFIGURATION` right after this
        // return, so the ordering here is the whole ordering.
        //
        // `minecraft:dimension_type` and `minecraft:world_clock` are the two
        // registries this crate resolves *holder ids* out of elsewhere
        // (`login`'s dimension type, `set_time`'s clock keys), so they stay
        // hand-built structured tables. Every other registry is relayed as
        // captured vanilla bytes — see
        // `registry_data_fixtures`'s module docs for why that is both safe
        // and sufficient, and for why this server does not wait for the
        // client's own `select_known_packs` reply before sending them.
        let mut directives = vec![crate::registry_data_fixtures::select_known_packs_directive()];
        directives.push(
            // From `DIMENSION_TYPE_REGISTRY`, not an inline literal: the order *is*
            // the holder-id mapping `encode_dimension_change` resolves against.
            encode_registry_data_packet("minecraft:dimension_type", &DIMENSION_TYPE_REGISTRY),
        );
        directives.push(encode_registry_data_packet(
            "minecraft:world_clock",
            &[
                ("minecraft:overworld", WORLD_CLOCK_OVERWORLD_NBT),
                ("minecraft:the_end", WORLD_CLOCK_END_NBT),
            ],
        ));
        directives.extend(crate::registry_data_fixtures::passthrough_registry_directives());
        directives.push(crate::registry_data_fixtures::update_tags_directive());
        directives
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        // Minimum sequence: go straight to the finish signal. The registries a
        // real client needs were already sent by
        // [`ServerProtocol::encode_registry_data`] (called by the server loop
        // before this), so the only thing left here is the finish itself.
        // Known-packs negotiation and the code-of-conduct exchange remain
        // unsent — real vanilla packets this join sequence still does not need
        // (see the module docs' scope note).
        vec![send(
            configuration::clientbound::FINISH_CONFIGURATION,
            &FinishConfiguration,
        )]
    }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        // The hardcoded fallback spawn — see the module doc
        // comment for why these unitless numbers exist. Delegates to
        // `begin_play_at` so the body lives in one place.
        self.begin_play_at(view_radius, Vec3::new(8.0, 100.0, 8.0), GameMode::Survival)
    }

    fn begin_play_at(&self, view_radius: i32, spawn: Vec3, mode: GameMode) -> Vec<ServerDirective> {
        self.begin_play_at_with_teleport_id(view_radius, spawn, mode, 0)
    }

    fn uses_teleport_acknowledgements(&self) -> bool {
        true
    }

    fn begin_play_at_with_teleport_id(
        &self,
        view_radius: i32,
        spawn: Vec3,
        mode: GameMode,
        teleport_id: i32,
    ) -> Vec<ServerDirective> {
        let login = GameLogin {
            entity_id: LOCAL_PLAYER_ENTITY_ID,
            hardcore: false,
            levels: vec!["minecraft:overworld".to_string()],
            max_players: 20,
            view_distance: view_radius.max(1),
            simulation_distance: view_radius.max(1),
            reduced_debug_info: false,
            show_death_screen: true,
            do_limited_crafting: false,
            dimension_type: 0,
            dimension: "minecraft:overworld".to_string(),
            seed: 0,
            // `GameLogin::game_type` is the unsigned byte the wire carries, and
            // the ordinal table is `0..=3`, so the cast is total.
            game_type: crate::adapter::game_mode_to_ordinal(mode) as u8,
            previous_game_type: -1,
            is_debug: false,
            // This server's own worldgen is not the superflat generator, so
            // the client applies the ordinary 32-block void fade. A flat
            // integrated world would have to set this, not just generate flat
            // terrain: the client has no other way to know.
            is_flat: false,
            rest: encode_game_login_rest(),
        };

        let spawn_block_x = spawn.x.floor() as i32;
        let spawn_block_y = spawn.y.floor() as i32;
        let spawn_block_z = spawn.z.floor() as i32;
        let spawn_position = SetDefaultSpawnPosition {
            location: GlobalPos {
                dimension: "minecraft:overworld".to_string(),
                position: pack_block_pos(spawn_block_x, spawn_block_y, spawn_block_z),
            },
            yaw: 0.0,
            pitch: 0.0,
        };

        let teleport_payload = encode_player_position_teleport(
            teleport_id,
            spawn.x,
            spawn.y,
            spawn.z,
            0.0,
            0.0,
        );

        // Chunk column containing the spawn point, derived from the
        // position rather than assumed (0, 0).
        let spawn_cx = (spawn.x / 16.0).floor() as i32;
        let spawn_cz = (spawn.z / 16.0).floor() as i32;

        vec![
            send(play::clientbound::LOGIN, &login),
            // The world border is the first world state a joining player is
            // told about, before the time sync and spawn position — vanilla's
            // vanilla's own server-side player-list class's own send level info order.
            // A full-size static default today; the live border's state lands
            // here when the world loop owns a shared `WorldBorder` (see
            // `crate::border`'s module doc, shape B).
            self.encode_initialize_border(&WorldBorder::default()),
            send(
                play::clientbound::SET_DEFAULT_SPAWN_POSITION,
                &spawn_position,
            ),
            ServerDirective::Send {
                packet_id: play::clientbound::PLAYER_POSITION,
                payload: teleport_payload,
            },
            // Chunk cache center must agree with `ViewTracker::new`'s
            // center in `serve_connection_inner`, so both derive from the
            // same `spawn` position — the existing comment's "when a real
            // spawn position arrives this and that line move together."
            self.encode_chunk_cache_center(spawn_cx, spawn_cz),
            // Vanilla fresh-spawn defaults. Without this the client's
            // `PlayerSnapshot::health` stays `None` (never having received a
            // `SetHealth`), which a HUD would show as absent/dead rather than
            // full health.
            send(
                play::clientbound::SET_HEALTH,
                &SetHealth {
                    health: 20.0,
                    food: 20,
                    saturation: 5.0,
                },
            ),
        ]
    }

    /// Vanilla's own clientbound game-event packet with the change-game-mode
    /// event code `3`, whose `f32` parameter is the `GameType` id
    /// (confirmed against the decompiled 26.2 source).
    fn encode_game_mode(&self, mode: GameMode) -> ServerDirective {
        send(
            play::clientbound::GAME_EVENT,
            &GameEvent {
                event: GAME_EVENT_CHANGE_GAME_MODE,
                param: crate::adapter::game_mode_to_ordinal(mode) as f32,
            },
        )
    }

    /// `ClientboundPlayerAbilitiesPacket` — the flags byte then flying and
    /// walking speed. `may_build` has **no wire bit**: vanilla's
    /// vanilla's own player-abilities record's own may build is server-side only and is not in the packet
    /// (`ServerboundPlayerAbilitiesPacket`/`ClientboundPlayerAbilitiesPacket`
    /// carry the four `ABILITY_FLAG_*` bits and nothing more), so it is
    /// deliberately dropped here rather than folded into a spare bit.
    fn encode_player_abilities(&self, abilities: Abilities) -> ServerDirective {
        let mut flags = 0u8;
        if abilities.invulnerable {
            flags |= ABILITY_FLAG_INVULNERABLE;
        }
        if abilities.flying {
            flags |= ABILITY_FLAG_FLYING;
        }
        if abilities.may_fly {
            flags |= ABILITY_FLAG_CAN_FLY;
        }
        if abilities.instabuild {
            flags |= ABILITY_FLAG_INSTABUILD;
        }
        send(
            play::clientbound::PLAYER_ABILITIES,
            &PlayerAbilities {
                flags,
                flying_speed: abilities.flying_speed,
                walking_speed: abilities.walking_speed,
            },
        )
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::CHUNK_BATCH_START,
            payload: Vec::new(),
        }
    }

    /// Delegates to this type's own [`ChunkEncoder`] impl so there is exactly one
    /// column-encode body in this crate. The two must be byte-identical
    /// ([`ServerProtocol::chunk_encoder`]'s contract) and the only way to
    /// guarantee that is not to have two.
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ServerChunkColumn) -> ServerDirective {
        ChunkEncoder::encode_chunk(self, cx, cz, column)
    }

    /// `Self`, because this protocol is a stateless unit struct — so the "encoder
    /// detached from `&self`" this seam asks for costs one `Arc` allocation per
    /// join and carries nothing. See [`ChunkEncoder`] for why the connection task
    /// must not do this work.
    fn chunk_encoder(&self) -> Option<std::sync::Arc<dyn ChunkEncoder>> {
        Some(std::sync::Arc::new(*self))
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        use crate::packets::game::ChunkBatchFinished;
        send(
            play::clientbound::CHUNK_BATCH_FINISHED,
            &ChunkBatchFinished { batch_size },
        )
    }

    /// `ClientboundLightUpdatePacket`: `cx`, `cz`, then the six-field light
    /// payload verbatim.
    ///
    /// [`ColumnLight::encode`] is *already* the exact
    /// `ClientboundLightUpdatePacketData` shape — the same bytes
    /// [`encode_column_body`] embeds inside `level_chunk_with_light` — so this is
    /// two varints and a delegation, deliberately. Note the wire order it writes
    /// is sky / block / empty-sky / empty-block masks and then the two array
    /// lists, which is **not** `LightPatch::from_light_masks`' argument order;
    /// `tests/light_update.rs` pins the encoder against the hand-written golden
    /// body the decode arm is gated on.
    fn encode_light_update(&self, cx: i32, cz: i32, light: &ColumnLight) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(cx);
        w.var_i32(cz);
        light.encode(&mut w);
        ServerDirective::Send {
            packet_id: play::clientbound::LIGHT_UPDATE,
            payload: w.as_slice().to_vec(),
        }
    }

    /// The same computation [`encode_chunk`](ServerProtocol::encode_chunk)
    /// performs — [`build_world_column`] to resolve state ids, then
    /// [`compute_served_light`] — so a `light_update` and a full column resend
    /// carry identical light for identical terrain. Read
    /// [`compute_served_light`]'s doc for why this is the isolated compute and
    /// what the residual is.
    fn compute_column_light(&self, column: &ServerChunkColumn) -> Option<ColumnLight> {
        // Through `shape_for_column` for the same reason `encode_chunk` is: a
        // `light_update` that framed a Nether column against the overworld's 24
        // sections would carry a different section count than the chunk packet that
        // preceded it, which is the one thing this method's own doc promises cannot
        // happen.
        let shape = shape_for_column(column);
        Some(compute_served_light(&build_world_column(&shape, column)))
    }

    fn welcome_message(&self) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: play::clientbound::SYSTEM_CHAT,
            payload: encode_system_chat("Welcome to Lodestone", false),
        }]
    }

    /// `overlay: false` — command feedback belongs in the chat history, not
    /// the action bar. Vanilla's own `CommandSourceStack::sendSuccess` routes
    /// to `ServerPlayer::sendSystemMessage(component, false)` for the same
    /// reason: an action-bar line is transient and a player who mistyped a
    /// command needs to be able to scroll back and read why it failed.
    fn encode_system_chat(&self, message: &str) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::SYSTEM_CHAT,
            payload: encode_system_chat(message, false),
        }
    }

    fn encode_resource_pack_push(&self, push: &ResourcePackPush) -> ServerDirective {
        // Hand-written, in the same "no existing struct" style as
        // `encode_system_chat`: there is no `ResourcePackPush` packet struct
        // here (the client side only ever *decodes* this packet), so the body
        // is written directly against `V770Adapter`'s decode logic — the
        // mirror-side specification. Wire layout (`ClientboundResourcePackPushPacket`):
        // a raw 16-byte uuid, a VarInt-prefixed UTF-8 url, a VarInt-prefixed
        // UTF-8 SHA-1 hash (vanilla caps it at 40 chars via
        // vanilla's own codec library's own string utf8(40)`), a bool `required` flag, then — only
        // if present — a network-NBT chat component prompt, exactly the
        // `write_network_nbt` path `encode_component_nbt` uses for a disconnect
        // reason. Both decode arms (`configuration` and `play`) read this with
        // `read_network_nbt` + `Text::from_nbt`, so the encoder mirrors that
        // with `text_to_nbt` — the inverse.
        //
        // Sent on the **play** id: the drain point this feed rides is
        // `serve_play`'s `container_sync_tick` arm, so the push reaches the
        // client after the configuration handoff. Vanilla pushes during
        // Configuration instead (its `ServerResourcePackConfigurationTask`),
        // and this crate's `begin_configuration` is a static vec with no
        // arguments to carry a pack; both decode arms are wire-identical, so
        // the play-phase push is what the current wiring can emit.
        let mut w = Writer::default();
        w.uuid(push.id);
        w.string(&push.url);
        w.string(&push.hash);
        w.bool(push.required);
        match &push.prompt {
            Some(prompt) => {
                w.bool(true);
                write_network_nbt(&mut w, &text_to_nbt(prompt))
                    .expect("a chat component built from a `Text` always encodes into a `Vec<u8>` writer");
            }
            None => {
                w.bool(false);
            }
        }
        ServerDirective::Send {
            packet_id: play::clientbound::RESOURCE_PACK_PUSH,
            payload: w.into_vec(),
        }
    }

    // Wire-level plugin messaging, server→client: the broadcast
    // drain this lifts runs in `serve_play`'s `container_sync_tick` arm, so
    // the payload reaches the client after the configuration handoff — same
    // reasoning as `encode_resource_pack_push`, and the **play** id is the one
    // a post-handoff frame carries. Both clientbound `custom_payload` ids
    // (`configuration` and `play`) share the same body; see
    // [`encode_custom_payload_body`].
    fn encode_custom_payload(&self, channel: &ResourceKey, data: &[u8]) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::CUSTOM_PAYLOAD,
            payload: encode_custom_payload_body(channel, data),
        }
    }

    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::ADD_ENTITY,
            payload: encode_add_entity_body(entity),
        }
    }

    fn encode_entity_update(
        &self,
        _prev: Option<&EntitySnapshot>,
        current: &EntitySnapshot,
    ) -> Vec<ServerDirective> {
        // MVP: always send an absolute position/rotation update rather than
        // computing a relative delta. `V770ServerProtocol` is a zero-sized,
        // stateless unit struct shared (via `Arc`) across every connection
        // (see `IntegratedServer::bind`), so it cannot safely hold per-entity
        // "last-sent" state itself — and vanilla's own `TELEPORT_ENTITY`
        // decodes into the exact same `ClientEvent::EntityMoved` a relative
        // move packet would produce, so this is 100% wire-valid, just not
        // bandwidth-optimal. `_prev` is accepted (unused for now) so a future
        // delta-encoding pass can use it without another signature change.
        vec![
            ServerDirective::Send {
                packet_id: play::clientbound::TELEPORT_ENTITY,
                payload: encode_teleport_entity(current),
            },
            ServerDirective::Send {
                packet_id: play::clientbound::ROTATE_HEAD,
                payload: encode_rotate_head(current.id, current.head_yaw),
            },
        ]
    }

    fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(ids.len() as i32);
        for &id in ids {
            w.var_i32(id);
        }
        ServerDirective::Send {
            packet_id: play::clientbound::REMOVE_ENTITIES,
            payload: w.into_vec(),
        }
    }

    /// Three VarInts: the item entity, the collector, and the count taken — the
    /// exact shape `V770Adapter`'s own `TAKE_ITEM_ENTITY` arm decodes back into
    /// `ClientEvent::ItemPickup`, which is the round-trip this crate's
    /// `entity_events.rs` gate already pins from the client side.
    fn encode_take_item_entity(
        &self,
        item_entity_id: i32,
        collector_entity_id: i32,
        amount: i32,
    ) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(item_entity_id);
        w.var_i32(collector_entity_id);
        w.var_i32(amount);
        ServerDirective::Send {
            packet_id: play::clientbound::TAKE_ITEM_ENTITY,
            payload: w.into_vec(),
        }
    }

    /// vanilla's own clientbound hurt-animation packet's own write: a **VarInt** id then an IEEE-754
    /// `float` yaw — the exact shape this crate's own `HURT_ANIMATION` decode arm
    /// reads back into `ClientEvent::EntityHurtAnimation`.
    ///
    /// The two fields differ in type, so a transposition cannot survive the wire
    /// here; the trap this packet *does* have is its sibling
    /// [`Self::encode_entity_event`], whose id is a fixed-width `int`.
    fn encode_hurt_animation(&self, entity_id: i32, yaw: f32) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(entity_id);
        w.f32(yaw);
        ServerDirective::Send {
            packet_id: play::clientbound::HURT_ANIMATION,
            payload: w.into_vec(),
        }
    }

    /// vanilla's own clientbound entity-event packet's own write: `writeInt` then `writeByte` — a
    /// **plain big-endian `i32`**, not a VarInt, matching this crate's own
    /// `ENTITY_EVENT` decode arm (whose comment already flags the same thing from
    /// the reading side).
    ///
    /// The status byte is written as-is: `EntityEvent`'s constants are `byte`s and
    /// every value this server sends (3, 6, 7, 18) is inside `i8`'s positive
    /// range, but the cast is `as i8` rather than a bounds check because vanilla
    /// itself has negative-valued events and a future one must round-trip.
    fn encode_entity_event(&self, entity_id: i32, event: u8) -> ServerDirective {
        let mut w = Writer::default();
        w.i32(entity_id);
        w.i8(event as i8);
        ServerDirective::Send {
            packet_id: play::clientbound::ENTITY_EVENT,
            payload: w.into_vec(),
        }
    }

    fn encode_commands(&self, tree: &WireCommandTree) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::COMMANDS,
            payload: encode_commands_body(tree),
        }
    }

    fn encode_command_suggestions(&self, response: &CommandSuggestionsResponse) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::COMMAND_SUGGESTIONS,
            payload: encode_command_suggestions_body(response),
        }
    }

    /// vanilla's own clientbound set-passengers packet's own write: `writeVarInt(vehicle)` then
    /// `writeVarIntArray(passengers)`.
    ///
    /// `writeVarIntArray` is a VarInt length followed by that many bare VarInts —
    /// **not** vanilla's own codec library's own var-int accessor.apply(list())`, which would be the same bytes
    /// by coincidence today and is a different codec. This crate's own
    /// `SET_PASSENGERS` *decode* arm in `crate::adapter` reads exactly this shape by
    /// hand and says so, so the two halves agree by construction.
    ///
    /// An empty `passenger_ids` is the dismount, and is a legal, meaningful frame:
    /// the base entity class's own stop riding re-sends the vehicle's now-empty list.
    fn encode_set_passengers(&self, vehicle_id: i32, passenger_ids: &[i32]) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(vehicle_id);
        w.var_i32(i32::try_from(passenger_ids.len()).unwrap_or(i32::MAX));
        for &id in passenger_ids {
            w.var_i32(id);
        }
        ServerDirective::Send {
            packet_id: play::clientbound::SET_PASSENGERS,
            payload: w.into_vec(),
        }
    }

    /// vanilla's own clientbound set-entity-link packet's own write: `writeInt(sourceId)` then
    /// `writeInt(destId)` — both **plain big-endian `i32`s**, not VarInts.
    /// Ported from `write`/`read` rather than the constructor or the field
    /// declaration, per this crate's own rule for a record whose fields share a
    /// type: here all three orders happen to agree (constructor takes
    /// `(sourceEntity, destEntity)`, fields declare `sourceId` then `destId`,
    /// `write` emits `sourceId` then `destId`), so there is no transposition to
    /// guard against on *this* packet — but the fixture still picks
    /// pairwise-distinct ids, because "this particular packet's orders happen to
    /// coincide" is not a reason to weaken the general habit.
    ///
    /// `target_id` is `None` for vanilla's own `destId == 0` sentinel
    /// (vanilla's own leashable interface's own drop leash/`removeLeash` pass a `null` `destEntity`, which the
    /// constructor turns into `0` before `write` ever runs) — a real client never
    /// has an entity id `0` to confuse this with; `LOCAL_PLAYER_ENTITY_ID` is `1`.
    fn encode_set_entity_link(&self, source_id: i32, target_id: Option<i32>) -> ServerDirective {
        let mut w = Writer::default();
        w.i32(source_id);
        w.i32(target_id.unwrap_or(0));
        ServerDirective::Send {
            packet_id: play::clientbound::SET_ENTITY_LINK,
            payload: w.into_vec(),
        }
    }

    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        // `KeepAlive` (`packets::common`) is identical on the wire in both
        // directions, so the same bidirectional struct this module's
        // `decode` arm above decodes the echo with also encodes the
        // challenge — no mirror-image encoder needed.
        send(play::clientbound::KEEP_ALIVE, &KeepAlive { id })
    }

    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::SET_TIME,
            payload: encode_set_time_body(game_time, day_time),
        }
    }

    fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::SET_CHUNK_CACHE_CENTER,
            payload: encode_chunk_cache_center_body(cx, cz),
        }
    }

    fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::FORGET_LEVEL_CHUNK,
            payload: encode_forget_chunk_body(cx, cz),
        }
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::BLOCK_UPDATE,
            payload: encode_block_update_body(x, y, z, resolve_state_id(state)),
        }
    }

    /// `ClientboundBlockEntityDataPacket`: a packed `BlockPos` i64, the
    /// `BLOCK_ENTITY_TYPE` registry id as a VarInt, then the nameless network-NBT
    /// update tag — the identical shape this crate's own `BLOCK_ENTITY_DATA`
    /// decode arm reads back (`adapter/chunk.rs`).
    ///
    /// Emits nothing when the type key does not resolve in this version's registry
    /// or when the payload does not serialize, for the same reason
    /// [`encode_block_entities`] filters an entry out of the chunk array rather
    /// than writing a wrong VarInt: a bad type id mis-draws one entity, while a
    /// malformed body desynchronises the stream and takes the connection down.
    fn encode_block_entity_data(
        &self,
        pos: lodestone_model::BlockPos,
        block_entity_type: &str,
        nbt: &lodestone_core::Nbt,
    ) -> ServerDirective {
        let Some(type_id) = resolve_block_entity_type_id(block_entity_type) else {
            return ServerDirective::None;
        };
        let mut body = Writer::default();
        if write_network_nbt(&mut body, nbt).is_err() {
            return ServerDirective::None;
        }
        let mut w = Writer::default();
        w.i64(pack_block_pos(pos.x, pos.y, pos.z));
        w.var_i32(type_id as i32);
        w.bytes(&body.into_vec());
        ServerDirective::Send {
            packet_id: play::clientbound::BLOCK_ENTITY_DATA,
            payload: w.into_vec(),
        }
    }

    /// Encodes air-supply as a one-field `SET_ENTITY_DATA` metadata update for
    /// [`LOCAL_PLAYER_ENTITY_ID`] — the same wire packet a mob's cosmetic
    /// metadata would use, restricted to the single `DATA_AIR_SUPPLY_ID`
    /// field vanilla's own the base entity class's own set air supply sync would send. Hand-written
    /// (no existing struct to derive `Encode` from — see this module's own
    /// doc comment on why that is the right call here) but byte-accurate
    /// against `crates/versions/26.2/src/packets/metadata.rs`'s
    /// `read_entity_metadata`, the decode side this must round-trip through:
    /// VarInt entity id, then `(index: u8, serializer: VarInt, value)`
    /// repeated, terminated by the `0xFF` sentinel that decoder's `EOF_MARKER`
    /// checks for.
    fn encode_air_supply_update(&self, air: i32) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(LOCAL_PLAYER_ENTITY_ID);
        w.u8(METADATA_IDX_AIR_SUPPLY);
        w.var_i32(METADATA_SER_INT);
        w.var_i32(air);
        w.u8(METADATA_EOF);
        ServerDirective::Send {
            packet_id: play::clientbound::SET_ENTITY_DATA,
            payload: w.into_vec(),
        }
    }

    /// `ClientboundSetExperiencePacket`. **Wire order is progress, level, total** —
    /// not declaration order, and not alphabetical. Hand-written against
    /// `V770Adapter::handle_play`'s own `SET_EXPERIENCE` decoder, which is the
    /// mirror-side specification and already carried that warning in a comment
    /// before anything encoded the packet.
    ///
    /// `progress` is clamped to `0.0..=1.0`: the client multiplies it by the bar
    /// width, so a value outside that draws past the end of the bar.
    fn encode_set_experience(&self, progress: f32, level: i32, total: i32) -> ServerDirective {
        let mut w = Writer::default();
        w.f32(progress.clamp(0.0, 1.0));
        w.var_i32(level.max(0));
        w.var_i32(total.max(0));
        ServerDirective::Send {
            packet_id: play::clientbound::SET_EXPERIENCE,
            payload: w.into_vec(),
        }
    }

    /// The general per-species `SET_ENTITY_DATA` encoder
    /// [`encode_air_supply_update`](Self::encode_air_supply_update)'s own doc
    /// comment says nothing on the server side had ever needed before it —
    /// that one is still hardcoded to [`LOCAL_PLAYER_ENTITY_ID`] and one
    /// `INT` field on purpose (a real, still-valid, still-narrower use case:
    /// syncing the *local player's own* air supply needs no entity-id
    /// parameter at all). This is the wire-format twin for an arbitrary
    /// entity id and an arbitrary [`MetadataField`] list, so a creeper's
    /// `DATA_SWELL_DIR`/`DATA_IS_IGNITED` — and the next mob's fields,
    /// whatever they are — reach this same encoder with no second mechanism.
    ///
    /// Byte-accurate against the same decode side
    /// `encode_air_supply_update` cites (`crates/versions/26.2/src/packets/metadata.rs`'s
    /// `read_entity_metadata`): VarInt entity id, then `(index: u8,
    /// serializer: VarInt, value)` once per field, terminated by the `0xFF`
    /// sentinel. `fields` empty returns [`ServerDirective::None`] rather than
    /// a wire-valid-but-pointless empty list — [`crate::server::EntityStreamer::sync`]
    /// never calls this with an empty list in practice (it only calls this
    /// when [`EntitySnapshot::metadata`] is non-empty or changed), but a
    /// defaulted metadata field list from some future caller should not
    /// spend a packet saying nothing.
    fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
        if fields.is_empty() {
            return ServerDirective::None;
        }
        let mut w = Writer::default();
        w.var_i32(entity_id);
        for field in fields {
            // `match field`, by reference, not `match *field`: `MetadataField`
            // stopped deriving `Copy` when it gained its first owned-value
            // variant (`Item`'s `ResourceKey`). There is deliberately still no
            // `_ =>` arm — a new field must be encoded or fail to compile, which
            // is the only thing that stops the next one becoming an island.
            match field {
                MetadataField::CreeperSwellDir(v) => {
                    w.u8(METADATA_IDX_CREEPER_SWELL_DIR);
                    w.var_i32(METADATA_SER_INT);
                    w.var_i32(*v);
                }
                MetadataField::CreeperIgnited(b) => {
                    w.u8(METADATA_IDX_CREEPER_IGNITED);
                    w.var_i32(METADATA_SER_BOOLEAN);
                    w.bool(*b);
                }
                MetadataField::Item { item, count } => {
                    w.u8(METADATA_IDX_ITEM_ENTITY_ITEM);
                    w.var_i32(METADATA_SER_ITEM_STACK);
                    // The `ITEM_STACK` serializer's payload is
                    // vanilla's own item-stack type's own optional-stream-codec accessor — the same VarInt
                    // count / VarInt registry id / empty `DataComponentPatch`
                    // shape [`write_optional_item_stack`] already writes for
                    // container slots, so this reuses it rather than restating
                    // it a third time. Byte-checked against a real vanilla
                    // capture: `tests/fixtures/item_entity_metadata_diamond.hex`.
                    let stack = ItemStack::new(item.clone(), u32::from(*count));
                    write_optional_item_stack(&mut w, Some(&stack));
                }
                MetadataField::ExperienceOrbValue { value } => {
                    // Index 8 again, and the *serializer* is what distinguishes this
                    // from the arm above: the experience-orb class's own value accessor is an `INT` where
                    // the item-entity class's own item accessor is an `ITEM_STACK`. Both numbers come off
                    // the `EntityDataIndexOracle` dump in the tree
                    // (`tests/support/entity_data_index_jvm.txt`: `8
                    // the experience-orb class's own value accessor 1 INT`) rather than being hand-counted,
                    // and the producer guard is the same as `Item`'s: only
                    // `MobSim::snapshots`' orb loop builds this variant, so every one
                    // that arrives here belongs to a `minecraft:experience_orb`.
                    w.u8(METADATA_IDX_EXPERIENCE_ORB_VALUE);
                    w.var_i32(METADATA_SER_INT);
                    w.var_i32(*value);
                }
                MetadataField::TamableFlags { tame, sitting } => {
                    // the tameable-animal class's own is in sitting pose is `& 1`, `isTame` is `& 4`.
                    // Both read off `TamableAnimal`'s own accessors, not from a
                    // flag-name table: the enum there has no names, only the two
                    // masks, and inventing an ordering (0x01, 0x02, 0x04, …) would
                    // put tame at `0x02` — which is the *horse's* bit.
                    let mut byte = 0i8;
                    if *sitting {
                        byte |= 0x01;
                    }
                    if *tame {
                        byte |= 0x04;
                    }
                    w.u8(METADATA_IDX_TAMABLE_FLAGS);
                    w.var_i32(METADATA_SER_BYTE);
                    w.i8(byte);
                }
                MetadataField::HorseFlags { tame } => {
                    // the abstract-horse class's own flag-tame accessor = 2` — deliberately a *different* bit
                    // from the arm above at the *same* index. See
                    // [`METADATA_IDX_HORSE_FLAGS`].
                    let mut byte = 0i8;
                    if *tame {
                        byte |= 0x02;
                    }
                    w.u8(METADATA_IDX_HORSE_FLAGS);
                    w.var_i32(METADATA_SER_BYTE);
                    w.i8(byte);
                }
                MetadataField::Baby(b) => {
                    w.u8(METADATA_IDX_BABY);
                    w.var_i32(METADATA_SER_BOOLEAN);
                    w.bool(*b);
                }
                MetadataField::VillagerData {
                    kind,
                    profession,
                    level,
                } => {
                    // `holderRegistry(type) + holderRegistry(profession) + VarInt
                    // level` — the exact mirror of `decode_value`'s
                    // `SER_VILLAGER_DATA` arm (`crates/versions/26.2/src/packets/metadata.rs`).
                    // Each holder is a registry id written as `id + 1`; an
                    // unresolvable key (should not happen for anything
                    // `crate::mobs::villager` can produce) falls back to `0`,
                    // vanilla's inline-direct-holder wire value, rather than
                    // corrupting the rest of the packet.
                    w.u8(METADATA_IDX_VILLAGER_DATA);
                    w.var_i32(METADATA_SER_VILLAGER_DATA);
                    w.var_i32(villager_registry_wire_id(
                        entity_variants::villager_type,
                        &kind.to_string(),
                    ));
                    w.var_i32(villager_registry_wire_id(
                        entity_variants::villager_profession,
                        &profession.to_string(),
                    ));
                    w.var_i32(*level);
                }
                MetadataField::TntFuse(fuse) => {
                    // the primed-tnt class's own fuse accessor — index 8 again, and the
                    // *producer* is what disambiguates it from `Item`'s
                    // `ITEM_STACK` and `ExperienceOrbValue`'s own `INT` at the
                    // same index: only `MobSim::snapshots`' TNT loop ever
                    // builds this variant. See its own doc comment
                    // (`lodestone_server::MetadataField::TntFuse`) for the
                    // full five-claimant list from the jar dump.
                    w.u8(METADATA_IDX_TNT_FUSE);
                    w.var_i32(METADATA_SER_INT);
                    w.var_i32(*fuse);
                }
                MetadataField::MinecartFuel(lit) => {
                    // the furnace-minecart class's own fuel accessor — index 13; only
                    // `MobSim::snapshots`' furnace-minecart arm ever builds
                    // this variant. See its own doc comment for the
                    // `MinecartCommandBlock` claimant this never collides
                    // with in practice.
                    w.u8(METADATA_IDX_MINECART_FUEL);
                    w.var_i32(METADATA_SER_BOOLEAN);
                    w.bool(*lit);
                }
                MetadataField::BoatPaddles { left, right } => {
                    // the abstract-boat class's own paddle-left accessor/`RIGHT` — indices
                    // 11/12, the same two-fields-one-arm shape
                    // `GoatHorns` above already uses. Only
                    // `MobSim::snapshots`' vehicle loop ever builds this
                    // variant; see `MetadataField::BoatPaddles`'s own doc
                    // for the claimants this never collides with in
                    // practice.
                    w.u8(METADATA_IDX_BOAT_PADDLE_LEFT);
                    w.var_i32(METADATA_SER_BOOLEAN);
                    w.bool(*left);
                    w.u8(METADATA_IDX_BOAT_PADDLE_RIGHT);
                    w.var_i32(METADATA_SER_BOOLEAN);
                    w.bool(*right);
                }
                MetadataField::VehicleHurt { time, dir, damage } => {
                    // `VehicleEntity`'s hurt triple -- indices 8/9/10, the same
                    // several-fields-one-arm shape `BoatPaddles` above uses.
                    // Only `MobSim::snapshots`' vehicle loop ever builds this
                    // variant; see `MetadataField::VehicleHurt`'s own doc for
                    // the index-8 and index-9 claimants it never collides with
                    // in practice.
                    w.u8(METADATA_IDX_VEHICLE_HURT_TIME);
                    w.var_i32(METADATA_SER_INT);
                    w.var_i32(*time);
                    w.u8(METADATA_IDX_VEHICLE_HURT_DIR);
                    w.var_i32(METADATA_SER_INT);
                    w.var_i32(*dir);
                    w.u8(METADATA_IDX_VEHICLE_DAMAGE);
                    w.var_i32(METADATA_SER_FLOAT);
                    w.f32(*damage);
                }
                MetadataField::DragonPhase(phase) => {
                    // the ender-dragon class's own phase accessor — index 16; only
                    // `MobSim::push_dragon_snapshots` ever builds this
                    // variant. See `METADATA_IDX_DRAGON_PHASE`'s own doc for
                    // the five other `INT` claimants this never collides with
                    // in practice.
                    w.u8(METADATA_IDX_DRAGON_PHASE);
                    w.var_i32(METADATA_SER_INT);
                    w.var_i32(*phase);
                }
                MetadataField::WitherInvulnerableTicks(ticks) => {
                    // the wither-boss class's own inv accessor — index 19; only
                    // `MobSim::push_wither_snapshots` ever builds this
                    // variant. See `METADATA_IDX_WITHER_INVULNERABLE_TICKS`'s
                    // own doc for the five other `INT` claimants this never
                    // collides with in practice.
                    w.u8(METADATA_IDX_WITHER_INVULNERABLE_TICKS);
                    w.var_i32(METADATA_SER_INT);
                    w.var_i32(*ticks);
                }
                MetadataField::GoatHorns { has_left, has_right } => {
                    // the goat class's own has-left-horn accessor/`DATA_HAS_RIGHT_HORN` — indices
                    // 19/20; only `SimMob::snapshot`'s `"goat"` arm ever
                    // builds this variant. See `METADATA_IDX_GOAT_HAS_LEFT_HORN`'s
                    // own doc for the claimants this never collides with in
                    // practice.
                    w.u8(METADATA_IDX_GOAT_HAS_LEFT_HORN);
                    w.var_i32(METADATA_SER_BOOLEAN);
                    w.bool(*has_left);
                    w.u8(METADATA_IDX_GOAT_HAS_RIGHT_HORN);
                    w.var_i32(METADATA_SER_BOOLEAN);
                    w.bool(*has_right);
                }
                MetadataField::PlayingDead(playing_dead) => {
                    // the axolotl class's own playing-dead accessor — index 19; only
                    // `MobSim::snapshots`' `"axolotl"` arm ever builds this
                    // variant. See `METADATA_IDX_AXOLOTL_PLAYING_DEAD`'s own
                    // doc for the claimants this never collides with in
                    // practice.
                    w.u8(METADATA_IDX_AXOLOTL_PLAYING_DEAD);
                    w.var_i32(METADATA_SER_BOOLEAN);
                    w.bool(*playing_dead);
                }
                MetadataField::Dash(is_dashing) => {
                    // the camel class's own dash accessor — index 19; only `SimMob::snapshot`'s
                    // `"camel"` arm ever builds this variant. See
                    // `METADATA_IDX_CAMEL_DASH`'s own doc for the claimants
                    // this never collides with in practice.
                    w.u8(METADATA_IDX_CAMEL_DASH);
                    w.var_i32(METADATA_SER_BOOLEAN);
                    w.bool(*is_dashing);
                }
                MetadataField::SnifferState(state) => {
                    // the sniffer class's own state accessor — index 18; only
                    // `SimMob::snapshot`'s `"sniffer"` arm ever builds this
                    // variant. See `METADATA_IDX_SNIFFER_STATE`'s own doc
                    // for the same-index `ARMADILLO_STATE` claimant this
                    // never collides with in practice.
                    w.u8(METADATA_IDX_SNIFFER_STATE);
                    w.var_i32(METADATA_SER_SNIFFER_STATE);
                    w.var_i32(i32::from(*state));
                }
                MetadataField::CrystalBeamTarget(target) => {
                    // the end-crystal class's own beam-target accessor — index 8,
                    // `OPTIONAL_BLOCK_POS`: a presence bool, then (if present)
                    // the packed-long block position `pack_block_pos` already
                    // writes for every other block-position field in this
                    // module.
                    w.u8(METADATA_IDX_CRYSTAL_BEAM_TARGET);
                    w.var_i32(METADATA_SER_OPTIONAL_BLOCK_POS);
                    match target {
                        Some(pos) => {
                            w.bool(true);
                            w.i64(pack_block_pos(pos.x, pos.y, pos.z));
                        }
                        None => w.bool(false),
                    }
                }
                MetadataField::Pose(id) => {
                    w.u8(METADATA_IDX_POSE);
                    w.var_i32(METADATA_SER_POSE);
                    w.var_i32(*id as i32);
                }
                MetadataField::CrystalShowBottom(show) => {
                    // the end-crystal class's own show-bottom accessor — index 9; only
                    // `MobSim::push_end_crystal_snapshots` ever builds this
                    // variant. See `METADATA_IDX_CRYSTAL_SHOW_BOTTOM`'s own
                    // doc for the other two `BOOLEAN` claimants this never
                    // collides with in practice.
                    w.u8(METADATA_IDX_CRYSTAL_SHOW_BOTTOM);
                    w.var_i32(METADATA_SER_BOOLEAN);
                    w.bool(*show);
                }
            }
        }
        w.u8(METADATA_EOF);
        ServerDirective::Send {
            packet_id: play::clientbound::SET_ENTITY_DATA,
            payload: w.into_vec(),
        }
    }

    /// Vanilla's own clientbound boss-event packet's add-packet factory's
    /// `ADD` operation (confirmed against the decompiled 26.2 source,
    /// its own add-operation writer), read for wire order rather than transcribed from
    /// the constructor: UUID, operation type (`ADD` = `0`, a `VarInt` —
    /// `writeEnum` writes the ordinal), then the `AddOperation` payload —
    /// network-NBT `name`, `f32` progress, color `VarInt`, overlay `VarInt`,
    /// one flags byte.
    ///
    /// Color and overlay are hardcoded to `PINK`/`PROGRESS` (both ordinal `0`)
    /// and the flags byte to `0b110` (`playMusic | createWorldFog`, no
    /// `darkenScreen`) — vanilla's own ender-dragon-fight class's own init's own
    /// `new ServerBossEvent(id, EVENT_DISPLAY_NAME, PINK, PROGRESS)` followed
    /// by `.setPlayBossMusic(true).setCreateWorldFog(true)` — the one producer
    /// this crate has today (`lodestone_server::BossBarSnapshot`'s own doc). A
    /// future second producer with different style would need these as
    /// parameters instead; not plumbed through today since nothing else
    /// builds a bar yet.
    fn encode_boss_event_add(&self, id: Uuid, name: &Text, progress: f32) -> ServerDirective {
        let mut w = Writer::default();
        w.uuid(id);
        w.var_i32(0); // vanilla's own boss-event operation-type add ordinal
        w.bytes(&encode_component_nbt(name));
        w.f32(progress);
        w.var_i32(0); // vanilla's own boss-bar color enum's pink ordinal
        w.var_i32(0); // vanilla's own boss-bar overlay enum's progress ordinal
        w.u8(0b110); // playMusic | createWorldFog, not darkenScreen
        ServerDirective::Send {
            packet_id: play::clientbound::BOSS_EVENT,
            payload: w.into_vec(),
        }
    }

    /// vanilla's own clientbound boss-event packet's own create update progress packet's
    /// `UPDATE_PROGRESS` operation (operation type `2`): UUID, type, one
    /// `f32`. See [`encode_boss_event_add`](Self::encode_boss_event_add)'s doc
    /// for the citation this mirrors.
    fn encode_boss_event_update_progress(&self, id: Uuid, progress: f32) -> ServerDirective {
        let mut w = Writer::default();
        w.uuid(id);
        w.var_i32(2); // vanilla's own boss-event operation-type update-progress ordinal
        w.f32(progress);
        ServerDirective::Send {
            packet_id: play::clientbound::BOSS_EVENT,
            payload: w.into_vec(),
        }
    }

    /// vanilla's own clientbound boss-event packet's own create remove packet's `REMOVE` operation
    /// (operation type `1`): UUID, type, no payload at all
    /// (`REMOVE_OPERATION.write` is an empty method).
    fn encode_boss_event_remove(&self, id: Uuid) -> ServerDirective {
        let mut w = Writer::default();
        w.uuid(id);
        w.var_i32(1); // vanilla's own boss-event operation-type remove ordinal
        ServerDirective::Send {
            packet_id: play::clientbound::BOSS_EVENT,
            payload: w.into_vec(),
        }
    }

    /// The other half of "our server cannot tell a client that
    /// anything is ... exploding" — `crate::adapter::decode_explode`'s own
    /// doc comment names the exact clientbound explosion packet field order
    /// this mirrors (confirmed against the decompiled 26.2 source):
    /// `center: Vec3` (three big-endian `f64`s), `radius: f32`,
    /// `blockCount: i32` (a **plain** fixed-width `INT` codec, not a VarInt —
    /// verified against that same decompiled record, not guessed from the
    /// decoder's own `reader.i32()` call, which would be the
    /// "our decoder validates our encoder" trap this crate's evidence
    /// standard warns against), `playerKnockback: Optional<Vec3>` (a bool
    /// presence flag, no `Vec3` following since this crate applies no
    /// knockback here), `explosionParticle` (a VarInt registry id — always
    /// [`PARTICLE_ID_EXPLOSION_EMITTER`], matching every real detonation:
    /// vanilla's own creeper-explosion routine and every other vanilla
    /// explosion source use
    /// the explosion-emitter particle type, never the plain `EXPLOSION` id
    /// `decode_explode` also accepts), `explosionSound` (a `Holder<SoundEvent>`
    /// — see below), then `blockParticles: WeightedList<ExplosionParticleInfo>`
    /// (a VarInt-prefixed list, always empty here: this crate tracks no
    /// block-destruction model, so there is nothing to report — `decode_explode`
    /// never reads this field at all, by its own doc comment, so an empty
    /// list costs one byte and loses nothing a client today consumes).
    ///
    /// `explosionSound` is encoded as a real registry **reference**, not the
    /// direct/literal-name path `read_sound_holder`'s decode side also
    /// accepts: verified against vanilla's own registry-holder codec's encode
    /// arm (confirmed against the decompiled codec source),
    /// which writes `registryId + 1` for a reference-kind holder — exactly
    /// what a real vanilla server sends for its own generic-explode sound
    /// constant (a
    /// registered constant, never a direct/inline holder). The registry id is
    /// resolved by name via [`lodestone_data::sound_events::sound_event_name`]
    /// (the same reverse-by-name-scan idiom [`stone_id`]/[`air_id`] above
    /// already establish for block states) rather than hand-picking a
    /// literal index, so a regenerated sound-event table cannot silently
    /// desync this from the real registry id.
    ///
    /// Every creeper detonation — charged or not — uses
    /// `minecraft:entity.generic.explode`: vanilla's own creeper-explosion
    /// routine
    /// only varies its explosion multiplier (radius,
    /// `2.0F` when powered, else `1.0F`) before calling the level's own
    /// six-argument `explode` overload, and **every** overload up to the
    /// twelve-argument one this crate's own creeper path effectively mirrors
    /// defaults `explosionSound` to vanilla's own generic-explode sound
    /// constant
    /// unconditionally (confirmed against the decompiled level source) — there
    /// is no powered-creeper
    /// sound variant to pick between. This crate has no charged-creeper
    /// producer today either way ([`lodestone_server::MobSim::take_detonations`]'s
    /// only source is [`lodestone_server::SwellGoal`]/`ignite()`, neither of
    /// which ever sets `DATA_IS_POWERED` — see
    /// `crates/lodestone-entity/src/ai/goals.rs`'s `SwellGoal`), so the
    /// constant is correct for every detonation this encoder can currently
    /// be asked to encode, not merely the common case.
    ///
    /// [`PARTICLE_ID_EXPLOSION_EMITTER`] is likewise the real choice, not an
    /// arbitrary pick between the two ids `decode_explode` accepts:
    /// `ServerLevel::explode` selects `largeExplosionParticles`
    /// (vanilla's own particle-type registry's own explosion-emitter accessor) whenever `ServerExplosion::isSmall`
    /// is false (vanilla's own server-side explosion class's own is small: `radius < 2.0F ||
    /// !interactsWithBlocks()`), and a creeper's `CREEPER_EXPLOSION_RADIUS`
    /// (`3.0`) is `>= 2.0` with block-interaction enabled under default game
    /// rules — the only configuration this crate's `MobSim` models — so
    /// `isSmall()` is false and vanilla sends this id too.
    /// Hand-written rather than derived, for the same reason
    /// `crate::packets::player_info`'s *decoder* is: `player_info_update` is an
    /// action-bitmask packet whose per-entry fields are conditional on the
    /// leading `EnumSet`, which the derive macros cannot express.
    ///
    /// Wire layout, mirroring that decoder exactly (it is the checked-in
    /// specification for this packet, written independently of this encoder and
    /// gated in `tests/player_list.rs`): a fixed bit set of `ceil(8/8) = 1`
    /// byte with bit `i` selecting action ordinal `i`
    /// (vanilla's own buffer-writer helper's own write fixed bit set), a VarInt entry count, then per
    /// entry the profile uuid followed by the fields for each set bit **in
    /// action ordinal order**.
    ///
    /// # Which action bits, and why not all nine
    ///
    /// Vanilla's own join broadcast (`ClientboundPlayerInfoUpdatePacket
    /// .createPlayerInitializing`, `:43-55`) sets all nine actions. This sets
    /// four — `ADD_PLAYER`, `UPDATE_GAME_MODE`, `UPDATE_LISTED`,
    /// `UPDATE_LATENCY` — because those are the four `lodestone-server` has any
    /// value for. The bitmask exists precisely so a subset is legal, and the
    /// client merges per action
    /// (vanilla's own client-side packet listener's own handle player info update, `:2011-2020`).
    ///
    /// `ADD_PLAYER` is the one that is **not** optional: it is the only action
    /// that carries a `GameProfile`, so it is the only one that creates the
    /// `PlayerInfo` entry (`:2004-2009`, `packet.newEntries()`) — and without
    /// that entry the player's own `ADD_ENTITY` is discarded (see
    /// [`ServerProtocol::encode_player_info_add`]'s doc comment for the exact
    /// jar lines).
    ///
    /// The omitted four are omitted rather than stubbed: `INITIALIZE_CHAT` and
    /// `UPDATE_DISPLAY_NAME` would each be a nullability `false` (no chat
    /// session, no scoreboard display name), and `UPDATE_LIST_ORDER`/
    /// `UPDATE_HAT` a `0`/`false`. Sending those bits would claim we had
    /// consulted a source of truth that does not exist here; leaving the bit
    /// clear says nothing at all, which is the accurate statement.
    ///
    /// The values for the three we do send:
    /// * game mode `0` (survival) — restated from
    ///   [`begin_play`](Self::begin_play)'s own `game_type: 0` rather than
    ///   invented, so a player's tab-list entry cannot contradict the game mode
    ///   their own Login packet announced. There is no per-connection game mode
    ///   in `lodestone-server` to read instead.
    /// * `listed: true` — an unlisted player is one deliberately hidden from
    ///   the tab list (vanilla's own default is listed), and nothing here hides
    ///   anyone.
    /// * latency `0` ms — this server measures no round-trip time. The
    ///   keep-alive loop has the timestamps to compute one; wiring that is a
    ///   separate change, and `0` renders as a full-bars ping rather than as a
    ///   plausible-looking lie.
    fn encode_player_info_add(&self, players: &[PlayerListing]) -> Vec<ServerDirective> {
        if players.is_empty() {
            return Vec::new();
        }
        let mut w = Writer::default();
        w.u8(PLAYER_INFO_ADD_ACTIONS);
        w.var_i32(i32::try_from(players.len()).unwrap_or(i32::MAX));
        for player in players {
            w.uuid(player.uuid);
            // ADD_PLAYER (ordinal 0): name, then the profile-property multimap.
            w.string(&player.username);
            w.var_i32(0); // no profile properties: no skin/cape signature to relay.
            // UPDATE_GAME_MODE (2), UPDATE_LISTED (3), UPDATE_LATENCY (4).
            w.var_i32(JOIN_GAME_MODE);
            w.bool(true);
            w.var_i32(0);
        }
        vec![ServerDirective::Send {
            packet_id: play::clientbound::PLAYER_INFO_UPDATE,
            payload: w.into_vec(),
        }]
    }

    /// The `UPDATE_GAME_MODE`-only form of `player_info_update`, for `/gamemode`.
    ///
    /// One action bit (ordinal 2) and therefore one field per entry: the uuid then
    /// the game type as a VarInt. **No `GameProfile`**, because `ADD_PLAYER` is not
    /// in the mask — the entry already exists and this only updates it.
    ///
    /// The `EnumSet` mask and the per-entry body must agree exactly, which is why
    /// the mask is written as the shifted ordinal here too rather than as `4`: a
    /// mask claiming an action whose field is not written reinterprets the next
    /// entry's uuid as this one's payload, and the client reports it as trailing
    /// bytes rather than as a missing field.
    fn encode_player_info_game_mode(
        &self,
        entries: &[(Uuid, lodestone_model::GameMode)],
    ) -> Vec<ServerDirective> {
        if entries.is_empty() {
            return Vec::new();
        }
        let mut w = Writer::default();
        w.u8(1 << 2);
        w.var_i32(i32::try_from(entries.len()).unwrap_or(i32::MAX));
        for (uuid, mode) in entries {
            w.uuid(*uuid);
            w.var_i32(crate::adapter::game_mode_to_ordinal(*mode));
        }
        vec![ServerDirective::Send {
            packet_id: play::clientbound::PLAYER_INFO_UPDATE,
            payload: w.into_vec(),
        }]
    }

    /// `ClientboundPlayerInfoRemovePacket` is a plain
    /// VarInt-prefixed list of profile uuids — see
    /// `crate::packets::player_info::PlayerInfoRemove`'s decoder, this
    /// encoder's independent specification.
    fn encode_player_info_remove(&self, uuids: &[Uuid]) -> Vec<ServerDirective> {
        if uuids.is_empty() {
            return Vec::new();
        }
        let mut w = Writer::default();
        w.var_i32(i32::try_from(uuids.len()).unwrap_or(i32::MAX));
        for uuid in uuids {
            w.uuid(*uuid);
        }
        vec![ServerDirective::Send {
            packet_id: play::clientbound::PLAYER_INFO_REMOVE,
            payload: w.into_vec(),
        }]
    }

    fn encode_explode(&self, centre: Vec3, radius: f32) -> ServerDirective {
        let mut w = Writer::default();
        w.f64(centre.x);
        w.f64(centre.y);
        w.f64(centre.z);
        w.f32(radius);
        w.i32(0); // blockCount: no block-destruction model.
        w.bool(false); // playerKnockback: Optional<Vec3>, never present.
        w.var_i32(PARTICLE_ID_EXPLOSION_EMITTER);
        let sound_id = explosion_sound_registry_id();
        w.var_i32(sound_id + 1); // Holder::REFERENCE encoding: registryId + 1.
        w.var_i32(0); // blockParticles: empty WeightedList.
        ServerDirective::Send {
            packet_id: play::clientbound::EXPLODE,
            payload: w.into_vec(),
        }
    }

    fn encode_game_event(&self, kind: u8, value: f32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::GAME_EVENT,
            payload: game_event_body(kind, value),
        }
    }

    /// `ClientboundSoundPacket`, the exact inverse of
    /// [`crate::adapter`]'s own `decode_sound`.
    ///
    /// Two byte-level details, both restated from the decode side rather than
    /// guessed:
    ///
    /// * the `Holder<SoundEvent>` is sent in the **registry-reference** form a
    ///   real vanilla server sends — `registryId + 1`, `0` being reserved to
    ///   introduce an inline definition. Same encoding
    ///   [`Self::encode_explode`] already uses for its own baked-in sound;
    /// * the position is fixed-point, `(int)(block * 8)`
    ///   (`LOCATION_ACCURACY`), **not** three `f64`s.
    ///
    /// A sound name outside 26.2's registry emits nothing rather than a
    /// packet the client cannot decode — `lodestone_server::effects` validates
    /// every name it derives, so this is a second line of defence, not the
    /// first.
    fn encode_sound(
        &self,
        sound: &str,
        category: SoundCategory,
        pos: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) -> ServerDirective {
        let Some(registry_id) = sound_event_registry_id(sound) else {
            return ServerDirective::None;
        };
        let mut w = Writer::default();
        w.var_i32(registry_id + 1); // Holder::REFERENCE: registryId + 1.
        w.var_i32(i32::from(category.ordinal()));
        w.i32((pos.x * SOUND_POSITION_SCALE) as i32);
        w.i32((pos.y * SOUND_POSITION_SCALE) as i32);
        w.i32((pos.z * SOUND_POSITION_SCALE) as i32);
        w.f32(volume);
        w.f32(pitch);
        w.i64(seed);
        ServerDirective::Send {
            packet_id: play::clientbound::SOUND,
            payload: w.into_vec(),
        }
    }

    /// `ClientboundLevelEventPacket` — the event code, the packed
    /// position, the event-specific data, then the global flag, matching
    /// [`crate::packets::game::LevelEvent`]'s own field order.
    fn encode_level_event(&self, event: i32, pos: BlockPos, data: i32, global: bool) -> ServerDirective {
        let mut w = Writer::default();
        w.i32(event);
        w.i64(pack_block_pos(pos.x, pos.y, pos.z));
        w.i32(data);
        w.bool(global);
        ServerDirective::Send {
            packet_id: play::clientbound::LEVEL_EVENT,
            payload: w.into_vec(),
        }
    }

    /// `ClientboundLevelParticlesPacket`, mirroring
    /// [`crate::packets::game::LevelParticles`]'s field order.
    ///
    /// The trailing particle field is a `minecraft:particle_type` registry id
    /// followed by that type's own option bytes. Only argument-less
    /// (`SimpleParticleType`) particles are sent, whose stream codec writes
    /// **no** further bytes — so the packet ends at the id. A type that does
    /// carry options (`dust`, `block`, `item`) would need those bytes and is
    /// rejected here rather than sent truncated, which the client would read as
    /// a misparse of the *next* packet.
    fn encode_level_particles(
        &self,
        particle: &str,
        pos: Vec3,
        offset: Vec3f,
        max_speed: f32,
        count: i32,
        long_distance: bool,
    ) -> ServerDirective {
        let Some(particle_id) = simple_particle_registry_id(particle) else {
            return ServerDirective::None;
        };
        let mut w = Writer::default();
        w.bool(long_distance); // overrideLimiter
        w.bool(false); // alwaysShow
        w.f64(pos.x);
        w.f64(pos.y);
        w.f64(pos.z);
        w.f32(offset.x);
        w.f32(offset.y);
        w.f32(offset.z);
        w.f32(max_speed);
        w.i32(count);
        w.var_i32(particle_id);
        ServerDirective::Send {
            packet_id: play::clientbound::LEVEL_PARTICLES,
            payload: w.into_vec(),
        }
    }

    /// Re-sends `SET_HEALTH` with the new health — the same packet and
    /// struct [`begin_play`](Self::begin_play) already sends once at join.
    /// `food`/`saturation` are resent at the same fresh-spawn constants
    /// `begin_play` uses (`20`, `5.0`): `lodestone-server` has no hunger
    /// model to track a real value for either (the same "no inventory model"
    /// scope this crate's `UseItemOn` handling already documents applies
    /// equally here — there is simply nothing that changes them), so
    /// restating the constant is honest about there being no hunger
    /// simulation, not a claim that hunger is unaffected by anything.
    fn encode_set_health(&self, health: f32, food: i32, saturation: f32) -> ServerDirective {
        send(
            play::clientbound::SET_HEALTH,
            &SetHealth {
                health: health.clamp(0.0, 20.0),
                // Clamped here rather than trusted: the wire field is the HUD's
                // haunch count, and a value outside `0..=20` draws an overflowing
                // bar. `food` used to be a hardcoded `20` and `saturation` a
                // hardcoded `5.0`, which is why hunger was invisible.
                food: food.clamp(0, 20),
                saturation: saturation.clamp(0.0, 20.0),
            },
        )
    }

    /// `ClientboundUpdateAttributesPacket` for the local player. Hand-written
    /// against [`write_update_attributes`], the mirror-side specification for
    /// this crate's own decode (`V770Adapter::handle_play`'s
    /// `UPDATE_ATTRIBUTES` arm) — the same "no derive macro" reasoning
    /// `encode_set_experience`/`encode_air_supply_update` already document:
    /// a modifier list is a variable-length nested structure the `Encode`
    /// derive does not model.
    fn encode_update_attributes(&self, attributes: &[EntityAttributeSnapshot]) -> ServerDirective {
        let mut w = Writer::default();
        write_update_attributes(&mut w, LOCAL_PLAYER_ENTITY_ID, attributes);
        ServerDirective::Send {
            packet_id: play::clientbound::UPDATE_ATTRIBUTES,
            payload: w.into_vec(),
        }
    }

    /// The death notification that raises the client's death screen — see
    /// [`ServerProtocol::encode_player_combat_kill`]'s trait doc comment for why
    /// `set_health(0.0)` alone does not.
    ///
    /// Hand-written, in the same "no existing struct" style as
    /// [`encode_system_chat`]: the client side only ever *decodes* this packet, and
    /// that decoder is the mirror-side specification —
    /// `V770Adapter::handle_play`'s `PLAYER_COMBAT_KILL` arm reads exactly a VarInt
    /// player id followed by `read_network_nbt`, matching vanilla's own
    /// clientbound player-combat-kill packet's own
    /// VarInt stream codec plus its trusted component-serialization stream
    /// codec (confirmed against the decompiled 26.2 client source).
    fn encode_player_combat_kill(&self, player_entity_id: i32, message: &Text) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(player_entity_id);
        w.bytes(&encode_component_nbt(message));
        ServerDirective::Send {
            packet_id: play::clientbound::PLAYER_COMBAT_KILL,
            payload: w.into_vec(),
        }
    }

    /// The respawn pair — see [`ServerProtocol::encode_respawn`]'s trait doc
    /// comment for why the position packet alone would leave the death screen up.
    ///
    /// `data_to_keep` is `0`. `ClientboundRespawnPacket` defines
    /// `KEEP_ATTRIBUTE_MODIFIERS = 0x01` and `KEEP_ENTITY_DATA = 0x02`, and a real
    /// **death** respawn keeps neither — vanilla's own server-side player-list class's own respawn passes the combined
    /// `KEEP_ALL_DATA` only for a dimension change. `0` is what makes the client
    /// rebuild its player state, which is the whole point of the packet.
    ///
    /// The fields that are not modelled carry `begin_play_at`'s own join values, so
    /// a respawn cannot silently change the dimension window a chunk is framed
    /// against: same `dimension_type` holder id `0`, same `minecraft:overworld`,
    /// same `game_type` survival, same `sea_level`. `previous_game_type` is `-1`
    /// ("there was none"), which is what this crate's decoder maps to `None`.
    fn encode_respawn(&self, spawn: Vec3) -> Vec<ServerDirective> {
        self.encode_respawn_with_teleport_id(0, spawn)
    }

    fn encode_respawn_with_teleport_id(&self, teleport_id: i32, spawn: Vec3) -> Vec<ServerDirective> {
        let respawn = Respawn {
            dimension_type: 0,
            dimension: "minecraft:overworld".to_string(),
            seed: 0,
            game_type: 0,
            previous_game_type: -1,
            is_debug: false,
            is_flat: false,
            last_death_location: None,
            portal_cooldown: 0,
            sea_level: OVERWORLD_SEA_LEVEL,
            data_to_keep: 0,
        };
        vec![
            send(play::clientbound::RESPAWN, &respawn),
            // The placement teleport. vanilla's own server-side player-list class's own respawn moves the rebuilt
            // player entity itself; over the wire that is the same
            // `player_position` packet `begin_play_at` sends at join, so the two
            // paths agree by construction rather than by coincidence.
            ServerDirective::Send {
                packet_id: play::clientbound::PLAYER_POSITION,
                payload: encode_player_position_teleport(
                    teleport_id,
                    spawn.x,
                    spawn.y,
                    spawn.z,
                    0.0,
                    0.0,
                ),
            },
            // Vanilla's vanilla's own server-side player-list class's own respawn also re-sends the player's health,
            // and the client's `Vitals` component is fed by `set_health` alone —
            // without this the HUD would keep showing the zero hearts it was left
            // on. `crate::server::apply_client_command` sends the authoritative
            // value from `PlayerVitals` immediately after this list, so this is
            // deliberately *not* duplicated here.
        ]
    }

    /// `/tp`'s producer — see [`ServerProtocol::encode_teleport`]'s trait doc
    /// for why this method exists at all. The wire body is the exact same
    /// `encode_player_position_teleport` free function the join sequence and
    /// [`encode_respawn`](Self::encode_respawn) already use, so all three stay
    /// byte-identical for the same inputs by construction, not by convention.
    /// The server calls [`encode_teleport_with_id`](Self::encode_teleport_with_id)
    /// for a live connection so its correction id and the following
    /// `ACCEPT_TELEPORTATION` reply are one state transition rather than two
    /// unrelated packets.
    fn encode_teleport(&self, x: f64, y: f64, z: f64, yaw: f32, pitch: f32) -> ServerDirective {
        self.encode_teleport_with_id(0, x, y, z, yaw, pitch)
    }

    fn encode_teleport_with_id(
        &self,
        teleport_id: i32,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
    ) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::PLAYER_POSITION,
            payload: encode_player_position_teleport(teleport_id, x, y, z, yaw, pitch),
        }
    }

    fn encode_tag_query(&self, transaction_id: i32, tag: Option<&Nbt>) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(transaction_id);
        write_network_nbt(&mut w, tag.unwrap_or(&Nbt::End))
            .expect("authoritative block-entity NBT encodes");
        ServerDirective::Send {
            packet_id: play::clientbound::TAG_QUERY,
            payload: w.into_vec(),
        }
    }

    /// Vanilla's own clientbound animate packet writer: `writeVarInt` then a
    /// **plain, unsigned byte** (`writeByte`, not a VarInt) — confirmed
    /// against the decompiled 26.2 source, whose own `id`/`action` fields this
    /// mirrors field-for-field. `ServerBound::Swing`'s own doc comment names
    /// the consumer this drives.
    fn encode_animate(&self, entity_id: i32, action: u8) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(entity_id);
        w.u8(action);
        ServerDirective::Send {
            packet_id: play::clientbound::ANIMATE,
            payload: w.into_vec(),
        }
    }

    /// Vanilla's own clientbound set-camera packet writer: a single
    /// `writeVarInt` — the smallest possible wire body, matching the
    /// decompiled 26.2 source.
    /// `ServerBound::SpectatorAction`'s own doc comment names the consumer.
    fn encode_set_camera(&self, entity_id: i32) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(entity_id);
        ServerDirective::Send {
            packet_id: play::clientbound::SET_CAMERA,
            payload: w.into_vec(),
        }
    }

    /// The dimension-change respawn pair — see
    /// [`ServerProtocol::encode_dimension_change`]'s trait doc for why this is a
    /// separate encoder from [`encode_respawn`](Self::encode_respawn) rather than
    /// the same one with a flag.
    ///
    /// # The holder id comes from this crate's own registry, by name
    ///
    /// [`encode_registry_data`](Self::encode_registry_data) publishes
    /// `minecraft:dimension_type` with four entries **in a fixed order**, and a
    /// holder id is that list's index — overworld 0, `overworld_caves` 1,
    /// `the_end` 2, `the_nether` 3. `dimension_type_holder_id` reads the mapping out
    /// of the same order, so adding a fifth registry entry cannot silently renumber
    /// the Nether. An unrecognised key returns `None` and this emits **nothing**,
    /// which the server treats as "cannot change dimension" and declines to move the
    /// player — the trait doc explains why guessing is worse.
    ///
    /// # `data_to_keep` is `KEEP_ALL_DATA`, and `sea_level` follows the dimension
    ///
    /// vanilla's own server-side player-list class's own respawn passes `KEEP_ATTRIBUTE_MODIFIERS | KEEP_ENTITY_DATA` for
    /// a dimension change, which is what keeps the arriving player's inventory, XP
    /// and health rather than rebuilding them. `sea_level` is the destination's, not
    /// the overworld's: the Nether's is 32 (`noise_settings/nether.json`'s
    /// `sea_level`), and it is what the client's own fluid-fog and ambient checks
    /// frame against.
    fn encode_dimension_change(
        &self,
        dimension: &str,
        spawn: Vec3,
        mode: GameMode,
    ) -> Vec<ServerDirective> {
        self.encode_dimension_change_with_teleport_id(0, dimension, spawn, mode)
    }

    fn encode_dimension_change_with_teleport_id(
        &self,
        teleport_id: i32,
        dimension: &str,
        spawn: Vec3,
        mode: GameMode,
    ) -> Vec<ServerDirective> {
        let Some(holder_id) = dimension_type_holder_id(dimension) else {
            return Vec::new();
        };
        let respawn = Respawn {
            dimension_type: holder_id,
            dimension: dimension.to_string(),
            seed: 0,
            game_type: crate::adapter::game_mode_to_ordinal(mode) as u8,
            previous_game_type: -1,
            is_debug: false,
            is_flat: false,
            last_death_location: None,
            portal_cooldown: 0,
            sea_level: sea_level_for_dimension(dimension),
            // vanilla's own clientbound respawn packet's own keep-all-data accessor.
            data_to_keep: 0x03,
        };
        vec![
            send(play::clientbound::RESPAWN, &respawn),
            ServerDirective::Send {
                packet_id: play::clientbound::PLAYER_POSITION,
                payload: encode_player_position_teleport(
                    teleport_id,
                    spawn.x,
                    spawn.y,
                    spawn.z,
                    0.0,
                    0.0,
                ),
            },
        ]
    }

    /// The difficulty confirmation — see
    /// [`ServerProtocol::encode_change_difficulty`]'s trait doc comment and
    /// `crate::server::apply_difficulty_change` for the consumer.
    fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
        send(
            play::clientbound::CHANGE_DIFFICULTY,
            &ChangeDifficultyClientbound {
                difficulty: difficulty_to_ordinal(difficulty),
                locked,
            },
        )
    }

    /// The game-rule confirmation — see
    /// [`ServerProtocol::encode_game_rule_values`]'s trait doc comment and
    /// `crate::server::apply_game_rule_changed` for the consumer. Carries
    /// only `entries` (the just-changed rules), not vanilla's full current
    /// table — see [`GameRuleValues`]'s own doc comment.
    fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
        send(
            play::clientbound::GAME_RULE_VALUES,
            &GameRuleValues {
                entries: entries
                    .iter()
                    .map(|(key, value)| GameRuleEntry {
                        key: key.clone(),
                        value: value.clone(),
                    })
                    .collect(),
            },
        )
    }

    /// See [`ServerProtocol::encode_open_screen`]'s trait doc comment and
    /// `crate::server`'s consumer (`lodestone-server`) for when this is
    /// called. `menu` with no entry in [`lodestone_data::menus`]'s generated
    /// table (should not happen for any of the menu names
    /// `crate::block_entities::BlockEntity::menu_name` can produce) emits
    /// nothing rather than a packet carrying a made-up registry id.
    fn encode_open_screen(&self, window_id: i32, menu: &str, title: &str) -> ServerDirective {
        match menu_id(menu) {
            Some(id) => ServerDirective::Send {
                packet_id: play::clientbound::OPEN_SCREEN,
                payload: encode_open_screen_body(window_id, id, title),
            },
            None => ServerDirective::None,
        }
    }

    /// See [`ServerProtocol::encode_merchant_offers`]'s trait doc comment and
    /// [`encode_merchant_offers_body`] for the wire layout.
    fn encode_merchant_offers(
        &self,
        window_id: i32,
        offers: &[MerchantOfferOut],
        level: i32,
        xp: i32,
        show_progress: bool,
        can_restock: bool,
    ) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::MERCHANT_OFFERS,
            payload: encode_merchant_offers_body(
                window_id,
                offers,
                level,
                xp,
                show_progress,
                can_restock,
            ),
        }
    }

    /// See [`ServerProtocol::encode_container_content`]'s trait doc comment.
    fn encode_container_content(
        &self,
        window_id: i32,
        state_id: i32,
        items: &[Option<ItemStack>],
        carried: Option<&ItemStack>,
    ) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::CONTAINER_SET_CONTENT,
            payload: encode_container_content_body(window_id, state_id, items, carried),
        }
    }

    /// See [`ServerProtocol::encode_container_slot`]'s trait doc comment.
    fn encode_container_slot(
        &self,
        window_id: i32,
        state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::CONTAINER_SET_SLOT,
            payload: encode_container_slot_body(window_id, state_id, slot, item),
        }
    }

    /// See [`ServerProtocol::encode_container_data`]'s trait doc comment.
    fn encode_container_data(&self, window_id: i32, property: i32, value: i32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::CONTAINER_SET_DATA,
            payload: encode_container_data_body(window_id, property, value),
        }
    }

    /// See [`ServerProtocol::encode_update_mob_effect`]'s trait doc comment.
    /// `None` for an effect this crate's registry table cannot resolve —
    /// degrading to no packet rather than writing a bogus id `mob_effect_id`
    /// itself already returned `None` for, matching `write_item_cost`'s own
    /// "an unresolvable id writes nothing rather than corrupting the rest of
    /// the packet" convention.
    fn encode_update_mob_effect(
        &self,
        entity_id: i32,
        effect: &str,
        amplifier: u32,
        duration_ticks: i32,
        ambient: bool,
        visible: bool,
        show_icon: bool,
        blend: bool,
    ) -> ServerDirective {
        match mob_effect_id(effect) {
            Some(effect_id) => ServerDirective::Send {
                packet_id: play::clientbound::UPDATE_MOB_EFFECT,
                payload: encode_update_mob_effect_body(
                    entity_id,
                    effect_id,
                    amplifier,
                    duration_ticks,
                    ambient,
                    visible,
                    show_icon,
                    blend,
                ),
            },
            None => ServerDirective::None,
        }
    }

    /// See [`ServerProtocol::encode_remove_mob_effect`]'s trait doc comment.
    fn encode_remove_mob_effect(&self, entity_id: i32, effect: &str) -> ServerDirective {
        match mob_effect_id(effect) {
            Some(effect_id) => ServerDirective::Send {
                packet_id: play::clientbound::REMOVE_MOB_EFFECT,
                payload: encode_remove_mob_effect_body(entity_id, effect_id),
            },
            None => ServerDirective::None,
        }
    }

    /// See [`ServerProtocol::encode_set_held_slot`]'s trait doc comment.
    /// The client side of this exact wire shape already exists
    /// (`adapter::player::handle_play_player`'s `SET_HELD_SLOT` arm decodes
    /// the same single VarInt into `ClientEvent::HeldSlotChanged`); this was
    /// the missing server-side encoder.
    fn encode_set_held_slot(&self, slot: u8) -> ServerDirective {
        send(play::clientbound::SET_HELD_SLOT, &SetHeldSlot { slot: i32::from(slot) })
    }

    /// See [`ServerProtocol::encode_initialize_border`]'s trait doc comment.
    /// The packet's `old_size`/`new_size`/`lerp_time` triple
    /// is the border's `size`/`lerp_target`/`lerp_time` readout, with
    /// `lerp_time` converted from the border's remaining **ticks** to the
    /// milliseconds the lodestone client's `BorderExtent::Moving` interpolates
    /// on — vanilla writes the raw tick count here, so this `* 50` is this
    /// crate's deliberate divergence (see [`InitializeBorder`]'s packet doc).
    /// For the full-size static default all three are the flat
    /// [`WorldBorder::size`] and the conversion is a no-op (`0 * 50`), exactly
    /// the state a vanilla client shows on join. Called from
    /// [`begin_play_at`](Self::begin_play_at) between the `login` and
    /// `set_default_spawn_position` packets.
    fn encode_initialize_border(&self, border: &WorldBorder) -> ServerDirective {
        send(
            play::clientbound::INITIALIZE_BORDER,
            &InitializeBorder {
                center_x: border.center_x(),
                center_z: border.center_z(),
                old_size: border.size(),
                new_size: border.lerp_target(),
                lerp_time: border.lerp_time() * 50,
                absolute_max_size: border.absolute_max_size(),
                warning_blocks: border.warning_blocks(),
                warning_time: border.warning_time(),
            },
        )
    }

    /// See [`ServerProtocol::encode_set_border_center`]'s trait doc comment.
    fn encode_set_border_center(&self, x: f64, z: f64) -> ServerDirective {
        send(
            play::clientbound::SET_BORDER_CENTER,
            &SetBorderCenter { center_x: x, center_z: z },
        )
    }

    /// See [`ServerProtocol::encode_set_border_lerp_size`]'s trait doc comment.
    /// `lerp_time_ms` is already **milliseconds** (vanilla's wire carries the
    /// raw tick count and this crate's client decodes the field as ms — see
    /// [`SetBorderLerpSize`]'s own doc comment, which is where the caller's
    /// ticks→ms conversion is documented); the encoder is the last hop and
    /// writes it verbatim.
    fn encode_set_border_lerp_size(
        &self,
        old_size: f64,
        new_size: f64,
        lerp_time_ms: i64,
    ) -> ServerDirective {
        send(
            play::clientbound::SET_BORDER_LERP_SIZE,
            &SetBorderLerpSize {
                old_size,
                new_size,
                lerp_time_ms,
            },
        )
    }

    /// See [`ServerProtocol::encode_set_border_size`]'s trait doc comment.
    fn encode_set_border_size(&self, size: f64) -> ServerDirective {
        send(
            play::clientbound::SET_BORDER_SIZE,
            &SetBorderSize { size },
        )
    }

    /// See [`ServerProtocol::encode_set_border_warning_delay`]'s trait doc
    /// comment.
    fn encode_set_border_warning_delay(&self, warning_time: i32) -> ServerDirective {
        send(
            play::clientbound::SET_BORDER_WARNING_DELAY,
            &SetBorderWarningDelay { warning_time },
        )
    }

    /// See [`ServerProtocol::encode_set_border_warning_distance`]'s trait doc
    /// comment.
    fn encode_set_border_warning_distance(&self, warning_blocks: i32) -> ServerDirective {
        send(
            play::clientbound::SET_BORDER_WARNING_DISTANCE,
            &SetBorderWarningDistance { warning_blocks },
        )
    }

    /// See [`ServerProtocol::encode_update_advancements`]'s trait doc comment.
    ///
    /// Before this override the trait default returned `ServerDirective::None`,
    /// so the whole advancement path — a real `AdvancementManager` with per-player
    /// progress, an every-tick `flush_dirty`, and a join-time `initial_update` —
    /// reached the wire as **nothing**, even in singleplayer against our own
    /// server. That is the island shape, with every intermediate piece green.
    ///
    /// Wire shape (`ClientboundUpdateAdvancementsPacket`'s own reader): a bool
    /// `reset`, a VarInt-counted list of `AdvancementHolder` (id, optional parent,
    /// optional `DisplayInfo`, the AND-of-ORs requirement groups, and the
    /// `sendsTelemetryEvent` bit), a VarInt-counted list of removed ids, a
    /// VarInt-counted map of id → per-criterion nullable `Instant`, and a bool
    /// `showAdvancements`.
    ///
    /// The display optional is always written absent — see
    /// [`encode_update_advancements_body`] for why, and note that a *vanilla*
    /// client hides a display-less advancement, so this override is complete for
    /// our own client and partial for vanilla's. Growing it needs a component
    /// model in `lodestone-server`, which is that crate's own scoped omission.
    fn encode_update_advancements(&self, update: &AdvancementUpdate) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::UPDATE_ADVANCEMENTS,
            payload: encode_update_advancements_body(update),
        }
    }

    /// See [`ServerProtocol::encode_recipe_book_add`]'s trait doc. This override
    /// is what makes `PLACE_RECIPE` reachable at all: the ids it
    /// hands out are the only ids any client can echo back.
    fn encode_recipe_book_add(
        &self,
        entries: &[ServerRecipeBookEntry],
        replace: bool,
    ) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::RECIPE_BOOK_ADD,
            payload: encode_recipe_book_add_body(entries, replace),
        }
    }

    /// Echoes the server-owned advancement tab selection to the client. The
    /// optional identifier uses a leading boolean, unlike the serverbound
    /// selection action whose discriminant decides whether an identifier
    /// follows.
    fn encode_select_advancements_tab(&self, tab: Option<&str>) -> ServerDirective {
        let mut w = Writer::default();
        w.bool(tab.is_some());
        if let Some(tab) = tab {
            w.string(tab);
        }
        ServerDirective::Send {
            packet_id: play::clientbound::SELECT_ADVANCEMENTS_TAB,
            payload: w.into_vec(),
        }
    }

    /// See [`ServerProtocol::encode_award_stats`]'s trait doc comment. Same
    /// missing-override story as
    /// [`encode_update_advancements`](Self::encode_update_advancements): the
    /// server already answered `ClientCommand(REQUEST_STATS)` by building a real
    /// snapshot and handing it to a seam that dropped it.
    ///
    /// A key whose value does not resolve in its stat type's registry is
    /// **skipped**, not encoded with a made-up id — the count is taken after
    /// resolution so the map length always matches the entries that follow.
    fn encode_award_stats(&self, stats: &[(StatKey, i32)]) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::AWARD_STATS,
            payload: encode_award_stats_body(stats),
        }
    }

    /// This host serves the embedded 26.2 worldgen bundle: the
    /// `assets/worldgen/` data `lodestone-server` embeds (its `worldgen_data`
    /// module's version gate, `bundled_worldgen_serves`) is this version's
    /// data, so the gate must recognise it. This is the one production
    /// override — every other implementor (test doubles, future families)
    /// keeps the trait default, which means "no worldgen this crate's bundle
    /// can serve".
    fn worldgen_scope(&self) -> WorldgenScope {
        WorldgenScope::V26_2
    }
}

#[cfg(test)]
mod block_edit_tests {
    use super::*;
    use lodestone_core::State;

    fn encode<T: Encode>(packet: &T) -> Vec<u8> {
        let mut w = Writer::default();
        packet.encode(&mut w, CTX).expect("well-formed struct encodes");
        w.into_vec()
    }

    /// `PLAYER_ACTION` ordinal `0` (`START_DESTROY_BLOCK`) round-trips
    /// through the real derived `Encode`/decode into
    /// `ServerBound::BlockAction`, with `pos`/`face` unpacked correctly —
    /// pinning both [`unpack_block_pos`] against [`pack_block_pos`] and
    /// [`face_from_ordinal`] against a non-trivial (non-zero) face.
    #[test]
    fn decode_player_action_start_destroy() {
        let proto = V770ServerProtocol;
        let body = encode(&PlayerAction {
            action: 0,
            pos: pack_block_pos(1, 2, 3),
            direction: 1, // Up
            sequence: 42,
        });
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body);
        assert_eq!(
            decoded,
            ServerBound::BlockAction {
                action: BlockActionKind::StartDestroy,
                pos: BlockPos::new(1, 2, 3),
                face: BlockFace::Up,
                sequence: 42,
            }
        );
    }

    /// The other two destroy ordinals decode to their matching
    /// `BlockActionKind`, proving the ordinal mapping is not just
    /// coincidentally right for `0`.
    #[test]
    fn decode_player_action_abort_and_stop() {
        let proto = V770ServerProtocol;
        for (ordinal, expected) in [
            (1, BlockActionKind::AbortDestroy),
            (2, BlockActionKind::StopDestroy),
        ] {
            let body = encode(&PlayerAction {
                action: ordinal,
                pos: pack_block_pos(0, 0, 0),
                direction: 0,
                sequence: 0,
            });
            let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body);
            assert_eq!(
                decoded,
                ServerBound::BlockAction {
                    action: expected,
                    pos: BlockPos::new(0, 0, 0),
                    face: BlockFace::Down,
                    sequence: 0,
                },
                "ordinal {ordinal}"
            );
        }
    }

    /// The item-action ordinals share the wire packet with the three destroy
    /// phases and must not fall into one of them.
    ///
    /// **This test used to require `3..=7` to be `Ignored`, and that made it a
    /// gate asserting a bug.** Its stated premise — *"this crate has no inventory
    /// model to act on them"* — was true when written and had stopped being:
    /// `lodestone-server` owns `PlayerInventory` and already spawns item entities
    /// for block drops. Meanwhile the *client* half was complete (a keybind, four
    /// adapters encoding ordinals 3 and 4), so `Q` did nothing whatsoever and this
    /// test required that it keep doing nothing. The premise had to be re-checked
    /// rather than the assertion trusted; see `DESIGN.md` §12.150.
    ///
    /// `7` (STAB) is still genuinely unmodelled, and keeping it in its own
    /// assertion is what stops the two drop arms from having been written as a
    /// `3..=7` catch-all.
    #[test]
    fn decode_player_action_drop_ordinals_lift_and_the_rest_are_ignored() {
        let proto = V770ServerProtocol;
        let body = |ordinal: i32| {
            encode(&PlayerAction {
                action: ordinal,
                pos: 0,
                direction: 0,
                sequence: 0,
            })
        };
        // 3 is DROP_ALL_ITEMS and 4 is DROP_ITEM, per the jar's own enum order —
        // backwards from the keys, where `Q` is one item and `Ctrl+Q` is the stack.
        for (ordinal, whole_stack) in [(3, true), (4, false)] {
            let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body(ordinal));
            assert_eq!(
                decoded,
                ServerBound::ItemDropped { whole_stack },
                "ordinal {ordinal}"
            );
        }
        // 5 is RELEASE_USE_ITEM, and it lifts now: it is the packet that fires a
        // drawn bow, so leaving it `Ignored` meant a player could draw and never
        // shoot. This assertion used to be part of the `5..=7` sweep below, which
        // is the shape CLAUDE.md warns about — a new subsystem silently breaking a
        // test that asserted its absence.
        assert_eq!(
            proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body(5)),
            ServerBound::ReleaseUseItem,
        );
        // 6 is SWAP_ITEM_WITH_OFFHAND, and it lifts now — see
        // `ServerBound::SwapItemInHand`'s own doc comment for the consumer.
        assert_eq!(
            proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body(6)),
            ServerBound::SwapItemInHand,
        );
        // 7 is STAB: still no server-side model.
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body(7));
        assert_eq!(decoded, ServerBound::Ignored, "ordinal 7");
        // Past the enum: still ignored, so the arm above is a specific lift rather
        // than a catch-all that swallowed the tail.
        for ordinal in [8, 99, -1] {
            let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body(ordinal));
            assert_eq!(decoded, ServerBound::Ignored, "ordinal {ordinal}");
        }
    }

    /// `USE_ITEM` lifts with its hand and the facing it carries — the launch
    /// direction for every player-thrown projectile.
    ///
    /// The yaw/pitch are what make a throw aimable without this crate tracking
    /// rotation per connection, so a decode that dropped them would leave every
    /// snowball flying due south. Asserted by value, and with a non-zero,
    /// non-symmetric pair so a transposed yaw/pitch read is caught too.
    #[test]
    fn decode_use_item_lifts_hand_and_facing() {
        let proto = V770ServerProtocol;
        let body = encode(&UseItem {
            hand: 1,
            sequence: 7,
            yaw: 137.5,
            pitch: -22.25,
        });
        assert_eq!(
            proto.decode(State::Play, play::serverbound::USE_ITEM, &body),
            ServerBound::UseItem {
                hand: 1,
                yaw: 137.5,
                pitch: -22.25,
            },
        );
        // A hand ordinal outside `0..=1` is carried through rather than dropping
        // the packet; the *consumer* treats anything but `1` as the main hand
        // (`apply_use_item`'s own branch), which is where the degradation belongs
        // because only it knows what a hand means. A **negative** ordinal cannot
        // survive the `u8` conversion and lands on `0`, which is the main hand too —
        // asserted so the two malformed shapes are known to agree.
        for wire in [42, -1] {
            let odd = encode(&UseItem {
                hand: wire,
                sequence: 0,
                yaw: 0.0,
                pitch: 0.0,
            });
            let ServerBound::UseItem { hand, .. } =
                proto.decode(State::Play, play::serverbound::USE_ITEM, &odd)
            else {
                panic!("a malformed hand must not drop the packet");
            };
            assert_ne!(hand, 1, "wire {wire} must not read as the off hand");
        }
    }

    /// The two packets a game-mode change writes, byte-exact. The flags byte is
    /// the whole reason creative flight works or does not: `0x0D` is
    /// `invulnerable | can_fly | instabuild`, and `flying` is deliberately
    /// **not** set for creative (vanilla's own game-type enum's own update player abilities sets it only
    /// for spectator). A fully-connected wire carrying the wrong byte here looks
    /// identical to a correct one from every coverage angle.
    #[test]
    fn encode_creative_writes_game_event_3_and_abilities_flags() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { packet_id, payload } =
            proto.encode_game_mode(GameMode::Creative)
        else {
            panic!("game-mode change must be a Send");
        };
        assert_eq!(packet_id, play::clientbound::GAME_EVENT);
        // `u8` event code then a big-endian `f32` parameter: code 3, param 1.0.
        assert_eq!(payload, vec![3, 0x3F, 0x80, 0x00, 0x00]);

        let ServerDirective::Send { packet_id, payload } =
            proto.encode_player_abilities(Abilities::for_mode(GameMode::Creative))
        else {
            panic!("abilities must be a Send");
        };
        assert_eq!(packet_id, play::clientbound::PLAYER_ABILITIES);
        assert_eq!(payload[0], 0x0D, "invulnerable | can_fly | instabuild");

        // Survival is the negative arm: same two packets, no flags at all.
        let ServerDirective::Send { payload, .. } =
            proto.encode_player_abilities(Abilities::for_mode(GameMode::Survival))
        else {
            panic!("abilities must be a Send");
        };
        assert_eq!(payload[0], 0x00);

        // Spectator is the one mode that ships `flying` already set.
        let ServerDirective::Send { payload, .. } =
            proto.encode_player_abilities(Abilities::for_mode(GameMode::Spectator))
        else {
            panic!("abilities must be a Send");
        };
        assert_eq!(payload[0], 0x07, "invulnerable | flying | can_fly");
    }

    /// The F4 switcher round-trips into a real `ServerBound` variant rather than
    /// the `Ignored` it used to decode to, and an out-of-range id is dropped.
    #[test]
    fn decode_change_game_mode() {
        let proto = V770ServerProtocol;
        for (id, mode) in [
            (0, GameMode::Survival),
            (1, GameMode::Creative),
            (2, GameMode::Adventure),
            (3, GameMode::Spectator),
        ] {
            let body = encode(&ChangeGameMode { mode: id });
            assert_eq!(
                proto.decode(State::Play, play::serverbound::CHANGE_GAME_MODE, &body),
                ServerBound::ChangeGameMode { mode }
            );
        }
        let body = encode(&ChangeGameMode { mode: 9 });
        assert_eq!(
            proto.decode(State::Play, play::serverbound::CHANGE_GAME_MODE, &body),
            ServerBound::Ignored
        );
    }

    /// `USE_ITEM_ON` round-trips into `ServerBound::UseItemOn`, including a
    /// negative Y (below `y = 0`) to pin `unpack_block_pos`'s sign extension.
    #[test]
    fn decode_use_item_on() {
        let proto = V770ServerProtocol;
        let body = encode(&UseItemOn {
            hand: 0,
            pos: pack_block_pos(5, -10, -7),
            face: 3, // South
            cursor_x: 0.5,
            cursor_y: 1.0,
            cursor_z: 0.5,
            inside_block: false,
            world_border_hit: false,
            sequence: 7,
        });
        let decoded = proto.decode(State::Play, play::serverbound::USE_ITEM_ON, &body);
        assert_eq!(
            decoded,
            ServerBound::UseItemOn {
                pos: BlockPos::new(5, -10, -7),
                face: BlockFace::South,
                cursor: Vec3f {
                    x: 0.5,
                    y: 1.0,
                    z: 0.5,
                },
                sequence: 7,
                hand: 0,
            }
        );
    }

    /// The off-hand ordinal (`1`) must survive decode — this is the field that
    /// used to be read off the wire and then discarded, which is why off-hand
    /// placement was impossible: every `UseItemOn` reached the server-side
    /// model reporting main hand regardless of which hand the client used.
    #[test]
    fn decode_use_item_on_carries_the_off_hand_ordinal() {
        let proto = V770ServerProtocol;
        let body = encode(&UseItemOn {
            hand: 1,
            pos: pack_block_pos(1, 2, 3),
            face: 0,
            cursor_x: 0.25,
            cursor_y: 0.0,
            cursor_z: 0.75,
            inside_block: false,
            world_border_hit: false,
            sequence: 11,
        });
        let decoded = proto.decode(State::Play, play::serverbound::USE_ITEM_ON, &body);
        let ServerBound::UseItemOn { hand, .. } = decoded else {
            panic!("expected UseItemOn, got {decoded:?}");
        };
        assert_eq!(hand, 1);
    }

    /// Same malformed-input convention as `USE_ITEM`'s own hand field: `hand`
    /// is decoded with `u8::try_from(..).unwrap_or(0)`, which degrades to main
    /// hand rather than dropping the packet — but only for a wire value
    /// outside `u8`'s own range (`256` and up, or negative). `300` rather
    /// than, say, `99`: the latter fits in a `u8` and survives the conversion
    /// unclamped, which is worth recording rather than assuming — this
    /// decoder does not validate the ordinal is `0` or `1`, only that it fits
    /// in a byte, matching `USE_ITEM`'s own established (if narrower than it
    /// sounds) convention.
    #[test]
    fn decode_use_item_on_clamps_a_malformed_hand_ordinal_to_main() {
        let proto = V770ServerProtocol;
        let body = encode(&UseItemOn {
            hand: 300,
            pos: pack_block_pos(0, 0, 0),
            face: 0,
            cursor_x: 0.0,
            cursor_y: 0.0,
            cursor_z: 0.0,
            inside_block: false,
            world_border_hit: false,
            sequence: 0,
        });
        let decoded = proto.decode(State::Play, play::serverbound::USE_ITEM_ON, &body);
        let ServerBound::UseItemOn { hand, .. } = decoded else {
            panic!("expected UseItemOn, got {decoded:?}");
        };
        assert_eq!(hand, 0);
    }

    /// [`resolve_state_id`] round-trips the two propertyless states this
    /// crate ever writes back to themselves via [`block_name`].
    #[test]
    fn resolve_state_id_round_trips_stone_and_air() {
        assert_eq!(
            block_name(resolve_state_id("minecraft:stone")),
            Some("minecraft:stone")
        );
        assert_eq!(
            block_name(resolve_state_id("minecraft:air")),
            Some("minecraft:air")
        );
    }

    /// [`resolve_state_id`] must match on properties too, not just the block
    /// name — otherwise every propertied state of a block would resolve to
    /// whichever one happens to be first in the table. Picks a real
    /// propertied entry straight from the generated table rather than
    /// guessing a property string, so this cannot pass by coincidence.
    #[test]
    fn resolve_state_id_matches_properties_not_just_name() {
        let propertied_id = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| !properties(id).unwrap().is_empty())
            .expect("generated table has at least one propertied state");
        let name = block_name(propertied_id).unwrap();
        let props = properties(propertied_id).unwrap();
        let state_str = format!(
            "{name}[{}]",
            props
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(resolve_state_id(&state_str), propertied_id);
    }

    /// [`resolve_state_id`] falls back to air rather than panicking on a
    /// string the generated table has no match for.
    #[test]
    fn resolve_state_id_falls_back_to_air_on_no_match() {
        assert_eq!(resolve_state_id("minecraft:not_a_real_block"), air_id());
    }

    /// The regression this fix exists for: `lodestone-worldgen`'s
    /// `OverworldGenerator` writes its default fluid as the **bare** literal
    /// `"minecraft:water"`, with no `level` property
    /// (`crates/lodestone-worldgen/src/overworld.rs`'s `default_fluid`) — and
    /// real water has no propertyless state (every id in `86..=101` carries
    /// `level=0..15`). Before `resolve_state_id`'s same-name-default
    /// fallback tier existed, this fell all the way through to **air** —
    /// found by this crate's own hermetic
    /// `encode_chunk_carries_real_block_states_including_a_fluid` gate below,
    /// which failed its `assert_ne!(water_id, air_id())` sanity check the
    /// first time it ran, exactly the trap a
    /// solids-only fix falls into. Pins the exact expected id
    /// (`blocks.json`'s own `"default": true` entry for `minecraft:water` is
    /// `level=0`, id `86` — `.cache/mc/26.2/generated/reports/blocks.json`),
    /// not just "not air", so a future table regeneration that changes which
    /// state is default cannot silently regress this to a *different* wrong
    /// answer.
    #[test]
    fn resolve_state_id_resolves_bare_water_to_its_default_level_state() {
        let bare_water_id = resolve_state_id("minecraft:water");
        assert_ne!(
            bare_water_id,
            air_id(),
            "bare `minecraft:water` (no `level` property) must not resolve to air"
        );
        assert_eq!(
            properties(bare_water_id),
            Some([("level", "0")].as_slice()),
            "expected the default (level=0) water state"
        );
        assert_eq!(
            bare_water_id,
            resolve_state_id("minecraft:water[level=0]"),
            "the bare-name fallback must agree with the fully-qualified default state"
        );
    }

    /// A bare name must resolve to the block's **default** state,
    /// not to its lowest id — and `grass_block` is the case where those differ
    /// visibly: `blocks.json` marks `snowy=false` (id 9) default while id 8 is
    /// `snowy=true`, so the old lowest-id fallback put every spread grass block
    /// on the wire as snowy. `lodestone-data`'s jar-derived default-state column
    /// supplies the expected id; nothing here asks the resolver what it thinks
    /// the default is.
    ///
    /// Also pins the property-override tier on the same block, since a merge
    /// that silently ignored the caller's value would still pass the first half.
    #[test]
    fn resolve_state_id_resolves_a_bare_name_to_the_jar_marked_default_state() {
        let lowest = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| block_name(id) == Some("minecraft:grass_block"))
            .expect("no grass_block in the generated table");
        let jar_default = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| {
                block_name(id) == Some("minecraft:grass_block")
                    && lodestone_data::block_states::StateId::new(id)
                        .expect("generated state-table index is valid")
                        .is_default()
            })
            .expect("no default grass_block state in the jar-derived column");

        assert_ne!(jar_default, lowest, "grass_block's default is not its lowest id");
        assert_eq!(resolve_state_id("minecraft:grass_block"), jar_default);
        assert_eq!(properties(jar_default), Some([("snowy", "false")].as_slice()));
        assert_eq!(
            properties(resolve_state_id("minecraft:grass_block[snowy=true]")),
            Some([("snowy", "true")].as_slice())
        );
    }

    /// The hermetic half of the whole-column wire-fidelity gate: a whole-column `encode_chunk`
    /// send, decoded back through the real wire codec
    /// ([`crate::packets::chunk::LevelChunkWithLight::decode`], the same
    /// decoder `tests/join_flow.rs`'s golden vectors and `tests/live_chunk
    /// .rs`'s live capture pin), must carry the real per-block state rather
    /// than a collapsed solid/air pair — including a **fluid**, the case a
    /// fix that only thinks about solids is most likely to miss (the old
    /// collapse mapped every fluid to air, not stone, so a half-fix would
    /// still pass a solids-only check here).
    ///
    /// This round-trips through this crate's own encode/decode, which
    /// `CLAUDE.md` flags as weaker evidence than an independent oracle
    /// (`decode(encode(x)) == x` is satisfiable by two symmetric
    /// misunderstandings) — the real-client gate in
    /// `tests/block_edit.rs`'s `dig_and_place_persist_through_forget_and
    /// _reload` is the honest one, checking a real `lodestone-client`'s
    /// `block_at` against an independent generator instance. This test is
    /// the fast, hermetic complement: no client/server machinery, so it
    /// pins the exact ids a regression would have to break.
    #[test]
    fn encode_chunk_carries_real_block_states_including_a_fluid() {
        use crate::packets::chunk::LevelChunkWithLight;
        use lodestone_server::{ChunkSource, overworld_chunk_source};

        // Same fixture `tests/block_edit.rs` uses (seed 1234, chunk (0, 0)):
        // real per-block content sampled from an *independent* generator
        // instance, per `CLAUDE.md`'s "an expected value must originate
        // outside the code under test" — this crate's own `resolve_state_id`
        // resolves the id, but the state *strings* being asserted come from
        // nothing this test constructs by hand.
        let seed: i64 = 1234;
        let independent_generator = lodestone_server::overworld_generator(seed);
        let real_column = independent_generator.column(0, 0);
        let deepslate_state = real_column.block_state(0, -50, 0);
        let gravel_state = real_column.block_state(0, 37, 0);
        let water_state = real_column.block_state(0, 38, 0);
        assert_eq!(deepslate_state.split('[').next(), Some("minecraft:deepslate"));
        assert_eq!(gravel_state, "minecraft:gravel");
        assert_eq!(water_state.split('[').next(), Some("minecraft:water"));

        let deepslate_id = resolve_state_id(deepslate_state);
        let gravel_id = resolve_state_id(gravel_state);
        let water_id = resolve_state_id(water_state);
        assert_ne!(
            water_id,
            air_id(),
            "fixture sanity: the real water state must not itself resolve to air"
        );

        // The column actually served, from a second, separately-constructed
        // source — proving `encode_chunk` (not this test) is what produces
        // the fidelity, the same source `V770ServerProtocol` would be given
        // in the live server.
        let source = overworld_chunk_source(seed);
        let served_column = source.column(0, 0);

        let proto = V770ServerProtocol;
        // Named through the trait: `V770ServerProtocol` implements both
        // `ServerProtocol` and `ChunkEncoder`, whose `encode_chunk` methods are
        // deliberately the same body (see the `ChunkEncoder` impl), so an
        // unqualified call is ambiguous rather than wrong.
        let directive = ServerProtocol::encode_chunk(&proto, 0, 0, &served_column);
        let payload = match directive {
            ServerDirective::Send { payload, .. } => payload,
            other => panic!("expected Send, got {other:?}"),
        };

        let shape = ChunkShape::overworld_1_21();
        let mut r = Reader::new(&payload);
        let decoded = LevelChunkWithLight::decode(&mut r, &shape).expect("decode column");
        r.ensure_empty().expect("no trailing bytes");

        assert_eq!(decoded.column.get_block(0, -50, 0), deepslate_id);
        assert_eq!(decoded.column.get_block(0, 37, 0), gravel_id);
        assert_eq!(
            decoded.column.get_block(0, 38, 0),
            water_id,
            "fluid cell must carry the real water id on the wire, not collapse to air"
        );

        // An untouched, definitely-air cell (well above this column's
        // terrain) still reads as air — the fix does not smear a stray
        // non-air write across cells the source itself reports as air.
        assert_eq!(decoded.column.get_block(5, 300, 5), air_id());
    }

    /// The wire half of server-side light: a served column carries the generator's real
    /// `MOTION_BLOCKING` map, not the zero-entry NBT this encoder sent for every
    /// column until now. The expected values come from the **generator's own**
    /// array through a second, independently constructed source — nothing here
    /// re-derives a height.
    #[test]
    fn a_served_column_carries_the_generators_motion_blocking_heightmap() {
        use crate::packets::chunk::LevelChunkWithLight;
        use lodestone_server::{ChunkSource, overworld_chunk_source};

        let seed: i64 = 1234;
        let expected = *lodestone_server::overworld_generator(seed)
            .column(0, 0)
            .motion_blocking_heightmap()
            .expect("the bundled generator computes MOTION_BLOCKING");

        let source = overworld_chunk_source(seed);
        let directive =
            ServerProtocol::encode_chunk(&V770ServerProtocol, 0, 0, &source.column(0, 0));
        let payload = match directive {
            ServerDirective::Send { payload, .. } => payload,
            other => panic!("expected Send, got {other:?}"),
        };

        let shape = ChunkShape::overworld_1_21();
        let mut r = Reader::new(&payload);
        let decoded = LevelChunkWithLight::decode(&mut r, &shape).expect("decode column");
        r.ensure_empty().expect("no trailing bytes");

        let map = decoded
            .heightmaps
            .get(MOTION_BLOCKING_HEIGHTMAP_TYPE_ID)
            .expect("MOTION_BLOCKING must be on the wire");
        for lz in 0..16usize {
            for lx in 0..16usize {
                assert_eq!(
                    map.get(lx, lz),
                    u32::from(expected[lx + lz * 16]),
                    "at ({lx}, {lz})"
                );
            }
        }
        // Non-degenerate: an all-zero map is what an empty `Heightmaps` would
        // decode to under a bug that framed 256 entries of nothing, so the
        // element-wise check above must be comparing real heights. (Chunk (0, 0)
        // at this seed is an ocean surface, so the values are *uniform* — a
        // variance assertion here would be false, not stronger.)
        assert!(expected.iter().all(|&h| h > 0), "{expected:?}");

        // An all-air column has no generated map at all, and still frames a
        // valid zero-entry NBT.
        let empty = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
        let directive = ServerProtocol::encode_chunk(&V770ServerProtocol, 0, 0, &empty);
        let payload = match directive {
            ServerDirective::Send { payload, .. } => payload,
            other => panic!("expected Send, got {other:?}"),
        };
        let mut r = Reader::new(&payload);
        let decoded = LevelChunkWithLight::decode(&mut r, &shape).expect("decode empty column");
        r.ensure_empty().expect("no trailing bytes");
        assert!(decoded.heightmaps.is_empty());
    }

    /// The island check for real per-quart biome assignment: it must
    /// reach the **encoded wire bytes**, not just [`ServerChunkColumn`] —
    /// the exact chain CLAUDE.md's rule 1 asks for (climate sample -> biome
    /// -> the column the encoder sends -> the actual bytes a client would
    /// decode). Chunk (0, 0) at seed 42 is the same fixture
    /// `biome_matches_vanilla_at_known_coordinates_seed_42`
    /// (`lodestone-server::worldgen_data`) proves against the JVM: quart
    /// (0, 0) is `dark_forest`, quart (2, 2) is `river` — two *different*
    /// biomes in the same chunk, so this also proves the wire encoder
    /// doesn't collapse a chunk to one id the way it used to have to (no
    /// per-quart biome existed before this issue).
    #[test]
    fn encode_chunk_carries_real_per_quart_biome() {
        use crate::packets::chunk::LevelChunkWithLight;
        use lodestone_server::{ChunkSource, overworld_chunk_source};

        let seed: i64 = 42;
        let source = overworld_chunk_source(seed);
        let served_column = source.column(0, 0);
        assert_eq!(served_column.biome_state(0, 0), "minecraft:dark_forest");
        assert_eq!(served_column.biome_state(8, 8), "minecraft:river");
        let dark_forest_id = resolve_biome_id("minecraft:dark_forest");
        let river_id = resolve_biome_id("minecraft:river");
        assert_ne!(
            dark_forest_id, river_id,
            "fixture sanity: the two biomes must resolve to different wire ids"
        );

        let proto = V770ServerProtocol;
        // Named through the trait: `V770ServerProtocol` implements both
        // `ServerProtocol` and `ChunkEncoder`, whose `encode_chunk` methods are
        // deliberately the same body (see the `ChunkEncoder` impl), so an
        // unqualified call is ambiguous rather than wrong.
        let directive = ServerProtocol::encode_chunk(&proto, 0, 0, &served_column);
        let payload = match directive {
            ServerDirective::Send { payload, .. } => payload,
            other => panic!("expected Send, got {other:?}"),
        };

        let shape = ChunkShape::overworld_1_21();
        let mut r = Reader::new(&payload);
        let decoded = LevelChunkWithLight::decode(&mut r, &shape).expect("decode column");
        r.ensure_empty().expect("no trailing bytes");

        // `lodestone_world::ChunkColumn::get_biome`'s `x`/`z` are in-chunk
        // **biome cells** (`0..4`, quart resolution), not block coordinates
        // — world block (8, 8) is biome cell (2, 2) (`8 >> 2`). World y=70
        // lands well inside this column's generated terrain range for both
        // probes, and biome is constant across y for a given cell per this
        // port's Phase 1 scope, so the exact y does not matter here.
        assert_eq!(
            decoded.column.get_biome(0, 70, 0),
            dark_forest_id,
            "quart (0,0) must carry dark_forest's real wire id, not a constant default"
        );
        assert_eq!(
            decoded.column.get_biome(2, 70, 2),
            river_id,
            "quart (2,2) must carry river's real wire id, distinct from quart (0,0)'s"
        );
    }

    /// Pins `encode_block_update`'s wire layout end to end: packed `BlockPos`
    /// then a VarInt state id, nothing else — the shape vanilla's own
    /// clientbound block-update packet's own stream codec specifies
    /// (confirmed against the decompiled 26.2 source).
    #[test]
    fn encode_block_update_wire_layout() {
        let proto = V770ServerProtocol;
        let directive = proto.encode_block_update(1, 2, 3, "minecraft:stone");
        match directive {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, play::clientbound::BLOCK_UPDATE);
                let mut r = Reader::new(&payload);
                let packed = r.i64().expect("packed pos");
                assert_eq!(packed, pack_block_pos(1, 2, 3));
                let id = r.var_i32().expect("state id");
                assert_eq!(id as u32, stone_id());
                r.ensure_empty().expect("no trailing bytes");
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }

    /// A moving piston reaches the wire as **two** packets, and the whole point of
    /// this gate is the pair: a `block_update` establishing the `moving_piston`
    /// state and a `block_entity_data` carrying the record that says which block is
    /// travelling. Either alone draws nothing.
    ///
    /// Decoded with the exact sequence `V770Adapter`'s own `BLOCK_ENTITY_DATA` arm
    /// uses — packed i64, VarInt type id, network NBT, `ensure_empty` — so the
    /// expectation for the *layout* comes from the reader that has to consume real
    /// server bytes, not from this encoder. The record's field names and tag types
    /// come from vanilla's own piston moving block-entity class's own save additional and are gated in
    /// `lodestone_server::block_entities`.
    ///
    /// The two packets are asserted in the order the server's own drain emits them
    /// (block-change lane first, effect lane second): reversed, a client applies a
    /// record to a cell whose state is still the *old* block, and
    /// `sync_block_entity` may discard it as a type mismatch.
    #[test]
    fn a_moving_piston_reaches_the_wire_as_a_state_then_a_record() {
        use lodestone_server::piston::{Direction, MovingBlockEntity, moving_piston_state};

        let proto = V770ServerProtocol;
        let entity = MovingBlockEntity {
            moved_state: "minecraft:piston_head[facing=east,short=false,type=sticky]".to_string(),
            direction: Direction::East,
            extending: true,
            source: true,
        };
        let pos = lodestone_model::BlockPos::new(11, 64, -4);

        // 1. The state. A `moving_piston` must resolve to a real 26.2 state id —
        // a fallback to the default would silently animate the wrong facing.
        let moving = moving_piston_state(Direction::East, true);
        let state_directive = proto.encode_block_update(pos.x, pos.y, pos.z, &moving);
        let ServerDirective::Send {
            packet_id: state_id_packet,
            payload: state_payload,
        } = state_directive
        else {
            panic!("the moving_piston state must reach the wire");
        };
        assert_eq!(state_id_packet, play::clientbound::BLOCK_UPDATE);
        let mut r = Reader::new(&state_payload);
        assert_eq!(r.i64().expect("packed pos"), pack_block_pos(pos.x, pos.y, pos.z));
        let wire_state = r.var_i32().expect("state id") as u32;
        r.ensure_empty().expect("no trailing bytes");
        assert_eq!(
            lodestone_data::block_states::state_id(&moving),
            Some(wire_state),
            "the moving_piston state must resolve exactly, not fall back to a default"
        );

        // 2. The record, through the same dispatch a world tick uses — so this also
        // proves `encode_world_effect` routes the new variant instead of dropping it.
        let effect = lodestone_server::effects::WorldEffect::BlockEntityData {
            pos,
            block_entity_type: lodestone_server::piston::PISTON_BLOCK_ENTITY.to_string(),
            nbt: entity.update_tag(),
        };
        let ServerDirective::Send {
            packet_id: record_packet,
            payload: record_payload,
        } = proto.encode_world_effect(&effect)
        else {
            panic!("the moving piston record must reach the wire");
        };
        assert_eq!(record_packet, play::clientbound::BLOCK_ENTITY_DATA);

        let mut r = Reader::new(&record_payload);
        let packed = r.i64().expect("packed pos");
        assert_eq!(unpack_block_pos(packed), pos);
        let type_id = r.var_i32().expect("type id") as u32;
        assert_eq!(
            lodestone_data::block_entity_types::block_entity_type_name(
                lodestone_data::block_entity_types::BlockEntityType::new(type_id)
                    .expect("wire type validates"),
            ),
            "minecraft:piston",
            "the block's key is `moving_piston` and its entity's key is `piston`; \
             sending the block's would resolve to some other entity"
        );
        let nbt = lodestone_core::read_network_nbt(&mut r).expect("network nbt");
        r.ensure_empty().expect("no trailing bytes");

        let lodestone_core::Nbt::Compound(fields) = &nbt else {
            panic!("the record must be a compound");
        };
        let field = |key: &str| {
            fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
                .unwrap_or(lodestone_core::Nbt::End)
        };
        assert_eq!(
            field("facing"),
            lodestone_core::Nbt::Byte(5),
            "`facing` must survive the NBT round trip as a Byte — as an Int a client \
             reads it as absent and animates toward DOWN"
        );
        assert_eq!(field("progress"), lodestone_core::Nbt::Float(0.0));
        assert_eq!(field("extending"), lodestone_core::Nbt::Byte(1));
        assert_eq!(field("source"), lodestone_core::Nbt::Byte(1));

        // 3. And the two together resolve to the head a client draws: the record's
        // own `blockState` must be a real state id, or `PistonHeadRenderer`'s first
        // arm never fires and nothing is drawn at all.
        assert!(
            lodestone_data::block_states::state_id(&entity.moved_state).is_some(),
            "the travelling head state must resolve in the 26.2 table"
        );

        // Control: a type key this version does not have must emit nothing rather
        // than a packet carrying a made-up registry id.
        assert!(matches!(
            proto.encode_block_entity_data(pos, "minecraft:not_a_block_entity", &nbt),
            ServerDirective::None
        ));
    }

    /// Pins `encode_game_event`'s wire layout end to end: one unsigned byte
    /// event id, then a big-endian `f32` param, nothing else — the shape
    /// vanilla's own clientbound game-event packet writes (confirmed against
    /// the decompiled 26.2 source)
    /// and the shape this crate's own `GameEvent` decode reads back. The param
    /// is pinned to a non-integral value (`0.5`) so a big-endian `f32` that
    /// somehow slid a byte cannot alias the integer `0`.
    #[test]
    fn encode_game_event_wire_layout() {
        let proto = V770ServerProtocol;
        let directive = proto.encode_game_event(7, 0.5);
        match directive {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, play::clientbound::GAME_EVENT);
                let mut r = Reader::new(&payload);
                assert_eq!(r.u8().expect("event id"), 7);
                assert_eq!(r.f32().expect("param"), 0.5);
                r.ensure_empty().expect("no trailing bytes");
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }
}

/// `CHANGE_DIFFICULTY`/`LOCK_DIFFICULTY`/`SET_GAME_RULE` decode
/// and their two confirmation encoders. Expected values come from
/// `.cache/mc/26.2/src`'s own record types
/// (`ServerboundChangeDifficultyPacket`, `ServerboundLockDifficultyPacket`,
/// `ServerboundSetGameRulePacket`, `ClientboundChangeDifficultyPacket`), not
/// from this module's own encoder — each decode test hand-builds wire bytes
/// with the *encode* side of the same struct (a real, if self-authored,
/// round trip through the derive macro) and each encode test independently
/// re-parses the produced bytes field by field instead of comparing structs,
/// so a decode bug and its mirror-image encode bug cannot cancel out.
#[cfg(test)]
mod world_admin_tests {
    use super::*;
    use lodestone_core::State;
    use lodestone_model::Difficulty;

    fn encode<T: Encode>(packet: &T) -> Vec<u8> {
        let mut w = Writer::default();
        packet.encode(&mut w, CTX).expect("well-formed struct encodes");
        w.into_vec()
    }

    #[test]
    fn decode_change_difficulty() {
        let proto = V770ServerProtocol;
        let body = encode(&ChangeDifficultyServerbound { difficulty: 3 });
        let decoded = proto.decode(State::Play, play::serverbound::CHANGE_DIFFICULTY, &body);
        assert_eq!(
            decoded,
            ServerBound::DifficultyChanged {
                difficulty: Difficulty::Hard
            }
        );
    }

    /// Control for [`decode_change_difficulty`]: an ordinal outside `0..=3`
    /// must drop the packet (`ServerBound::Ignored`), not alias to some other
    /// difficulty — see [`difficulty_from_ordinal`]'s own doc comment for why
    /// this departs from vanilla's `WRAP` strategy.
    #[test]
    fn decode_change_difficulty_rejects_out_of_range_ordinal() {
        let proto = V770ServerProtocol;
        let body = encode(&ChangeDifficultyServerbound { difficulty: 9 });
        let decoded = proto.decode(State::Play, play::serverbound::CHANGE_DIFFICULTY, &body);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    #[test]
    fn decode_lock_difficulty() {
        let proto = V770ServerProtocol;
        let body = encode(&LockDifficulty { locked: true });
        let decoded = proto.decode(State::Play, play::serverbound::LOCK_DIFFICULTY, &body);
        assert_eq!(decoded, ServerBound::DifficultyLockChanged { locked: true });
    }

    #[test]
    fn decode_set_game_rule() {
        let proto = V770ServerProtocol;
        let body = encode(&SetGameRule {
            entries: vec![
                GameRuleEntry {
                    key: "minecraft:doDaylightCycle".to_string(),
                    value: "false".to_string(),
                },
                GameRuleEntry {
                    key: "minecraft:randomTickSpeed".to_string(),
                    value: "0".to_string(),
                },
            ],
        });
        let decoded = proto.decode(State::Play, play::serverbound::SET_GAME_RULE, &body);
        assert_eq!(
            decoded,
            ServerBound::GameRuleChanged {
                entries: vec![
                    (
                        "minecraft:doDaylightCycle".to_string(),
                        "false".to_string()
                    ),
                    ("minecraft:randomTickSpeed".to_string(), "0".to_string()),
                ]
            }
        );
    }

    /// Pins `encode_change_difficulty`'s wire layout: VarInt difficulty
    /// ordinal, then a bool locked flag, nothing else
    /// (vanilla's own clientbound change-difficulty packet's own stream codec).
    #[test]
    fn encode_change_difficulty_wire_layout() {
        let proto = V770ServerProtocol;
        let directive = proto.encode_change_difficulty(Difficulty::Hard, true);
        match directive {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, play::clientbound::CHANGE_DIFFICULTY);
                let mut r = Reader::new(&payload);
                assert_eq!(r.var_i32().expect("difficulty"), 3);
                assert!(r.bool().expect("locked"));
                r.ensure_empty().expect("no trailing bytes");
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }

    /// Pins `encode_game_rule_values`'s wire layout: a VarInt-prefixed list
    /// of (string key, string value) pairs, in the order given — and that it
    /// carries exactly the entries passed in, not some full default table
    /// (this crate has none to send).
    #[test]
    fn encode_game_rule_values_wire_layout() {
        let proto = V770ServerProtocol;
        let entries = vec![("minecraft:doDaylightCycle".to_string(), "false".to_string())];
        let directive = proto.encode_game_rule_values(&entries);
        match directive {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, play::clientbound::GAME_RULE_VALUES);
                let decoded = decode_full::<GameRuleValues>(&payload)
                    .expect("well-formed GameRuleValues decodes");
                assert_eq!(decoded.entries.len(), 1);
                assert_eq!(decoded.entries[0].key, "minecraft:doDaylightCycle");
                assert_eq!(decoded.entries[0].value, "false");
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }
}

/// `CHAT_COMMAND_SIGNED` — see the decode arm's own comment for why this
/// routes through the same `ServerBound::ChatCommand` consumer as the plain
/// `CHAT_COMMAND` rather than a dedicated variant.
#[cfg(test)]
mod chat_command_signed_tests {
    use super::*;
    use lodestone_core::State;

    fn encode<T: Encode>(packet: &T) -> Vec<u8> {
        let mut w = Writer::default();
        packet.encode(&mut w, CTX).expect("well-formed struct encodes");
        w.into_vec()
    }

    /// Pairwise-distinct fixture: `command`, `timestamp` and `salt` are each
    /// individually distinguishable, and a non-empty `argument_signatures`
    /// list plus a non-zero `last_seen_offset`/`acknowledged`/`checksum` tail
    /// are present precisely so a decoder that stops early (or misreads the
    /// frame length) fails loudly rather than by coincidence.
    #[test]
    fn decode_chat_command_signed_runs_the_same_command_as_the_unsigned_form() {
        let proto = V770ServerProtocol;
        let mut sig_bytes = [0u8; 256];
        for (i, b) in sig_bytes.iter_mut().enumerate() {
            *b = i as u8;
        }
        let body = encode(&ChatCommandSigned {
            command: "gamemode creative Notch".to_owned(),
            timestamp: 1_700_000_000_123,
            salt: 42,
            argument_signatures: vec![crate::packets::game::ArgumentSignatureEntry {
                name: "player".to_owned(),
                signature: crate::packets::game::MessageSignature(sig_bytes),
            }],
            last_seen_offset: 3,
            acknowledged: [0b0000_0001, 0, 0],
            checksum: 7,
        });
        let decoded = proto.decode(State::Play, play::serverbound::CHAT_COMMAND_SIGNED, &body);
        assert_eq!(
            decoded,
            ServerBound::ChatCommand {
                command: "gamemode creative Notch".to_owned(),
            }
        );
    }

    /// Control: a truncated frame (missing the acknowledgement tail) must
    /// drop to `Ignored`, not silently accept a shorter-than-declared packet.
    #[test]
    fn decode_chat_command_signed_rejects_a_truncated_frame() {
        let proto = V770ServerProtocol;
        let mut body = encode(&ChatCommandSigned {
            command: "help".to_owned(),
            timestamp: 1,
            salt: 2,
            argument_signatures: vec![],
            last_seen_offset: 0,
            acknowledged: [0, 0, 0],
            checksum: 0,
        });
        body.truncate(body.len() - 1);
        let decoded = proto.decode(State::Play, play::serverbound::CHAT_COMMAND_SIGNED, &body);
        assert_eq!(decoded, ServerBound::Ignored);
    }
}

/// Server-authoritative inventory: `SET_CARRIED_ITEM`/`CONTAINER_CLICK`
/// decode. Where possible the expected wire bytes come from the **real**
/// client-side encoder (`crate::adapter`'s `V770Adapter::encode_action`),
/// not a hand-authored fixture — this is the same "real client already sends
/// this packet in ordinary singleplayer play" encoder a prior investigation
/// found already existed with zero server-side consumer, so decoding what it
/// actually produces (rather than a bespoke test-only byte layout) is the
/// strongest hermetic evidence available that this module's decoder agrees
/// with production. The two malformed-input controls below (nonzero
/// component-patch counts, an out-of-range hotbar slot) *are* hand-built,
/// deliberately, because the real encoder can never produce them — they are
/// the negative-control class CLAUDE.md's evidence standard asks for, run
/// and watched failing (`ServerBound::Ignored`, not a panic or a
/// misdecoded slot).
#[cfg(test)]
mod inventory_decode_tests {
    use super::*;
    use lodestone_core::State;
    use lodestone_model::{
        ClientAction, ConnectionState, ContainerClickType, ContainerSlotChange, VersionAdapter,
    };

    fn encode<T: Encode>(packet: &T) -> Vec<u8> {
        let mut w = Writer::default();
        packet.encode(&mut w, CTX).expect("well-formed struct encodes");
        w.into_vec()
    }

    fn stack(name: &str, count: u32) -> ItemStack {
        ItemStack::new(name.parse().expect("valid resource key"), count)
    }

    /// The real client's `SetCarriedItem` encoder, decoded back into
    /// [`ServerBound::CarriedItemChanged`].
    #[test]
    fn decode_set_carried_item_from_real_client_encoder() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = crate::adapter()
            .encode_action(ConnectionState::Play, &ClientAction::SetCarriedItem { slot: 4 })
            .expect("encodes")
            .expect("SetCarriedItem always encodes in Play");
        assert_eq!(packet_id, play::serverbound::SET_CARRIED_ITEM);
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(decoded, ServerBound::CarriedItemChanged { slot: 4 });
    }

    /// Control: a slot outside `0..HOTBAR_SIZE` must drop the packet
    /// (`ServerBound::Ignored`), never alias into some other hotbar slot —
    /// mirrors `decode_change_difficulty_rejects_out_of_range_ordinal`'s
    /// pattern above. The real client encoder can never produce this (it
    /// only ever selects a real hotbar key), so this is hand-built directly
    /// against the wire struct.
    #[test]
    fn decode_set_carried_item_rejects_out_of_range_slot() {
        let proto = V770ServerProtocol;
        let body = encode(&SetCarriedItem { slot: 9 });
        let decoded = proto.decode(State::Play, play::serverbound::SET_CARRIED_ITEM, &body);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    // ---- `SET_CREATIVE_MODE_SLOT` / `CLIENT_COMMAND`, the two variants that
    // had consumers but no constructor.
    //
    // Every byte below is hand-derived from sources **outside this crate**, so
    // no `decode(encode(x))` symmetry can satisfy them:
    //
    // - Vanilla's own serverbound set-creative-mode-slot packet's composite
    //   stream codec:
    //   its own fixed-width `SHORT` codec then its own untrusted-optional
    //   item-stack stream codec.
    //   The `SHORT` codec is a plain big-endian `i16` write.
    // - Vanilla's own serverbound client-command packet's whole body is one
    //   plain enum-ordinal write, i.e. a VarInt of the ordinal, over
    //   `Action { PERFORM_RESPAWN, REQUEST_STATS, REQUEST_GAMERULE_VALUES }`.
    // - `minecraft:cobblestone`'s item protocol id `62` is read from Mojang's
    //   own `generated/reports/registries.json`, the authoritative generator
    //   output — not from our registry tables.
    // - The menu-slot number `36` is vanilla's own inventory-menu's first
    //   hotbar slot,
    //   which vanilla's own server-side set-creative-mode-slot handler
    //   accepts as a valid slot (`1..=45`) and writes via that menu's own
    //   slot lookup.

    /// A creative-mode palette write of a full stack into the first hotbar
    /// slot, decoded from bytes laid out by hand against vanilla's own
    /// `STREAM_CODEC` (see the block comment above for every byte's source).
    ///
    /// This arm returned [`ServerBound::Ignored`] until a wiring
    /// pass fixed it, while `apply_creative_mode_slot_set` and
    /// `ServerBound::CreativeModeSlotSet` had both already existed since
    /// `c4ad474` — so a real client's entire creative inventory was silently
    /// discarded. `tests/serverbound_wiring.rs` now gates that class
    /// structurally; this gates the wire layout.
    #[test]
    fn decode_set_creative_mode_slot_from_hand_built_vanilla_bytes() {
        let proto = V770ServerProtocol;
        let body = [
            0x00, 0x24, // vanilla's own fixed-width SHORT codec: big-endian i16 36
            0x40, // optional-stack count, VarInt 64
            0x3E, // item id, VarInt 62 = minecraft:cobblestone
            0x00, // added components, VarInt 0
            0x00, // removed components, VarInt 0
        ];
        let decoded = proto.decode(State::Play, play::serverbound::SET_CREATIVE_MODE_SLOT, &body);
        assert_eq!(
            decoded,
            ServerBound::CreativeModeSlotSet {
                slot: 36,
                item: Some(stack("minecraft:cobblestone", 64)),
            }
        );
    }

    /// The clear-a-slot case: vanilla's own item-stack type's own create optional stream codec uses a
    /// `count` of zero as the absence marker rather than a leading presence
    /// bool (see [`read_optional_item_stack`]'s doc comment), so an empty
    /// write is three bytes with no item id at all. A decoder that expected a
    /// bool prefix here would read the `0x00` count as "absent" and then
    /// choke on `ensure_empty`, or read a spurious id — this pins the real
    /// shape.
    #[test]
    fn decode_set_creative_mode_slot_clears_a_slot_with_a_zero_count() {
        let proto = V770ServerProtocol;
        let body = [0x00, 0x2D, 0x00]; // slot 45 (off-hand), count 0 = empty
        let decoded = proto.decode(State::Play, play::serverbound::SET_CREATIVE_MODE_SLOT, &body);
        assert_eq!(decoded, ServerBound::CreativeModeSlotSet { slot: 45, item: None });
    }

    /// Vanilla's `slotNum() < 0` "drop into the world" case, which this crate
    /// has no model for. The variant must still carry the raw negative slot
    /// rather than the decoder swallowing the packet, because the decision to
    /// drop it belongs to the consumer — `apply_creative_mode_slot_set`
    /// no-ops on any slot `player_menu_native_index` does not recognise, and
    /// its doc comment says so.
    ///
    /// `-1` as big-endian `i16` is `0xFF 0xFF`.
    #[test]
    fn decode_set_creative_mode_slot_preserves_vanillas_negative_drop_slot() {
        let proto = V770ServerProtocol;
        let body = [0xFF, 0xFF, 0x01, 0x3E, 0x00, 0x00];
        let decoded = proto.decode(State::Play, play::serverbound::SET_CREATIVE_MODE_SLOT, &body);
        assert_eq!(
            decoded,
            ServerBound::CreativeModeSlotSet {
                slot: -1,
                item: Some(stack("minecraft:cobblestone", 1)),
            }
        );
    }

    /// Control for the three gates above: the detector must reject a payload
    /// with a trailing byte rather than accepting a prefix.
    ///
    /// Without this, a decoder that stopped reading early would satisfy every
    /// positive assertion above while misaligning any future field, and the
    /// `ensure_empty` in the arm would be unproven. Observed to fail when
    /// `ensure_empty` is removed from the arm.
    #[test]
    fn decode_set_creative_mode_slot_rejects_a_trailing_byte() {
        let proto = V770ServerProtocol;
        let body = [0x00, 0x24, 0x40, 0x3E, 0x00, 0x00, 0x99];
        let decoded = proto.decode(State::Play, play::serverbound::SET_CREATIVE_MODE_SLOT, &body);
        assert_eq!(
            decoded,
            ServerBound::Ignored,
            "a trailing byte must fail the whole decode; if this passes, the positive gates \
             above prove nothing about field alignment"
        );
    }

    /// `PERFORM_RESPAWN`, ordinal `0` — the packet a real client sends when
    /// the player clicks **Respawn** on the death screen.
    ///
    /// This arm returned [`ServerBound::Ignored`] until a wiring
    /// pass fixed it, while `apply_client_command`'s respawn path already existed, so
    /// the button did nothing on a `lodestone` server. That is not a cosmetic
    /// gap: per `CLAUDE.md`'s live-server hazards a dead player is held on the
    /// death screen and is sent **no chunks**, so the connection became a
    /// permanent silent chunk blackout with keep-alives still flowing.
    #[test]
    fn decode_client_command_perform_respawn_from_hand_built_vanilla_bytes() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::CLIENT_COMMAND, &[0x00]);
        assert_eq!(decoded, ServerBound::ClientCommand { action: 0 });
    }

    /// `REQUEST_GAMERULE_VALUES`, ordinal `2`. The ordinal is passed through
    /// unmapped, so this also proves the arm does not collapse distinct
    /// actions onto one value — a decoder that hardcoded `action: 0` would
    /// pass the respawn gate above and fail here.
    #[test]
    fn decode_client_command_distinguishes_the_gamerule_request_ordinal() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::CLIENT_COMMAND, &[0x02]);
        assert_eq!(decoded, ServerBound::ClientCommand { action: 2 });
        // `REQUEST_STATS`, ordinal 1 — decoded and passed through even though
        // the consumer documents it as a no-op (no stats model in this crate).
        let decoded = proto.decode(State::Play, play::serverbound::CLIENT_COMMAND, &[0x01]);
        assert_eq!(decoded, ServerBound::ClientCommand { action: 1 });
    }

    /// Control: an empty `client_command` body carries no ordinal at all and
    /// must not decode to a plausible-looking `action: 0`, which would make
    /// the respawn gate above satisfiable by a decoder that read nothing.
    #[test]
    fn decode_client_command_rejects_an_empty_body() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::CLIENT_COMMAND, &[]);
        assert_eq!(
            decoded,
            ServerBound::Ignored,
            "an empty body must not produce `action: 0`; if it does, the respawn gate is \
             satisfied by a decoder that never reads the wire"
        );
    }

    /// The real client's `ContainerClick` encoder (a hotbar-swap style
    /// click predicting one changed slot and an empty cursor), decoded back
    /// into [`ServerBound::ContainerClicked`] — the packet's changed-slots
    /// map, which is what this crate's consumer actually applies, survives
    /// the round trip through the real production wire layout.
    #[test]
    fn decode_container_click_from_real_client_encoder() {
        let proto = V770ServerProtocol;
        let action = ClientAction::ContainerClick {
            window_id: 0,
            state_id: 7,
            slot: 36,
            button: 0,
            click_type: ContainerClickType::Pickup,
            changed_slots: vec![ContainerSlotChange {
                slot: 36,
                item: Some(stack("minecraft:diamond_pickaxe", 1)),
            }],
            carried_item: None,
        };
        let (packet_id, payload) = crate::adapter()
            .encode_action(ConnectionState::Play, &action)
            .expect("encodes")
            .expect("ContainerClick always encodes in Play");
        assert_eq!(packet_id, play::serverbound::CONTAINER_CLICK);
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(
            decoded,
            ServerBound::ContainerClicked {
                window_id: 0,
                state_id: 7,
                slot: 36,
                button: 0,
                click_type: 0,
                changed_slots: vec![(36, Some(stack("minecraft:diamond_pickaxe", 1)))],
                carried_item: None,
            }
        );
    }

    /// The same real-encoder round trip, but with a non-empty carried
    /// (cursor) stack and two changed slots — proves the loop over multiple
    /// entries and the trailing carried-item read both land correctly, not
    /// just the single-entry case above.
    #[test]
    fn decode_container_click_carries_cursor_stack_and_multiple_changes() {
        let proto = V770ServerProtocol;
        let action = ClientAction::ContainerClick {
            window_id: 0,
            state_id: 12,
            slot: 9,
            button: 0,
            click_type: ContainerClickType::Swap,
            changed_slots: vec![
                ContainerSlotChange {
                    slot: 9,
                    item: Some(stack("minecraft:cobblestone", 32)),
                },
                ContainerSlotChange { slot: 40, item: None },
            ],
            carried_item: Some(stack("minecraft:torch", 16)),
        };
        let (packet_id, payload) = crate::adapter()
            .encode_action(ConnectionState::Play, &action)
            .expect("encodes")
            .expect("ContainerClick always encodes in Play");
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(
            decoded,
            ServerBound::ContainerClicked {
                window_id: 0,
                state_id: 12,
                slot: 9,
                // `ContainerClickType::Swap` is ordinal 2 — the whole point of
                // carrying these three now, so they are asserted rather than
                // wildcarded.
                button: 0,
                click_type: 2,
                changed_slots: vec![
                    (9, Some(stack("minecraft:cobblestone", 32))),
                    (40, None),
                ],
                carried_item: Some(stack("minecraft:torch", 16)),
            }
        );
    }

    /// Control: a `HashedStack` carrying a nonzero added-component count is
    /// something the real client encoder can never produce (it always
    /// writes `0`/`0`, `write_hashed_stack`'s own doc comment), so this is
    /// hand-built directly against the documented wire layout — a container
    /// id, state id, slot, button, click type, one changed-slot entry whose
    /// item claims one added component. [`read_hashed_stack`]'s guard must
    /// fail the *whole* decode rather than silently misalign the reader on
    /// the (nonexistent, in this crate) per-component bytes that would
    /// follow.
    #[test]
    fn decode_container_click_rejects_nonzero_component_patch() {
        let proto = V770ServerProtocol;
        let mut w = Writer::default();
        w.var_i32(0); // window id
        w.var_i32(0); // state id
        w.i16(0); // slot
        w.i8(0); // button
        w.var_i32(0); // click type: pickup
        w.var_i32(1); // one changed slot
        w.i16(9); // slot 9
        w.bool(true); // present
        w.var_i32(0); // item id 0 (whatever it resolves to; irrelevant, decode must fail first)
        w.var_i32(1); // count
        w.var_i32(1); // added components: nonzero
        w.var_i32(0); // removed components
        w.bool(false); // carried item: empty
        let body = w.into_vec();
        let decoded = proto.decode(State::Play, play::serverbound::CONTAINER_CLICK, &body);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    /// Control: a truncated payload (claims one changed slot but supplies no
    /// bytes for it) must drop the packet, not panic or read garbage.
    #[test]
    fn decode_container_click_rejects_truncated_payload() {
        let proto = V770ServerProtocol;
        let mut w = Writer::default();
        w.var_i32(0);
        w.var_i32(0);
        w.i16(0);
        w.i8(0);
        w.var_i32(0);
        w.var_i32(1); // claims one changed slot, but the packet ends here
        let body = w.into_vec();
        let decoded = proto.decode(State::Play, play::serverbound::CONTAINER_CLICK, &body);
        assert_eq!(decoded, ServerBound::Ignored);
    }
}

/// Decode tests for `minecraft:attack` and
/// `minecraft:player_input`, the two packets the melee-damage/knockback
/// pipeline depends on.
#[cfg(test)]
mod combat_decode_tests {
    use super::*;
    use lodestone_core::State;
    use lodestone_model::{
        ClientAction, ConnectionState, EntityInteraction, Hand, PlayerInput, VersionAdapter,
    };

    /// Round-trips through the **real client encoder**
    /// (`ClientAction::InteractEntity { interaction: EntityInteraction::Attack,
    /// .. }`) rather than hand-building the wire body — the same "prove the
    /// two sides actually agree" style `decode_set_carried_item_from_real_
    /// client_encoder` already established, and the one that matters most
    /// here: `Sim::attack_entity` (`lodestone-shell`) already sends exactly
    /// this action in production (`docs/combat.md`'s "Sending the attack"
    /// section) — this is the decode side finally meeting a real producer.
    #[test]
    fn decode_attack_from_the_real_client_encoder() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = crate::adapter()
            .encode_action(
                ConnectionState::Play,
                &ClientAction::InteractEntity {
                    entity_id: 1234,
                    interaction: EntityInteraction::Attack,
                    sneaking: true, // must be dropped — the wire body carries no such bit.
                },
            )
            .expect("encodes")
            .expect("Attack always encodes in Play");
        assert_eq!(packet_id, play::serverbound::ATTACK);
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(decoded, ServerBound::Attack { entity_id: 1234 });
    }

    /// Control: a malformed/truncated `Attack` payload must drop the packet,
    /// not panic.
    #[test]
    fn decode_attack_rejects_a_truncated_payload() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::ATTACK, &[]);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    /// `minecraft:interact` (a plain `Interact`, not `Attack`) now decodes into
    /// [`ServerBound::InteractEntity`], through the **real client encoder**.
    ///
    /// This test used to assert `Ignored`, with a doc comment saying the variant was
    /// deliberately absent because *"this crate has no interaction model"* — and it
    /// asked whoever added taming to change it rather than discover a gap. That is
    /// what this is.
    ///
    /// The expected value comes from the other side of the seam: `V770Adapter`'s own
    /// `encode_action` builds the payload, so nothing here restates the field order.
    ///
    /// Values are **pairwise distinct** so the two adjacent VarInts cannot transpose
    /// unnoticed: entity `1234`, hand `1` (off hand), and `sneaking: true` — the
    /// trailing boolean set deliberately *different* from what a default fixture
    /// would carry, because two adjacent booleans (or a boolean and a defaulted
    /// field) coincide half the time by chance and a fixture that sets them equal
    /// cannot see a swap at all.
    #[test]
    fn decode_plain_interact_reaches_the_interact_entity_variant() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = crate::adapter()
            .encode_action(
                ConnectionState::Play,
                &ClientAction::InteractEntity {
                    entity_id: 1234,
                    interaction: EntityInteraction::Interact { hand: Hand::Off },
                    sneaking: true,
                },
            )
            .expect("encodes")
            .expect("Interact always encodes in Play");
        assert_eq!(packet_id, play::serverbound::INTERACT);
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(
            decoded,
            ServerBound::InteractEntity {
                entity_id: 1234,
                hand: 1,
                using_secondary_action: true,
            },
            "the right-click half must reach MobSim::interact; `Ignored` here is \
             what made a real client's right-click on a wolf do nothing"
        );
    }

    /// Control: a truncated `interact` payload must drop the packet rather than
    /// panic or produce a half-decoded variant.
    ///
    /// Without this, the `unwrap_or(Ignored)` in the decode arm is an untested
    /// branch — and it is the branch that stands between a malformed frame and a
    /// `MobSim::interact` call against a garbage entity id.
    #[test]
    fn decode_interact_rejects_a_truncated_payload() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::INTERACT, &[0x01]);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    /// Round-trips through the real client encoder: `sprint`, `shift` and
    /// `jump` survive, bit-identical, out the other side; the other four
    /// `Input` flags are decoded off the wire (so a malformed byte still
    /// fails cleanly) but do not appear in `ServerBound::PlayerInput` — see
    /// that variant's own doc comment for why. One-hot across the three
    /// arms (exactly one of `jump`/`shift`/`sprint` true per arm, the other
    /// two false) so a transposition of any adjacent pair of the three bits
    /// (`0x10`/`0x20`/`0x20`/`0x40`) cannot survive this round trip.
    #[test]
    fn decode_player_input_jump_sprint_and_shift_from_the_real_client_encoder() {
        let proto = V770ServerProtocol;
        for (jump, shift, sprint) in [(true, false, false), (false, true, false), (false, false, true)] {
            let (packet_id, payload) = crate::adapter()
                .encode_action(
                    ConnectionState::Play,
                    &ClientAction::SetPlayerInput(PlayerInput {
                        forward: true,
                        backward: false,
                        left: false,
                        right: false,
                        jump,
                        shift,
                        sprint,
                    }),
                )
                .expect("encodes")
                .expect("SetPlayerInput always encodes in Play");
            assert_eq!(packet_id, play::serverbound::PLAYER_INPUT);
            let decoded = proto.decode(State::Play, packet_id, &payload);
            assert_eq!(
                decoded,
                ServerBound::PlayerInput { sprint, shift, jump },
                "sprint={sprint} shift={shift} jump={jump}"
            );
        }
    }

    /// Control: an empty payload must drop the packet, not panic on the
    /// missing flags byte.
    #[test]
    fn decode_player_input_rejects_an_empty_payload() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_INPUT, &[]);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    /// Sanity check on the bit layout itself, independent of the real
    /// encoder: bit `0x40` alone must decode to `sprint: true` (the other two
    /// false), `0x20` alone to `shift: true`, and `0x10` alone to
    /// `jump: true`, so a future change to `ServerBound::PlayerInput`'s
    /// fields can be checked against a known byte, not only against the
    /// adapter's own (also-changeable) encoder. Covers a transposition of
    /// any of the three adjacent bits, not just their presence.
    #[test]
    fn decode_player_input_bit_layout_pins_sprint_at_0x40_shift_at_0x20_jump_at_0x10() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_INPUT, &[0x40]);
        assert_eq!(
            decoded,
            ServerBound::PlayerInput {
                sprint: true,
                shift: false,
                jump: false,
            }
        );
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_INPUT, &[0x20]);
        assert_eq!(
            decoded,
            ServerBound::PlayerInput {
                sprint: false,
                shift: true,
                jump: false,
            }
        );
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_INPUT, &[0x10]);
        assert_eq!(
            decoded,
            ServerBound::PlayerInput {
                sprint: false,
                shift: false,
                jump: true,
            }
        );
        // forward|backward|left|right — none of the three modelled flags.
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_INPUT, &[0x0F]);
        assert_eq!(
            decoded,
            ServerBound::PlayerInput {
                sprint: false,
                shift: false,
                jump: false,
            }
        );
    }
}

/// Regression coverage for the chunk-streaming investigation's bug
/// (see the doc comment on the `CLIENT_INFORMATION`/`CHUNK_BATCH_RECEIVED`
/// decode arms above): both packet ids used to hit the generic
/// decode-then-drop `Ignored` family from before this crate had any
/// consumer for either, and a later fix added
/// `ServerBound::ClientInformationChanged`/`ChunkBatchAcknowledged` plus
/// `crate::server`'s consumers without ever updating this decode arm to
/// construct them — so both variants were dead code, and every
/// view-streaming chunk batch after the connection's first queued behind a
/// permanently-`true` `awaiting_chunk_batch_ack` and was never flushed.
/// `cargo test -p lodestone-v26-2 --test block_edit -- \
/// dig_and_place_persist_through_forget_and_reload` reproduced this at
/// committed `main` before the fix (a real player walking back into a
/// forgotten chunk never got it re-sent) and passes after it.
#[cfg(test)]
mod view_streaming_decode_tests {
    use super::*;
    use lodestone_core::State;
    use lodestone_model::{
        ChatMode, ClientAction, ClientSettings, ConnectionState, DisplayedSkinParts, MainHand,
        Directive, ParticleStatus, VersionAdapter,
    };
    use crate::packets::game::ChunkBatchFinished;
    use lodestone_world::World;

    fn encode<T: Encode>(packet: &T) -> Vec<u8> {
        let mut w = Writer::default();
        packet.encode(&mut w, CTX).expect("well-formed struct encodes");
        w.into_vec()
    }

    /// The real client's `SetClientSettings` encoder (the same one a
    /// render-distance change in the shell's settings screen would send),
    /// decoded back into [`ServerBound::ClientInformationChanged`]. Before
    /// the fix this decoded to `ServerBound::Ignored` unconditionally, so
    /// `crate::server`'s `ViewTracker::set_view_radius` consumer was never
    /// reached by a real client no matter what it sent.
    #[test]
    fn decode_client_information_changed_from_real_client_encoder() {
        let proto = V770ServerProtocol;
        let settings = ClientSettings {
            locale: "en_us".to_owned(),
            view_distance: 12,
            chat_mode: ChatMode::Full,
            chat_colors: true,
            skin_parts: DisplayedSkinParts {
                cape: false,
                jacket: false,
                left_sleeve: false,
                right_sleeve: false,
                left_pants_leg: false,
                right_pants_leg: false,
                hat: false,
            },
            main_hand: MainHand::Right,
            text_filtering: false,
            allow_server_listing: true,
            particle_status: ParticleStatus::All,
        };
        let (packet_id, payload) = crate::adapter()
            .encode_action(ConnectionState::Play, &ClientAction::SetClientSettings(settings))
            .expect("encodes")
            .expect("SetClientSettings always encodes in Play");
        assert_eq!(packet_id, play::serverbound::CLIENT_INFORMATION);
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(decoded, ServerBound::ClientInformationChanged { view_distance: 12 });
    }

    /// Control: a malformed payload must still drop the packet rather than
    /// panic on the missing fields.
    #[test]
    fn decode_client_information_changed_rejects_a_truncated_payload() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::CLIENT_INFORMATION, &[]);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    /// Chains the real *client* chunk-batch-flow-control reply — produced by
    /// feeding a genuine clientbound `CHUNK_BATCH_FINISHED` through the real
    /// `V770Adapter::handle_packet` (the same code path
    /// `crate::server`'s own connection loop drives, per this module's own
    /// `CHUNK_BATCH_START`/`CHUNK_BATCH_FINISHED` handling) — into this
    /// module's server-side decoder. This proves the whole
    /// server-sends-a-batch / client-acks-it / server-reads-the-ack loop
    /// closes through two independently-written, real production
    /// encode/decode paths, not a hand-rolled fixture on either end.
    #[test]
    fn decode_chunk_batch_acknowledged_from_the_real_client_adapter() {
        let finished_body = encode(&ChunkBatchFinished { batch_size: 7 });
        let mut world = World::new();
        let directives = crate::adapter()
            .handle_packet(
                &mut world,
                ConnectionState::Play,
                play::clientbound::CHUNK_BATCH_FINISHED,
                &finished_body,
            )
            .expect("a real client must accept its own CHUNK_BATCH_FINISHED body");
        let (ack_packet_id, ack_payload) = directives
            .into_iter()
            .find_map(|d| match d {
                Directive::Send { packet_id, payload } => Some((packet_id, payload)),
                _ => None,
            })
            .expect("CHUNK_BATCH_FINISHED must produce a CHUNK_BATCH_RECEIVED reply");
        assert_eq!(ack_packet_id, play::serverbound::CHUNK_BATCH_RECEIVED);

        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, ack_packet_id, &ack_payload);
        match decoded {
            ServerBound::ChunkBatchAcknowledged { desired_chunks_per_tick } => {
                assert!(
                    desired_chunks_per_tick > 0.0,
                    "a real client's desired rate must be positive, got {desired_chunks_per_tick}"
                );
            }
            other => panic!(
                "expected ServerBound::ChunkBatchAcknowledged (this is exactly the variant that \
                 was dead code before the fix — see this module's own doc comment), got {other:?}"
            ),
        }
    }

    /// Pins the exact numeric field against a hand-built payload too,
    /// independent of whatever rate-estimation formula the real client picks
    /// — a future change to that formula should not silently stop this test
    /// from noticing a decode regression.
    #[test]
    fn decode_chunk_batch_acknowledged_bit_layout() {
        let proto = V770ServerProtocol;
        let body = encode(&ChunkBatchReceived {
            desired_chunks_per_tick: 32.0,
        });
        let decoded = proto.decode(State::Play, play::serverbound::CHUNK_BATCH_RECEIVED, &body);
        assert_eq!(
            decoded,
            ServerBound::ChunkBatchAcknowledged { desired_chunks_per_tick: 32.0 }
        );
    }

    /// Control: a malformed payload must still drop the packet rather than
    /// panic.
    #[test]
    fn decode_chunk_batch_acknowledged_rejects_a_truncated_payload() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::CHUNK_BATCH_RECEIVED, &[]);
        assert_eq!(decoded, ServerBound::Ignored);
    }
}

/// Encode-side wire layouts for the six world-border packets.
///
/// Each test drives [`V770ServerProtocol`]'s `encode_*` and re-parses the
/// produced bytes field by field against the vanilla field order, instead of
/// comparing structs — so an encoder bug and a mirror-image decode bug in the
/// same derive cannot cancel out (the decode side of these packets is pinned
/// independently in `crates/versions/26.2/tests/world_border.rs`).
#[cfg(test)]
mod border_wire_tests {
    use super::*;

    fn unwrap_send(directive: ServerDirective) -> (i32, Vec<u8>) {
        match directive {
            ServerDirective::Send { packet_id, payload } => (packet_id, payload),
            other => panic!("expected Send, got {other:?}"),
        }
    }

    /// The join broadcast (`encode_initialize_border`), against the field
    /// order of vanilla's own clientbound initialize-border packet's own write: two `f64` centre
    /// coords, `old_size`, `new_size`, then VarLong lerp time and three
    /// VarInts. A static border carries `old_size == new_size` and lerp time
    /// `0`.
    #[test]
    fn encode_initialize_border_wire_layout() {
        let proto = V770ServerProtocol;
        let mut border = WorldBorder::default();
        border.set_center(10.0, -10.0);
        border.set_size(1000.0);
        border.set_warning_blocks(10);
        border.set_warning_time(20);
        let (packet_id, payload) = unwrap_send(proto.encode_initialize_border(&border));
        assert_eq!(packet_id, play::clientbound::INITIALIZE_BORDER);
        let mut r = Reader::new(&payload);
        assert_eq!(r.f64().expect("center_x"), 10.0);
        assert_eq!(r.f64().expect("center_z"), -10.0);
        assert_eq!(r.f64().expect("old_size"), 1000.0);
        assert_eq!(r.f64().expect("new_size"), 1000.0, "static border targets its own size");
        assert_eq!(r.var_i64().expect("lerp_time"), 0, "static border has no lerp");
        assert_eq!(r.var_i32().expect("absolute_max_size"), ABSOLUTE_MAX_SIZE);
        assert_eq!(r.var_i32().expect("warning_blocks"), 10);
        assert_eq!(r.var_i32().expect("warning_time"), 20);
        r.ensure_empty().expect("no trailing bytes");
    }

    /// A mid-lerp join must carry the *remaining* time converted from ticks to
    /// the milliseconds the lodestone client interpolates on (this crate's
    /// deliberate divergence from vanilla's raw ticks — see
    /// [`InitializeBorder`]'s packet doc).
    #[test]
    fn encode_initialize_border_converts_remaining_ticks_to_ms() {
        let proto = V770ServerProtocol;
        let mut border = WorldBorder::default();
        border.lerp_size_between(500.0, 100.0, 200, 0); // 200 ticks remaining
        let (_, payload) = unwrap_send(proto.encode_initialize_border(&border));
        let mut r = Reader::new(&payload);
        let _ = r.f64().expect("center_x");
        let _ = r.f64().expect("center_z");
        assert_eq!(r.f64().expect("old_size"), 500.0, "the lerp's start size");
        assert_eq!(r.f64().expect("new_size"), 100.0, "the lerp's target");
        assert_eq!(
            r.var_i64().expect("lerp_time"),
            200 * 50,
            "remaining ticks are broadcast as milliseconds"
        );
    }

    /// `encode_set_border_center`: two big-endian `f64` coords, nothing else
    /// (`ClientboundSetBorderCenterPacket`).
    #[test]
    fn encode_set_border_center_wire_layout() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = unwrap_send(proto.encode_set_border_center(100.5, -200.25));
        assert_eq!(packet_id, play::clientbound::SET_BORDER_CENTER);
        let mut r = Reader::new(&payload);
        assert_eq!(r.f64().expect("center_x"), 100.5);
        assert_eq!(r.f64().expect("center_z"), -200.25);
        r.ensure_empty().expect("no trailing bytes");
    }

    /// `encode_set_border_lerp_size`: `old_size`, `new_size`, then a VarLong
    /// lerp time in milliseconds (verbatim — this encoder is the last hop).
    #[test]
    fn encode_set_border_lerp_size_wire_layout() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = unwrap_send(proto.encode_set_border_lerp_size(200.0, 100.0, 30_000));
        assert_eq!(packet_id, play::clientbound::SET_BORDER_LERP_SIZE);
        let mut r = Reader::new(&payload);
        assert_eq!(r.f64().expect("old_size"), 200.0);
        assert_eq!(r.f64().expect("new_size"), 100.0);
        assert_eq!(r.var_i64().expect("lerp_time_ms"), 30_000);
        r.ensure_empty().expect("no trailing bytes");
    }

    /// `encode_set_border_size`: a single big-endian `f64`
    /// (`ClientboundSetBorderSizePacket`).
    #[test]
    fn encode_set_border_size_wire_layout() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = unwrap_send(proto.encode_set_border_size(60_000_000.0));
        assert_eq!(packet_id, play::clientbound::SET_BORDER_SIZE);
        let mut r = Reader::new(&payload);
        assert_eq!(r.f64().expect("size"), 60_000_000.0);
        r.ensure_empty().expect("no trailing bytes");
    }

    /// `encode_set_border_warning_delay`: a single VarInt seconds value.
    #[test]
    fn encode_set_border_warning_delay_wire_layout() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = unwrap_send(proto.encode_set_border_warning_delay(15));
        assert_eq!(packet_id, play::clientbound::SET_BORDER_WARNING_DELAY);
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("warning_time"), 15);
        r.ensure_empty().expect("no trailing bytes");
    }

    /// `encode_set_border_warning_distance`: a single VarInt blocks value.
    #[test]
    fn encode_set_border_warning_distance_wire_layout() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = unwrap_send(proto.encode_set_border_warning_distance(5));
        assert_eq!(packet_id, play::clientbound::SET_BORDER_WARNING_DISTANCE);
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("warning_blocks"), 5);
        r.ensure_empty().expect("no trailing bytes");
    }
}

#[cfg(test)]
mod vehicle_wire_tests {
    use super::*;
    use lodestone_core::State;

    /// vanilla's own clientbound set-passengers packet's own write — a VarInt vehicle id then
    /// `writeVarIntArray`.
    ///
    /// The ids are **pairwise distinct and none is a small ordinal** (`517`, `41`,
    /// `9`), which is what makes a transposition of the vehicle and its first
    /// passenger fail: the two are adjacent VarInts of the same type, so
    /// `decode(encode(x))` through this crate's own pair is byte-perfect either way
    /// and the visible symptom would be a boat riding a player.
    ///
    /// The length prefix is asserted separately from the elements for the same
    /// reason: `writeVarIntArray` is not vanilla's own codec library's own var-int accessor.apply(list())`, and
    /// the two are only accidentally the same bytes.
    #[test]
    fn set_passengers_writes_the_vehicle_then_a_varint_array() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { packet_id, payload } =
            proto.encode_set_passengers(517, &[41, 9])
        else {
            panic!("a passenger list must be sent");
        };
        assert_eq!(packet_id, play::clientbound::SET_PASSENGERS);
        let mut r = Reader::new(&payload);
        let mut wrong = Vec::new();
        if r.var_i32().expect("vehicle id") != 517 {
            wrong.push("the vehicle id must come first");
        }
        if r.var_i32().expect("count") != 2 {
            wrong.push("then the array length");
        }
        if r.var_i32().expect("first passenger") != 41 {
            wrong.push("then the passengers, in order");
        }
        if r.var_i32().expect("second passenger") != 9 {
            wrong.push("both of them");
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
        r.ensure_empty().expect("no trailing bytes");

        // **The dismount frame.** Vanilla announces a dismount as the same packet
        // with an empty list, and this client folds exactly that into "we got out"
        // (`lodestone_ecs::session`'s `Riding` fold). A zero-length array is
        // therefore a meaningful frame, not a degenerate one, and an encoder that
        // declined to send it would leave a dismounted player stuck in a seat.
        let ServerDirective::Send { payload: empty, .. } = proto.encode_set_passengers(517, &[])
        else {
            panic!("an empty list is still a real packet");
        };
        let mut r = Reader::new(&empty);
        assert_eq!(r.var_i32().expect("vehicle id"), 517);
        assert_eq!(r.var_i32().expect("count"), 0);
        r.ensure_empty().expect("no trailing bytes");
    }

    /// `ServerboundMoveVehiclePacket` decodes into a real variant now that the
    /// server has a vehicle to apply it to.
    ///
    /// Every field value is distinct and none is a round number, because the packet
    /// is three `f64`s followed by two `f32`s: any transposition inside either run
    /// is wire-legal and survives a round trip through our own codec. `-40.25` for
    /// pitch versus `137.5` for yaw also separates them by *sign*, so swapping the
    /// pair is visible rather than merely numerically different.
    ///
    /// The fixture is built with the packet struct's own encoder rather than by
    /// hand, so this gate is about the **lift** (that `MOVE_VEHICLE` reaches
    /// `ServerBound::VehicleMoved` rather than `Ignored`); the byte layout itself is
    /// pinned by `crate::packets::game`'s own round-trip gates.
    #[test]
    fn move_vehicle_lifts_into_a_variant_rather_than_being_ignored() {
        let body = MoveVehicle {
            x: 118.5,
            y: 63.25,
            z: -204.75,
            yaw: 137.5,
            pitch: -40.25,
            on_ground: true,
        };
        let payload = encode_body(&body);
        let decoded = V770ServerProtocol.decode(
            State::Play,
            play::serverbound::MOVE_VEHICLE,
            &payload,
        );
        assert_eq!(
            decoded,
            ServerBound::VehicleMoved {
                position: Vec3::new(118.5, 63.25, -204.75),
                yaw: 137.5,
                pitch: -40.25,
            }
        );
        // A truncated frame must be `Ignored`, not a partially-read transform: a
        // half-decoded position would teleport the boat.
        assert_eq!(
            V770ServerProtocol.decode(
                State::Play,
                play::serverbound::MOVE_VEHICLE,
                &payload[..payload.len() - 3],
            ),
            ServerBound::Ignored
        );
    }
}

#[cfg(test)]
mod dimension_wire_tests {
    use super::*;

    /// The holder-id mapping is `DIMENSION_TYPE_REGISTRY`'s order, and the Nether is
    /// **3**, not 1 — `overworld_caves` and `the_end` sit between them.
    ///
    /// The expectation comes from the registry table this crate publishes, and the
    /// negative arm is what makes it a test rather than a restatement: an
    /// unrecognised key must be `None`, because guessing a holder id reframes every
    /// subsequent chunk against the wrong build height.
    #[test]
    fn dimension_type_holder_ids_follow_the_published_registry_order() {
        assert_eq!(dimension_type_holder_id("minecraft:overworld"), Some(0));
        assert_eq!(dimension_type_holder_id("minecraft:overworld_caves"), Some(1));
        assert_eq!(dimension_type_holder_id("minecraft:the_end"), Some(2));
        assert_eq!(dimension_type_holder_id("minecraft:the_nether"), Some(3));
        assert_eq!(dimension_type_holder_id("mypack:mine"), None);
    }

    /// `encode_dimension_change` carries `KEEP_ALL_DATA` and the destination's own
    /// `sea_level`, and emits **nothing** for a dimension this server's registry does
    /// not publish.
    ///
    /// The `data_to_keep` byte is the one field that separates this from
    /// `encode_respawn`: `0` there makes the client rebuild its player state, which
    /// for a portal trip would empty the inventory. Asserting the *byte* rather than
    /// "a respawn was sent" is what makes that checkable.
    #[test]
    fn a_dimension_change_keeps_player_data_and_declines_an_unknown_level() {
        let proto = V770ServerProtocol;
        let directives = proto.encode_dimension_change(
            "minecraft:the_nether",
            Vec3::new(215.5, 96.0, -65.5),
            GameMode::Survival,
        );
        assert_eq!(directives.len(), 2, "the respawn record, then the teleport");
        let ServerDirective::Send { packet_id, payload } = &directives[0] else {
            panic!("expected a Send, got {:?}", directives[0]);
        };
        assert_eq!(*packet_id, play::clientbound::RESPAWN);
        let mut r = Reader::new(payload);
        assert_eq!(r.var_i32().expect("dimension_type"), 3);
        assert_eq!(r.string(32767).expect("dimension"), "minecraft:the_nether");
        assert_eq!(r.i64().expect("seed"), 0);
        assert_eq!(r.u8().expect("game_type"), 0);
        assert_eq!(r.i8().expect("previous_game_type"), -1);
        assert!(!r.bool().expect("is_debug"));
        assert!(!r.bool().expect("is_flat"));
        assert!(!r.bool().expect("has last_death_location"));
        assert_eq!(r.var_i32().expect("portal_cooldown"), 0);
        assert_eq!(
            r.var_i32().expect("sea_level"),
            NETHER_SEA_LEVEL,
            "the destination's sea level, not the overworld's 63"
        );
        assert_eq!(
            r.u8().expect("data_to_keep"),
            0x03,
            "KEEP_ATTRIBUTE_MODIFIERS | KEEP_ENTITY_DATA — a portal trip keeps the \
             player's inventory, XP and health"
        );
        r.ensure_empty().expect("no trailing bytes");

        assert!(
            proto
                .encode_dimension_change("mypack:mine", Vec3::new(0.0, 0.0, 0.0), GameMode::Survival)
                .is_empty(),
            "an unpublished level must emit nothing rather than guess a holder id"
        );
    }

    /// A served column is framed against a **dimension window**, never against its
    /// own height.
    ///
    /// The first arm is the regression that six live loopback tests caught: fixtures
    /// serve deliberately tiny columns (`ChunkColumn::new(0, 16)`), and framing one
    /// against its own height emits a one-section packet to a 24-section client,
    /// which joins, spawns and then decodes nothing at all.
    #[test]
    fn a_columns_shape_comes_from_its_dimension_not_its_height() {
        let short = ServerChunkColumn::new(0, 16);
        assert_eq!(
            shape_for_column(&short).section_count,
            24,
            "an unrecognised window keeps the overworld's 24 sections"
        );
        assert_eq!(shape_for_column(&short).min_y, -64);

        let overworld = ServerChunkColumn::new(-64, 384);
        assert_eq!(shape_for_column(&overworld).section_count, 24);

        let nether = ServerChunkColumn::new(0, 256);
        assert_eq!(
            shape_for_column(&nether).section_count,
            16,
            "the Nether's own window is 16 sections"
        );
        assert_eq!(shape_for_column(&nether).min_y, 0);
    }
}

/// Index-18's four `BYTE` claimants, checked mechanically against the committed
/// jar dump rather than cited in prose — and the two flag *layouts*, which the
/// dump cannot check because it records the index and serializer, not the bits.
#[cfg(test)]
mod index_eighteen_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{
        METADATA_IDX_CREEPER_IGNITED, METADATA_IDX_HORSE_FLAGS, METADATA_IDX_TAMABLE_FLAGS,
        METADATA_SER_BOOLEAN, METADATA_SER_BYTE, V770ServerProtocol,
    };

    /// `EntityDataIndexOracle`'s output, committed so this gate does not need a JVM.
    /// The same file `crates/versions/26.2/src/packets/metadata.rs`'s own dump gate
    /// reads, for the same reason: the expected value has to come from the jar.
    const INDEX_DUMP: &str = include_str!("../tests/support/entity_data_index_jvm.txt");

    /// `(index, serializer)` for `Owner.FIELD`, or a panic naming the miss.
    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    /// The three constants this module uses at index 18 name accessors the jar
    /// really does put there, with the serializers this encoder writes.
    ///
    /// Collected rather than asserted inside the loop, so a failure reports every
    /// wrong row instead of aborting on the first — three rows named individually is
    /// what makes "which one drifted" answerable.
    #[test]
    fn every_index_eighteen_constant_matches_the_jar_dump() {
        let claims: &[(u8, i32, &str, &str)] = &[
            (
                METADATA_IDX_TAMABLE_FLAGS,
                METADATA_SER_BYTE,
                "TamableAnimal.DATA_FLAGS_ID",
                "METADATA_IDX_TAMABLE_FLAGS",
            ),
            (
                METADATA_IDX_HORSE_FLAGS,
                METADATA_SER_BYTE,
                "AbstractHorse.DATA_ID_FLAGS",
                "METADATA_IDX_HORSE_FLAGS",
            ),
            (
                METADATA_IDX_CREEPER_IGNITED,
                METADATA_SER_BOOLEAN,
                "Creeper.DATA_IS_IGNITED",
                "METADATA_IDX_CREEPER_IGNITED",
            ),
        ];
        let mut wrong: Vec<String> = Vec::new();
        for &(index, serializer, accessor, name) in claims {
            let (want_index, want_serializer) = dump_row(accessor);
            if index != want_index || serializer != want_serializer {
                wrong.push(format!(
                    "{name} says ({index}, ser {serializer}) but the jar says \
                     {accessor} is ({want_index}, ser {want_serializer})"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// **The collision itself**, asserted rather than described: at least four
    /// distinct `BYTE` fields share index 18.
    ///
    /// This is the premise the producer-side species switch in
    /// `lodestone_server::mobs::SimMob::snapshot` exists for. If a future version
    /// collapsed them, the switch would be pointless ceremony and this gate says so;
    /// if a *fifth* appears, the count moves and whoever is adding a metadata field
    /// at 18 is forced to look.
    #[test]
    fn index_eighteen_really_is_shared_by_several_byte_fields() {
        let byte_claimants: Vec<&str> = INDEX_DUMP
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let mut tok = l.split_whitespace();
                let index: u8 = tok.next()?.parse().ok()?;
                let owner = tok.next()?;
                let serializer: i32 = tok.next()?.parse().ok()?;
                (index == 18 && serializer == METADATA_SER_BYTE).then_some(owner)
            })
            .collect();
        for expected in [
            "TamableAnimal.DATA_FLAGS_ID",
            "AbstractHorse.DATA_ID_FLAGS",
            "Sheep.DATA_WOOL_ID",
            "Shulker.DATA_COLOR_ID",
        ] {
            assert!(
                byte_claimants.contains(&expected),
                "{expected} must be one of index 18's BYTE claimants; the dump lists \
                 {byte_claimants:?}"
            );
        }
        assert!(
            byte_claimants.len() >= 4,
            "index 18 must still be shared — if it is not, the species switch in \
             SimMob::snapshot is unnecessary. Claimants: {byte_claimants:?}"
        );
    }

    /// **The two layouts differ, and neither variant sets the other's bit.**
    ///
    /// This is the arm that would have caught one shared "tamed" variant. Note the
    /// direction of the failure it guards: `0x04` is not in `AbstractHorse`'s flag
    /// set at all (`FLAG_TAME` is `2`, `FLAG_BRED` is `8`) and `0x02` is not in
    /// `TamableAnimal`'s, so a shared variant does not set a *wrong* named flag — it
    /// sets an unnamed bit and the animal reads as **untamed**, with a
    /// perfectly-formed packet on the wire and nothing visibly wrong to chase.
    ///
    /// `sitting: false` with `tame: true` on purpose: setting both would make
    /// `0x01 | 0x04 = 0x05` and a gate that only checked "non-zero" could not tell
    /// the tame bit from the sitting bit. The `sitting` bit gets its own arm below.
    #[test]
    fn the_tamable_and_horse_flag_bytes_use_different_bits() {
        let proto = V770ServerProtocol;

        let tamable = flag_byte(
            &proto,
            &MetadataField::TamableFlags {
                tame: true,
                sitting: false,
            },
        );
        let horse = flag_byte(&proto, &MetadataField::HorseFlags { tame: true });

        assert_eq!(tamable, 0x04, "TamableAnimal.isTame() is `& 4`");
        assert_eq!(horse, 0x02, "AbstractHorse.FLAG_TAME is 2");
        assert_ne!(
            tamable, horse,
            "one shared variant would put the same bit on both species, and the one \
             it is wrong for reads as untamed"
        );
        // Neither sets the other's bit, stated as its own claim: equality above could
        // hold for two bytes that both happen to carry both bits.
        assert_eq!(tamable & 0x02, 0, "a wolf must not carry the horse's tame bit");
        assert_eq!(horse & 0x04, 0, "a horse must not carry the wolf's tame bit");
    }

    /// The sitting bit is `0x01` and is independent of tameness.
    ///
    /// Three inputs rather than one, because `sitting` and `tame` are two adjacent
    /// booleans in the same expression: a fixture that sets them equal coincides with
    /// a swapped implementation half the time and cannot see it at all.
    #[test]
    fn the_sitting_bit_is_independent_of_the_tame_bit() {
        let proto = V770ServerProtocol;
        let cases = [
            ((false, false), 0x00u8),
            ((true, false), 0x04),
            ((false, true), 0x01),
            ((true, true), 0x05),
        ];
        let mut wrong: Vec<String> = Vec::new();
        for ((tame, sitting), want) in cases {
            let got = flag_byte(&proto, &MetadataField::TamableFlags { tame, sitting });
            if got != want {
                wrong.push(format!("(tame {tame}, sitting {sitting}) gave {got:#04x}, want {want:#04x}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// The last byte of a one-field `SET_ENTITY_DATA` payload, after checking the
    /// index and serializer it was written under.
    fn flag_byte(proto: &V770ServerProtocol, field: &MetadataField) -> u8 {
        let ServerDirective::Send { payload, .. } =
            proto.encode_set_entity_data(11, std::slice::from_ref(field))
        else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 11);
        assert_eq!(r.u8().expect("metadata index"), 18);
        assert_eq!(r.var_i32().expect("serializer id"), METADATA_SER_BYTE);
        let byte = r.i8().expect("flag byte") as u8;
        // The terminator vanilla's `SynchedEntityData` writes after the last entry.
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
        byte
    }
}

/// Index-13's two claimants, checked mechanically against the committed jar
/// dump — the same shape [`index_eighteen_tests`] establishes: a producer
/// disambiguation (here, "only the furnace-minecart loop ever builds
/// `MetadataField::MinecartFuel`") is only as trustworthy as the premise that
/// the two claimants really do carry different serializers, asserted here
/// rather than assumed.
#[cfg(test)]
mod index_thirteen_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{METADATA_IDX_MINECART_FUEL, METADATA_SER_BOOLEAN, V770ServerProtocol};

    const INDEX_DUMP: &str = include_str!("../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    /// `METADATA_IDX_MINECART_FUEL` names the accessor the jar really puts at
    /// index 13 with the `BOOLEAN` serializer this encoder writes.
    #[test]
    fn minecart_fuel_index_matches_the_jar_dump() {
        let (index, serializer) = dump_row("MinecartFurnace.DATA_ID_FUEL");
        assert_eq!(index, METADATA_IDX_MINECART_FUEL, "MinecartFurnace.DATA_ID_FUEL must be index 13");
        assert_eq!(serializer, METADATA_SER_BOOLEAN, "MinecartFurnace.DATA_ID_FUEL must be a BOOLEAN");
    }

    /// **The premise the producer-based disambiguation depends on**: index
    /// 13's other real claimant, the command-block-minecart class's own command-name accessor,
    /// really is a *different* serializer (`STRING`, not `BOOLEAN`). If a
    /// future jar ever made it a `BOOLEAN` too, this gate — not a silent wire
    /// collision discovered later — is what would catch it.
    #[test]
    fn index_thirteens_other_claimant_is_a_different_serializer() {
        let (index, serializer) = dump_row("MinecartCommandBlock.DATA_ID_COMMAND_NAME");
        assert_eq!(index, METADATA_IDX_MINECART_FUEL, "both claimants share index 13");
        assert_ne!(
            serializer, METADATA_SER_BOOLEAN,
            "MinecartCommandBlock.DATA_ID_COMMAND_NAME must not also be a BOOLEAN, or MinecartFuel is ambiguous on the wire"
        );
    }

    /// The encoder actually writes index 13 with the `BOOLEAN` serializer id,
    /// end to end through `encode_set_entity_data`.
    #[test]
    fn minecart_fuel_encodes_at_index_thirteen() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } = proto.encode_set_entity_data(9, &[MetadataField::MinecartFuel(true)]) else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 9);
        assert_eq!(r.u8().expect("metadata index"), METADATA_IDX_MINECART_FUEL);
        assert_eq!(r.var_i32().expect("serializer id"), METADATA_SER_BOOLEAN);
        assert!(r.bool().expect("fuel flag"));
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }
}

/// Index-16's `BOOLEAN` baby claimants, checked mechanically against the
/// committed jar dump — the wire-level twin of the species switch in
/// `lodestone_server::mobs::SimMob::snapshot`, which decides *which* species
/// this crate ever builds a `MetadataField::Baby` for.
#[cfg(test)]
mod index_sixteen_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{
        METADATA_IDX_BABY, METADATA_IDX_CREEPER_SWELL_DIR, METADATA_SER_BOOLEAN, METADATA_SER_INT,
        V770ServerProtocol,
    };

    /// `EntityDataIndexOracle`'s output, committed so this gate does not need a JVM.
    const INDEX_DUMP: &str = include_str!("../tests/support/entity_data_index_jvm.txt");

    /// `(index, serializer)` for `Owner.FIELD`, or a panic naming the miss.
    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    /// The three real baby accessors the producer-side species switch relies
    /// on — `AgeableMob` for the breedable-animal family, and `Zombie`
    /// (inherited by husk/zombie_villager/drowned/zombified_piglin) and
    /// `Zoglin` declaring their own — all land at [`METADATA_IDX_BABY`] under
    /// the `BOOLEAN` serializer. Collected rather than asserted per-row so a
    /// failure names every wrong one, not just the first.
    #[test]
    fn every_real_baby_accessor_matches_the_jar_dump() {
        let mut wrong: Vec<String> = Vec::new();
        for accessor in [
            "AgeableMob.DATA_BABY_ID",
            "Zombie.DATA_BABY_ID",
            "Zoglin.DATA_BABY_ID",
        ] {
            let (index, serializer) = dump_row(accessor);
            if index != METADATA_IDX_BABY || serializer != METADATA_SER_BOOLEAN {
                wrong.push(format!(
                    "{accessor} is ({index}, ser {serializer}) in the jar, expected \
                     ({METADATA_IDX_BABY}, ser {METADATA_SER_BOOLEAN})"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// **The collision that makes the producer-side species switch load-bearing,
    /// asserted rather than described.** the creeper class's own swell-dir accessor shares index 16
    /// with the baby accessors above but is an `INT`, not a `BOOLEAN` — so a
    /// `MetadataField::Baby` built for a creeper would put a boolean where the
    /// swell direction belongs, and `MobSim::snapshot` never emitting `Baby` for
    /// `"creeper"` is the only thing preventing that.
    #[test]
    fn the_shared_index_really_is_a_different_serializer_for_the_creeper() {
        let (index, serializer) = dump_row("Creeper.DATA_SWELL_DIR");
        assert_eq!(index, METADATA_IDX_CREEPER_SWELL_DIR);
        assert_eq!(index, METADATA_IDX_BABY, "the whole point is that these collide");
        assert_eq!(serializer, METADATA_SER_INT);
        assert_ne!(
            serializer, METADATA_SER_BOOLEAN,
            "if the creeper's swell direction ever became a BOOLEAN, index 16 would no \
             longer distinguish it from Baby and the producer-side guard would need \
             re-checking"
        );
    }

    /// Byte-accurate encode: index, serializer id, the boolean itself, then the
    /// `0xFF` terminator, with no trailing bytes.
    #[test]
    fn baby_encodes_to_the_exact_index_and_serializer() {
        let proto = V770ServerProtocol;
        for value in [true, false] {
            let ServerDirective::Send { payload, .. } =
                proto.encode_set_entity_data(7, &[MetadataField::Baby(value)])
            else {
                panic!("encode_set_entity_data must emit a Send");
            };
            let mut r = Reader::new(&payload);
            assert_eq!(r.var_i32().expect("entity id"), 7);
            assert_eq!(r.u8().expect("metadata index"), METADATA_IDX_BABY);
            assert_eq!(r.var_i32().expect("serializer id"), METADATA_SER_BOOLEAN);
            assert_eq!(r.bool().expect("baby bool"), value);
            assert_eq!(r.u8().expect("terminator"), 0xFF);
            assert!(r.ensure_empty().is_ok(), "no trailing bytes");
        }
    }
}

/// Index 16's `INT` claimants, checked mechanically against the committed jar
/// dump — the same shape [`index_sixteen_tests`] establishes for the
/// `BOOLEAN`-serializer baby collision, one level over: a producer
/// disambiguation ("only `MobSim::push_dragon_snapshots` ever builds
/// `MetadataField::DragonPhase`") is only as trustworthy as the premise that
/// index 16's *other* real claimants really do carry the collision this
/// module assumes.
#[cfg(test)]
mod index_sixteen_dragon_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{METADATA_IDX_DRAGON_PHASE, METADATA_SER_INT, V770ServerProtocol};

    const INDEX_DUMP: &str = include_str!("../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    /// the ender-dragon class's own phase accessor really is index 16 under the `INT` serializer
    /// this encoder writes.
    #[test]
    fn dragon_phase_index_matches_the_jar_dump() {
        let (index, serializer) = dump_row("EnderDragon.DATA_PHASE");
        assert_eq!(index, METADATA_IDX_DRAGON_PHASE, "EnderDragon.DATA_PHASE must be index 16");
        assert_eq!(serializer, METADATA_SER_INT, "EnderDragon.DATA_PHASE must be an INT");
    }

    /// **The premise the producer-based disambiguation depends on**: every
    /// other index-16 `INT` claimant the jar dump lists really is a different
    /// owner (so the producer, not the wire, is what keeps them apart).
    /// Collected rather than asserted per-row so a failure names every wrong
    /// one, not just the first.
    #[test]
    fn index_sixteens_other_int_claimants_are_all_distinct_from_the_dragon() {
        let mut wrong: Vec<String> = Vec::new();
        for accessor in [
            "Creeper.DATA_SWELL_DIR",
            "Display.DATA_BRIGHTNESS_OVERRIDE_ID",
            "Phantom.ID_SIZE",
            "Warden.CLIENT_ANGER_LEVEL",
            "WitherBoss.DATA_TARGET_A",
        ] {
            let (index, serializer) = dump_row(accessor);
            if index != METADATA_IDX_DRAGON_PHASE || serializer != METADATA_SER_INT {
                wrong.push(format!(
                    "{accessor} is ({index}, ser {serializer}) in the jar, expected the same \
                     collision ({METADATA_IDX_DRAGON_PHASE}, ser {METADATA_SER_INT}) `DragonPhase` shares"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// Byte-accurate encode: index, serializer id, the phase int itself, then
    /// the `0xFF` terminator, with no trailing bytes. Pairwise-distinct from
    /// the entity id so a transposition cannot survive.
    #[test]
    fn dragon_phase_encodes_to_the_exact_index_and_serializer() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } =
            proto.encode_set_entity_data(11, &[MetadataField::DragonPhase(4)])
        else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 11);
        assert_eq!(r.u8().expect("metadata index"), METADATA_IDX_DRAGON_PHASE);
        assert_eq!(r.var_i32().expect("serializer id"), METADATA_SER_INT);
        assert_eq!(r.var_i32().expect("phase int"), 4);
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }
}

/// The end crystal's two claimed indices — 8 (`OPTIONAL_BLOCK_POS`, no
/// collision the encoder needs a producer guard for) and 9 (`BOOLEAN`, a
/// three-way collision) — checked against the committed jar dump.
#[cfg(test)]
mod end_crystal_index_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{
        METADATA_IDX_CRYSTAL_BEAM_TARGET, METADATA_IDX_CRYSTAL_SHOW_BOTTOM, METADATA_SER_BOOLEAN,
        METADATA_SER_OPTIONAL_BLOCK_POS, V770ServerProtocol,
    };

    const INDEX_DUMP: &str = include_str!("../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    #[test]
    fn crystal_beam_target_index_matches_the_jar_dump() {
        let (index, serializer) = dump_row("EndCrystal.DATA_BEAM_TARGET");
        assert_eq!(index, METADATA_IDX_CRYSTAL_BEAM_TARGET);
        assert_eq!(serializer, METADATA_SER_OPTIONAL_BLOCK_POS);
    }

    /// **The premise that lets the beam-target decode arm skip a class
    /// guard**: no *other* index-8 claimant in the jar carries
    /// `OPTIONAL_BLOCK_POS`. If one ever did, `(index, serializer)` alone
    /// would stop uniquely identifying the crystal and a class guard would
    /// become necessary, exactly as index 9 already needs one below.
    #[test]
    fn index_eight_has_exactly_one_optional_block_pos_claimant() {
        let claimants: Vec<&str> = INDEX_DUMP
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter(|l| {
                let mut tok = l.split_whitespace();
                let index: u8 = tok.next().expect("index").parse().expect("u8");
                let _owner = tok.next();
                let serializer: i32 = tok.next().expect("serializer").parse().expect("i32");
                index == METADATA_IDX_CRYSTAL_BEAM_TARGET && serializer == METADATA_SER_OPTIONAL_BLOCK_POS
            })
            .collect();
        assert_eq!(
            claimants.len(),
            1,
            "expected exactly EndCrystal.DATA_BEAM_TARGET at index 8 with OPTIONAL_BLOCK_POS, got {claimants:?}"
        );
    }

    #[test]
    fn crystal_show_bottom_index_matches_the_jar_dump() {
        let (index, serializer) = dump_row("EndCrystal.DATA_SHOW_BOTTOM");
        assert_eq!(index, METADATA_IDX_CRYSTAL_SHOW_BOTTOM);
        assert_eq!(serializer, METADATA_SER_BOOLEAN);
    }

    /// **The premise the producer-based disambiguation depends on for
    /// `CrystalShowBottom`**: index 9's other two `BOOLEAN` claimants really
    /// are different owners.
    #[test]
    fn index_nines_other_boolean_claimants_are_distinct_from_the_crystal() {
        let mut wrong: Vec<String> = Vec::new();
        for accessor in ["AreaEffectCloud.DATA_WAITING", "FishingHook.DATA_BITING"] {
            let (index, serializer) = dump_row(accessor);
            if index != METADATA_IDX_CRYSTAL_SHOW_BOTTOM || serializer != METADATA_SER_BOOLEAN {
                wrong.push(format!(
                    "{accessor} is ({index}, ser {serializer}) in the jar, expected the same \
                     collision ({METADATA_IDX_CRYSTAL_SHOW_BOTTOM}, ser {METADATA_SER_BOOLEAN}) \
                     `CrystalShowBottom` shares"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// Byte-accurate encode of a present beam target: presence bool, then the
    /// packed-long block position. Coordinates are pairwise-distinct so a
    /// transposition against `pack_block_pos`'s own `(x, y, z)` order cannot
    /// survive.
    #[test]
    fn crystal_beam_target_encodes_a_present_position() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } = proto.encode_set_entity_data(
            13,
            &[MetadataField::CrystalBeamTarget(Some(lodestone_model::BlockPos::new(11, 65, 4)))],
        ) else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 13);
        assert_eq!(r.u8().expect("metadata index"), METADATA_IDX_CRYSTAL_BEAM_TARGET);
        assert_eq!(r.var_i32().expect("serializer id"), METADATA_SER_OPTIONAL_BLOCK_POS);
        assert!(r.bool().expect("presence bool"));
        let packed = r.i64().expect("packed block pos");
        // Unpack the same way `crate::packets::metadata`'s decode side does,
        // independently re-derived here rather than calling that function, so
        // this assertion cannot pass by construction against a shared bug.
        let x = (packed >> 38) as i32;
        let y = ((packed << 52) >> 52) as i32;
        let z = ((packed << 26) >> 38) as i32;
        assert_eq!((x, y, z), (11, 65, 4));
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }

    /// Byte-accurate encode of a cleared beam target: presence bool `false`,
    /// no position bytes at all.
    #[test]
    fn crystal_beam_target_encodes_absence_as_a_bare_false() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } =
            proto.encode_set_entity_data(13, &[MetadataField::CrystalBeamTarget(None)])
        else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 13);
        assert_eq!(r.u8().expect("metadata index"), METADATA_IDX_CRYSTAL_BEAM_TARGET);
        assert_eq!(r.var_i32().expect("serializer id"), METADATA_SER_OPTIONAL_BLOCK_POS);
        assert!(!r.bool().expect("presence bool"));
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }

    /// Byte-accurate encode of `CrystalShowBottom`, both values — and
    /// deliberately alongside a `CrystalBeamTarget` set to the *other*
    /// boolean-shaped state (`Some`, not `None`) in the same field list, so a
    /// transposition between the two adjacent crystal fields cannot survive
    /// (`CLAUDE.md`: "two adjacent bools coincide half the time by chance").
    #[test]
    fn crystal_show_bottom_encodes_to_the_exact_index_and_serializer() {
        let proto = V770ServerProtocol;
        for value in [true, false] {
            let ServerDirective::Send { payload, .. } = proto.encode_set_entity_data(
                13,
                &[
                    MetadataField::CrystalBeamTarget(Some(lodestone_model::BlockPos::new(2, 70, -3))),
                    MetadataField::CrystalShowBottom(value),
                ],
            ) else {
                panic!("encode_set_entity_data must emit a Send");
            };
            let mut r = Reader::new(&payload);
            assert_eq!(r.var_i32().expect("entity id"), 13);
            // Beam target first: presence bool, packed position.
            assert_eq!(r.u8().expect("beam index"), METADATA_IDX_CRYSTAL_BEAM_TARGET);
            assert_eq!(r.var_i32().expect("beam serializer"), METADATA_SER_OPTIONAL_BLOCK_POS);
            assert!(r.bool().expect("beam presence"));
            r.i64().expect("packed pos");
            // Then show-bottom.
            assert_eq!(r.u8().expect("show-bottom index"), METADATA_IDX_CRYSTAL_SHOW_BOTTOM);
            assert_eq!(r.var_i32().expect("show-bottom serializer"), METADATA_SER_BOOLEAN);
            assert_eq!(r.bool().expect("show-bottom bool"), value);
            assert_eq!(r.u8().expect("terminator"), 0xFF);
            assert!(r.ensure_empty().is_ok(), "no trailing bytes");
        }
    }
}

/// `BOSS_EVENT`'s three operations this crate emits, checked against
/// vanilla's own clientbound boss-event packet's own `write` method
/// (confirmed against the decompiled 26.2 source)
/// rather than its constructors — see `encode_boss_event_add`'s own doc for
/// why the field order there differs from a naive transcription.
#[cfg(test)]
mod boss_event_tests {
    use lodestone_core::Reader;
    use lodestone_model::Text;
    use lodestone_server::{ServerDirective, ServerProtocol};
    use uuid::Uuid;

    use super::V770ServerProtocol;

    /// A fixed, non-nil UUID so a byte-order mistake in `Writer::uuid` cannot
    /// coincidentally read back correctly.
    fn id() -> Uuid {
        Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10)
    }

    #[test]
    fn add_writes_uuid_type_name_progress_color_overlay_flags_in_that_order() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } =
            proto.encode_boss_event_add(id(), &Text::literal("Ender Dragon"), 0.75)
        else {
            panic!("encode_boss_event_add must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.uuid().expect("boss bar id"), id());
        assert_eq!(r.var_i32().expect("operation type"), 0, "ADD");
        // Network-NBT component: skip via the same path the decode side uses
        // elsewhere in this crate (`read_network_nbt`), so this assertion does
        // not re-implement NBT parsing.
        lodestone_core::read_network_nbt(&mut r).expect("name component");
        assert_eq!(r.f32().expect("progress"), 0.75);
        assert_eq!(r.var_i32().expect("color"), 0, "PINK");
        assert_eq!(r.var_i32().expect("overlay"), 0, "PROGRESS");
        assert_eq!(r.u8().expect("flags"), 0b110, "playMusic | createWorldFog");
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }

    #[test]
    fn update_progress_writes_uuid_type_then_a_bare_float() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } = proto.encode_boss_event_update_progress(id(), 0.25) else {
            panic!("encode_boss_event_update_progress must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.uuid().expect("boss bar id"), id());
        assert_eq!(r.var_i32().expect("operation type"), 2, "UPDATE_PROGRESS");
        assert_eq!(r.f32().expect("progress"), 0.25);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }

    #[test]
    fn remove_writes_uuid_and_type_with_no_payload_at_all() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } = proto.encode_boss_event_remove(id()) else {
            panic!("encode_boss_event_remove must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.uuid().expect("boss bar id"), id());
        assert_eq!(r.var_i32().expect("operation type"), 1, "REMOVE");
        assert!(r.ensure_empty().is_ok(), "no trailing bytes — REMOVE carries no payload");
    }

    /// Two different progress values must not collide on the wire — the
    /// control for `update_progress`'s own assertion above (a transposition
    /// or an endianness bug that happened to read back `0.25` correctly would
    /// still pass that test alone).
    #[test]
    fn add_and_update_progress_carry_genuinely_different_bytes_for_different_progress() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload: low, .. } = proto.encode_boss_event_update_progress(id(), 0.1)
        else {
            panic!("must emit a Send");
        };
        let ServerDirective::Send { payload: high, .. } = proto.encode_boss_event_update_progress(id(), 0.9)
        else {
            panic!("must emit a Send");
        };
        assert_ne!(low, high);
    }
}

#[cfg(test)]
mod play_ping_request_tests {
    use lodestone_core::State;
    use lodestone_server::{ServerBound, ServerProtocol};

    use super::V770ServerProtocol;
    use crate::packet_ids::play;

    /// `ServerboundPingRequestPacket`: a single big-endian `i64`. Bytes are
    /// hand-built rather than round-tripped through this crate's own encoder — a
    /// symmetric transposition/endianness bug would otherwise pass against
    /// itself — and the value is non-zero/non-palindromic so a byte-order
    /// mistake cannot coincidentally read back correctly.
    #[test]
    fn decode_play_ping_request_lifts_the_time() {
        let proto = V770ServerProtocol;
        let body = 0x0102_0304_0506_0708_i64.to_be_bytes().to_vec();
        assert_eq!(
            proto.decode(State::Play, play::serverbound::PING_REQUEST, &body),
            ServerBound::PingRequest {
                time: 0x0102_0304_0506_0708,
            },
        );
    }

    /// A malformed (short) frame must not construct a variant with a
    /// truncated/zeroed time — the control for the assertion above: without
    /// it, an implementation that always returned `PingRequest { time: 0 }`
    /// regardless of the payload would also pass the happy-path test.
    #[test]
    fn decode_play_ping_request_rejects_a_short_frame() {
        let proto = V770ServerProtocol;
        let short = vec![1, 2, 3];
        assert_eq!(
            proto.decode(State::Play, play::serverbound::PING_REQUEST, &short),
            ServerBound::Ignored,
        );
    }

    /// A valid `pong` is an explicit acknowledgement boundary rather than an
    /// ignored frame. Its raw fixed-width id must survive decoding so the
    /// connection can deliberately consume it without inventing state.
    #[test]
    fn decode_play_pong_lifts_the_big_endian_id() {
        let proto = V770ServerProtocol;
        let body = 0x0102_0304_i32.to_be_bytes().to_vec();
        assert_eq!(
            proto.decode(State::Play, play::serverbound::PONG, &body),
            ServerBound::Pong { id: 0x0102_0304 },
        );
    }

    /// The acknowledgement needs its entire four-byte body. This control
    /// distinguishes the valid no-op above from an arm that lifted a constant
    /// id regardless of the received frame.
    #[test]
    fn decode_play_pong_rejects_a_short_frame() {
        let proto = V770ServerProtocol;
        assert_eq!(
            proto.decode(State::Play, play::serverbound::PONG, &[1, 2, 3]),
            ServerBound::Ignored,
        );
    }
}

#[cfg(test)]
mod seen_advancements_tests {
    use lodestone_core::{Reader, State};
    use lodestone_server::{AdvancementManager, ServerBound, ServerDirective, ServerProtocol};
    use uuid::Uuid;

    use super::V770ServerProtocol;
    use crate::packet_ids::play;

    /// The body is deliberately raw rather than produced by the client
    /// adapter: action 0, then the independently counted UTF-8 identifier.
    /// This catches a decoder that accepts the right action but consumes the
    /// wrong string framing.
    #[test]
    fn seen_advancements_opened_tab_lifts_from_raw_wire_bytes() {
        let proto = V770ServerProtocol;
        let body = [
            0, // OPENED_TAB
            20, // byte length of minecraft:story/root
            b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':', b's', b't', b'o', b'r',
            b'y', b'/', b'r', b'o', b'o', b't',
        ];
        assert_eq!(
            proto.decode(State::Play, play::serverbound::SEEN_ADVANCEMENTS, &body),
            ServerBound::SeenAdvancements {
                tab: Some("minecraft:story/root".to_owned()),
            },
        );
    }

    /// The close action carries no identifier; accepting an extra byte would
    /// hide a stream framing error in the packet immediately after it.
    #[test]
    fn seen_advancements_close_has_no_identifier_or_trailing_bytes() {
        let proto = V770ServerProtocol;
        assert_eq!(
            proto.decode(State::Play, play::serverbound::SEEN_ADVANCEMENTS, &[1]),
            ServerBound::SeenAdvancements { tab: None },
        );
        assert_eq!(
            proto.decode(State::Play, play::serverbound::SEEN_ADVANCEMENTS, &[1, 0]),
            ServerBound::Ignored,
        );
        assert_eq!(
            proto.decode(State::Play, play::serverbound::SEEN_ADVANCEMENTS, &[2]),
            ServerBound::Ignored,
        );
    }

    /// The production server consumes the lifted selection through
    /// `AdvancementManager` and emits this directive. Check both ends of that
    /// seam here so the new state cannot become a write-only counter.
    #[test]
    fn seen_advancements_selection_reaches_the_clientbound_tab_directive() {
        let proto = V770ServerProtocol;
        let mut manager = AdvancementManager::builtin();
        let selected = manager.select_tab(
            Uuid::nil(),
            Some("minecraft:adventure/root".to_owned()),
        );
        let ServerDirective::Send { packet_id, payload } =
            proto.encode_select_advancements_tab(selected.as_deref())
        else {
            panic!("selected advancement tab must be sent to the client");
        };
        assert_eq!(packet_id, play::clientbound::SELECT_ADVANCEMENTS_TAB);
        let mut reader = Reader::new(&payload);
        assert!(reader.bool().expect("tab-present flag"));
        assert_eq!(
            reader.string(32767).expect("tab identifier"),
            "minecraft:adventure/root"
        );
        assert!(reader.ensure_empty().is_ok(), "selection body has no trailing bytes");
    }
}

/// the goat class's own has-left-horn accessor/`DATA_HAS_RIGHT_HORN` at indices 19/20 — the
/// same census-premise-plus-encode-exactness shape `index_thirteen_tests`
/// already uses for its own claimed index.
#[cfg(test)]
mod goat_horns_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{
        METADATA_IDX_GOAT_HAS_LEFT_HORN, METADATA_IDX_GOAT_HAS_RIGHT_HORN, METADATA_SER_BOOLEAN,
        V770ServerProtocol,
    };

    const INDEX_DUMP: &str = include_str!("../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    /// `METADATA_IDX_GOAT_HAS_LEFT_HORN`/`_RIGHT_HORN` name the accessors the
    /// jar really puts at indices 19/20, both `BOOLEAN`.
    #[test]
    fn goat_horn_indices_match_the_jar_dump() {
        let (left_index, left_ser) = dump_row("Goat.DATA_HAS_LEFT_HORN");
        assert_eq!(left_index, METADATA_IDX_GOAT_HAS_LEFT_HORN);
        assert_eq!(left_ser, METADATA_SER_BOOLEAN);
        let (right_index, right_ser) = dump_row("Goat.DATA_HAS_RIGHT_HORN");
        assert_eq!(right_index, METADATA_IDX_GOAT_HAS_RIGHT_HORN);
        assert_eq!(right_ser, METADATA_SER_BOOLEAN);
    }

    /// The encoder writes both fields, in order, each with the `BOOLEAN`
    /// serializer id, end to end through `encode_set_entity_data` — with the
    /// two bools **deliberately different** (`false`/`true`) so a
    /// transposition of the pair cannot survive this assertion, per
    /// `DESIGN.md`'s own warning about adjacent same-typed fields.
    #[test]
    fn goat_horns_encode_at_indices_nineteen_and_twenty() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } = proto.encode_set_entity_data(
            11,
            &[MetadataField::GoatHorns { has_left: false, has_right: true }],
        ) else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 11);
        assert_eq!(r.u8().expect("left horn index"), METADATA_IDX_GOAT_HAS_LEFT_HORN);
        assert_eq!(r.var_i32().expect("left horn serializer"), METADATA_SER_BOOLEAN);
        assert!(!r.bool().expect("left horn value"), "has_left was false");
        assert_eq!(r.u8().expect("right horn index"), METADATA_IDX_GOAT_HAS_RIGHT_HORN);
        assert_eq!(r.var_i32().expect("right horn serializer"), METADATA_SER_BOOLEAN);
        assert!(r.bool().expect("right horn value"), "has_right was true");
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }
}

/// the axolotl class's own playing-dead accessor at index 19 — the same census-premise-plus-
/// encode-exactness shape [`goat_horns_tests`] already uses for its own
/// claimed index.
#[cfg(test)]
mod axolotl_playing_dead_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{METADATA_IDX_AXOLOTL_PLAYING_DEAD, METADATA_SER_BOOLEAN, V770ServerProtocol};

    const INDEX_DUMP: &str = include_str!("../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    #[test]
    fn axolotl_playing_dead_index_matches_the_jar_dump() {
        let (index, ser) = dump_row("Axolotl.DATA_PLAYING_DEAD");
        assert_eq!(index, METADATA_IDX_AXOLOTL_PLAYING_DEAD);
        assert_eq!(ser, METADATA_SER_BOOLEAN);
    }

    #[test]
    fn playing_dead_encodes_at_index_nineteen() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } =
            proto.encode_set_entity_data(11, &[MetadataField::PlayingDead(true)])
        else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 11);
        assert_eq!(r.u8().expect("index"), METADATA_IDX_AXOLOTL_PLAYING_DEAD);
        assert_eq!(r.var_i32().expect("serializer"), METADATA_SER_BOOLEAN);
        assert!(r.bool().expect("value"), "true was pushed");
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }
}

/// the camel class's own dash accessor at index 19 — the same census-premise-plus-encode-exactness
/// shape [`goat_horns_tests`] already uses for its own claimed index.
#[cfg(test)]
mod camel_dash_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{METADATA_IDX_CAMEL_DASH, METADATA_SER_BOOLEAN, V770ServerProtocol};

    const INDEX_DUMP: &str = include_str!("../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, i32) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok.next().expect("serializer column").parse().expect("i32");
            if owner == owner_field {
                return (index, serializer);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    #[test]
    fn camel_dash_index_matches_the_jar_dump() {
        let (index, ser) = dump_row("Camel.DASH");
        assert_eq!(index, METADATA_IDX_CAMEL_DASH);
        assert_eq!(ser, METADATA_SER_BOOLEAN);
    }

    #[test]
    fn dash_encodes_at_index_nineteen() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } =
            proto.encode_set_entity_data(11, &[MetadataField::Dash(true)])
        else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 11);
        assert_eq!(r.u8().expect("index"), METADATA_IDX_CAMEL_DASH);
        assert_eq!(r.var_i32().expect("serializer"), METADATA_SER_BOOLEAN);
        assert!(r.bool().expect("value"), "true was pushed");
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }
}

/// the sniffer class's own state accessor at index 18, serializer 35 — the same census-
/// premise-plus-encode-exactness shape [`goat_horns_tests`] already uses for
/// its own claimed index, plus a check that the wire value is the real
/// the sniffer class's own state ordinal rather than a crate-local renumbering.
#[cfg(test)]
mod sniffer_state_tests {
    use lodestone_core::Reader;
    use lodestone_server::{MetadataField, ServerDirective, ServerProtocol};

    use super::{METADATA_IDX_SNIFFER_STATE, METADATA_SER_SNIFFER_STATE, V770ServerProtocol};

    const INDEX_DUMP: &str = include_str!("../tests/support/entity_data_index_jvm.txt");

    fn dump_row(owner_field: &str) -> (u8, &'static str) {
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("u8");
            let owner = tok.next().expect("owner.FIELD column");
            let _serializer_id: i32 = tok.next().expect("serializer column").parse().expect("i32");
            let serializer_name = tok.next().expect("serializer name column");
            if owner == owner_field {
                return (index, serializer_name);
            }
        }
        panic!("{owner_field} is not in the jar dump — read the dump before changing the constant")
    }

    #[test]
    fn sniffer_state_index_matches_the_jar_dump() {
        let (index, serializer_name) = dump_row("Sniffer.DATA_STATE");
        assert_eq!(index, METADATA_IDX_SNIFFER_STATE);
        assert_eq!(serializer_name, "SNIFFER_STATE");
    }

    /// The same index also claims the armadillo class's own armadillo-state accessor under a
    /// *different* serializer id — the tell that species alone cannot
    /// disambiguate this index and the serializer id is load-bearing.
    #[test]
    fn index_eighteen_also_claims_a_different_armadillo_serializer() {
        let (armadillo_index, armadillo_serializer_name) = dump_row("Armadillo.ARMADILLO_STATE");
        assert_eq!(armadillo_index, METADATA_IDX_SNIFFER_STATE);
        assert_eq!(armadillo_serializer_name, "ARMADILLO_STATE");
    }

    #[test]
    fn sniffer_state_encodes_at_index_eighteen_as_a_real_ordinal() {
        let proto = V770ServerProtocol;
        let ServerDirective::Send { payload, .. } =
            proto.encode_set_entity_data(11, &[MetadataField::SnifferState(5)])
        else {
            panic!("encode_set_entity_data must emit a Send");
        };
        let mut r = Reader::new(&payload);
        assert_eq!(r.var_i32().expect("entity id"), 11);
        assert_eq!(r.u8().expect("index"), METADATA_IDX_SNIFFER_STATE);
        assert_eq!(r.var_i32().expect("serializer"), METADATA_SER_SNIFFER_STATE);
        // `5` is the sniffer class's own state.DIGGING`'s real ordinal, not `0`/`1` — a
        // wrong-serializer or off-by-one bug would still pass a `true`/`false`
        // shaped assertion, so this pins the actual integer.
        assert_eq!(r.var_i32().expect("state ordinal"), 5);
        assert_eq!(r.u8().expect("terminator"), 0xFF);
        assert!(r.ensure_empty().is_ok(), "no trailing bytes");
    }
}
