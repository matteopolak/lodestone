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
//! (empty) configuration phase, the play join sequence (join game, default
//! spawn, initial teleport, chunk-cache center), `level_chunk_with_light`
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

use std::collections::HashMap;

use lodestone_core::{Ctx, Decode, Encode, Nbt, Reader, Writer, write_network_nbt};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, Difficulty, ItemStack};
use lodestone_server::{
    ChunkColumn as ServerChunkColumn, EntitySnapshot, HOTBAR_SIZE, ServerBound, ServerDirective,
    ServerProtocol,
};
use lodestone_world::{ChunkColumn as WorldChunkColumn, ChunkSection, ColumnLight, Heightmaps};
use uuid::Uuid;

use lodestone_data::block_states::{block_name, properties};
use lodestone_data::entity_types::entity_type_id;
use lodestone_data::items::{item_id, item_name};
use lodestone_data::menus::menu_id;
use lodestone_data::mob_effects::mob_effect_name;
use crate::packet_ids::{configuration, handshaking, login, play};
use crate::packets::chunk::ChunkShape;
use crate::packets::common::{
    BrandPayload, ClientInformation, KeepAlive, PingRequest, Pong, ResourcePackResponse,
    TeleportToEntity,
};
use crate::packets::configuration::FinishConfiguration;
use crate::packets::entity::{pack_degrees, read_lp_vec3, write_lp_vec3};
use crate::packets::game::{
    AcceptTeleportation, Attack, BlockEntityTagQuery, ChangeDifficultyClientbound,
    ChangeDifficultyServerbound, ChangeGameMode, ChunkBatchReceived, ClientCommand, ClientTickEnd,
    ConfigurationAcknowledged, ContainerButtonClick, ContainerSlotStateChanged, EditBook,
    EntityTagQuery, GameLogin, GameRuleEntry, GameRuleValues, GlobalPos, JigsawGenerate,
    LockDifficulty, MOVE_FLAG_ON_GROUND, MovePlayerPos, MovePlayerPosRot, MovePlayerRot,
    MovePlayerStatusOnly, MoveVehicle, PaddleBoat, PickItemFromBlock, PickItemFromEntity,
    PlaceRecipe, PlayerAction, PlayerCommand, PlayerLoaded, RecipeBookChangeSettings,
    RecipeBookSeenRecipe, RenameItem, SERVERBOUND_ABILITY_FLAG_FLYING, SelectBundleItem,
    SelectTrade, ServerboundPlayerAbilities, SetCarriedItem, SetCommandBlock, SetCommandMinecart,
    SetDefaultSpawnPosition, SetGameRule, SetHealth, SetJigsawBlock,
    SetStructureBlock, SetTestBlock, SignUpdate, Swing, UseItem, UseItemOn,
};
use crate::packets::handshake::Intention;
use crate::packets::login::{LoginFinished, LoginHello};

/// The local player's fixed network entity id, matching
/// [`begin_play`](V770ServerProtocol::begin_play)'s `GameLogin { entity_id:
/// LOCAL_PLAYER_ENTITY_ID, .. }` below — the id a real client latches onto as
/// "self" from the join packet, and therefore also the id
/// `encode_air_supply_update` must tag its metadata update with for a client
/// to apply it to its own local-player state rather than treating it as some
/// other entity's cosmetic bubble state.
const LOCAL_PLAYER_ENTITY_ID: i32 = 1;

/// `Entity`'s `DATA_AIR_SUPPLY_ID` metadata index (`Entity.java:268`,
/// verified index `1` — see `crates/protocol/v770/src/packets/metadata.rs`'s
/// `IDX_AIR_SUPPLY` doc comment) and the `INT` serializer it is registered
/// under
/// (`EntityDataSerializers` registration order; that module's `SER_INT`).
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
/// id. Test-only since issue #363: `build_world_column` used to write this
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

/// Fallback: the block-state id for `minecraft:air`, resolved the same way as
/// [`stone_id`] and for the same reason. Used both as
/// [`resolve_state_id`]'s no-match fallback and, indirectly, wherever this
/// module needs air's id without hardcoding registry id `0`.
fn air_id() -> u32 {
    (0..).find(|&id| block_name(id) == Some("minecraft:air")).expect(
        "generated block-state table has no `minecraft:air` entry — regenerate or fix the table",
    )
}

/// This port's own biome registry id space (issue #405) — index in this
/// **sorted** array is the wire id [`resolve_biome_id`] uses. Regenerable
/// with `awk '/^row\./{print $2}' scripts/worldgen-oracle/biome_java.txt |
/// sort -u`, the exact set `lodestone-worldgen`'s embedded overworld
/// parameter table can ever resolve a column to.
///
/// # Why "sorted by name" and not vanilla's own biome registry order
///
/// Real vanilla assigns biome wire ids by **registration order** in a
/// `minecraft:worldgen/biome` dynamic-registry sync sent during the
/// configuration phase — and this server sends **no registry data at all**
/// yet (`begin_configuration`'s own doc comment: "registry data … are all
/// real vanilla packets this join sequence does not yet need to send"). So
/// there is no existing id space this needs to agree with, and no consumer
/// on the client side to agree with it either — `lodestone-shell` has no
/// `impl BiomeTint` yet (checked directly: zero implementors in
/// `crates/lodestone-shell/src` at the time this was written), so the
/// `ChunkSection::biomes()` container this now populates reaches the wire
/// and nothing downstream reads it back into a name. Any stable,
/// reproducible convention is therefore safe **for now**; alphabetical is
/// the simplest one that needs no extra bookkeeping. **This is provisional**
/// — once real registry-data sync exists (or a render-layers agent needs a
/// name for a wire id), replace this table with that sync's actual order,
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

/// Resolves a canonical block-state string ([`ServerChunkColumn`]'s own
/// vocabulary, e.g. `"minecraft:water[level=0]"`, `"minecraft:stone"`) to its
/// protocol-776 registry id, via a linear scan matching both the block name
/// and its property values against the generated state table —
/// [`stone_id`]/[`air_id`] special-case the two propertyless states this
/// module writes unconditionally; this is the general form needed for
/// [`V770ServerProtocol::encode_block_update`] (which must echo back
/// whatever pre-existing, possibly-propertied state already occupied a
/// placement's neighbour cell) and for [`build_world_column`] (which must
/// carry the real per-block state a whole-column send resolves for every
/// cell). `O(`[`lodestone_data::block_states::STATE_COUNT`]`)` per call —
/// [`build_world_column`] memoizes it per distinct string it sees in a
/// column rather than calling it per block; do not reach for this function
/// itself in a true per-block hot path without the same memoization.
///
/// # Three-tier fallback: exact match, then same-name default, then air
///
/// 1. **Exact match** — name and every property value agree. The common case
///    for anything decoded off a real edit or a fully-qualified generator
///    state (`"minecraft:deepslate[axis=y]"`).
/// 2. **Same block name, any properties** — falls back to the **lowest-id**
///    state sharing `name`. This exists for issue #363's own fluid case:
///    `lodestone-worldgen`'s `OverworldGenerator` writes its default fluid as
///    the bare literal `"minecraft:water"`
///    (`crates/lodestone-worldgen/src/overworld.rs`'s `default_fluid`), with
///    **no `level` property** — and real water has no propertyless state at
///    all (every one of ids `86..=101` carries `level=0..15`).
///
///    "Lowest id" happens to equal water's real default (`86`, `level=0`,
///    `blocks.json`'s own `"default": true` entry for `minecraft:water`,
///    `.cache/mc/26.2/generated/reports/blocks.json`) — **but this is not a
///    general vanilla-registration guarantee**, and was checked, not
///    assumed: a one-off scan of that same `blocks.json` found the
///    lowest-id state disagrees with the marked default for 661 of 797
///    multi-state blocks (e.g. `minecraft:acacia_button`'s default is id
///    `10780`, not its lowest id `10771`). It happens to hold for both
///    fluids this codebase's fallback can currently reach (water: `86`
///    lowest = `86` default; lava: `102` lowest = `102` default) — confirmed
///    directly, not inferred from a pattern. **Do not extend this fallback's
///    coverage to a new bare, property-requiring block name without
///    checking `blocks.json`'s own `"default"` marker for that specific
///    block first** — "lowest id" is a coincidence here, not a rule.
///
///    Before this tier existed, any bare block name for a block that
///    *requires* properties (water chief among them) fell straight to air —
///    the exact trap `CLAUDE.md` and issue #363 flag: "a fix that only
///    thinks about solids will leave \[fluids\] broken and still look like
///    progress."
/// 3. **No name match at all** — falls back to air.
///
/// A block-update confirmation is best-effort feedback (see
/// `docs/block-edit.md`), not the server's authoritative state — that stays
/// in [`ServerChunkColumn`]'s own string form, which this function only
/// reads. Tier 3 exists so a state string this version's table cannot parse
/// back at all (an unknown name, or a property spelling/order drift on a
/// nonexistent variant) degrades to a visibly-wrong confirmation rather than
/// a panic or a corrupted wire id.
fn resolve_state_id(state: &str) -> u32 {
    let (name, raw_props) = match state.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest)),
        None => (state, ""),
    };
    let mut wanted: Vec<(&str, &str)> = if raw_props.is_empty() {
        Vec::new()
    } else {
        raw_props
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .collect()
    };
    wanted.sort_unstable();

    let mut same_name_default: Option<u32> = None;
    for id in 0..lodestone_data::block_states::STATE_COUNT {
        if block_name(id) != Some(name) {
            continue;
        }
        if same_name_default.is_none() {
            same_name_default = Some(id);
        }
        let mut have: Vec<(&str, &str)> = properties(id).unwrap_or(&[]).to_vec();
        have.sort_unstable();
        if have == wanted {
            return id;
        }
    }
    same_name_default.unwrap_or_else(air_id)
}

/// Unpacks vanilla's `BlockPos.asLong` form (the inverse of
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

/// Maps `Direction.get3DDataValue` (`0` down … `5` east) back to a
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
/// `Difficulty.STREAM_CODEC`) to [`Difficulty`], mirroring `V770Adapter`'s
/// own `CHANGE_DIFFICULTY` decode (`adapter.rs`, the clientbound direction of
/// the same wire concept): an out-of-range id decodes to `None` rather than
/// vanilla's `ByIdMap.OutOfBoundsStrategy::WRAP` silently aliasing it to a
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

/// Decodes one serverbound container-click item written as a `HashedStack`
/// (`ByteBufCodecs.optional(HashedStack.ActualItem.STREAM_CODEC)`), the
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
/// The clicked slot/button/click-type fields are decoded (so the reader
/// advances correctly) but not carried into [`ServerBound`] — see that
/// variant's own doc comment for why `changed_slots` alone is what this
/// crate's consumer needs: the client has already run the full `doClick`
/// locally and this packet's changed-slots map **is** its predicted result,
/// not raw button input to re-interpret.
fn decode_container_click(payload: &[u8]) -> Option<ServerBound> {
    let mut r = Reader::new(payload);
    let window_id = r.var_i32().ok()?;
    let state_id = r.var_i32().ok()?;
    let _slot = r.i16().ok()?;
    let _button = r.i8().ok()?;
    let _click_type = r.var_i32().ok()?;
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
        changed_slots,
        carried_item,
    })
}

/// Reads a serverbound `set_creative_mode_slot` item
/// (`ItemStack.OPTIONAL_UNTRUSTED_STREAM_CODEC`, the inverse of the
/// client-side encoder `crate::adapter::write_optional_item_stack`): a VarInt
/// count where `<= 0` means empty, otherwise the item registry id as a
/// VarInt, then an empty `DataComponentPatch` (two VarInt `0`s, added then
/// removed).
///
/// Deliberately **not** the same shape as [`read_hashed_stack`]: that one has
/// a leading presence bool and puts the item id before the count
/// (`HashedStack.ActualItem.STREAM_CODEC`); this one has no presence bool at
/// all — a `count` of zero or less *is* the absence marker
/// (`ItemStack.createOptionalStreamCodec`, verified against
/// `.cache/mc/26.2/src/net/minecraft/world/item/ItemStack.java`) — and puts
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
/// (`ByteBufCodecs.optional(MobEffect.STREAM_CODEC)`, the inverse of
/// `crate::adapter::write_optional_mob_effect`): a bool presence flag, then,
/// only if present, the effect's `minecraft:mob_effect` registry id as a
/// direct VarInt.
///
/// Returns the effect's canonical name on success so a future consumer has
/// something to act on immediately; today's decode arm for `SET_BEACON`
/// still discards it (`ServerBound` has no beacon-effect variant — see
/// this module's `SET_BEACON` decode arm doc comment for why).
fn read_optional_mob_effect(r: &mut Reader) -> Option<Option<&'static str>> {
    if !r.bool().ok()? {
        return Some(None);
    }
    let id = r.var_i32().ok()?;
    Some(Some(mob_effect_name(id)?))
}

/// Packs a block position into vanilla's `BlockPos.asLong` form: `x` in the
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
/// vanilla's `ChunkPos.pack` exactly as `V770Adapter::handle_play`'s
/// `FORGET_LEVEL_CHUNK` decode arm already reads it (`adapter.rs`, the
/// `packed as i32` / `(packed >> 32) as i32` pair).
fn encode_forget_chunk_body(cx: i32, cz: i32) -> Vec<u8> {
    let packed = (i64::from(cx) & 0xFFFF_FFFF) | ((i64::from(cz) & 0xFFFF_FFFF) << 32);
    let mut w = Writer::default();
    w.i64(packed);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `block_update` packet: a packed
/// `BlockPos` long ([`pack_block_pos`]) followed by a VarInt block-state
/// registry id — mirrors `ClientboundBlockUpdatePacket.STREAM_CODEC`
/// (`BlockPos.STREAM_CODEC` composed with `ByteBufCodecs.idMapper(Block
/// .BLOCK_STATE_REGISTRY)`, `ClientboundBlockUpdatePacket.java:14-20`) and
/// this crate's own decode of the same packet in `V770Adapter::handle_play`'s
/// `BLOCK_UPDATE` arm (`adapter.rs`), which reads the identical
/// packed-i64-then-VarInt shape.
fn encode_block_update_body(x: i32, y: i32, z: i32, state_id: u32) -> Vec<u8> {
    let mut w = Writer::default();
    w.i64(pack_block_pos(x, y, z));
    w.var_i32(state_id as i32);
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

/// Hand-written encoder for the clientbound `system_chat` packet, which has no
/// existing struct because it is currently only ever *decoded* (see
/// `V770Adapter::handle_play`'s `SYSTEM_CHAT` arm). Wire layout (mirrors the
/// decode side exactly): a network-form NBT text component (root tag id +
/// payload, no root name — vanilla's `ComponentSerialization.TRUSTED_STREAM_CODEC`),
/// then a big-endian `bool` overlay flag (`false` selects normal chat history,
/// `true` the action-bar overlay).
fn encode_system_chat(message: &str, overlay: bool) -> Vec<u8> {
    let component = Nbt::Compound(vec![("text".to_owned(), Nbt::String(message.to_owned()))]);
    let mut w = Writer::default();
    write_network_nbt(&mut w, &component).expect("plain string NBT component always encodes");
    w.bool(overlay);
    w.into_vec()
}

/// Writes one `ItemStack.OPTIONAL_STREAM_CODEC` value (used by both
/// `container_set_content`'s list/carried entries and `container_set_slot`'s
/// single item): a VarInt count (`<= 0` is the empty stack), then, only if
/// non-empty, the item registry id as a VarInt and an empty
/// `DataComponentPatch` (VarInt `0` added, VarInt `0` removed).
///
/// This is the clientbound twin of `crate::adapter::write_optional_item_stack`
/// (the serverbound `set_creative_mode_slot` encoder), restated here rather
/// than imported: that function is private to its own module, and
/// `adapter.rs` is presently owned by another agent extracting shared
/// plumbing (see this crate's own repo-hazard notes) — not a good time to add
/// a new `pub(crate)` export to it. Both directions genuinely share the same
/// wire shape (`ItemStack.OPTIONAL_STREAM_CODEC` is the same stream codec
/// constant either way), so this restatement is the same "no existing struct
/// to derive `Encode` from" situation `encode_system_chat` is already in, not
/// a new inconsistency. An item whose canonical key has no entry in the
/// generated registry table (should not happen for anything this crate's own
/// block-entity/inventory models can produce) degrades to writing an empty
/// stack rather than panicking or corrupting the rest of the packet.
fn write_optional_item_stack(w: &mut Writer, item: Option<&ItemStack>) {
    match item.filter(|stack| stack.count > 0) {
        None => w.var_i32(0),
        Some(stack) => match item_id(&stack.item.to_string()) {
            Some(id) => {
                w.var_i32(i32::try_from(stack.count).unwrap_or(i32::MAX));
                w.var_i32(id);
                w.var_i32(0); // added components
                w.var_i32(0); // removed components
            }
            None => w.var_i32(0),
        },
    }
}

/// Hand-written encoder for the clientbound `open_screen` packet
/// (`ClientboundOpenScreenPacket`), which has no existing struct because it
/// is currently only ever *decoded* (see `V770Adapter::decode_open_screen`,
/// the exact mirror of this wire layout). Wire layout: VarInt container id
/// (`ByteBufCodecs.CONTAINER_ID`), VarInt `minecraft:menu` registry id
/// (`ByteBufCodecs.registry(Registries.MENU)` — a plain, non-holder registry
/// id, the same as `decode_open_screen`'s own `menu_name` lookup), then the
/// title as a network-form NBT text component — the identical plain-string
/// shape [`encode_system_chat`] already writes.
fn encode_open_screen_body(window_id: i32, menu_registry_id: i32, title: &str) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(window_id);
    w.var_i32(menu_registry_id);
    let component = Nbt::Compound(vec![("text".to_owned(), Nbt::String(title.to_owned()))]);
    write_network_nbt(&mut w, &component).expect("plain string NBT component always encodes");
    w.into_vec()
}

/// Hand-written encoder for the clientbound `container_set_content` packet
/// (`ClientboundContainerSetContentPacket`), which has no existing struct
/// because it is currently only ever *decoded* (see
/// `V770Adapter::handle_play`'s `CONTAINER_SET_CONTENT` arm, the exact mirror
/// of this wire layout). Wire layout: VarInt container id, VarInt state id,
/// then `ItemStack.OPTIONAL_LIST_STREAM_CODEC` (a VarInt count followed by
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
/// (`ClientboundContainerSetDataPacket`), mirroring the decode side exactly
/// (`V770Adapter::handle_play`'s `CONTAINER_SET_DATA` arm): VarInt container
/// id, then the property index and its value as two big-endian `short`s
/// (`FriendlyByteBuf.writeContainerId` for the first field only — `id`/
/// `value` are plain `writeShort` calls, `ClientboundContainerSetDataPacket
/// .java:29-31`).
fn encode_container_data_body(window_id: i32, property: i32, value: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(window_id);
    w.i16(property as i16);
    w.i16(value as i16);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `add_entity` packet, which has no
/// existing struct because it is currently only ever *decoded* (see
/// `V770Adapter::handle_add_entity`, the exact mirror of this wire layout).
///
/// Wire layout: VarInt id, UUID, VarInt entity-type id, position `f64`×3,
/// low-precision velocity ([`write_lp_vec3`]), then three signed-byte angles
/// in **pitch, yaw, head_yaw** order (note: this order is reversed from
/// `move_entity`'s yaw-then-pitch), then a trailing VarInt `data` field
/// (vanilla sends `0` for ordinary mobs).
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
    w.var_i32(0);
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
    w.i8(-1); // previous_game_type: none
    w.bool(false); // is_debug
    w.bool(false); // is_flat
    w.bool(false); // has_last_death_location
    w.var_i32(0); // portal_cooldown
    w.var_i32(63); // sea_level
    w.bool(false); // online_mode (no auth in the integrated server)
    w.bool(false); // enforces_secure_chat
    w.into_vec()
}

/// Converts one `lodestone-server` [`ServerChunkColumn`] into the
/// version-free [`WorldChunkColumn`] the wire codec speaks, carrying the
/// **real** per-block state the source already computed (grass, dirt,
/// deepslate, gravel, water, …) rather than a solid/air classification —
/// see issue #363 — and, since issue #405, the source's real per-quart
/// biome assignment rather than one constant id everywhere. Every block
/// cell is resolved via [`ServerChunkColumn::block_state`] (the same string
/// source [`V770ServerProtocol::encode_block_update`] already reads for a
/// single cell) through [`resolve_state_id`]; every biome quart via
/// [`ServerChunkColumn::biome_state`] through [`resolve_biome_id`].
///
/// # Why this does not cost a linear scan per block
///
/// [`resolve_state_id`] is `O(STATE_COUNT)` (~32k) per call, and a column is
/// 98,304 cells — calling it unmemoized here would be billions of
/// comparisons per column, every join and every view-tracker resend. `seen`
/// memoizes by the block-state string itself: a real column's *distinct*
/// state strings number in the dozens (`docs/chunk-memory-pool-footprint.md`
/// records live sections as 4-bit indirect palettes with at most 6 entries
/// each), so the expensive scan runs once per distinct string, not once per
/// block. The map borrows its keys from `source` and is not carried across
/// calls — the columns a server sends are different data every time (edits,
/// different chunk coordinates), so there is nothing durable to cache
/// across them without the source outliving one `encode_chunk` call.
/// [`resolve_biome_id`] is a 55-entry linear scan and biome only varies at
/// 16-per-column quart resolution, so it is resolved once per column
/// (`biome_ids` below), not memoized per section — 16 calls per column is
/// already cheaper than memoizing would be.
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

    let mut seen: HashMap<&str, u32> = HashMap::new();

    // This column's 16 real biome ids (issue #405), row-major `qz * 4 + qx`
    // — constant across every section (biome is broadcast vertically, see
    // `ChunkColumn::biome_state`'s doc comment), so resolved once up front
    // rather than recomputed per section.
    let mut biome_ids = [0u32; 16];
    for qz in 0..4i32 {
        for qx in 0..4i32 {
            let name = source.biome_state(qx * 4, qz * 4);
            biome_ids[(qz * 4 + qx) as usize] = resolve_biome_id(name);
        }
    }

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
                    let state = source.block_state(lx as i32, wy, lz as i32);
                    let id = *seen
                        .entry(state)
                        .or_insert_with(|| resolve_state_id(state));
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
        for qz in 0..4usize {
            for qx in 0..4usize {
                let id = biome_ids[qz * 4 + qx];
                for qy in 0..4usize {
                    section.set_biome(qx, qy, qz, id);
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
/// block-state container then the biome container), an empty block-entity
/// list, then the trailing light payload.
///
/// Heightmaps are sent empty and light is sent as all-`Missing`: both are
/// valid, decodable wire forms (confirmed against `Heightmaps`/`ColumnLight`'s
/// own encode logic), so the client accepts the column even though real
/// lighting and heightmap computation are not implemented yet — a documented
/// gap, not a hidden one.
fn encode_column_body(cx: i32, cz: i32, shape: &ChunkShape, column: &WorldChunkColumn) -> Vec<u8> {
    let mut w = Writer::default();
    w.i32(cx);
    w.i32(cz);

    Heightmaps::new().encode(&mut w);

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

    w.var_i32(0); // block entities: none generated yet

    ColumnLight::new(shape.section_count).encode(&mut w);

    w.into_vec()
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
            State::Configuration
                if packet_id == configuration::serverbound::FINISH_CONFIGURATION =>
            {
                ServerBound::ConfigurationFinished
            }
            State::Play if packet_id == play::serverbound::KEEP_ALIVE => {
                match decode_full::<KeepAlive>(payload) {
                    Some(keep_alive) => ServerBound::KeepAlive { id: keep_alive.id },
                    None => ServerBound::Ignored,
                }
            }
            // Only the two movement packets that carry a position matter to
            // the loop (view streaming needs x/z, fall damage needs y/
            // on_ground); rotation-only and status-only movement stay
            // `Ignored` — see `ServerBound::PlayerMoved`'s doc comment.
            State::Play if packet_id == play::serverbound::MOVE_PLAYER_POS => {
                match decode_full::<MovePlayerPos>(payload) {
                    Some(m) => ServerBound::PlayerMoved {
                        x: m.x,
                        y: m.y,
                        z: m.z,
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
                        on_ground: m.flags & MOVE_FLAG_ON_GROUND != 0,
                    },
                    None => ServerBound::Ignored,
                }
            }
            // Ordinals 0-2 are the three destroy phases; 3-7 are the item
            // actions (drop/release/swap/stab) this crate has no inventory
            // model to act on, so they decode to `Ignored` rather than a new
            // `ServerBound` variant — see `ServerBound::BlockAction`'s doc
            // comment.
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
                        sequence: use_item.sequence,
                    },
                    None => ServerBound::Ignored,
                }
            }
            // Issue #12: the `Attack` packet is the whole trigger for a
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
            // `ServerboundPlayerInputPacket`: a single flags byte
            // (`Input.STREAM_CODEC`, `Input.java`) — bit `0x40` is `sprint`,
            // the only flag `ServerBound::PlayerInput` carries (see its own
            // doc comment for why the rest are decoded off the wire here and
            // then dropped rather than threaded further).
            State::Play if packet_id == play::serverbound::PLAYER_INPUT => {
                let mut r = Reader::new(payload);
                match r.u8() {
                    Ok(flags) if r.ensure_empty().is_ok() => ServerBound::PlayerInput {
                        sprint: flags & 0x40 != 0,
                    },
                    _ => ServerBound::Ignored,
                }
            }
            // Issue #268: world/block-admin decode. `CHANGE_DIFFICULTY`,
            // `LOCK_DIFFICULTY` and `SET_GAME_RULE` are the three cheap,
            // observable packets from that issue's 13 — see
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
            // Server-authoritative inventory model: the prerequisite `#266`
            // itself asked for, and the two packets that unblock it end to
            // end — see `lodestone_server::inventory`'s module doc comment.
            State::Play if packet_id == play::serverbound::SET_CARRIED_ITEM => {
                match decode_full::<SetCarriedItem>(payload).and_then(|p| u8::try_from(p.slot).ok())
                {
                    // Mirrors vanilla's `Inventory.isHotbarSlot` guard
                    // (`Inventory.java:70-76`) at the decode boundary, per
                    // `ServerBound::CarriedItemChanged`'s own doc comment.
                    Some(slot) if slot < HOTBAR_SIZE => ServerBound::CarriedItemChanged { slot },
                    _ => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::CONTAINER_CLICK => {
                decode_container_click(payload).unwrap_or(ServerBound::Ignored)
            }
            // `ServerboundContainerClosePacket`: a single VarInt container id
            // (`FriendlyByteBuf.writeContainerId`, the same plain-VarInt
            // `ByteBufCodecs.CONTAINER_ID` codec `decode_container_click`
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

            // Issue #262 (movement/player-state), remaining 8 of 11 — see
            // `ServerBound::PlayerMoved`'s doc comment for why the two
            // sibling movement packets without a position (`MOVE_PLAYER_ROT`,
            // `MOVE_PLAYER_STATUS_ONLY`) stay `Ignored` by design. Every
            // wire layout below is checked directly against
            // `.cache/mc/26.2/src`'s `ServerboundMovePlayerPacket`/
            // `ServerboundPlayerAbilitiesPacket`/`ServerboundMoveVehiclePacket`/
            // etc. — not merely `decode(encode(x))` against this crate's own
            // client encoder, which already sends every one of these
            // (`crate::adapter`). All eight still decode to `Ignored`: none
            // has an existing `ServerBound` variant to lift into, and
            // `lodestone-server` (issue #284's tick-loop work, out of this
            // crate's reach) has no flight/load-timeout/tick-alignment/
            // teleport-confirmation/vehicle/boat model yet for any of them.
            State::Play if packet_id == play::serverbound::MOVE_PLAYER_ROT => {
                let _ = decode_full::<MovePlayerRot>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::MOVE_PLAYER_STATUS_ONLY => {
                let _ = decode_full::<MovePlayerStatusOnly>(payload);
                ServerBound::Ignored
            }
            // `SERVERBOUND_ABILITY_FLAG_FLYING` is decoded so the value is
            // ready the moment a consumer exists; the flag itself is the one
            // vanilla actually reads server-side (`Abilities.flying` echo).
            State::Play if packet_id == play::serverbound::PLAYER_ABILITIES => {
                let _ = decode_full::<ServerboundPlayerAbilities>(payload)
                    .map(|p| p.flags & SERVERBOUND_ABILITY_FLAG_FLYING != 0);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::PLAYER_LOADED => {
                let _ = decode_full::<PlayerLoaded>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::ACCEPT_TELEPORTATION => {
                let _ = decode_full::<AcceptTeleportation>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::CLIENT_TICK_END => {
                let _ = decode_full::<ClientTickEnd>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::MOVE_VEHICLE => {
                let _ = decode_full::<MoveVehicle>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::PADDLE_BOAT => {
                let _ = decode_full::<PaddleBoat>(payload);
                ServerBound::Ignored
            }

            // Issue #264 (entity actions/combat/interaction), remaining 6 of
            // 9 — `ATTACK`, `PLAYER_ACTION` and `USE_ITEM_ON` are already
            // decoded and applied above. All six below are field-verified
            // against `.cache/mc/26.2/src`'s decompiled packet classes.
            //
            // `ServerboundInteractPacket`: VarInt target entity id, VarInt
            // `InteractionHand` ordinal, a low-precision `Vec3` location
            // (`Vec3.LP_STREAM_CODEC` — the same codec
            // [`read_lp_vec3`](crate::packets::entity::read_lp_vec3) already
            // decodes and unit-tests for entity velocity), then a trailing
            // boolean for the secondary-action (shift) modifier. 26.2 split
            // the old combined interact/attack packet in two
            // (`ServerBound::Attack`'s own doc comment); this is what is left
            // once attack is removed — right-click entity interaction
            // (taming/feeding/mounting/etc.), for which this crate has no
            // interaction model at all yet.
            State::Play if packet_id == play::serverbound::INTERACT => {
                let mut r = Reader::new(payload);
                let decoded = (|| -> lodestone_core::Result<()> {
                    let _entity_id = r.var_i32()?;
                    let _hand = r.var_i32()?;
                    let _location = read_lp_vec3(&mut r)?;
                    let _using_secondary_action = r.bool()?;
                    r.ensure_empty()
                })();
                let _ = decoded;
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::SWING => {
                let _ = decode_full::<Swing>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::USE_ITEM => {
                let _ = decode_full::<UseItem>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::PLAYER_COMMAND => {
                let _ = decode_full::<PlayerCommand>(payload);
                ServerBound::Ignored
            }
            // `ServerboundSpectatorActionPacket`: a single VarInt using
            // `ByteBufCodecs.OPTIONAL_VAR_INT`'s offset encoding (`0` = no
            // target, a present id `i` written as `i + 1`) — the exact
            // inverse of `crate::adapter::encode_spectator_action`, which
            // already documents why this must be hand-decoded rather than a
            // derived `Option<i32>` (a bool-prefixed optional would silently
            // misparse this packet).
            State::Play if packet_id == play::serverbound::SPECTATOR_ACTION => {
                let mut r = Reader::new(payload);
                let decoded = (|| -> lodestone_core::Result<()> {
                    let raw = r.var_i32()?;
                    let _target_entity_id = if raw == 0 { None } else { Some(raw - 1) };
                    r.ensure_empty()
                })();
                let _ = decoded;
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::TELEPORT_TO_ENTITY => {
                let _ = decode_full::<TeleportToEntity>(payload);
                ServerBound::Ignored
            }

            // Issue #266 (inventory/container), remaining packets beyond the
            // three already decoded and applied above (`CONTAINER_CLICK`,
            // `CONTAINER_CLOSE`, `SET_CARRIED_ITEM`, into the real
            // `PlayerInventory` model issue #408 built). Every struct below
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
            State::Play if packet_id == play::serverbound::CONTAINER_BUTTON_CLICK => {
                let _ = decode_full::<ContainerButtonClick>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::CONTAINER_SLOT_STATE_CHANGED => {
                let _ = decode_full::<ContainerSlotStateChanged>(payload);
                ServerBound::Ignored
            }
            // `ServerboundSetCreativeModeSlotPacket`: big-endian `i16` slot,
            // then an [`read_optional_item_stack`] item — see that helper's
            // doc comment for why it is not the same shape as
            // [`read_hashed_stack`].
            State::Play if packet_id == play::serverbound::SET_CREATIVE_MODE_SLOT => {
                let mut r = Reader::new(payload);
                // Qualified as `self::` (not a bare call) so
                // `cargo xtask connectedness`'s delegate-following classifier
                // doesn't try to recurse into a helper that returns
                // `Option<Option<ItemStack>>`, not `ServerBound`, and
                // misreport this arm as unclassified instead of the
                // `Ignored` it always produces below.
                let decoded = (|| -> Option<()> {
                    let _slot = r.i16().ok()?;
                    let _item = self::read_optional_item_stack(&mut r)?;
                    r.ensure_empty().ok()
                })();
                let _ = decoded;
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::PLACE_RECIPE => {
                let _ = decode_full::<PlaceRecipe>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::RECIPE_BOOK_CHANGE_SETTINGS => {
                let _ = decode_full::<RecipeBookChangeSettings>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::RECIPE_BOOK_SEEN_RECIPE => {
                let _ = decode_full::<RecipeBookSeenRecipe>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::SELECT_TRADE => {
                let _ = decode_full::<SelectTrade>(payload);
                ServerBound::Ignored
            }
            // `ServerboundSetBeaconPacket`: two `Optional<Holder<MobEffect>>`
            // values (primary then secondary power), each read by
            // [`read_optional_mob_effect`] — the exact inverse of
            // `crate::adapter::encode_set_beacon`.
            State::Play if packet_id == play::serverbound::SET_BEACON => {
                let mut r = Reader::new(payload);
                // See `SET_CREATIVE_MODE_SLOT`'s comment above for why these
                // are qualified as `self::` rather than bare calls.
                let decoded = (|| -> Option<()> {
                    let _primary = self::read_optional_mob_effect(&mut r)?;
                    let _secondary = self::read_optional_mob_effect(&mut r)?;
                    r.ensure_empty().ok()
                })();
                let _ = decoded;
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::EDIT_BOOK => {
                let _ = decode_full::<EditBook>(payload);
                ServerBound::Ignored
            }
            // Block-entity text, not item state — arguably miscategorized in
            // this issue's own packet list (see #266's investigation
            // comment). Decoded here anyway since sign storage lives in
            // `lodestone-world`, not `PlayerInventory`, so this cannot
            // collide with that territory; still `Ignored` regardless.
            State::Play if packet_id == play::serverbound::SIGN_UPDATE => {
                let _ = decode_full::<SignUpdate>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::RENAME_ITEM => {
                let _ = decode_full::<RenameItem>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::PICK_ITEM_FROM_BLOCK => {
                let _ = decode_full::<PickItemFromBlock>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::PICK_ITEM_FROM_ENTITY => {
                let _ = decode_full::<PickItemFromEntity>(payload);
                ServerBound::Ignored
            }
            // Bonus beyond this issue's own 16-packet count (its "remaining
            // container/recipe-adjacent ids" clause) — the same bundle-select
            // struct the client already encodes.
            State::Play if packet_id == play::serverbound::BUNDLE_ITEM_SELECTED => {
                let _ = decode_full::<SelectBundleItem>(payload);
                ServerBound::Ignored
            }

            // Issue #268 (world/block-admin), remaining packets beyond
            // `CHANGE_DIFFICULTY`/`LOCK_DIFFICULTY`/`SET_GAME_RULE` above.
            // A prior pass on this issue deliberately left the seven below
            // undecoded, reasoning they are "deep features, not decode
            // gaps" (command-block/jigsaw/structure/game-test state, none
            // of which this crate models). That reasoning about the
            // *feature* stands — nothing here builds command blocks,
            // jigsaw structures, or the game-test framework. What changed:
            // this pass decodes the wire shape anyway, straight against
            // `.cache/mc/26.2/src`'s decompiled packet classes (the
            // independent source `CLAUDE.md`'s evidence standard calls for
            // when no client encoder exists to cross-check against, which
            // is the case for all seven), and maps to `Ignored` — the same
            // "examined, no consumer" bucket `cargo xtask connectedness`
            // already tracks separately from "never examined" for
            // `PLAYER_ACTION`'s item-action ordinals. This is additive
            // measurement/documentation, not a claim that any of these
            // features now exist.
            State::Play if packet_id == play::serverbound::SET_COMMAND_BLOCK => {
                let _ = decode_full::<SetCommandBlock>(payload);
                ServerBound::Ignored
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
            // (`ByteBufCodecs.lengthPrefixed(65536)` wraps
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
                let _ = decode_full::<ChangeGameMode>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::CONFIGURATION_ACKNOWLEDGED => {
                let _ = decode_full::<ConfigurationAcknowledged>(payload);
                ServerBound::Ignored
            }
            // Deliberately left undecoded (falls through to the wildcard
            // below), unlike the rest of this issue's family:
            // `TEST_INSTANCE_BLOCK_ACTION`'s body
            // (`TestInstanceBlockEntity.Data.STREAM_CODEC`) is a nested
            // `Optional<ResourceKey>`/`Vec3i`/`Rotation`/`Status`/
            // `Optional<...>` composite this crate has no codec support for
            // yet, and — like its sibling `SET_TEST_BLOCK` above — it
            // drives the game-test framework only, which this crate does
            // not implement at all. Left for whoever adds game-test
            // support, at which point the real `Data` type will exist to
            // decode into anyway.

            // Issue #270 (connection-lifecycle/system), remaining packets
            // beyond `KEEP_ALIVE` above. `PONG`/`PING_REQUEST` already have
            // structs exercised by this crate's client encoder; the rest
            // follow the same field-verified-against-decompiled-source
            // convention as the other four families above.
            State::Play if packet_id == play::serverbound::PING_REQUEST => {
                let _ = decode_full::<PingRequest>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::PONG => {
                let _ = decode_full::<Pong>(payload);
                ServerBound::Ignored
            }
            // `ServerboundCustomPayloadPacket`: a channel identifier then a
            // channel-specific payload; vanilla dispatches on the channel
            // and falls back to `DiscardedPayload` (read-and-drop) for any
            // channel it does not recognize. This crate models only the
            // `minecraft:brand` channel client info actually sends
            // (`BrandPayload`, already used by the clientbound direction of
            // the same channel) — any other channel, or a `brand` payload
            // that fails to parse, is treated exactly like vanilla's
            // `DiscardedPayload` fallback and dropped.
            State::Play if packet_id == play::serverbound::CUSTOM_PAYLOAD => {
                let _ = decode_full::<BrandPayload>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::RESOURCE_PACK => {
                let _ = decode_full::<ResourcePackResponse>(payload);
                ServerBound::Ignored
            }
            // Issue #425 investigation (chunk-streaming regression): this
            // arm and `CHUNK_BATCH_RECEIVED` below used to decode-then-drop
            // like every other packet in this `Ignored` family, from when
            // this crate had no consumer for either. Issue #270 later added
            // `ServerBound::ClientInformationChanged`/`ChunkBatchAcknowledged`
            // and their consumers in `crate::server` (`ViewTracker::set_view_radius`
            // and the `awaiting_chunk_batch_ack` flow-control gate), but never
            // came back to update *this* decode arm — so both variants were
            // dead code, constructed nowhere, and every view-streaming batch
            // after the first queued behind a permanently-`true`
            // `awaiting_chunk_batch_ack` and was never flushed. Reproduced at
            // committed `main`: `cargo test -p lodestone-v770 --test block_edit
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
            State::Play if packet_id == play::serverbound::CLIENT_COMMAND => {
                let _ = decode_full::<ClientCommand>(payload);
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
                let decoded = (|| -> lodestone_core::Result<()> {
                    let action = r.var_i32()?;
                    if action == 0 {
                        let _tab = r.string(32767)?;
                    }
                    r.ensure_empty()
                })();
                let _ = decoded;
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::ENTITY_TAG_QUERY => {
                let _ = decode_full::<EntityTagQuery>(payload);
                ServerBound::Ignored
            }
            State::Play if packet_id == play::serverbound::BLOCK_ENTITY_TAG_QUERY => {
                let _ = decode_full::<BlockEntityTagQuery>(payload);
                ServerBound::Ignored
            }
            // Deliberately left undecoded (fall through to the wildcard
            // below), unlike the rest of this issue's family:
            // - `COOKIE_RESPONSE`: this crate's client cannot send this
            //   either (see "Cookies and transfers are dead ends," the
            //   completeness epic) — there is no existing encoder to
            //   cross-check a hand-decode against, and no cookie this crate
            //   ever sets to receive a response about.
            // - `DEBUG_SUBSCRIPTION_REQUEST`: its body is a
            //   registry-keyed (`Registries.DEBUG_SUBSCRIPTION`) set with no
            //   VarInt-id table in this crate to resolve against — an F3
            //   debug-sample-graph subscription with no gameplay effect,
            //   the same "low priority, file for completeness" packet this
            //   issue's own text already flags.
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        let finished = LoginFinished {
            profile_id: uuid,
            name: username.to_string(),
            properties: Vec::new(),
            session_id: uuid,
        };
        vec![send(login::clientbound::LOGIN_FINISHED, &finished)]
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        // Minimum sequence: go straight to the finish signal. The client only
        // needs dimension type/biome registries if it derives chunk shape from
        // them, and `ChunkShape::for_dimension` hardcodes shape by dimension
        // name instead — so registry data, known-packs negotiation, and the
        // code-of-conduct exchange are all real vanilla packets this join
        // sequence does not yet need to send. See the module docs' scope note.
        vec![send(
            configuration::clientbound::FINISH_CONFIGURATION,
            &FinishConfiguration,
        )]
    }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
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
            game_type: 0, // survival
            rest: encode_game_login_rest(),
        };

        let spawn_x = 8;
        let spawn_y = 100;
        let spawn_z = 8;
        let spawn_position = SetDefaultSpawnPosition {
            location: GlobalPos {
                dimension: "minecraft:overworld".to_string(),
                position: pack_block_pos(spawn_x, spawn_y, spawn_z),
            },
            yaw: 0.0,
            pitch: 0.0,
        };

        let teleport_payload = encode_player_position_teleport(
            0,
            f64::from(spawn_x),
            f64::from(spawn_y),
            f64::from(spawn_z),
            0.0,
            0.0,
        );

        vec![
            send(play::clientbound::LOGIN, &login),
            send(
                play::clientbound::SET_DEFAULT_SPAWN_POSITION,
                &spawn_position,
            ),
            ServerDirective::Send {
                packet_id: play::clientbound::PLAYER_POSITION,
                payload: teleport_payload,
            },
            // Spawn is chunk (0, 0) (`spawn_x`/`spawn_z` = 8, inside that
            // column), matching `serve_connection`'s own initial view
            // center — reused via the trait method rather than duplicating
            // the encoder so join-time and move-time cache-center packets
            // can never drift apart.
            self.encode_chunk_cache_center(0, 0),
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

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::CHUNK_BATCH_START,
            payload: Vec::new(),
        }
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ServerChunkColumn) -> ServerDirective {
        let shape = ChunkShape::overworld_1_21();
        let world_column = build_world_column(&shape, column);
        let payload = encode_column_body(cx, cz, &shape, &world_column);
        ServerDirective::Send {
            packet_id: play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
            payload,
        }
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        use crate::packets::game::ChunkBatchFinished;
        send(
            play::clientbound::CHUNK_BATCH_FINISHED,
            &ChunkBatchFinished { batch_size },
        )
    }

    fn welcome_message(&self) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: play::clientbound::SYSTEM_CHAT,
            payload: encode_system_chat("Welcome to Lodestone", false),
        }]
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

    /// Encodes air-supply as a one-field `SET_ENTITY_DATA` metadata update for
    /// [`LOCAL_PLAYER_ENTITY_ID`] — the same wire packet a mob's cosmetic
    /// metadata would use, restricted to the single `DATA_AIR_SUPPLY_ID`
    /// field vanilla's own `Entity.setAirSupply` sync would send. Hand-written
    /// (no existing struct to derive `Encode` from — see this module's own
    /// doc comment on why that is the right call here) but byte-accurate
    /// against `crates/protocol/v770/src/packets/metadata.rs`'s
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

    /// Re-sends `SET_HEALTH` with the new health — the same packet and
    /// struct [`begin_play`](Self::begin_play) already sends once at join.
    /// `food`/`saturation` are resent at the same fresh-spawn constants
    /// `begin_play` uses (`20`, `5.0`): `lodestone-server` has no hunger
    /// model to track a real value for either (the same "no inventory model"
    /// scope this crate's `UseItemOn` handling already documents applies
    /// equally here — there is simply nothing that changes them), so
    /// restating the constant is honest about there being no hunger
    /// simulation, not a claim that hunger is unaffected by anything.
    fn encode_set_health(&self, health: f32) -> ServerDirective {
        send(
            play::clientbound::SET_HEALTH,
            &SetHealth {
                health: health.clamp(0.0, 20.0),
                food: 20,
                saturation: 5.0,
            },
        )
    }

    /// Issue #268's difficulty confirmation — see
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

    /// Issue #268's game-rule confirmation — see
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

    /// The item-action ordinals (`3`..=`7`: drop/release/swap/stab) share the
    /// wire packet but carry no terrain edit — this crate has no inventory
    /// model to act on them, so they must decode to `Ignored`, not silently
    /// fall into one of the three destroy phases.
    #[test]
    fn decode_player_action_item_ordinals_are_ignored() {
        let proto = V770ServerProtocol;
        for ordinal in 3..=7 {
            let body = encode(&PlayerAction {
                action: ordinal,
                pos: 0,
                direction: 0,
                sequence: 0,
            });
            let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body);
            assert_eq!(decoded, ServerBound::Ignored, "ordinal {ordinal}");
        }
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
                sequence: 7,
            }
        );
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
    /// first time it ran, exactly the trap issue #363 warned a
    /// solids-only fix would fall into. Pins the exact expected id
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

    /// The hermetic half of issue #363's gate: a whole-column `encode_chunk`
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
        let directive = proto.encode_chunk(0, 0, &served_column);
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

    /// Issue #405's own island check: real per-quart biome assignment must
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
        let directive = proto.encode_chunk(0, 0, &served_column);
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
    /// then a VarInt state id, nothing else — the shape
    /// `ClientboundBlockUpdatePacket.STREAM_CODEC` specifies
    /// (`ClientboundBlockUpdatePacket.java:14-20`).
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
}

/// Issue #268: `CHANGE_DIFFICULTY`/`LOCK_DIFFICULTY`/`SET_GAME_RULE` decode
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
    /// (`ClientboundChangeDifficultyPacket.STREAM_CODEC`).
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

/// Server-authoritative inventory: `SET_CARRIED_ITEM`/`CONTAINER_CLICK`
/// decode. Where possible the expected wire bytes come from the **real**
/// client-side encoder (`crate::adapter`'s `V770Adapter::encode_action`),
/// not a hand-authored fixture — this is the same "real client already sends
/// this packet in ordinary singleplayer play" encoder the #266 investigation
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

/// Issue #12: decode tests for `minecraft:attack` and
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

    /// `minecraft:interact` (a plain `Interact`, not `Attack`) is deliberately
    /// **not** given its own `ServerBound` variant — see that variant's own
    /// doc comment. Pinning it here as `Ignored` rather than leaving it
    /// undocumented: a future agent adding taming/feeding must change this
    /// test, not silently discover a gap.
    #[test]
    fn decode_plain_interact_from_the_real_client_encoder_is_ignored() {
        let proto = V770ServerProtocol;
        let (packet_id, payload) = crate::adapter()
            .encode_action(
                ConnectionState::Play,
                &ClientAction::InteractEntity {
                    entity_id: 1234,
                    interaction: EntityInteraction::Interact { hand: Hand::Main },
                    sneaking: false,
                },
            )
            .expect("encodes")
            .expect("Interact always encodes in Play");
        assert_eq!(packet_id, play::serverbound::INTERACT);
        let decoded = proto.decode(State::Play, packet_id, &payload);
        assert_eq!(decoded, ServerBound::Ignored);
    }

    /// Round-trips through the real client encoder: `sprint` survives,
    /// bit-identical, out the other side; the other six `Input` flags are
    /// decoded off the wire (so a malformed byte still fails cleanly) but do
    /// not appear in `ServerBound::PlayerInput` — see that variant's own doc
    /// comment for why.
    #[test]
    fn decode_player_input_sprint_from_the_real_client_encoder() {
        let proto = V770ServerProtocol;
        for sprint in [true, false] {
            let (packet_id, payload) = crate::adapter()
                .encode_action(
                    ConnectionState::Play,
                    &ClientAction::SetPlayerInput(PlayerInput {
                        forward: true,
                        backward: false,
                        left: false,
                        right: false,
                        jump: false,
                        shift: false,
                        sprint,
                    }),
                )
                .expect("encodes")
                .expect("SetPlayerInput always encodes in Play");
            assert_eq!(packet_id, play::serverbound::PLAYER_INPUT);
            let decoded = proto.decode(State::Play, packet_id, &payload);
            assert_eq!(decoded, ServerBound::PlayerInput { sprint }, "sprint={sprint}");
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
    /// encoder: bit `0x40` alone must decode to `sprint: true`, so a future
    /// change to `ServerBound::PlayerInput`'s field can be checked against a
    /// known byte, not only against the adapter's own (also-changeable)
    /// encoder.
    #[test]
    fn decode_player_input_bit_layout_pins_sprint_at_0x40() {
        let proto = V770ServerProtocol;
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_INPUT, &[0x40]);
        assert_eq!(decoded, ServerBound::PlayerInput { sprint: true });
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_INPUT, &[0x1F]); // every other flag, not sprint
        assert_eq!(decoded, ServerBound::PlayerInput { sprint: false });
    }
}

/// Regression coverage for the #425 investigation's chunk-streaming bug
/// (see the doc comment on the `CLIENT_INFORMATION`/`CHUNK_BATCH_RECEIVED`
/// decode arms above): both packet ids used to hit the generic
/// decode-then-drop `Ignored` family from before this crate had any
/// consumer for either, and issue #270 later added
/// `ServerBound::ClientInformationChanged`/`ChunkBatchAcknowledged` plus
/// `crate::server`'s consumers without ever updating this decode arm to
/// construct them — so both variants were dead code, and every
/// view-streaming chunk batch after the connection's first queued behind a
/// permanently-`true` `awaiting_chunk_batch_ack` and was never flushed.
/// `cargo test -p lodestone-v770 --test block_edit -- \
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
