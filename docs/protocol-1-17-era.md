# The 1.17 era crate: one family, two protocols, a world that moved

## What it is

`crates/versions/1.17` (package `lodestone-v1-17`) serves Minecraft 1.17.1 and 1.18.2 (protocols 756 and 758, read from each jar's own `version.json`) from one adapter, two generated packet-id tables, one block-state table, one entity table and a handful of explicit shape deltas. It is where the world stopped being sixteen sections tall: the vertical range is data carried in the dimension entry, and a wrong section count consumes the wrong byte count and desynchronises the stream.

## How it works

### Protocol selection

`adapter_for(protocol)` builds a `V756Adapter` that resolves, once, a `&'static PacketIds`, the block `CanonicalTable`, the `EntityTypeTable` and a `ChunkShape`. `V756Adapter::ctx()` builds the `Ctx { version }` so `#[mc(since)]`/`#[mc(until)]`/`#[mc(protocols)]` see the negotiated protocol.

Id drift is clientbound only: 15 of 97 shared clientbound play ids differ (those at or above 1.18's `simulation_distance`); all 47 shared serverbound play ids agree. Every id still goes through the table so a later member cannot move one silently. Dispatch is one `lodestone_core::dispatch::Table` per protocol in a two-slot `OnceLock` array; `simulation_distance` is `IGNORED::ranged` over `758..=758`.

### One block table and one entity table

| table | 1.17.1 | 1.18.2 | check |
|---|---|---|---|
| `blocks.json` | 898 blocks, 20,342 states | identical | dumps byte-identical (MD5 `644230a6388623ab774fac03cc154a9e`), pinned by content hash |
| `minecraft:entity_type` | 113 entries | identical | `both_dumps_agree_id_for_id` compares both committed dumps |

1.18 added no block and inserted no entity. Particle (88 ids) and sound (1,044 ids) registries do differ, but this crate translates neither. `canonical::table_for` and `entity_types::table_for` still route by protocol so a renumbering third member cannot inherit by accident.

### Shape deltas and their mechanism

Adjacency: 79% identity at 1.16.5 to 1.17.1, 94% at 1.17.1 to 1.18.2.

| packet | at | delta | carried by |
|---|---|---|---|
| `login` | 758 | `simulation_distance` after view distance | `#[mc(since = 758)]` field |
| `map_chunk` | 758 | section mask and column biomes removed; per-section biomes and light added | second function |
| `settings` (serverbound) | 756, 758 | trailing flag at 1.17, second at 1.18 | local; `since = 758` on the second |
| `entity_effect`, `remove_entity_effect` | 758 | effect id `i8` to VarInt | second struct |
| `position` | 756 | trailing `dismount_vehicle` bool | plain field |
| `spawn_position` | 756 | trailing `f32` angle | plain field |
| `tile_entity_data` | 758 | action `u8` to VarInt | `IGNORED` |
| `multi_block_change` | 758 | signed packed `y` | handled in adapter |

A field appearing or disappearing fits the derive predicates; a retype cannot, because reading one byte for a VarInt silently misparses any id above 127. 1.17 also split three action-selected packets (title, combat, world border) into fourteen with unchanged bodies, so this crate has fourteen handlers emitting the events the older era emits from three. Shared `#[mc(protocols)]` ranges widened to 758 only for packets measured unchanged (keep-alive, serverbound chat and arm animation, attach, passengers, collect, relative moves, teleport, resource-pack), each exercised by a capture.

### Chunk framing, the real risk

- The vertical window is data: `ChunkShape::from_dimension_nbt` reads `min_y` and `height` from the dimension NBT in `login` and `respawn`. An unusable blob leaves the shape alone. `ChunkShape::overworld` is only the pre-join default (0..256 at 756, -64..320 at 758).
- 756 uses a counted `i64[]` section bitset, always whole columns, a column-wide counted biome VarInt array, and light via `update_light`.
- 758 drops the mask and biome array: every section is present with its own biome container, block entities are positioned records, and light rides in the same packet.
- `update_light`'s four masks are counted `i64[]` bitsets, shared via `lodestone_world::ColumnLight::decode`.

1.18's `chunkData` buffer is longer than its sections: exactly one trailing zero byte per section whose block palette is single-valued (leftovers 23/21/19 across three captured columns). The decoder reads `section_count` sections and requires the remainder be all zero and no longer than the count.

A zero bits-per-entry container is one VarInt plus a VarInt long count of `0`. `lodestone_world::PalettedContainer::decode` returns at zero width without that count (the 1.21.5+ shape), so `packets::chunk::decode_container` peeks the width and handles it.

### Bounded clientbound world and entity events

- Explosion: VarInt count capped at 65,536 and by remaining payload; offsets applied to loaded storage as air with block-entity removal; knockback preserved.
- Game-state codes 1, 2, 3, 7, 8 map to weather or game-mode; other reasons are consumed silently.
- A level event 2001 keeps its protocol-local `BlockStateRef`; never look it up in the canonical table.
- `multi_block_change` applies via `WorldSink::set_blocks`, decodes x/z as signed 22-bit and section Y as unsigned at 756 but signed at 758.
- Equipment resolves item ids through per-protocol jar reports; legacy-NBT items fail explicitly. `block_action` resolves block ids and emits `BlockEvent`.
- `entity_metadata` is decoded but exports only the shared flags byte (index 0); unmodelled particle serializers and unknown type ids are rejected by name.
- Attribute keys `minecraft:generic.*` are normalized to canonical keys; unknown keys are consumed and dropped. Malformed counts and trailing bytes fail closed.

### Hosted protocols

`V756ServerProtocol` and `V758ServerProtocol` (via `server_protocol_for_protocol`) go straight from login success to Play with unbatched chunks.

- Chunks: 756 projects the canonical y=0..255 window into a long-array mask, column biomes and a separate body. 758 requires y=-64..319, sends all 24 sections with their own biome containers, keeps the zero long-array count for single-valued containers and appends inline light. Both require full coverage, no block entities and plains-only biomes; missing or ambiguous states are rejected rather than substituted.
- Join: 758 adds `simulation_distance`.
- `client_command` is one VarInt; `[0x00]` becomes `ClientCommand { action: 0 }` (respawn).
- Death is the split `death_combat_event` (player id, killer id, JSON), followed on respawn by the dimension state and an absolute `position`; dimension bounds stay independent per protocol.
- Containers: one chest session per protocol. `open_window` has the menu registry id and JSON title; `window_items`/`set_slot` carry a state id; `window_click` carries predicted slot pairs and cursor, and the server sends a full correction when they disagree. Legacy NBT or component items are rejected.
- Movement: all four bodies lift (`PlayerMoved`, `PlayerRotated`, `PlayerStatusOnly`); a moved position recentres `ViewTracker`, streams chunks and moves the ticket.
- `block_place` becomes `UseItemOn` (hand, packed position, face, three cursor floats, `inside_block`) with sequence `0`; the server resolves the held item from its own inventory.
- Chat: the one-string body becomes unsigned `ServerBound::Chat`, broadcast as system chat. Arm animation: `0`/`1` become main/off-hand `Swing`; broadcast as the animation id with VarInt entity id and action byte (`0` main, `3` off-hand).

Real 1.17.1 and 1.18.2 clients remain the validation these in-memory tests do not replace. The opt-in gate `just external-client-acceptance --protocol 756|758 --output /private/tmp/lodestone-v756|v758` records `login_to_play` and `unbatched` (0 batches), then requires join, movement, a `start_destroy_block` result and a clean disconnect; it has not been run.

### Captures and the negative control

`tests/captures/join_{1_17_1,1_18_2}.txt` are real clientbound bytes, recorded by an `#[ignore]`d recorder in `tests/capture_join.rs` and replayed hermetically (format in the captures' README). The replay pins survival overworld, the flat preset floor (bedrock at the floor, dirt above, grass three above, none four above) and `the_two_captures_declare_different_vertical_windows` (`(0, 16)` versus `(-64, 24)`), which a hardcoded sixteen-section column fails.

The cross-feed control: `update_time` is id 88 at 756 and 89 at 758. Over the 1.17.1 capture four ids mean something else at 758: one errors, two hit ignored ids and one yields a plausible wrong event (id 101, `declare_recipes` read as `entity_effect`, producing a levitation `MobEffectApplied`). So the guarantee is whole-stream, not per-packet; the 26-agree/1-error/2-silent/1-plausible split is pinned.

## How to change it

- A third protocol: 1.19.4 agrees with 1.18.2 on only 77% of shapes, so it is a new era. If one did fit, generate its id table (`cargo run -p xtask -- gen-packet-ids --source minecraft-data`), dump `blocks.json` and `registries.json` and compare with the committed ones first (any difference splits the shared tables), then add a `PROTOCOL_*` const, `PROTOCOLS` entry, `IDS_*` static, `ids_for` arm, dispatch slot, `table_for` arms, an oracle row in `scripts/live-oracles/legacy.sh` and a `MEMBERS` row with recorder and replay.
- Never widen a `#[mc(protocols)]` range without evidence from the protocol it claims.
- The adapter is `V756Adapter` because the folder is named for the opening version.
- Regenerating jar dumps needs a Java 17 image (1.17.1 and 1.18.2 refuse Java 8); 1.18.2 is a bundler jar, so select the generator via the bundler's main-class property.

## Configuration

Feature `v1-17` on `lodestone-registry`. Oracle ports in `scripts/live-oracles/legacy.sh`: 1.17.1 game 25592 / RCON 25593, 1.18.2 game 25594 / RCON 25595, both `level-type=FLAT` (case-insensitive from 1.17). Unsettled: `minecraft-data` describes the serverbound settings flag with opposite senses at 1.17 and 1.18; it is carried as an opaque flag and needs a server-side oracle to settle.

## Dependencies

`lodestone-core`, `lodestone-macros`, `lodestone-protocol-common` (eleven ranges widened here), `lodestone-world` (`PalettedContainer`, `ColumnLight`, `LightPatch`), `lodestone-data`. Recording needs Apple `container`; regenerating tables needs a Java 17 data generator; replay needs nothing.
