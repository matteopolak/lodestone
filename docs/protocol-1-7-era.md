# Protocol 5 era (Minecraft 1.7.6-1.7.10)

## What it is

`lodestone-v1-7` (`crates/versions/1.7`) is the client protocol crate for protocol 5, spoken by Minecraft 1.7.6 through 1.7.10: the bottom of the version ladder and the era that shares almost nothing with its neighbour. It provides the joining adapter and `V5ServerProtocol`; the registry selects the latter only for protocol 5.

## How it works

### The protocol number and why it is a singleton

Protocol 5 was read from a real 1.7.10 server's status reply, with a control: handshakes claiming 0, 47 and 5 all got 5, so the server reports its own number. `minecraft-data`'s `version.json` agrees and adds that protocol 4 covers 1.7.2-1.7.5. `PROTOCOLS` is `&[5]`; protocol 4 is not claimed because no jar or dataset is present, and admitting it needs a shape diff plus a join capture against a real server.

Eras group versions agreeing on 85% of packet shapes. Protocols 5 and 47 agree on 37 of 112 shapes (33%), eight of them handshake and status packets that never changed, so there is nothing to fold in.

### Layout

Standard era crate: `src/packets/` per wire module, `src/generated/` tables, `src/adapter.rs` for `VersionAdapter`, and a construction-checked dispatch table (`lodestone_core::dispatch`) where each clientbound id is handled, listed as deliberately ignored with a reason, or fails construction. `cargo xtask connectedness` reports clientbound decoded 50/65, emits 50/65, decoded-but-stranded 0, serverbound encoded 19/24.

### What is genuinely different

| difference | where | why it bites |
|---|---|---|
| chunk payloads are zlib streams inside the packet body | `packets::chunk` | no whole-connection compression exists; inflate is per chunk packet |
| block ids and metadata in separate arrays grouped by type across the column, plus a conditional `add` nibble array for ids above 255 | `packets::chunk` | later eras pack 16 bits per block; both give identical total length |
| bulk chunk metadata trails the payload | `packets::chunk` | protocol 47 moved it first; a single-column packet parses under either order |
| positions are three separate numbers in three width combinations (`iii`, `ibi`, `isi`) | `packets::position` | the packed 64-bit position arrives at 47 |
| serverbound movement carries a `stance` after feet `y` | `packets::game` | removed at 47; wrong order is silent (see below) |
| clientbound teleport's middle `f64` is the eye position | `packets::game` | every later protocol sends feet |
| entity ids are `i32` except in the four spawn packets | `packets::entity` | a varint decoder takes the wrong byte count |
| `keep_alive` is `i32`; food, experience, effect duration are `i16`; `entity_destroy` has a `u8` count; `custom_payload` an `i16` length | throughout | same class |
| movement carries `bool on_ground`, not a relative-flags byte | `packets::game` | a 47 decoder reads `true` as relative x |
| chat is one string with no position byte | `packets::game` | a leading slash is a command; replies cannot be system or action-bar |
| item NBT is gzip behind an `i16` length | `packets::slot` | later eras use a bare optional tag |
| custom name is data-watcher index 10, not 2 | `entity_metadata` | |
| two fixed-point scales: 32 per block for entities, 8 for sound positions | `adapter` | inconsistent within one protocol |
| mob-effect ids are one-based | `adapter` | |
| attribute keys are dotted camelCase (not a valid `Identifier`) | `adapter` | explicit translation table |
| plugin channel controls are bare uppercase (`REGISTER`), brand is `MC|Brand` | `server_protocol` | normalized to shared keys before `ResourceKey` validation |
| all minecart variants share object type 10 | `generated/entity_types` | variant is in metadata |
| objects and mobs have separate id spaces | `generated/entity_types` | an id needs its spawn packet |

Ruled out by measurement: strings are `varint(byte count) + UTF-8` as in 1.8, and login-success and named-entity-spawn UUIDs are dashed 36-character strings, not 128-bit pairs.

### The movement chain

Three defects on one chain were invisible to hermetic tests because the server's response to all three is identical: it stops accepting movement with no error, disconnect or log line.

1. Serverbound movement orders four doubles `x`, `y`, `stance`, `z`. Either order encodes to the same length and round-trips.
2. The clientbound teleport's middle double is the eye position; reading it as feet puts the player 1.62 blocks up.
3. A teleport is confirmed by echoing a matching serverbound `position_look`. There is no teleport id or confirm packet until protocol 340, and the server holds the player until the echo.

They interact: a wrong (2) makes the echo carry a refused stance, a wrong (1) makes it never match, and the server's held-player branch returns before the stance range check. Over a 320-block walk the broken arm saw the server re-send its position 65-70 times (fixed: once), loaded 445 columns (fixed: 759) and unloaded 0 (fixed: 420); with the transposition the server even saved the player at spawn, having discarded 1,600 movement packets. `tests/live_movement.rs` keeps these apart, and `tests/movement.rs` pins both field orders at byte offsets because a struct compared to itself cannot see a transposition.

### Hosted path

`V5ServerProtocol` goes from login success straight to Play (no configuration exchange or compression). The join packet uses a signed one-byte dimension, and the initial placement's middle double is the eye position; the adapter echoes it with separate `y` and `stance`.

- **Chunks:** single-column `map_chunk` with two 16-bit masks and an `i32` compressed length, then zlib: per section a type array, metadata nibbles, block-light nibbles and sky-light nibbles, then a biome footer (fixture in `crates/versions/1.7/tests/`). A column must cover y=0..255 and every state needs an exact legacy inverse in `0..=164` or `170..=175`; unsupported states fail encoding rather than becoming air. A chunk unload is 12 compressed bytes inflating to a 256-byte biome footer (ground-up implies a footer), not an empty payload.
- **Keep-alive:** fixed signed `i32` both ways; the host clears the pending challenge via `ServerBound::KeepAlive` and rejects a trailing byte.
- **Block break animation:** the stage is a signed byte but the shared event owns the raw byte, so -1 becomes 255, the clear sentinel.
- **Explosion:** four `f32` (centre, radius), an `i32` offset count checked against remaining bytes before allocation, signed-byte offsets, then three unconditional `f32` motion components (always `Some`, even when zero, as the era has no presence flag). Offsets are applied to the world as air with block-entity removal (`tests/explosion.rs`).
- **Vitals:** `update_health` is `f32` health, big-endian `i16` food, `f32` saturation (newer families use a VarInt food), emitted on damage or respawn.
- **Settings:** only the signed view-distance byte is consumed (`ClientInformationChanged`); the tail is difficulty then cape, so the 47 decoder must not be reused.
- **Block use:** `block_place` (separate `i32, u8, i32`, face, inline item slot, three signed sixteenth cursor offsets) becomes `UseItemOn` with main hand and sequence 0 fixed. Invalid faces, cursor outside `0..=15`, the use-in-air sentinel and trailing bytes are rejected, not clamped.
- **Hotbar:** `held_item_slot` is a big-endian `i16`, accepted for `0..=8` as `CarriedItemChanged`.
- **No-target digs:** `block_dig` statuses 3, 4, 5 lift to `ItemDropped` (stack, one) and `ReleaseUseItem`; other statuses and malformed forms are ignored.
- **Plugin messages:** `custom_payload` is a channel string, `i16` count and data, reaching `ServerBound::CustomPayload`; `REGISTER`/`UNREGISTER`/`MC|Brand` normalize to `minecraft:register`/`unregister`/`brand`, other channels must parse as resource keys, and malformed or non-Play frames are ignored.
- **Chat:** the single string becomes unsigned `ServerBound::Chat` (zero timestamp and salt) or, after a leading slash is removed, `ChatCommand`. Replies are JSON component strings with no position byte.
- **Animation:** serverbound `arm_animation` is an entity id (discarded) plus ordinal 1, becoming `Swing { hand: 0 }`; broadcasts use a varint id and action byte, with the off-hand action 3 degraded to main-hand 0 (3 is critical-hit in this era).
- **Entity action:** ordinals start at 1; `3` is leave-bed (`2` only ends crouching), mapped to the shared wake action. The client uses the same numbering for sprint and riding commands.
- **Use entity:** target id plus a mouse ordinal (0 attack, 1 interact); hand and sneak are supplied as main hand and false.
- **Containers:** `generic_9x3` opens as a literal `minecraft:chest` with 27 slots; `window_click` carries window, slot, button, action counter, mode and pre-click slot, and the host ignores the prediction (`tests/inventory.rs`).
- **Entity state census:** metadata decodes fully but raises only five protocol-wide fields (index 0 flags, 1 air, 6 health, 10 custom name, 11 name visibility). Equipment ordinals `0..=4` map to main hand, feet, legs, chest, head; damage and compressed tag have no canonical field. Attributes support seven dotted keys (`generic.maxHealth`, `followRange`, `knockbackResistance`, `movementSpeed`, `attackDamage`, `horse.jumpStrength`, `zombie.spawnReinforcements`) translated to snake case, with UUID-only modifiers getting stable generated ids; unknown keys are dropped without discarding known ones.
- **Movement:** all four serverbound shapes lift (`position`, `position_look`, `look`, grounded-only `flying`); `stance` is a wire-only constraint, and the echoed `position_look` is the placement confirmation.

Each family has a byte-level control in `tests/server_protocol.rs` and a visible client/server assertion in `tests/server_integration.rs`; none replaces a live protocol-5 client, which remains the external check.

### The name-only player list

The list carries `(name, online, i16 ping)` with no UUID, so `PlayerListEntry.uuid` stays `None`; the adapter will not turn an offline derivation into a plausible wrong identity or do a network lookup. `online = false` becomes `ClientEvent::PlayerListRemoveByName`, and `lodestone_game::tablist::TabList` keeps UUID-backed and name-backed rows in distinct key spaces. Remote player spawns supply both UUID and profile name; the adapter emits `EntitySpawned` plus `PlayerProfileNamed` (stored as `PlayerProfileName`) so entities find their tab row when names match exactly. If the server decorates or truncates the list name differently, there is no honest fallback. Pinned by `tests/capture_join.rs` and `tests/player_list.rs`.

### External-client acceptance

Row 5 of the opt-in gate (`just external-client-acceptance --protocol 5 --output /private/tmp/lodestone-v5`) records `configuration.mode: "login_to_play"` and `chunk_batch_acknowledgement.mode: "unbatched", batch_count: 0`, then requires join, deliberate movement, one `start_destroy_block` result and a clean client disconnect, with provenance naming the exact 1.7.10 client build. It has not been run; protocol 5 is unverified by a real release client until a manual run passes.

## How to change it

- **Adapter** (`src/adapter.rs`): `CLIENTBOUND` lists every translated packet and `IGNORED` every untranslated one with a reason. Spell entries literally; `cargo xtask connectedness` anchors on the text `Handler::new(` and a helper function defeats it.
- **Hosting** (`src/server_protocol.rs`): keep the zlib chunk body local to this era.
- **Tables** in `src/generated/` are regenerated, never edited: `LODESTONE_REGEN=1 cargo test -p lodestone-v1-7 --test entity_types` rebuilds them from the committed wire transcript byte for byte.
- **Admitting protocol 4:** measure shape identity against a real 1.7.2-1.7.5 server, add a join capture, declare `&[4, 5]`. Nothing keys off the folder or feature name.
- **Removing the era:** delete `crates/versions/1.7/` and its dependency and feature lines in `lodestone-registry`.
- Gotchas: no family is on by default and the shell's `live` feature enables only `v26-2`. A vanilla server uses single-column `map_chunk` only for unloads (loaded columns arrive in `map_chunk_bulk`), so `tests/chunk.rs` builds the loading path by hand. This era maps to `mc_1_8`'s physics profile because it predates the 1.9 input rewrite. Oracle ports are 25602/25603, not the "next two free" 25600/25601, which `scripts/live-oracles/lovelier.sh` already publishes.

## Configuration

| knob | where | effect |
|---|---|---|
| feature `v1-7` | `lodestone-registry` | registers the adapter and protocol-5 server family; off by default |
| `LODESTONE_REGEN=1` | `tests/entity_types.rs` | regenerate entity-type tables |
| `./scripts/live-oracles/legacy.sh 1.7.10` | | oracle on `:25602`, RCON `:25603`, container `lodestone-mc1710`, `eclipse-temurin:8-jdk`, flat quiet overworld |

## Dependencies

- `lodestone-canonical` (pre-Flattening `(id << 4) | meta` translation shared with 1.8 and 1.9).
- `lodestone-protocol-common`: measured-identical packet definitions with their own `#[mc(protocols = "...")]` ranges. Founding this era widened only `LoginSuccess` from `47..=578` to `5..=578`; the range is enforced at decode, so a mistake surfaces as a refusal, not a silent mis-parse.
- `lodestone-core`, `lodestone-macros`, `lodestone-model`, `lodestone-world`, `lodestone-data`; `lodestone-server` (host trait and chunk source), `lodestone-client` (in-memory consumer control); `flate2` for chunk inflate/deflate, `md-5` for the offline UUID.

## Evidence

No first-party source exists for this protocol, so provenance is:

| subject | outside source |
|---|---|
| protocol number | a real server's status reply with an echo control; `minecraft-data` agrees |
| packet ids and shapes | `minecraft-data` 1.7 as bootstrap, then a recorded real join (`tests/captures/`) |
| entity type ids | a wire transcript: each name summoned over RCON, the id read from the spawn packet, accepted only when its fixed-point position matched |
| item ids | `minecraft-data` `items.json`, cross-checked against a recorded container packet |
| block ids and metadata | `minecraft-data` `blocks.json` for wire values; expected states from `lodestone_data::block_states` (jar-derived) |
| chunk array grouping | the biome footer's position in a real bulk packet (both groupings have equal length) |
| nibble parity | four wool blocks at adjacent x with metadata 14, 1, 5, 11 (no value equals its byte-partner) read back from a real server |
| explosion frame | `minecraft-data` field widths, plus literal and malformed-count controls in `tests/explosion.rs` |
| movement field order | server behaviour over a 320-block walk, three ways (corrections, columns streamed, saved logout position) |
| eye-position reading | an RCON teleport to y=80.0 producing 81.62 on the wire, plus the server login log |
| chunk unload framing | 420 recorded unloads from one walk; twelve bytes inlined in `tests/chunk.rs` |
