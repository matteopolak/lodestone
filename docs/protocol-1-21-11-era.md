# The 1.21.11 era crate: a wire era with four protocols, and a client that must answer its own teleports

## What it is

`crates/versions/1.21.11` (package `lodestone-v1-21-11`, feature `v1-21-11`) joins Minecraft 1.21.11, protocol 774, with one adapter, one generated packet-id table, one block-state table, one entity registry and this era's own chunk, velocity, chat and tab-list codecs. The same feature registers `V774ServerProtocol`. The number agrees between the jar's `version.json` and `minecraft-data`; `VersionAdapter::supports` answers from `PROTOCOLS`, never from folder, package or feature names.

## How it works

### The era is four protocols wide, only one implemented

Pairwise packet-shape identity (named types inlined, primitive aliases kept) against 774:

| protocol | Minecraft | identity | in era? |
|---|---|---|---|
| 767 | 1.21, 1.21.1 | 66.8% | no |
| 768 | 1.21.2, 1.21.3 | 75.1% | no |
| 769 | 1.21.4 | 77.3% | no |
| 770 | 1.21.5 | 80.4% | no |
| 771 | 1.21.6 | 88.5% | yes |
| 772 | 1.21.7, 1.21.8 | 87.4% | yes |
| 773 | 1.21.9, 1.21.10 | 94.0% | yes |

So the era is 771-774 and `PROTOCOLS` is `[774]`; the others are unimplemented, not excluded, and each needs its own recorded join. The instrument reproduced the 1.20.6 era's 766-vs-767 figure exactly (204 of 226, 90.3%), confirming 767 belongs to the 1.20.6 era; 768-770 belong to no era.

### The join

774 keeps the three-state join (handshake, login, configuration, play), replies to known-packs with an empty list, resolves vertical windows from `minecraft:dimension_type` by index and answers chunk-batch pacing (see [`protocol-1-20-6-era.md`](./protocol-1-20-6-era.md)). New here:

| mechanism | shape | silent when wrong? |
|---|---|---|
| section paletted containers | no long-array length prefix; single-valued containers have no trailing zero count | no (shared decoder validates the count) |
| heightmaps | `varint`-counted list of `(type, long array)`, not named NBT | only the chunk buffer's length prefix catches it |
| entity velocity | packed: one byte when zero, else two bytes plus big-endian `u32` and optional scale varint | yes (eats five bytes of a stationary entity's successor) |
| `add_entity` order | velocity before the three angle bytes | yes |
| `player_info_update` tail | list-order priority varint before the hat bool | yes |
| `forget_level_chunk` | `chunk_z` then `chunk_x` | yes (square view hides it) |
| teleports | leading varint id; 32-bit relative-flag word, nine assigned bits (four velocity) | no |
| movement | flags byte (`0x01` on ground, `0x02` horizontal collision) on all four packets | no |
| chat | server-global index leads `player_chat`; format is an `id + 1` holder; serverbound ends in a checksum byte | no (server closes) |

The recorded join sends a chat message because a serverbound acknowledgement tail a real server rejects is not observable from a capture otherwise.

### Clientbound world and entity events

- `explode`: `f64` centre, `f32` radius, `i32` block count, optional `f64` knockback vector, then a particle entry, sound holder and weighted block-particle list. Particle options use a local 774 schema (block state, colour, power, item slot, vibration, trail); the vibration discriminator is validated before its body. The adapter consumes the whole tail and requires the packet to end (stopping early would misframe the next). The event carries centre, radius and impulse only, since removals arrive as block updates.
- `block_destruction`: breaker varint, packed position, raw stage byte to `ClientEvent::BlockDestruction`; out-of-range stages stay raw so an overlay can clear a crack.
- `set_equipment` translates the 1,505-item registry via `src/item_registry.rs` (explicit empty slots, opaque component patches); `update_attributes` validates a 31-key mapper, bounded modifiers and string UUIDs; `block_event` translates the 1,166-entry block registry via `src/block_registry.rs` and keeps both opaque bytes.
- `set_entity_data` resolves the serializer list and emits only the shared flags byte as `EntityMetadataUpdated` (the literal body in `tests/packet_events.rs`: entity 300, index 0, serializer 0, flags `0xa1`, `0xff`).
- `block_update` and `section_blocks_update` resolve protocol-local flat ids and write through the canonical sink (the latter as one `WorldSink::set_blocks` batch: single section fork, per-cell block-entity sync); both emit section-relative dirty cells, an empty bulk change none.
- The registry bridges are generated from `minecraft-data` and map by canonical name into 26.2 model enums (never index canonical ids directly: later releases insert entries). `tests/support/protocol_774_registries.json` pins the ordered name extract (SHA-256 `e6c7331e064307012bd6460f0bc5e029dcdd5abfc6f144883cf47ced1441adfb4`; item table `0031a948bf8210aaf68d4964499cb2c31f2c3bf5d7339439540ca82a9eb28686`, block table `894b926701b73047f1e24ad9650356d8d6f4c1a6d79f65143facbc9c58598a47`); drift guards resolve every name.

### Evidence

- The jar's data-generator reports are the authority: it emits a packet report, so `src/generated/packet_ids.rs` is generated from it (`cargo run -p xtask -- gen-packet-ids --version 1.21.11 --protocol 774 --source mojang`); `minecraft-data` agrees on all 220 ids. Block-state and entity tables come from jar dumps under `tests/support/` with pinned content hashes.
- `tests/captures/join_1_21_11.txt` holds real server bodies, recorded by the `#[ignore]`d half of `tests/capture_join.rs` against `scripts/live-oracles/mc-1-21-11.sh` and replayed hermetically. It settled four of the six silent rows above, two against the earlier port.
- `minecraft-data` is cross-check grade and internally inconsistent on the tab-list tail (field order one way, bit assignment the other); the capture settles order, bit assignment is still its claim (noted in `packets::player_info`).

Two outside constants pin codecs a round trip cannot: packed velocity decodes every non-zero capture velocity to -0.0784 block/tick in `y` (gravity then drag, `-0.08 * 0.98`) within one quantisation step (`2 / 32766`), independently implemented in `lodestone-physics`; and all 29,671 block states resolve to a 26.2 state by name and property set with no rename table, with a control proving the reverse index can miss.

Cost of founding: 8,278 hand-written lines, 31,691 generated, 2,362 test (`xtask codegen-ratio`), the largest of the era crates. `cargo run -p xtask -- connectedness` reports 64/139 clientbound decoded, 63 emitting, 0 stranded, 34/66 serverbound encoded, 63 arms examined, 0 unclassified. The `CLIENTBOUND` table spells each entry with a literal `Handler::new(` after its resource-name string; hiding it behind a helper drops the score to 0/139 while the crate still works (it happened to the 1.20.6 era).

### Death and respawn

Three-packet boundary: `V774ServerProtocol::decode` accepts `client_command` action `0`; `encode_player_combat_kill` sends victim id and a network-NBT message; `encode_respawn_with_teleport_id` sends the state-reset `respawn` then an absolute `player_position`. `V774Adapter::handle_play_player_combat_kill` emits the shared death event and the existing respawn handler clears death state. `tests/death_respawn.rs` keeps directions independent with literal bytes (a health update alone does not clear a death screen, nor does a position correction rebuild state).

### Hosting (`V774ServerProtocol`)

- Offline login, configuration, Overworld join and teleport, chunk batches, movement, block-break updates, block-target uses and held-item air uses (hand plus yaw/pitch at use time, for projectile direction). Movement decodes the grounded bit from the flags byte.
- `player_loaded` reaches `ServerBound::PlayerLoaded` (trailing byte rejected); the default `lodestone-client` emits it after the placement teleport and the connection then enables fall simulation. Health updates follow applied damage.
- Chunk batching: `ChunkBatchStart`, columns, `ChunkBatchFinished`; `ChunkBatchReceived` becomes `ServerBound::ChunkBatchAcknowledged`; the shared server keeps `awaiting_chunk_batch_ack` and `pending_chunk_batches` (one batch until acknowledged). `chunks_per_tick` is the client's request; change batch sizing in the shared consumer.
- `src/generated/hosting-configuration.txt`: all 23 synchronized registries plus features and tags from the headless server (jar SHA-256 `f83b8e093865806f931c7e34aae41b177d4c076335263dd124c75d6d65dd1726`), every entry with payload, no known-pack agreement.
- Chunks cover y=-64..319 with typed heightmap arrays, uncounted palette long arrays and inline light. Teleports carry leading id, three velocity fields and 32-bit flags. States need a unique exact inverse; unsupported states, non-plains biomes and block entities fail explicitly. Light uses the shared solver with a 3x3 loaded neighbourhood (missing columns stay opaque seams); an edit recomputes the bounded footprint.
- Basic container session: the adapter consumes `open_screen`, `container_set_content`, `container_set_slot`, `container_set_data`, `container_close`; `container_click` with plain stacks encodes through the adapter and decodes in the host. Wire: VarInt window and state ids, signed-short slot, component-shaped slots; only empty slots and plain stacks are supported, componentful predictions rejected. Menu ordering and item bridge are local. The joined-server control submits a forged prediction and verifies the server corrects it and never persists client-supplied contents.
- Controls: `tests/server_protocol.rs`, `hosting_configuration.rs` (30 s bound, no gameplay), `server_integration.rs` (login, Play, chunks, recentering, both hands' uses, break, and lighting: sky 0 inside and 15 outside, torch 14, adjacent air 13, extinction; boundary gate sees 14 levels entering an open column). Regenerate with `LODESTONE_REGEN=1 cargo test -p lodestone-v1-21-11 --test hosting_configuration --no-fail-fast -- --ignored --nocapture`.
- External acceptance row 774 (`just external-client-acceptance --protocol 774 --output /private/tmp/lodestone-v774`): configuration, acknowledged batch, join, movement, one `start_destroy_block`, clean disconnect. Not yet run.

## How to change it

- Adding 771, 772 or 773: widen `PROTOCOLS` and the `#[mc(protocols = "774..=774")]` ranges, add an id table, and record a join for it (87-94% identity means the remaining 6-13% is what a capture finds and a widened range hides).
- Each of the 76 `IGNORED` entries records why that packet has no consumer; a negative control asserts dropping one fails construction.
- Re-record: `./scripts/live-oracles/mc-1-21-11.sh`, then `cargo test -p lodestone-v1-21-11 --test capture_join -- --ignored --nocapture record_1_21_11` (the recorder writes the file before asserting completeness).
- Regenerate tables: `LODESTONE_REGEN=1` with the `#[ignore]`d generator in `tests/canonicalisation.rs` or `tests/entity_types.rs`; dump hashes are pinned.

Gotchas:
- A recorder must answer its own teleports with a movement packet. Echoing the id is not enough: until the client reports a position at the new location the server unloads every column and then sends nothing, with no error or log line.
- The unload-order check needs an asymmetric move: the recorder goes 1000 blocks along +x and back and keeps only columns dropped on the return leg.
- The dimension registry carries payloads only because this client claims no known packs.
- Every text component and registry payload is anonymous NBT; `#[mc(nbt)]` reads the named form, so use `packets::common::NetworkNbt`.
- The keep-alive pair is local because the shared definition is declared `340..=762` and refuses to encode here.
- The entity registry is insertion order with mid-list inversions (four pinned by id), not alphabetical.

## Configuration

- Feature `v1-21-11` on `lodestone-registry`; off by default (the shell's `live` feature enables only `v26-2`); `just check-seam` proves the shell compiles with no family.
- Oracle `scripts/live-oracles/mc-1-21-11.sh`: container `lodestone-mc12111`, Apple `container`, game 25604, RCON 25605 (password `lodestone`, read in `tests/capture_join.rs`'s `ERA`), `eclipse-temurin:21-jdk`, 3 GB, flat, offline, secure-profile enforcement off, hostile and ambient spawning off.
- `LODESTONE_REGEN=1` switches the two table tests from asserting to generating.

## Dependencies

`lodestone-core`, `lodestone-macros`, `lodestone-model`, `lodestone-world`, `lodestone-data`, `lodestone-protocol-common` (ranges cover 774), `lodestone-server`; registration in `lodestone-registry` (`cargo run -p xtask -- check-deletable 1.21.11` reports it deletable). Tests add `lodestone-client`, `lodestone-net`, `lodestone-testsupport`, `tokio`, `serde_json`, `uuid`.
