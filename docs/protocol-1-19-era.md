# The 1.19 era crate: one protocol, signed chat, one spawn packet

## What it is

`crates/versions/1.19` (package `lodestone-v1-19`) joins and hosts Minecraft 1.19.4, protocol 762, from one generated packet-id table, block-state table and entity registry plus the era's own chat, spawn and chunk-shape code. The protocol number is read from the jar's own `version.json` (`.cache/mc/1.19.4/server.jar`); the other 1.19.x releases (759-761) have different chat shapes and are not served.

Two changes define the era: chat stopped being a string, and every non-player entity spawns through one packet.

## How it works

### A singleton era

`PROTOCOLS` lists one number, measured from `minecraft-data` with named types inlined and primitive aliases kept (collapsing `varint`/`i64`/`u8`/`f32` hides retypes). Identical packet shapes: 1.18.2 to 1.19.4 is 137 of 175 (78%), 1.19.4 to 1.20.6 is 113 of 201 (56%). Both are under the 85% grouping threshold, so neither neighbour joins. Dispatch is one `lodestone_core::dispatch::Table` in a one-slot `OnceLock` array indexed like `ids_for`.

### Chat

Clientbound chat is split by what the client may trust:

| packet | author | signed | sender id |
|---|---|---|---|
| `player_chat` | a player | optionally, over the exact bytes it also carries | yes |
| `profileless_chat` | server, on a player's behalf | no | no |
| `system_chat` | server | no | no |

Only `player_chat` fills a `ChatAckInfo` and has a profile id for hide-in-chat filters. It keeps both texts: the decorated form is displayed, but the signature is hashed over the plain `raw_content`.

The server keeps a list of pending signed messages per connection. Every serverbound chat packet carries a last-seen update draining it, and a client that only reads chat must send a standalone `message_acknowledgement`. `V762Adapter` holds a `pending_ack` counter that `player_chat` bumps and every serverbound chat path takes, or the server eventually disconnects.

Three differences from the modern family silently mis-frame a stream if inherited: components are JSON strings, not network NBT (moved in 1.20.3); there is no server-global message index (the packet opens with the sender UUID); and there is no acknowledgement checksum byte (a 1.21.5 addition).

This crate sends unsigned chat (zero timestamp and salt, no signature), accepted only when the server is not enforcing secure profiles, since signing needs an authenticated session key and online-mode login is not implemented. The oracle sets `enforce-secure-profile=false`; with enforcement on the server rejects the join, not the message. Modelled but unsent: `packets::chat::ChatSessionUpdate` and the signature fields. `ChatAckInfo::verified` is constructed `false`, fail-closed.

### Gameplay packet handling

- `entity_equipment` decodes the continuation-bit slot list and pre-component item shape through the committed 1.19.4 jar registry. `multi_block_change` applies canonical states, syncs block entities and emits `SectionBlocksChanged`. `block_action` resolves its block-type id through the era registry. These registries are separate from the canonical block-state and item tables (different orderings). `block_break_animation` keeps entity id, packed position and raw stage (including the clear sentinel).
- `entity_metadata` reuses the typed sentinel-terminated codec but reports only index 0's flags byte, since other indexes need an entity category the packet lacks. The serializer table is not inherited from 1.17: 762 inserted `VarLong` at id 2 and shifted later ids (id 3 is a float; 15 optional block state, 16 compound NBT, 17 particle, 18 villager tuple, 19 optional uint, 20 pose, 21-27 later variants). Particle (17) and optional-global-position (23) fail loudly rather than guess a width, because the former needs a particle registry and the local schema mis-describes the latter.
- `explosion` keeps the offset-list shape with a VarInt count (not a fixed `i32`) and an unconditional three-`f32` knockback tail. The decoder budgets the count against remaining bytes, requires the whole packet consumed, floors the centre, applies offsets as air, and removes block entities.
- `game_state_change` models reasons 1, 2, 3, 7 and 8 (rain start/stop, mode, rain and thunder level); others get an exact frame check only. `entity_update_attributes` uses textual names; the committed attribute registry maps thirteen `generic.*`, `horse.*`, `zombie.*` names to version-neutral keys, and a modifier UUID becomes `lodestone:legacy_modifier_<uuid>`. Lists are bounded before allocation and unknown names are decoded and omitted.

### One spawn packet, per-era entity table

1.19.4 removed the separate mob-spawn packet. The head-rotation byte moved into the object-spawn packet before the object data, which widened from `i32` to a VarInt. A decoder with the older order reads head pitch as object data and yields a plausible spawn with nonsense motion. With no "this is a mob" packet, the kind is purely a registry lookup, and that registry is per era: `allay` sorts first, so id 0 is `minecraft:allay` here and `minecraft:area_effect_cloud` below, shifting every later id (`tests/entity_types.rs` asserts it from the committed dump).

### Vertical window as a lookup

Through 1.18 the join carried the resolved dimension entry inline. At 762 it carries the whole `dimension_codec` registry plus the dimension type name. `ChunkShape::from_dimension_registry` walks `minecraft:dimension_type` to the matching entry's `min_y` and `height`; an unknown name leaves the shape alone rather than taking the first entry (the overworld window). `respawn` names a dimension type without describing it, so the adapter retains the join's registry blob and re-resolves. Chunk columns keep positioned block-entity records, deriving the canonical type from the translated block state at that position and reshaping pre-1.20 sign fields into the two-sided model; dropping them would make signs and containers vanish.

### Static registry reports

The committed jar registry reports used by the 1.17 and 1.19 adapters decode into a typed map: names stay strings, but every consumed `protocol_id` is an integer in the schema, so a missing or string id fails before a table is built. Unrelated registries pass through, unknown fields are ignored. Refresh a fixture and its digest together and run the registry tests; do not swap in a generic JSON tree.

### Hosted play

`V762ServerProtocol` (selected by `lodestone-registry`) stays in the legacy login-to-Play path: `login_success` writes the profile-property list, no Configuration phase. The join carries an overworld dimension entry for `-64..319`, and the position packet has no trailing dismount flag. Every hosted column supplies all 24 sections with block and biome palettes and inline light framing; a column with block entities or a non-plains biome is rejected until those forms exist. The outbound state inverse comes only from the committed state table, and a missing or ambiguous state is an error.

- `tests/server_integration.rs` completes login, renders the fixture chunk, applies a block-break update, and crosses a chunk boundary over an in-memory connection. The host lifts `position`, `position_look`, `look` and status-only `flying` to their narrower consumers.
- The `block_place` body reaches `ServerBound::UseItemOn` with hand, cursor and prediction sequence; literal-byte decoder tests are independent of the adapter encoder and reject out-of-range faces. Arm swings accept hand ordinals 0 and 1 and broadcast to observers. Hotbar selection reaches `CarriedItemChanged` for values `0..=8` only.
- `tests/server_protocol.rs` checks packet ids against the real 1.19.4 capture and decodes literal bodies with negative controls. This is not real-client validation.
- **Containers:** the jar-backed `minecraft:menu` registry (generic 9x3 chest is id 2); `open_window`, `window_items`, `set_slot` and `window_click` have literal fixtures in `tests/inventory.rs`. The server re-derives each click against its authoritative inventory and sends full contents when the client's claimed diff disagrees; only bare item stacks are representable (component-bearing stacks fail at the encoder).
- **External-client acceptance:** row 762 of the opt-in gate (`just external-client-acceptance --protocol 762 --output /private/tmp/lodestone-v762`) uses `configuration.mode: "login_to_play"` and `chunk_batch_acknowledgement.mode: "unbatched"` with `batch_count: 0`. It has not been run; 762 is unverified by a real external client until a manual run produces `report.json`.

### Shape deltas and what carries each

| packet | delta | carried by |
|---|---|---|
| `login`, `respawn` | inline dimension entry becomes type name; optional death location added | this crate's struct |
| `position` (clientbound) | trailing `dismount_vehicle` removed | this crate's struct |
| `spawn_entity` | head-rotation byte inserted; object data `i32` to VarInt | this crate's struct |
| `spawn_entity_living` | gone, folded into `spawn_entity` | deleted |
| `entity_effect` | optional NBT factor data; `0x08` blend flag | this crate's struct |
| `login_success` / `login_start` | signed-property list added / optional profile UUID added | this crate's struct |
| `block_dig`/`block_place`/`use_item` | trailing prediction `sequence` | plain fields |
| `player_info` | action ordinal becomes bitmask; removal split into `player_remove` | hand-written decoder |
| chat, both directions | replaced by three clientbound packets / `chat_message` + `chat_command` | `packets::chat` |

Every delta is at a protocol boundary, so each is this crate's own definition rather than a `since`/`until` predicate. Eight shared definitions widened to 762 instead (the three relative-movement packets, `teleport_confirm`, `arm_animation`, both keep-alives and `resource_pack_receive`), each reported shape-identical 758 to 762 by `minecraft-data` and each decoded from the committed capture. Serverbound `chat` deliberately does not widen: its string is still first, so a widened definition would encode an acceptable prefix and fail only when the server closes the connection.

### Captures and negative controls

`tests/captures/join_{1_19_4,1_18_2,1_20_6}.txt` are clientbound bytes from real servers; `tests/capture_join.rs` holds the `#[ignore]`d recorders and hermetic replays ([captures README](../crates/versions/1.19/tests/captures/README.md)). The era recorder sends one chat message after the join settles and the server echoes it as `player_chat`, the only check on the serverbound signing tail (a malformed one closes the connection). The replay pins server-chosen values: survival on `minecraft:overworld`, the flat floor in canonical ids at the registry-declared height (bedrock floor, dirt above, grass three above and not four, so the probe discriminates), and the message text, chain index and timestamp.

**The chunk buffer is longer than the sections.** The committed column's `chunkData` is 2,268 bytes over 24 sections with 23 left over, one zero byte per single-valued block palette. The decoder reads exactly `section_count` sections then requires the remainder to be all zero and no longer than the section count, giving up only the exact-length half of the detector.

A singleton has no sibling to misroute against, so the control replays real 1.18.2 and 1.20.6 joins through the 762 adapter. Neither neighbour can join 762 at all (758 reads a bare username, 766 requires a 16-byte profile UUID), so both captures use a hand-written login.

| neighbour | errored | silent | plausible wrong events | ids 762 lacks |
|---|---|---|---|---|
| 1.18.2 (758) | 38 | 16 | 10 | 0 |
| 1.20.6 (766) | 42 | 10 | 3 | 10 |

Examples: at 758, `entity_metadata` sits where 762's `held_item_slot` does, so a metadata update becomes a hotbar selection. At 766, `spawn_entity` is id 1 on both sides, so only the shape and entity registry differ: a 1.20.6 minecart reads as a `spawner_minecart` at a plausible position. The guarantee is whole-stream, asserted by `neither_neighbours_capture_replays_as_a_clean_join`, and the measured split is pinned.

## How to change it

- **A second protocol in this era:** there is no candidate. If one appears: `cargo run -p xtask -- gen-packet-ids --source minecraft-data`, run the jar's data generator for `blocks.json` and `registries.json` (check them against the committed dumps first), then add a `PROTOCOL_*` const, a `PROTOCOLS` entry, an `IDS_*` static, an `ids_for` arm, a `play_dispatch_table` slot, a `table_for` arm in `canonical` and `entity_types`, an oracle row in `scripts/live-oracles/legacy.sh`, and a recorder and replay test.
- Never widen a `#[mc(protocols)]` range without evidence from the protocol it claims.
- Signing is blocked on a session key (an authenticated profile), not on wire shape.
- The adapter type is `V762Adapter`; the folder is named for the Minecraft version.
- Keep `V762ServerProtocol` independent of adjacent eras. Add a captured-byte control before changing join, section, light or action framing, and expand the registry entry only with a client-facing test.
- Regenerating jar dumps needs a Java 17 container image (the jar declares `java_version` 17 and ships a bundler, selected through its main-class property).

## Configuration

The era is selected by the `v1-19` feature on `lodestone-registry`, which reads `PROTOCOLS` and selects `V762ServerProtocol` for hosting. Oracle ports are in [`scripts/live-oracles/legacy.sh`](../scripts/live-oracles/legacy.sh): 1.19.4 game 25596 / RCON 25597; 1.20.6 (in no family, only for the neighbour capture) 25598 / 25599. The 1.19.4 row sets `level-type=FLAT` and `enforce-secure-profile=false`.

`minecraft-data` models `player_info`'s `update_listed` as a VarInt where the wire writes a boolean; they coincide for all occurring values.

## Dependencies

`lodestone-core` (`Ctx`, `ProtocolRange`, `Nbt`, `dispatch`), `lodestone-macros` (`since`/`until`/`protocols`/`present_if`), `lodestone-protocol-common`, `lodestone-world` (`PalettedContainer`, `ColumnLight`, `LightPatch`), `lodestone-data` (canonical block states and mob-effect names), `lodestone-model` (`ChatAckInfo`, `ChatSessionInfo`), and `lodestone-server` (the version-free hosted seam). Recording needs Apple `container` and `legacy.sh`; regenerating tables needs the jar's data generator under Java 17; replay needs nothing. Sibling eras: [1.9](./protocol-1-9-era.md), [1.13](./protocol-1-13-era.md), [1.14](./protocol-1-14-era.md), [1.17](./protocol-1-17-era.md).
