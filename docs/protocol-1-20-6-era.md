# The 1.20.6 era crate: a join with a configuration phase, and items made of components

## What it is

`crates/versions/1.20.6` (package `lodestone-v1-20-6`) serves Minecraft 1.20.5 and 1.20.6 (both protocol 766) from one adapter, one generated packet-id table, one block-state table, one entity registry and the era's own configuration-phase, item-component and chunk-shape code. The protocol number agrees between the jar's `version.json` and `minecraft-data`.

Two breaks reshape the join: a connection now has a configuration phase (login no longer ends in Play; registries, feature flags and tags arrive in a state of their own), and an item stack is a component map (count, item id, components to add, components to remove).

## How it works

### A singleton inside a wider era

Shape identity (named types inlined, primitive aliases kept):

| boundary | identical shapes | identity |
|---|---|---|
| 1.19.4 to 1.20.6 | 119 of 220 | 54% |
| 1.20.4 to 1.20.6 | 177 of 220 | 80% |
| 1.20.6 to 1.21 | 204 of 226 | 90% |

The threshold for one crate serving two protocols is 85%. The lower boundary is real; protocol 767 (1.21, 1.21.1) is the natural second member but `PROTOCOLS` lists only what is checked against real bytes, 766. The next protocol, 774 (1.21.11), agrees with its predecessor on 66% and is a separate era. `adapter_for` selects the id table by negotiated protocol, so adding 767 is a table entry and an arm.

### Configuration phase

`handle_configuration` handles four packets: `registry_data` (records `minecraft:dimension_type` entries in order), `select_known_packs` (answers an empty list), `keep_alive`/`ping` (echoes) and `finish_configuration` (acknowledges, then `SetState(Play)`); tags, feature flags, resource packs and cookies are passed over.

The empty known-packs reply is load-bearing: claiming the core pack lets the server elide registry payloads, so dimension `min_y` and `height` never arrive and every column is unframeable with nothing logged. `start_configuration` can also pull a playing connection back into configuration (resource-pack change, datapack reload); the play table replies `configuration_acknowledged` and returns `SetState(Configuration)`.

### Vertical window

The join packet names no dimension: `SpawnInfo` starts with a varint index into the dimension-type registry. Flow: `registry_data` to `DimensionRegistry::adopt` (keeping `id`, `min_y`, `height`), then join or respawn to `ChunkShape::from_dimension_index`, which returns `None` rather than guess when the index is missing, the payload absent, or `height` is not a positive multiple of 16 (a wrong section count yields a populated but wrong column, not an error). `ChunkShape::overworld` (`min_y` -64, 24 sections) is only the pre-join fallback. `respawn` re-resolves the shape.

### Item components

`packets::slot::Slot` is a varint count, and when non-zero an item id, add-count, remove-count and the components. Payloads have no length prefix, so an unmodelled component cannot be skipped: `read_component_payload` errors by name (`Error::InvalidEnumVariant`). The 56-entry component id table comes from the jar report, cross-checked with `minecraft-data`. The server encoder writes bare stacks only; stacks with component patches are rejected rather than losing data.

### Metadata serializers

31 entries, renumbered here (armadillo-state and wolf-variant serializers inserted). A wrong number misreads silently. Particle, particle-list and optional-global-position are refused by name. `handle_play_entity_metadata` reports only index 0 (shared flags), since every other index is claimed by several categories with one serializer and the adapter has no id-to-category map; the whole entry list is still decoded so unmodelled serializers fail rather than desynchronise.

### Other wire facts

- `chunk_batch_finished` must be answered with `chunk_batch_received` (columns per tick, a request not a measurement) or the server throttles delivery to its floor.
- Login-state disconnect carries a JSON string; configuration and play carry anonymous NBT. Two functions (`json_reason_text`, `nbt_reason_text`) are kept, not a sniffer.
- `multi_block_change`: coordinate long with signed 22-bit x, 22-bit z, 20-bit y; VarInt records `state << 12 | x << 8 | z << 4 | y`; states translate through the 766 table, apply via the batched sink with block-entity sync, and emit one `ClientEvent::SectionBlocksChanged`. `block_break_animation` feeds `ClientEvent::BlockDestruction`.
- Explosion consumes the full packet (centre, radius, offsets, motion, interaction kind, two particles, sound holder). A local schema supplies all 109 particle ids and option shapes, the sound holder is inline or by reference; malformed framing fails before any world write. Offsets become canonical air with block-entity cleanup, with `SectionBlocksChanged` emitted before the explosion event.
- `game_state_change`: reasons 1/2 rain start/stop, 7/8 rain and thunder intensity; reason 3 updates game mode only for an integral ordinal 0..=3.
- `entity_equipment`, `entity_update_attributes` and `block_action` use `generated_registry` (1,330 item, 22 attribute, 1,060 block rows rendered by `tests/registry_mappings.rs`; production binary-searches it). Equipment keeps every continued slot and records added/removed patches as `ItemComponents::has_unmodeled`. Attribute prefixes `generic.*`, `player.*`, `zombie.*` are stripped to canonical keys.
- Anything text-shaped uses `packets::common::NetworkNbt`, not `#[mc(nbt)]` (which reads the named form).

### Hosting (`server_protocol::V766ServerProtocol`)

- Offline login, configuration, Overworld join, teleport, chunk batches, movement, block breaking, drops, held-item release, hand swaps, chat and block use.
- `chat_message` (bounded text, timestamp, salt, optional signature, ack tail) becomes `ServerBound::Chat`; replies use anonymous-NBT `system_chat`. `block_place` lifts hand, signed packed position, face, cursor and sequence into `ServerBound::UseItemOn`, rejecting invalid hand or face. `block_dig` statuses 3/4 drop the stack or one item, 5 releases held-item use, 6 swaps hands; target, face and sequence are padding; unknown statuses are ignored.
- Chunks: `ChunkBatchStart`, columns, `ChunkBatchFinished`; `ChunkBatchReceived` becomes `ServerBound::ChunkBatchAcknowledged`. The shared server keeps `awaiting_chunk_batch_ack` and `pending_chunk_batches`, writing one batch until acknowledged; change batch sizing in the shared consumer, not the codecs. Columns cover y=-64..319 with anonymous-NBT heightmaps, counted palette long arrays and inline light without a trust-edges byte; the inverse state map accepts only unique mappings and rejects unsupported states, non-plains biomes and block entities.
- Light is solved over canonical states with the shared solver and 3x3 loaded neighbourhood (missing columns stay opaque seams); an edit recomputes and sends the whole bounded footprint.
- `src/generated/hosting-configuration.txt` holds all eight synchronized registries plus features and tags recorded from the headless 1.20.6 server (jar SHA-256 `c6d01d018ca782e506f0ec60652d47fd565078be9122b625c1681bc86c29c7ec`), every entry with payload so no known-pack agreement is needed.
- Controls: `tests/server_protocol.rs` (outside wire bytes), `hosting_configuration.rs` (capture, 30 s bound, no gameplay), `server_integration.rs` (Play, chunk receipt, view recentering, break, chat, and lighting: sky 0 inside and 15 outside an enclosed room, torch 14, adjacent air 13, extinction on removal; plus a boundary gate seeing 14 levels enter from an open column). Regenerate with `LODESTONE_REGEN=1 cargo test -p lodestone-v1-20-6 --test hosting_configuration --no-fail-fast -- --ignored --nocapture`.
- External acceptance row 766 (`just external-client-acceptance --protocol 766 --output /private/tmp/lodestone-v766`): configuration completion, an acknowledged chunk batch, join, movement, one `start_destroy_block` result and clean disconnect. Not yet run; 766 is unverified by a real client.

### Evidence

The jar ships no packet report, so ids come from `minecraft-data` (cross-check grade); the authority is a recorded join, `tests/capture_join.rs`, committed as `tests/captures/join_1_20_6.txt`. It asserts both Configuration and Play were reached, every dimension entry has a payload, the column parses to the last byte, the flat floor reads as canonical bedrock/dirt/grass, chat round-trips, and every metadata body decodes to `0xff`.

`unload_chunk` is plain big-endian ints with z first; a square view distance hides a swap, so the recorder teleports the player 1000 blocks along +x only and `unload_chunk_reads_z_before_x` rejects x in `chunk_z`. Block-state and entity tables are pinned by an FNV-1a hash with an `#[ignore]`d drift guard. `cargo xtask connectedness` reports 70/122 decoded, 69/122 emitting, 0 stranded, 32/58 serverbound encoded; the 55 undecoded are in `adapter::IGNORED` with reasons (mostly missing registry tables for item, sound and attribute ids). `tests/packet_parity.rs` has independent literal bodies for metadata, equipment (continued list), attributes and block events (non-square packed position).

## How to change it

- Adding 767: `cargo run -p xtask -- gen-packet-ids --version 1.21 --protocol 767 --source minecraft-data`, add a `packet_ids_from!` static and `ids_for` arm, extend `PROTOCOLS`, widen `#[mc(protocols)]` only on packets measured unchanged, record a second capture. The 22 differing shapes are the work.
- Wiring an ignored packet: move it from `IGNORED` to `CLIENTBOUND` and write the handler, spelling the row as a literal `Handler::new(` (`cargo xtask connectedness` anchors on it).
- Refreshing a registry bridge: update the jar report, adjust pinned counts in `tests/registry_mappings.rs`, run its ignored `committed_table_matches_dump` with `LODESTONE_REGEN=1`. Never borrow another protocol's numeric order.
- New component: extend `read_component_payload` in `packets/slot.rs`; never add a default arm.
- Re-recording: `./scripts/live-oracles/legacy.sh 1.20.6`, then `cargo test -p lodestone-v1-20-6 --test capture_join -- --ignored --nocapture record_1_20_6` (needs RCON).
- Do not derive the protocol from the folder (`1.20.6`), package (`v1-20-6`) or feature; ask `VersionAdapter::supports` or `PROTOCOLS`.

## Configuration

| knob | where | effect |
|---|---|---|
| feature `v1-20-6` | `lodestone-registry` | registers the adapter and `V766ServerProtocol`; off by default |
| `LODESTONE_REGEN=1` | with `--ignored` | rewrites committed generated tables |
| ports 25598 / 25599 | `scripts/live-oracles/legacy.sh` | game and RCON, container `lodestone-mc1206` |

The crate reads no environment variable; the negotiated protocol is resolved once at construction.

## Dependencies

`lodestone-core`, `lodestone-model`, `lodestone-macros`, `lodestone-protocol-common` (only the brand payload; every other shared definition is range-capped at 762 or below and was not widened, since at 766 the resource-pack reply is UUID-keyed, `settings` moved to configuration with two more fields and `abilities` lost its speed hints), `lodestone-data`, `lodestone-world`; hosting uses `lodestone-server`'s protocol trait. Tests add `lodestone-client`, `lodestone-registry`, `lodestone-net`, `lodestone-testsupport`, `tokio`, `serde_json`. Only `lodestone-registry` depends on this crate; `cargo xtask check-deletable 1.20.6` reports removal.
