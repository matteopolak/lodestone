# The 1.9 era crate: one family, four protocols

## What it is

`crates/versions/1.9` (package `lodestone-v1-9`) is the first era crate: one family serving Minecraft 1.9.4, 1.10.2, 1.11.2 and 1.12.2 (protocols 110, 210, 316, 340) from one adapter, four generated packet-id tables and eight explicit shape deltas. It demonstrates the range and per-protocol-table rules in [`plans/multi-version-protocol-dedup.md`](./plans/multi-version-protocol-dedup.md).

## How it works

### Protocol selection

`adapter_for(protocol)` builds a `V340Adapter` (a historical name) that stores the protocol and resolves once a `&'static PacketIds` (the id of every named packet plus that protocol's clientbound `ENTRIES`). `V340Adapter::ctx()` builds the `Ctx { version }` so `#[mc(since/until/protocols)]` see the negotiated protocol.

Each generated table is its own module, named only inside `packet_ids_from!`. 1.12 inserted four clientbound and three serverbound packets mid-table: `update_health` is 62 at 110/210/316 and 65 at 340. `update_health_dispatches_on_each_protocols_own_id` guards it (it was watched failing first: with a shared 340 table, a 110 adapter turned id 62 into `EntityVelocity { entity_id: 65 }`, a plausible wrong event with no error).

Dispatch is one `lodestone_core::dispatch::Table` per protocol in a four-slot `OnceLock` array. A handler or `IGNORED` entry may declare a `ProtocolRange`; `Table::build` skips one excluding the protocol and demands one including it, so one handler list serves differing tables.

### Shape deltas

Measured from `minecraft-data` `protocol.json` with named types inlined. Do not collapse primitive aliases (`varint`, `i64`, `u8`, `f32` all become `"native"`, hiding every retype; a first pass doing so missed `keep_alive`).

| packet | at | delta | carried by |
|---|---|---|---|
| `resource_pack_receive` | 210 | leading pack-hash string dropped | `until = 110` field |
| `named_sound_effect`, `sound_effect` | 210 | pitch `u8` to `f32` | second struct |
| `collect` | 316 | stack-size VarInt added | `since = 316` field |
| `spawn_entity_living` | 316 | mob type `u8` to VarInt | second struct |
| `title` | 316 | action-bar inserted as action `2` | normalised in the arm |
| `block_place` | 316 | cursor `i8`x3 to `f32`x3 | second struct |
| `keep_alive` (both) | 340 | id VarInt to `i64` | second struct |
| entity-metadata types | 340 | NBT joins as type 13 | version gate in the codec |

Field appearing or disappearing fits the predicates (on the shared definitions in `lodestone-protocol-common`, ranges widened to `110..=754`). A retype cannot (reading eight bytes where one to five were sent consumes the next packet's header and corrupts the rest). The metadata gate is real: encoding NBT below 340 errors, and decoding type 13 below 340 errors rather than consuming the packet.

### Hosted protocols (`V110ServerProtocol`, `V210ServerProtocol`, `V316ServerProtocol`, `V340ServerProtocol`)

Logins go straight to Play (no configuration). Protocols 110/210/316 share their own tables (captured position packet id 46; 340 uses 47). Per-family details (literal-body controls for all four tables, in-memory registry-selected controls, none replacing a real client):

- Join and absolute position, then y=0..255 columns as legacy `map_chunk`. Block states go through `lodestone_canonical::inverse` to an exact `(old_id << 4) | meta` or an error (never air). Direct block updates use the same conversion; bulk updates group by ascending section while keeping wire order within a section. A column must cover y=0..255. 110/210 accept old ids 0..=212 and 255, 316 accepts 0..=234 and 255; others are explicit errors. Break decoder: three statuses, six faces.
- Keep-alive: signed VarInt id at 110/210/316, fixed `i64` at 340; the exact echo clears `lodestone-server`'s pending challenge; trailing bytes rejected.
- `settings` (id 4 in all four; locale, distance, VarInt chat mode, colours, skin parts, VarInt hand): only the signed-byte view distance is consumed (`ClientInformationChanged`).
- `held_item_slot`: signed big-endian `i16`, only `0..=8` becomes `CarriedItemChanged`; negatives, 9+, malformed and trailing are ignored.
- Chat: serverbound id 2, one bounded unsigned string (zero timestamp and salt, no signature); broadcast at clientbound id 15 as a JSON literal in the system-chat position.
- Arm animation: one VarInt hand (`0` main, `1` off; others rejected) becomes `ServerBound::Swing`; broadcast uses action `0` main and `3` off hand; the 340 control uses packet id `29` and rejects a third ordinal.
- `block_place`: ordinary targets become `UseItemOn`; the `(-1, -1, -1)`/direction `-1` sentinel becomes `UseItem` with the explicit hand and zero rotation. 110/210 encode cursor as unsigned sixteenths, 316/340 as three big-endian `f32`s; malformed hands, faces, cursor ranges and trailing bytes are rejected. In-memory 110 and 340 controls toggle a lever.
- `use_entity`: attack (`mouse = 1`) is target only, to `ServerBound::Attack`; interact (`0`) adds a VarInt hand; interact-at (`2`) adds three big-endian `f32` coordinates (consumed, validated, dropped) then hand. Unknown mouse or hand values, incomplete coordinates and trailing bytes are ignored. A 340 control toggles a tamed wolf's sitting order.
- Teleport: unlike protocol 47, all four put a teleport id on clientbound `position` and accept `teleport_confirm` (serverbound id 0) as `TeleportationAccepted`, enabling the pending-id gate; a mismatched reply leaves movement inert. The four movement shapes lift per table; controls cross into chunk (1, 0).
- Containers: a canonical `generic_9x3` opens as the old `minecraft:chest` window with a full `window_items`; `window_click` reaches the authoritative consumer, which sends a full correction on mismatch (background changes use `set_slot`); the legacy transaction number is accepted while the server derives the result from slot, button and mode.

### Legacy world-state and attribute packets

- Explosion becomes `ClientEvent::Explosion`: signed `i32` count bounded at 8,192 before allocation, signed-byte offsets kept, local motion always `Some(Vec3)` (even zero; no presence flag); no particle or sound invented. Offsets are applied as canonical air at `floor(centre) + offset` with block-entity removal first.
- `game_state_change`: reason 1 ends rain, 2 begins it, 3 sets game mode only for an integral valid float, 7/8 rain/thunder intensity; others decoded and dropped.
- `block_break_animation`: VarInt breaker, packed position, signed stage to `BlockDestruction`; `0..=9` cracks, `-1` clears.
- `entity_update_attributes`: VarInt id, signed `i32` count, dotted camelCase keys, `f64` bases, VarInt-counted UUID modifiers; at most 128 properties and 1,024 modifiers each, three operations; ten built-in keys map to canonical keys; modifiers get `lodestone:legacy_modifier_<hex>`; unknown keys skip individually.
- Metadata: `crates/versions/1.9/src/entity_metadata.rs` is the supported-field census. Only index 0 (flags, serializer `Byte`) is raised into `EntityMetadataUpdate::flags` for both spawn-embedded and incremental lists; class-specific indices, health, names, pose need an entity-class registry this family lacks. All known serializers decode; unknown ones fail. `tests/entity_metadata.rs` feeds literal bodies per protocol id.

### Exact inversion of legacy ids

`lodestone-canonical::inverse` is the reverse lookup for canonical 26.2 states to `(old_id << 4) | meta`: it scans the forward `canonical::resolve` once, keeps the minimum packed representative among aliases, and indexes with a `OnceLock`. Only exact forward resolutions enter (missing, context-dependent, out-of-bounds and bridge-unmapped are excluded); anything outside the image returns `InverseError::Unsupported`, never a nearby state.

### Captures

`tests/captures/join_{1_9_4,1_10_2,1_11_2}.txt` are real clientbound bytes (recorder and hermetic replay in `tests/capture_join.rs`; format in the captures' README). They are the authority these protocols lack (no data generator before 1.13; `minecraft-data` is cross-check only). On first use they found `play_world_border` had inherited `play_title`'s renumbering (a real action-3 body decoded with 38 trailing bytes). They also fixed the pre-1.10 sound pitch scale: pitches 1.5 and 0.5 put 94 and 31 on the wire from a 1.9.4 server, `pitch * 63` truncated (62 gives 93/31, 64 gives 96/32; only the pair separates them). 1.10.2 and 1.11.2 put exact floats `3fc00000`/`3f000000`, the committed differential.

### External-client acceptance

Rows 110, 210, 316, 340: each records `login_to_play` and `unbatched` chunks then join, movement, one `start_destroy_block` and clean disconnect (`just external-client-acceptance --protocol 110 --output /private/tmp/lodestone-v110`, repeat). Not yet run; all four are unverified by a real client.

## How to change it

- A fifth protocol: `cargo run -p xtask -- gen-packet-ids --source minecraft-data`, then `PROTOCOL_*` const, `PROTOCOLS` entry, `IDS_*` static, `ids_for` arm, `play_dispatch_table` slot, `minecraft_versions` entry, a `MEMBERS` row with recorder and replay.
- Never widen a `#[mc(protocols)]` range without a capture from the claimed protocol.
- `V340Adapter` serves four protocols; renaming touches about 150 references (tests, `lodestone-fuzz`), so do it apart from a wire change.
- `minecraft-data` keeps 1.10.2 under `1.10` and 1.11.2 under `1.11`; `gen-packet-ids` resolves the fallback, so pass the real version and protocol.
- The hosted types are per-protocol on purpose; add a host and an explicit `ServerFamily` predicate before exposing another protocol (the broader `PROTOCOLS` list is not evidence server layouts match).

## Configuration

Feature `v1-9` on `lodestone-registry`; the client adapter reads `PROTOCOLS`, the server registry exposes 110, 210, 316, 340. Oracle ports in `scripts/live-oracles/legacy.sh`, read by `tests/capture_join.rs`'s `MEMBERS`.

## Dependencies

`lodestone-core`, `lodestone-macros`, `lodestone-protocol-common`, `lodestone-canonical`, `lodestone-world`, `lodestone-data`, `lodestone-server`. Recording needs Apple `container`; replay needs nothing.
