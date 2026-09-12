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
//! for every column in the initial view, entity
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
    Abilities, ChunkColumn as ServerChunkColumn, ChunkEncoder, ColumnLightSettlement,
    EntitySnapshot, HOTBAR_SIZE, RetainedLightStatus,
    MOTION_BLOCKING_HEIGHTMAP_TYPE_ID, MerchantOfferOut, MetadataField, PlayerListing,
    ResourcePackPush, ServerBound, ServerDirective, ServerProtocol,
    WorldBorder, WorldgenScope,
};
use lodestone_server::dimension::Dimension;
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
    LightData, LightProperties, LightStorage, Neighbourhood, NibbleArray, compute_column_light,
    compute_column_light_for_initial_chunk, compute_column_light_with_neighbours,
    compute_column_lights_with_neighbours_and_storage,
    compute_column_light_with_neighbours_for_initial_chunk,
    compute_column_light_with_neighbours_seeded,
};
use lodestone_data::block::Block;
use lodestone_data::item::Item;
use uuid::Uuid;

// Test-only since the string→id resolver moved into `lodestone-data`
// (`block_states::state_id`): this module's production code no longer reads the
// forward table at all — `resolve_state_id` and `air_id` are one-line wrappers —
// while `stone_id` and the resolver's own gates below still walk it by name
// rather than trusting a literal id.
#[cfg(test)]
use lodestone_data::block_states::{block_name, properties};
#[cfg(test)]
use lodestone_world::PaletteKind;
use lodestone_data::entity_type::EntityType;
use lodestone_data::menus::{MenuId, menu_id};
use lodestone_data::mob_effects::{MobEffectId, mob_effect_id, mob_effect_name_for};
use lodestone_data::sound_events::{SoundEventId, sound_event_id};
use serde::Serialize;
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

// The facade stays here so `V770ServerProtocol` and its trait implementations
// remain at the historical path. Phase-specific helpers and tests live in
// private sibling modules and are re-exported only inside this module.
mod clientbound;
mod chunk;
mod registry;
mod serverbound;
#[cfg(test)]
mod tests;

use clientbound::*;
use chunk::*;
use registry::*;
use serverbound::*;

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
const METADATA_IDX_SHARED_FLAGS: u8 = 0;

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
/// states — the typed lookup makes a name this table has never had (a stale or
/// ahead-of-version generated table) fail loudly here. Used by
/// [`V770ServerProtocol::encode_explode`]
/// to build the `Holder<SoundEvent>` **registry-reference** encoding a real
/// vanilla server sends for this sound — see that method's own doc comment
/// for why that is the byte-accurate choice, verified against
/// vanilla's own codec library's own holder's decompiled encode arm, not the decoder's own
/// (weaker) direct-literal-name path.
fn explosion_sound_registry_id() -> SoundEventId {
    sound_event_id("minecraft:entity.generic.explode")
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
/// The data census indexes names once and validates its result. The `None` is
/// load-bearing — see [`V770ServerProtocol::encode_sound`].
fn sound_event_registry_id(name: &str) -> Option<SoundEventId> {
    sound_event_id(name)
}

/// The `minecraft:particle_type` registry id for `name`, or `None`
/// for an unknown one.
///
/// Named "simple" as a warning rather than a filter: this crate has no census of
/// *which* particle types carry option bytes, so the id it returns is only safe
/// to send for an argument-less `SimpleParticleType`. Every producer in
/// `lodestone_server::effects` is one; a future option-carrying particle needs
/// the options written too, not just this id.
fn simple_particle_registry_id(
    name: &str,
) -> Option<lodestone_data::particle_types::ParticleTypeId> {
    static INDEX: std::sync::OnceLock<
        std::collections::HashMap<&'static str, lodestone_data::particle_types::ParticleTypeId>,
    > =
        std::sync::OnceLock::new();
    INDEX
        .get_or_init(|| {
            (0..)
                .map_while(|id| {
                    let id = lodestone_data::particle_types::ParticleTypeId::new(id)?;
                    Some((lodestone_data::particle_types::particle_type_name(id), id))
                })
                .collect()
        })
        .get(name)
        .copied()
}

/// Resolves a biome id string ([`ServerChunkColumn::biome_state`]'s
/// vocabulary) to the holder id in the registry fixture sent to the client.
/// Falls back to `minecraft:plains` for any name outside that registry.
///
/// # Panics
/// Panics if the captured registry has no `"minecraft:plains"` entry.
pub fn biome_registry_id(name: &str) -> u32 {
    static BIOME_REGISTRY_IDS: std::sync::OnceLock<std::collections::HashMap<String, u32>> =
        std::sync::OnceLock::new();
    let ids = BIOME_REGISTRY_IDS.get_or_init(|| {
        crate::registry_data_fixtures::biome_registry_names()
            .into_iter()
            .enumerate()
            .map(|(id, name)| (name, id as u32))
            .collect()
    });
    ids.get(name).copied().unwrap_or_else(|| {
        ids.get("minecraft:plains")
            .copied()
            .expect("biome registry missing minecraft:plains")
    })
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

/// Resolves a built-in item key for a protocol-776 writer. A custom or future
/// key has no id in this build's fixed registry and must not be substituted.
fn item_registry_id_by_name(name: &str) -> Option<i32> {
    Item::from_name(name).map(|item| i32::from(item.registry_id()))
}

/// Validates a raw item holder at a packet boundary before using the built-in
/// registry's total name accessor.
fn item_from_wire_id(raw: i32) -> Option<Item> {
    u16::try_from(raw).ok().and_then(Item::from_registry_id)
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
        | StatType::Dropped => item_registry_id_by_name(value)?,
        StatType::Killed | StatType::KilledBy => {
            i32::from(EntityType::from_name(value)?.registry_id())
        }
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
        ServerSlotDisplay::Item(item) => match item_registry_id_by_name(&item.to_string()) {
            Some(id) => {
                w.var_i32(slot_display::ITEM);
                w.var_i32(id);
            }
            None => w.var_i32(slot_display::EMPTY),
        },
        ServerSlotDisplay::Stack { item, count } => match item_registry_id_by_name(&item.to_string()) {
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
                    .filter_map(|item| item_registry_id_by_name(&item.to_string()))
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
#[derive(Debug, Serialize, Default)]
struct JsonTextComponent {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    translate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fallback: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    with: Vec<Self>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extra: Vec<Self>,
}

impl JsonTextComponent {
    fn literal(text: &str) -> Self {
        Self {
            text: Some(text.to_owned()),
            ..Self::default()
        }
    }
}

fn text_to_json(text: &Text) -> JsonTextComponent {
    let mut component = match &text.content {
        TextContent::Literal(literal) => JsonTextComponent::literal(literal),
        TextContent::Translate {
            key,
            with,
            fallback,
        } => JsonTextComponent {
            translate: Some(key.clone()),
            fallback: fallback.clone(),
            with: with.iter().map(text_to_json).collect(),
            ..JsonTextComponent::default()
        },
    };
    component.extra = text.extra.iter().map(text_to_json).collect();
    component
}

#[derive(Debug, Serialize)]
struct StatusResponseDocument {
    description: JsonTextComponent,
    players: StatusPlayers,
    version: StatusVersion,
    #[serde(skip_serializing_if = "Option::is_none")]
    favicon: Option<String>,
    #[serde(
        rename = "enforcesSecureChat",
        skip_serializing_if = "is_false"
    )]
    enforces_secure_chat: bool,
}

#[derive(Debug, Serialize)]
struct StatusPlayers {
    max: i32,
    online: i32,
    sample: Vec<StatusPlayerSample>,
}

#[derive(Debug, Serialize)]
struct StatusPlayerSample {
    id: String,
    name: String,
}

#[derive(Debug, Serialize)]
struct StatusVersion {
    name: &'static str,
    protocol: i32,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn encode_json<T: Serialize>(value: &T, context: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|error| {
        panic!("{context}: {error}");
    })
}

fn text_to_json_string(text: &Text) -> String {
    encode_json(&text_to_json(text), "text component JSON serialization")
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
    let document = StatusResponseDocument {
        description: JsonTextComponent::literal(description),
        players: StatusPlayers {
            max: players_max,
            online: players_online,
            sample: sample
                .iter()
                .map(|(id, name)| StatusPlayerSample {
                    id: id.to_string(),
                    name: name.clone(),
                })
                .collect(),
        },
        version: StatusVersion {
            name: MINECRAFT_VERSION,
            protocol: crate::PROTOCOL,
        },
        favicon: favicon_png
            .map(|png| format!("data:image/png;base64,{}", base64_encode(png))),
        enforces_secure_chat,
    };
    let json = encode_json(&document, "status response JSON serialization");
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
        Some(stack) => match item_registry_id_by_name(&stack.item.to_string()) {
            Some(id) => {
                w.var_i32(i32::try_from(stack.count).unwrap_or(i32::MAX));
                w.var_i32(id);
                write_item_component_patch(w, &stack.components);
            }
            None => w.var_i32(0),
        },
    }
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
        let component = lodestone_data::data_component_types::component_type_id(
            "minecraft:custom_data",
        )
        .expect("generated data-component-type table has custom_data");
        w.var_i32(component.raw());
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
    let component = lodestone_data::data_component_types::component_type_id(
        "minecraft:writable_book_content",
    )
    .expect("generated data-component-type table has writable_book_content");
    w.var_i32(component.raw());
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
    let component = lodestone_data::data_component_types::component_type_id(
        "minecraft:written_book_content",
    )
    .expect("generated data-component-type table has written_book_content");
    w.var_i32(component.raw());
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
    match item_registry_id_by_name(&item.to_string()) {
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
    effect_id: MobEffectId,
    amplifier: u32,
    duration_ticks: i32,
    ambient: bool,
    visible: bool,
    show_icon: bool,
    blend: bool,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(entity_id);
    w.var_i32(effect_id.registry_id());
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
fn encode_remove_mob_effect_body(entity_id: i32, effect_id: MobEffectId) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(entity_id);
    w.var_i32(effect_id.registry_id());
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
/// An `entity_type` with no match in this version's fixed registry (a typo, a
/// custom key, or a key from a version this table does not cover) keeps the
/// established recoverable fallback. The branch deliberately names
/// [`EntityType::AcaciaBoat`] rather than treating `0` as an interchangeable
/// integer: a session-synchronized/custom registry must remain a
/// `ResourceKey` at the protocol seam, while this writer only accepts a
/// validated built-in type at the final VarInt boundary.
fn encode_add_entity_body(entity: &EntitySnapshot) -> Vec<u8> {
    let type_id = EntityType::from_resource_key(&entity.entity_type)
        .unwrap_or(EntityType::AcaciaBoat)
        .registry_id();
    let mut w = Writer::default();
    w.var_i32(entity.id);
    w.uuid(entity.uuid);
    w.var_i32(i32::from(type_id));
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
/// format, driving `lodestone-server`'s [`ServerProtocol`] seam.
///
/// Holds no per-connection state: unlike [`V770Adapter`] (which tracks the
/// current dimension's [`ChunkShape`] across `login`/`respawn`), the server
/// always joins into the overworld today, so the shape is a constant rather
/// than connection state. A future respawn/dimension-change feature would
/// need to thread shape through here the same way the adapter does.
#[derive(Debug, Clone, Copy, Default)]
pub struct V770ServerProtocol;

impl V770ServerProtocol {
    /// Computes one initial Nether light snapshot from the columns admitted by
    /// the caller and retained block-light levels from earlier admissions.
    ///
    /// The centre's terrain is the only fresh emission source.  Previously
    /// admitted neighbours are supplied as opaque-aware terrain and their
    /// retained block-light values seed the flood.  A missing neighbour is not
    /// represented in the neighbourhood, so it remains a real seam barrier
    /// instead of leaking terrain emissions into the centre.  Allocation masks
    /// are owned by the admission store and are deliberately not inferred from
    /// this value-only result.
    #[must_use]
    pub fn compute_initial_column_light_with_neighbours_seeded<F>(
        &self,
        center: &ServerChunkColumn,
        neighbours: &[(i32, i32, ServerChunkColumn)],
        dimension: Dimension,
        seed: F,
    ) -> ColumnLight
    where
        F: Fn(i32, i32, usize, i32, usize) -> u8,
    {
        let shape = shape_for_dimension(dimension);
        let center_world = build_world_column(&shape, center);
        let neighbour_world = neighbours
            .iter()
            .map(|(_, _, neighbour)| build_world_column(&shape, neighbour))
            .collect::<Vec<_>>();
        let mut neighbourhood = Neighbourhood::new(&center_world);
        for ((dx, dz, _), neighbour) in neighbours.iter().zip(&neighbour_world) {
            neighbourhood = neighbourhood.with(*dx, *dz, neighbour);
        }
        compute_column_light_with_neighbours_seeded(
            &neighbourhood,
            &V770LightProps {
                has_skylight: dimension.has_skylight(),
            },
            initial_full_sky_sections(dimension),
            seed,
            |dx, dz| (dx, dz) == (0, 0),
        )
    }

    /// Computes and returns all nine retained light snapshots produced by one
    /// shared three-by-three admission. The array uses row-major offset slots.
    pub fn compute_initial_column_lights_with_neighbours_and_storage_in_dimension(
        &self,
        column: &lodestone_server::ChunkColumn,
        neighbours: &[(i32, i32, lodestone_server::ChunkColumn)],
        stored: &[Option<&lodestone_world::ColumnLight>; 9],
        dimension: Dimension,
    ) -> Option<[lodestone_world::ColumnLight; 9]> {
        let shape = shape_for_dimension(dimension);
        let center = build_world_column(&shape, column);
        let statuses = std::array::from_fn(|slot| {
            stored[slot].map(|_| RetainedLightStatus::CentreSettled)
        });
        Some(compute_served_initial_lights_with_neighbours_and_storage(
            &center,
            &shape,
            neighbours,
            stored,
            &statuses,
            dimension,
        ))
    }
}

// ---------------------------------------------------------------------------
// Configuration-phase `registry_data` payloads
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
                        hand: lodestone_model::Hand::from_wire_ordinal(s.hand)
                            .unwrap_or(lodestone_model::Hand::Main),
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
                        mode: lodestone_model::CommandBlockMode::from_wire_ordinal(mode),
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
                    reason: text_to_json_string(reason),
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

    fn try_encode_chunk_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ServerChunkColumn,
        dimension: Dimension,
    ) -> Result<ServerDirective, lodestone_server::ChunkEncodeError> {
        ChunkEncoder::try_encode_chunk_in_dimension(self, cx, cz, column, dimension)
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

    /// The isolated fallback computation: [`build_world_column`] resolves state
    /// ids, then [`compute_served_light`] floods one column. The server calls
    /// [`Self::compute_column_light_with_neighbours`] for this family, so this
    /// remains for one-column callers and must not become a second source of
    /// cross-column policy.
    fn compute_column_light(&self, column: &ServerChunkColumn) -> Option<ColumnLight> {
        // Through `shape_for_column` for the same reason `encode_chunk` is: a
        // `light_update` that framed a Nether column against the overworld's 24
        // sections would carry a different section count than the chunk packet that
        // preceded it, which is the one thing this method's own doc promises cannot
        // happen.
        let shape = shape_for_column(column);
        Some(compute_served_light(
            &build_world_column(&shape, column),
            Dimension::Overworld,
        ))
    }

    fn compute_column_light_in_dimension(
        &self,
        column: &ServerChunkColumn,
        dimension: Dimension,
    ) -> Option<ColumnLight> {
        let shape = shape_for_dimension(dimension);
        Some(compute_served_light(&build_world_column(&shape, column), dimension))
    }

    fn uses_cross_column_light(&self) -> bool {
        true
    }

    fn retains_initial_column_light(&self) -> bool {
        true
    }

    fn try_encode_chunk_with_neighbours(
        &self,
        cx: i32,
        cz: i32,
        column: &ServerChunkColumn,
        neighbours: &[(i32, i32, ServerChunkColumn)],
    ) -> Result<ServerDirective, lodestone_server::ChunkEncodeError> {
        self.try_encode_chunk_with_neighbours_in_dimension(
            cx,
            cz,
            column,
            neighbours,
            Dimension::Overworld,
        )
    }

    fn try_encode_chunk_with_neighbours_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ServerChunkColumn,
        neighbours: &[(i32, i32, ServerChunkColumn)],
        dimension: Dimension,
    ) -> Result<ServerDirective, lodestone_server::ChunkEncodeError> {
        let shape = shape_for_dimension(dimension);
        let world_column = build_world_column(&shape, column);
        let light = if let Some(retained) = column
            .retained_light()
            .filter(|_| column.retained_light_status() == Some(RetainedLightStatus::CentreSettled))
            .filter(|light| light.light_section_count() == shape.section_count + 2)
        {
            let mut retained = retained.clone();
            if dimension == Dimension::End {
                let neighbour_columns = neighbours
                    .iter()
                    .map(|(_, _, neighbour)| build_world_column(&shape, neighbour))
                    .collect::<Vec<_>>();
                let storage = initial_end_light_storage_sections_with_prior(
                    &world_column,
                    &neighbour_columns,
                    Some(&retained),
                );
                restore_end_retained_storage_allocation(&mut retained, &storage);
                restore_end_retained_storage_gaps(&mut retained);
            }
            retained
        } else {
            compute_served_initial_light_with_neighbours(
                &world_column,
                &shape,
                neighbours,
                dimension,
            )
        };
        let payload = encode_column_body(cx, cz, &shape, &world_column, &light, column);
        Ok(ServerDirective::Send {
            packet_id: play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
            payload,
        })
    }

    fn compute_initial_column_light_with_neighbours_in_dimension(
        &self,
        column: &ServerChunkColumn,
        neighbours: &[(i32, i32, ServerChunkColumn)],
        dimension: Dimension,
    ) -> Option<ColumnLight> {
        let shape = shape_for_dimension(dimension);
        let world_column = build_world_column(&shape, column);
        Some(compute_served_initial_light_with_neighbours(
            &world_column,
            &shape,
            neighbours,
            dimension,
        ))
    }

    fn compute_initial_column_lights_with_neighbours_in_dimension(
        &self,
        column: &ServerChunkColumn,
        neighbours: &[(i32, i32, ServerChunkColumn)],
        dimension: Dimension,
    ) -> Option<ColumnLightSettlement> {
        let mut stored: [Option<&ColumnLight>; 9] = [None; 9];
        let mut statuses: [Option<RetainedLightStatus>; 9] = [None; 9];
        stored[4] = column.retained_light();
        statuses[4] = column.retained_light_status();
        for &(dx, dz, ref neighbour) in neighbours {
            let slot = ((dz + 1) * 3 + (dx + 1)) as usize;
            if slot < stored.len() {
                stored[slot] = neighbour.retained_light();
                statuses[slot] = neighbour.retained_light_status();
            }
        }
        let shape = shape_for_dimension(dimension);
        let center = build_world_column(&shape, column);
        let mut lights = compute_served_initial_lights_with_neighbours_and_storage(
            &center,
            &shape,
            neighbours,
            &stored,
            &statuses,
            dimension,
        );
        if dimension == Dimension::End {
            // A newly admitted dependency needs the same sparse allocation
            // shape as the complete footprint, but a retained dependency
            // snapshot remains its own light-engine result. Do not replace a
            // retained layer merely because this admission selected another
            // centre.
            let neighbour_world = neighbours
                .iter()
                .map(|(_, _, neighbour)| build_world_column(&shape, neighbour))
                .collect::<Vec<_>>();
            let admitted_columns = std::iter::once(center.clone())
                .chain(neighbour_world.iter().cloned())
                .collect::<Vec<_>>();
            for ((dx, dz, _), dependency) in neighbours.iter().zip(&neighbour_world) {
                let slot = ((*dz + 1) * 3 + (*dx + 1)) as usize;
                if stored[slot].is_none() {
                    let storage = initial_end_light_storage_sections(dependency, &admitted_columns);
                    normalize_initial_chunk_light(
                        &mut lights[slot],
                        dimension,
                        Some(&storage),
                    );
                }
            }
        } else {
            for slot in 0..lights.len() {
                if slot != 4 && stored[slot].is_none() {
                    lights[slot] = empty_light_storage_like(&lights[slot]);
                }
            }
        }
        let dependency_lights = neighbours.iter().filter_map(|&(dx, dz, _)| {
            if (dx, dz) == (0, 0)
                || !(-1..=1).contains(&dx)
                || !(-1..=1).contains(&dz)
            {
                return None;
            }
            let slot = ((dz + 1) * 3 + (dx + 1)) as usize;
            Some((dx, dz, lights[slot].clone()))
        });
        ColumnLightSettlement::with_neighbours(lights[4].clone(), dependency_lights)
    }

    fn compute_column_light_with_neighbours(
        &self,
        column: &ServerChunkColumn,
        neighbours: &[(i32, i32, ServerChunkColumn)],
    ) -> Option<ColumnLight> {
        self.compute_column_light_with_neighbours_in_dimension(
            column,
            neighbours,
            Dimension::Overworld,
        )
    }

    fn compute_column_light_with_neighbours_in_dimension(
        &self,
        column: &ServerChunkColumn,
        neighbours: &[(i32, i32, ServerChunkColumn)],
        dimension: Dimension,
    ) -> Option<ColumnLight> {
        Some(compute_served_light_with_neighbours(column, neighbours, dimension))
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
        let Some(type_id) =
            lodestone_data::block_entity_types::block_entity_type_id(block_entity_type)
        else {
            return ServerDirective::None;
        };
        let mut body = Writer::default();
        if write_network_nbt(&mut body, nbt).is_err() {
            return ServerDirective::None;
        }
        let mut w = Writer::default();
        w.i64(pack_block_pos(pos.x, pos.y, pos.z));
        w.var_i32(type_id.raw() as i32);
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
                MetadataField::SharedFlags(flags) => {
                    w.u8(METADATA_IDX_SHARED_FLAGS);
                    w.var_i32(METADATA_SER_BYTE);
                    w.i8(*flags as i8);
                }
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
    /// resolved by name via [`lodestone_data::sound_events::sound_event_id`]
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
        w.var_i32(sound_id.raw() + 1); // Holder::REFERENCE encoding: registryId + 1.
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
        w.var_i32(registry_id.raw() + 1); // Holder::REFERENCE: registryId + 1.
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
        w.var_i32(particle_id.raw());
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
