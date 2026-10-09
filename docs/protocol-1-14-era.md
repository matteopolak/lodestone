# The 1.14 era crate: one family, three protocols

## What it is

`crates/versions/1.14` (package `lodestone-v1-14`) serves Minecraft 1.14.4, 1.15.2 and 1.16.5 (protocols 498, 578, 754) from one adapter, three generated packet-id tables, three block-state tables, three entity registries and a set of explicit shape deltas. It applies the era-sharing rules in [`plans/multi-version-protocol-dedup.md`](./plans/multi-version-protocol-dedup.md). The folder is named for the opening release and is not a protocol number; ask `VersionAdapter::supports`. The family also hosts all three (`V498ServerProtocol`, `V578ServerProtocol`, `V754ServerProtocol`).

## How it works

### Protocol selection

`adapter_for(protocol)` builds a `V735Adapter` (a historical name; 735 is not served) that resolves, once, a `&'static PacketIds`, the `CanonicalTable`, the `EntityTypeTable` and a `ChunkShape`. `V735Adapter::ctx()` builds the `Ctx { version }` so `#[mc(since/until/protocols)]` see the negotiated protocol.

No clientbound play id past 7 is stable across the era: 1.15 moved `acknowledge_player_digging` to id 8 (shifting 84 ids by one) and 1.16 dropped `spawn_entity_weather` from id 2. Each generated table is its own module, named only inside the `packet_ids_from!` macro. Dispatch is one `lodestone_core::dispatch::Table` per protocol in a three-slot `OnceLock` array; `spawn_entity_weather` is an `IGNORED::ranged` entry over `498..=578`.

### Three data sets

| table | 498 | 578 | 754 | first disagreement |
|---|---|---|---|---|
| block states | 11,271 | 11,337 | 17,112 | state 72 (498 vs 754) |
| entity types | 102 | 103 | 108 | id 4 (bee, 1.15) |
| clientbound play ids | 89 | 89 | 88 | id 8 |

Wire state 11214 is a lantern at 498, a bell at 578 and a prismarine wall at 754 (a trapped chest if unmapped); the committed probe in `tests/canonicalisation.rs` can only pass with all four answers distinct. Tables come from each jar's `--reports` dump under `tests/support/`. Two mappings no dump supplies (wall side properties becoming a `none`/`low`/`tall` enum in 1.16, and jigsaw `facing` becoming `orientation`) come from booting a real 1.15.2 world with those states under the real 26.2 server and reading them back over RCON; probes, procedure and answers are in `tests/support/state_upgrade_1_15_2_to_26_2.txt`. Without them 902 states of each pre-1.16 dump have no mapping.

### Shape deltas and mechanism

Measured from `minecraft-data` `protocol.json`, primitive aliases kept (collapsing hides retypes).

| packet | at | delta | carried by |
|---|---|---|---|
| `login` | 754 | dimension + level type becomes world-name string + two NBT blobs | second struct |
| `login` | 578 | seed hash inserted, respawn-screen flag appended | `since = 578` fields |
| `respawn` | 754 / 578 | numeric dimension becomes NBT / seed hash inserted | second struct / `since = 578` |
| login `success` | 754 | UUID string becomes 128 bits | second struct (shared widens to `47..=578`) |
| `chat` | 754 | trailing sender UUID | `since = 754` |
| `use_entity` (3 forms) | 754 | trailing sneaking flag | `since = 754` |
| `abilities` (serverbound) | 754 | two trailing `f32` removed | `until = 578` |
| `update_light` | 754 | leading `trustEdges` bool | protocol branch |
| `crafting_book_data` to `recipe_book` | 754 | one packet split in two | second struct + `RecipeBookShape` |
| `map_chunk` biomes, long packing | 578, 754 | see below | protocol branch |

A field appearing or disappearing fits the predicates; a retype cannot (reading 16 raw bytes for a length-prefixed string eats the username).

### Chunk and light framing

- Biomes: at 498 a 16x16 big-endian `i32` array sits inside `chunkData` after the last section (the container fabricates a vertical dimension for them, as in v1-8/v1-9); at 578 a bare 1,024-entry (4x4x4) `i32` array, no count, before the buffer; at 754 it gains a VarInt length and VarInt elements.
- Packing: 498/578 use straddling layout (so `PalettedContainer::decode` cannot serve them); 754 pads each long. The declared long count is checked against straddling geometry, which is what makes a 754 column fail in the older decoder.
- `update_light` gained a leading `trustEdges` byte at 754, before four VarInt masks.

`minecraft-data` is wrong here: its 1.14.4 `map_chunk` has no biome field, leaving 1,024 buffer bytes unread, which no round trip can see.

### Hosted protocols

Each host handles its handshake and login shape, goes straight to Play, and emits join, position, chunk, block-update and disconnect packets. Chunk encoders require a 0..256 column and a named heightmap and reject non-plains biomes, block entities and states absent from that protocol's committed table. 498 writes 256 biome ints inside `chunkData` with straddling palettes; 578 writes 1,024 before the buffer, straddling; 754 writes a counted VarInt array with padded palettes. Light-update encoding and most serverbound interactions need their own evidence.

Decoded Play inputs (all rejecting non-Play and malformed bodies, with literal wire fixtures plus registry-selected in-memory controls):
- `block_place` (hand, packed target, face, three cursor floats, `inside_block`) becomes `UseItemOn` with sequence 0.
- `use_entity` (498/578 target, action fields, hand; 754 appends sneaking) with precise hit coordinates consumed; a tamed-wolf interaction is verified end to end.
- Containers: open-screen menu id, full window list, slot correction, click, close, with an in-memory chest move and authoritative corrections; the legacy slot bridge uses each selector's historical item registry and leaves unsupported keys empty.
- `client_command` action `0` (respawn); `arm_animation` (`0`/`1` hands) becomes `Swing`, broadcast as VarInt entity id plus action byte (`0` main, `3` off) into `ClientEvent::EntityAnimation`.
- Movement: `PlayerMoved`, `PlayerRotated`, `PlayerStatusOnly`; moved positions recentre `ViewTracker`, move tickets and stream chunks, verified crossing chunk (0,0) to (1,0).

Each protocol has a literal reference join body (578's seed/respawn fields and 754's NBT join kept distinct from the emitting codec) and an in-memory join, chunk and break test. No live client capture is committed, so that remains the gate before calling the host production-ready.

### Captures

`tests/captures/join_{1_14_4,1_15_2}.txt` are real clientbound bytes with a recorder and hermetic replay in `tests/capture_join.rs` (format in the captures' README). The strongest assertion is the flat floor: every column uniformly canonical bedrock at y=0 and grass at y=3 (ids from `lodestone_data::block_states`), covering biome placement, long packing and the state table at once.

Negative control: unlike the 1.9 era, across all 28 captured packets no misroute yields a plausible wrong event; exact-decode turns each into a trailing-bytes or truncation error or an ignored id (`update_health` is id 72 at 498 and 73 at 754, where 72 is `experience`). `misrouting_between_protocols_is_never_a_plausible_wrong_event` holds the line.

### Incremental world and entity signals

- Single and bulk block changes use the per-protocol canonical table; every cell syncs its block entity and emits a section dirty signal. Bulk layouts: 498/578 name a chunk with `x/z`, `y`, state triples; 754 names a packed section with `state << 12 | local-position`. Records group by section and apply via `WorldSink::set_blocks`, keeping duplicate order. Fixtures (negative section coordinates included) are field-assembled and query the world (`tests/block_updates.rs`).
- Explosion lifts centre, radius, signed offsets and the always-present local impulse; offsets floor the centre, write canonical air into loaded sections and clear block entities.
- Break progress keeps the stage byte exactly. Game-state reasons 1/2/3/7/8 map to rain, game mode and rain/thunder levels; other reasons are consumed silently; the mode must be a finite integer 0..=3.
- Metadata is decoded and only index 0 (flags) exported (later indices are category-reused). Attributes: 498/578 dotted camelCase and 754 `generic.*` keys map to canonical names; unknown keys skip after their modifiers; legacy UUIDs become `lodestone:legacy_modifier_*`; at most 128 properties, 1,024 modifiers each, operations 0..=2.
- 498 appends a metadata list to a living-entity spawn (578/754 do not); the codec gates it and emits flags after the spawn.
- Equipment and block events resolve historical registry ids (498/578 from release-jar reports, 754 from the 1.16 census); absent ids are rejected. 754 equipment is a top-bit-continued sequence, all entries kept; legacy NBT becomes `has_unmodeled`; zero or negative counts are rejected.

### External-client acceptance

Rows 498 (1.14.4), 578 (1.15.2), 754 (1.16.5): each records `login_to_play` and `unbatched` chunks, then join, movement, one `start_destroy_block` and a clean disconnect (`just external-client-acceptance --protocol 498 --output /private/tmp/lodestone-v498`, repeat). Not yet run; all three are unverified by a real client.

## How to change it

- A fourth protocol (none; 1.13.2 carries light in the chunk, 1.17 changes world height): `cargo run -p xtask -- gen-packet-ids --source minecraft-data`, the jar's `blocks.json` and `registries.json`, then a `PROTOCOL_*` const, `PROTOCOLS` entry, `IDS_*` static, `ids_for` arm, `play_dispatch_table` slot, `table_for` arms, a `Source`/`JarSource` row per generator, and a `MEMBERS` row with recorder and replay (about 69 hand-written lines for the second added version).
- Never widen `#[mc(protocols)]` without a capture from the claimed protocol (`LoginSuccess` moved from `47..=340` to `47..=578` with its captures).
- Rename `V735Adapter` (tests and `lodestone-fuzz` touch it) in a change that does not also move the wire. `minecraft-data` has 1.14.4 and 1.15.2 under their own directories, so pass real versions.
- Incremental-world packets keep the order decode, canonicalize, write world, sync block entity, emit dirty section; omitting the last two makes invisible blocks or stale entities. Route fixtures through all applicable id tables.
- Generate and commit one historical registry per protocol before wiring equipment or block events, and test an id that differs between rows.

## Configuration

Feature `v1-14` on `lodestone-registry`; join and hosting both resolve all three protocols. Oracle ports in `scripts/live-oracles/legacy.sh`, read by `tests/capture_join.rs`'s `MEMBERS`.

## Dependencies

`lodestone-core`, `lodestone-macros`, `lodestone-protocol-common` (one range widened), `lodestone-world`, `lodestone-data`, `lodestone-server`. Recording needs Apple `container`; regenerating tables needs each jar's generator; replay needs nothing.
