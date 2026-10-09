# The 1.13 era crate: one family, one protocol, two breaks

## What it is

`crates/versions/1.13` (package `lodestone-v1-13`) serves Minecraft 1.13.2 (protocol 404, read from a live server-list ping) from one adapter, one generated packet-id table, one block-state table, two entity tables and one hosted protocol. It is deliberately a single-member era: 1.13 is the Flattening (flat block-state ids, no item damage, renumbered ids), agreeing with 1.12.2 on 104 of 125 shared packet shapes (72%) and with 1.14.4 on 114 of 142 (73%), both below the ~88% that makes releases one era. Two committed neighbour captures make that falsifiable.

## How it works

### Selection and dispatch

`PROTOCOLS` is `[404]`; `adapter_for` builds a `V404Adapter` resolving `&'static PacketIds`, `CanonicalTable`, `EntityTypeTable` and `ChunkShape` once. The indirection is kept so a second protocol is a table plus an `ids_for` arm; `ids_for`/`table_for` panic outside `PROTOCOLS` rather than answer with a neighbour's numbering. Dispatch is one `lodestone_core::dispatch::Table` over the 86 clientbound `ENTRIES`, a `CLIENTBOUND` list and an `IGNORED` list; an id neither handled nor ignored fails construction. Two ignored entries name no later packet: `minecraft:bed` (removed in 1.14) and `minecraft:entity` (abstract, never sent).

### Clientbound world and entity events

- Consumed: the explosion frame, game-state reasons 1/2/3/7/8, multi-block changes, break overlays, block events and single-slot equipment.
- Flat 404 state ids go through the committed canonical table; the bulk decoder validates the full record list, groups by section and calls `WorldSink::set_blocks` once per touched section (wire order kept for duplicates, one copy-on-write fork per section, not per record). Each write syncs block-entity presence and emits a section dirty signal. `tests/block_updates.rs` queries the real world after dispatch.
- Explosion keeps offsets and the always-present local impulse (including explicit zero); removed offsets are applied at `floor(center) + signed_offset` as canonical air with block-entity removal before the event.
- Metadata is decoded but raises only the universal base fields (flags, optional custom name, name visibility). Attribute names are textual; modifier UUIDs are kept as `minecraft:uuid/<uuid>`. `tests/game_events.rs` pins both through the ingest route.
- `block_action`: packed pre-1.14 position, two opaque bytes, a block-type id from a 598-entry census generated from the vendored 1.13.2 data, deliberately separate from the state table; unknown types fail explicitly.
- `entity_equipment`: one record per packet, resolved against a 789-entry flattened item registry committed from the vendored census (hermetic low/middle/high pins; ignored drift test). Legacy NBT stacks are marked `has_unmodeled`; unknown ids and slot ordinals fail.
- Containers: a canonical `generic_9x3` opens as a string-typed `minecraft:chest` with 27 slots; `window_click` keeps the legacy action counter and pre-click slot while the server derives the result (`tests/inventory.rs`, `tests/container_integration.rs`).

### What breaks at each side

| difference | 1.12.2 | **1.13.2** | 1.14.4 |
|---|---|---|---|
| chunk palette entry | `(id << 4) \| meta` | flat state id | flat state id |
| light | in `map_chunk` | **in `map_chunk`** | `update_light` |
| heightmaps / section block count | absent | **absent** | inline NBT / leading `i16` |
| column biomes | 256 bytes, tail | **256 big-endian `i32`, tail** | 256 `i32` (1.15: 1024, before buffer) |
| packed `position` | `x,y,z` | **`x,y,z`** | `x,z,y` |
| slot | `i16` id + damage | **`present` bool + flat VarInt id** | same as 1.13 |
| `spawn_entity` type | object id space, `i8` | **object id space, `i8`** | unified registry, VarInt |
| `spawn_entity_living` type | mob id space | **unified registry** | unified registry |
| metadata types | 0..12 | **0..15** | 0..18 |
| join/respawn difficulty byte | present | **present** | removed |
| recipe books | 1 | **2** | 4 |

1.13.2 is the only protocol here that is post-Flattening and still carries light in the chunk packet, so neither neighbour's chunk decoder serves it. Fifteen of the 28 packets that change between 1.13.2 and 1.14.4 change only because they carry a position; so `lodestone-protocol-common`'s pre-1.14 `Position` and its two embedding packets widened from `47..=340` to `47..=404`.

### Two entity id spaces

1.13 unified the entity registry (95 alphabetical entries) but not the wire id spaces: `spawn_entity_living` carries a VarInt into the unified registry, while `spawn_entity` carries a signed byte into the pre-1.13 object id space. A real 1.13.2 server spawns `armor_stand` with type 78 (`vex` in the unified registry) and `boat` with 1 (`armor_stand` there), so a unified lookup names a real wrong entity for every object. 1.14 widened the field to a VarInt over the unified registry.

All minecart variants share object id 10 (the variant is in `object_data`), so the table names the family. The object table is generated from the wire transcript `tests/captures/entity_types_1_13_2.txt`; it covers 23 ids (summonable entities spawning via `spawn_entity`) and an uncovered id resolves to `None`.

### Data provenance

| table | source | authority |
|---|---|---|
| packet ids (86 clientbound, 43 serverbound) | `minecraft-data` 1.13.2 | cross-check, verified by the join capture |
| block states (8,599) | jar `--reports` `blocks.json` | authority |
| unified entity registry (95) | `minecraft-data` | cross-check, id-by-id on the wire |
| object id space (23) | wire transcript | authority |
| entity names (144) | jar language file | authority |

The 1.13.2 generator has no registry dump (arrived in 1.14), so entities need an oracle. `minecraft-data`'s entity list is wrong four ways: 123 rows for 95 entries (28 stale pre-1.13 object rows, so id 1 is both `armor_stand` and `boat`); three non-identifiers (`iron_golem}`, `fireworks_rocket`, `commandblock_minecart`) that `/summon` rejects; and object rows with names no version uses. The generator fails on any name absent from the jar language file.

### Captures and boundary controls

`tests/captures/join_1_13_2.txt` is a real join through this adapter. `join_1_12_2.txt` and `join_1_14_4.txt` are joins on the neighbouring protocols recorded with a hand-written handshake and no adapter (a crate may not depend on a sibling; handshake and login `set_compression`/`success` ids are the same in all three). See the captures' README.

The replay pins server-chosen values: every decoded column has canonical bedrock at y=0, dirt at y=1, grass at y=3 (ids from `lodestone_data::block_states`, not this crate), sky light 0 inside the floor and 15 above. That covers inline light, the 256-int biome tail, straddling long packing and the state table at once; any failure makes a populated wrong world, not an error.

Negative control, measured: unlike 1.14, misroutes here do produce well-formed wrong events: 7 of 25 across the lower boundary and 7 of 50 across the upper (a 1.12.2 `abilities` body as `open_sign_entity` yields a `SignEditorOpened` at an absurd position; `update_health` as `entity_velocity`; a 1.14.4 `map_chunk` as `keep_alive` gives `KeepAlive { id: 4294967298 }`). Short fixed-width unvalidated packets decode whatever has the right length. So the guarantee is whole-stream (a neighbour's join never comes out clean); the measured split is asserted so a change surfaces for re-derivation.

### Hosted path (`V404ServerProtocol`)

Login success goes straight to Play with login compression, then fixed-shape join and position packets and a full y=0..255 column: straddling palette sections, block and sky light, and a 256-big-endian-int biome tail. Outbound state ids are the unique reverse of the committed table (`generated_canonical::STATE_TO_CANONICAL`); absent or ambiguous states are rejected. The byte-level control uses dandelion state 1111, and the in-memory control sees it in a chunk, breaks it, and sees air.

- **Teleport:** clientbound `position` carries a teleport id; `teleport_confirm` becomes `ServerBound::TeleportationAccepted`, activating the pending-id gate (movement blocked until the match). `position`, `position_look`, `look`, `flying` decode separately; the control crosses into chunk (1, 0).
- **Keep-alive:** fixed signed `i64` both ways; only an exact echo is lifted, trailing bytes rejected.
- **Settings:** locale, signed view distance, VarInt chat mode, colour flag, skin parts, VarInt main hand; only the view distance (`ClientInformationChanged`) is consumed; six-field layout pinned, trailing byte rejected.
- **Chat:** one serverbound string becomes unsigned chat (zero timestamp and salt); broadcast is a JSON component plus position byte as system chat.
- **`block_dig`:** drop-one, drop-stack, release-use and hand swap lift to server inputs; other statuses are ignored.
- **`arm_animation`:** becomes `Swing`; broadcast `animation` is VarInt entity id plus an unsigned selector (`0`, `3` map to main/off-hand).
- **`block_place`:** packed `x,y,z` before direction and hand, three cursor floats, no held stack or sequence; ordinals validated, held item resolved from the server's inventory, lifted to `UseItemOn` with sequence 0 (negative-coordinate fixture guards the pre-1.14 layout).
- **`use_entity`:** target and action VarInts; interact adds a hand, interact-at adds three hit coordinates (dropped) then hand; only main/off-hand accepted; a live attack and mount are verified. Malformed, truncated, trailing and non-Play frames are ignored.

All are in-memory routing proofs, not release-client acceptance. The opt-in gate `just external-client-acceptance --protocol 404 --output /private/tmp/lodestone-v404` records `login_to_play`, `unbatched` chunks, join, movement, one `start_destroy_block` result and clean disconnect; it has not been run.

## How to change it

- A second protocol (1.13.0/1.13.1 are 393/401, not fetched): generate the id table (`cargo run -p xtask -- gen-packet-ids --source minecraft-data`), the jar's `blocks.json`, then add a `PROTOCOL_*` const, `PROTOCOLS` entry, `IDS_*` static, `ids_for` arm, `play_dispatch_table` slot, `table_for` arms in `canonical` and `entity_types`, an oracle row, and a capture.
- Never widen `#[mc(protocols)]` without evidence from the claimed protocol. Four ranges widened to `47..=404` (`ClientboundChat`, `SpawnPosition`, `BlockDig`, `PlayerAbilities`), each reported unchanged; the first two also decode from the capture.
- Regenerating the entity oracle (`#[ignore]`d, ~90 s) needs a live server; `kill @e` over RCON crashes a ticking 1.13.2 server, and a recorder that does not answer keep-alives is silently disconnected.
- Hosting is `src/server_protocol.rs`; keep the state lookup the reverse of `STATE_TO_CANONICAL`. Add a byte-level control in `tests/server_protocol.rs` and a visible assertion in `tests/server_integration.rs`.

## Configuration

Feature `v1-13` on `lodestone-registry`. Oracle ports in `scripts/live-oracles/legacy.sh`: game 25590, RCON 25591, flat peaceful spawn-free world (`level-type=FLAT` is still correct, measured from `level.dat`; no natural spawns lets the oracle correlate one summon to one spawn packet).

## Dependencies

`lodestone-core`, `lodestone-macros`, `lodestone-protocol-common` (four widened ranges), `lodestone-world`, `lodestone-data`, `lodestone-server` (host seam); tests use `lodestone-client`. Recording needs Apple `container`; regenerating the state table needs the jar generator and a real 26.2 server for its bridging rules; replay needs nothing.
