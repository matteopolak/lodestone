# The 1.21.11 era crate: a wire era with four protocols, and a client that must answer its own teleports

## What it is

`crates/versions/1.21.11` (package `lodestone-v1-21-11`, feature `v1-21-11`)
joins Minecraft **1.21.11** — protocol **774** — with one adapter, one
generated packet-id table, one generated block-state table, one generated
entity registry, and this era's own chunk, velocity, chat and tab-list codecs.
The same feature registers `V774ServerProtocol` for hosting protocol 774.

The protocol number comes from two independent sources that agree. The jar's
own `version.json` inside `.cache/mc/1.21.11/server.jar` reports
`"protocol_version": 774`, and `minecraft-data`'s `protocolVersions.json` maps
1.21.11 to 774. Nothing about the number is taken from the folder name, the
package suffix or the feature: `VersionAdapter::supports` answers for it, from
`PROTOCOLS`.

## How it works

### The era is four protocols wide, and only one is implemented

Measured, not assumed. The era-grouping criterion in
[`docs/plans/multi-version-protocol-dedup.md`](./plans/multi-version-protocol-dedup.md)
is pairwise packet-shape identity with named types inlined recursively and
primitive aliases kept; this table applies that 85% threshold to 774's
neighbours:

| against 774 | Minecraft | identity | inside the era? |
|---|---|---|---|
| 767 | 1.21, 1.21.1 | 66.8% | no |
| 768 | 1.21.2, 1.21.3 | 75.1% | no |
| 769 | 1.21.4 | 77.3% | no |
| 770 | 1.21.5 | 80.4% | no |
| 771 | 1.21.6 | 88.5% | **yes** |
| 772 | 1.21.7, 1.21.8 | 87.4% | **yes** |
| 773 | 1.21.9, 1.21.10 | 94.0% | **yes** |

So the era is **771–774**, Minecraft 1.21.6 through 1.21.11, and the break sits
between 770 (80.4%) and 771 (88.5%). The crate ships a `PROTOCOLS` of one
number all the same: the other three are unimplemented, not excluded, and each
needs its own recorded join before it is claimed. Widening `PROTOCOLS` without
one is the mistake this measurement exists to make expensive rather than easy.

The instrument was validated before being trusted, by reproducing a published
figure from an earlier era's doc exactly: 766 against 767 comes back at 204 of
226 shapes, 90.3%, the same number the 1.20.6 doc records. That also **confirms**
the finding this crate inherited — 767 clears the threshold against 766, so
1.21 and 1.21.1 belong to the 1.20.6 era rather than this one — and refines it:
768, 769 and 770 clear the threshold against *neither* neighbour, so three
protocols currently belong to no era at all.

### The join, and the parts of it that are not the era below's

774 keeps the three-state join the 1.20.6 era introduced (handshake → login →
**configuration** → play), replies to the known-packs offer with an empty list
so no registry entry arrives elided, resolves a column's vertical window from
the `minecraft:dimension_type` registry by **index**, and answers the chunk-batch
pacing packet. All of that is described in
[`protocol-1-20-6-era.md`](./protocol-1-20-6-era.md) and is not repeated here.

What is new, and what each costs when missed:

| mechanism | shape | silent when wrong? |
|---|---|---|
| section paletted containers | no `varint` long-array length prefix; single-valued containers have no trailing zero count either | no — the shared decoder validates a declared count against the layout |
| heightmaps | a `varint`-counted list of `(type, long array)` pairs, not a named-NBT compound | only because the chunk-data buffer's own length prefix follows, and the decode is bounded and length-checked |
| entity velocity | a packed variable-length vector: one byte when zero, otherwise two bytes plus a big-endian `u32`, and an optional trailing scale varint | **yes** — it eats the five bytes after it for a stationary entity |
| `add_entity` field order | velocity **before** the three angle bytes, not after the type-specific data | **yes** — same byte count either way |
| `player_info_update` tail | list-order priority (`varint`) **before** the hat flag (`bool`) | **yes** — both are one byte for the values a server sends |
| `forget_level_chunk` | `chunk_z` then `chunk_x` | **yes** — invisible in a square view distance |
| teleports | a `varint` teleport id **leads** the packet, and the relative-flag set is a 32-bit word with nine assigned bits, four of them velocity | no |
| movement | a flags byte (`0x01` on ground, `0x02` horizontal collision) on all four serverbound movement packets | no |
| chat | a server-global index leads `player_chat`; the chat format is a registry-entry holder writing `id + 1`; the serverbound packet ends in a checksum byte | no — the server closes the connection on a malformed one |

The last row is why the recorded join sends a chat message rather than only
receiving one: a serverbound acknowledgement tail that a real server rejects is
not otherwise observable from a capture.

### The evidence, and where each claim rests

Three sources, in descending authority, and the doc says which carries which:

* **The jar's own data-generator reports.** 1.21.11 emits a packet report, so
  `src/generated/packet_ids.rs` is generated straight from it
  (`cargo run -p xtask -- gen-packet-ids --version 1.21.11 --protocol 774
  --source mojang`) rather than reconstructed. `minecraft-data`'s own table for
  1.21.11 agrees on **all 220 ids across all five states**, which is a
  cross-check rather than the source. The block-state and entity-type tables
  come from the jar too, via committed dumps under `tests/support/` whose
  content hashes the tests pin.
* **A recorded real join, replayed hermetically.** `tests/captures/join_1_21_11.txt`
  holds the bodies a vanilla server actually sent, recorded by the `#[ignore]`d
  half of `tests/capture_join.rs` against
  `scripts/live-oracles/mc-1-21-11.sh`. The default test run replays it through
  the real adapter. Four of the six "silent when wrong" rows above were settled
  by those bytes, and two of them were settled *against* the port that existed
  first.
* **`minecraft-data`.** Cross-check grade, and it is internally inconsistent
  about the tab-list tail — it lists the two fields in one order and assigns
  their bits in the other. The capture settles the field order; the bit
  assignment is still minecraft-data's, and the doc comment in
  `crates::packets::player_info` says so rather than implying otherwise.

Two independent outside constants pin codecs that a round trip could not:

* The packed velocity's bit layout is pinned by **vanilla's falling-entity
  integration**: gravity then vertical air drag is `-0.08 * 0.98 = -0.0784`
  block/tick, and every non-zero velocity in the capture decodes to that in `y`
  to inside a single quantisation step (`2 / 32766`). That number comes from
  the physics, is independently implemented in `lodestone-physics`, and cannot
  be produced by this crate's own codec.
* The block-state table is checked against the **26.2** registry rather than
  against itself: all 29,671 of this era's states resolve to a 26.2 state by
  name and property set with no rename table at all, and a control test proves
  the reverse index can still miss a name.

### The measured cost of founding it

8,278 hand-written source lines (`cargo run -p xtask -- codegen-ratio`), plus
31,691 generated lines and 2,362 test lines. This is the largest measured
founding cost among the implemented era crates.

`cargo run -p xtask -- connectedness` reports **63/139 clientbound decoded, 62
emitting, 0 decoded-but-stranded, 34/66 serverbound encoded, 63 arms examined**
— and, importantly, zero arms it could not classify. That command hard-fails
when it finds dispatch evidence it cannot parse, so the `CLIENTBOUND` table in
`src/adapter.rs` spells each entry with a literal `Handler::new(` immediately
after its resource-name string, which is one of the two spellings the reader
recognises. Restructuring that table behind a helper is the single easiest way
to take this crate's score to 0/139 while leaving it working, which is what
happened to the 1.20.6 era.

## How to change it

* **Adding a version to this era** (771, 772 or 773) means widening `PROTOCOLS`
  and the `#[mc(protocols = "774..=774")]` ranges, adding a generated id table
  for the new number, and **recording a join capture for it**. The measurement
  above says the shapes are 87–94% identical, not identical; the remaining
  6–13% is exactly what a capture finds and a widened range hides.
* **The `IGNORED` table is not a to-do list with reasons stripped.** Each of its
  76 entries records why that packet has no consumer. A negative-control test
  asserts that dropping one entry fails table construction, so the table cannot
  quietly fall out of sync with the id list.
* **Re-recording the capture** needs the oracle running
  (`./scripts/live-oracles/mc-1-21-11.sh`) and then
  `cargo test -p lodestone-v1-21-11 --test capture_join -- --ignored --nocapture
  record_1_21_11`. The recorder writes the file *before* asserting its own
  completeness, so a run that reaches the wire and then fails a check still
  leaves the bytes to diagnose it with.
* **Regenerating a table** is `LODESTONE_REGEN=1` plus the `#[ignore]`d
  generator test in `tests/canonicalisation.rs` or `tests/entity_types.rs`; both
  assert against the committed dump otherwise, and both pin the dump's own
  content hash so a silently swapped dump fails rather than regenerating.

### Gotchas

* **A recorder must answer its own teleports with a movement packet.** Echoing
  the teleport id is necessary and not sufficient: until the client reports a
  position of its own at the new location, the server unloads every column the
  client had and then sends nothing further, indefinitely — no error, no
  disconnect, no log line. This cost three full-length diagnostic runs.
* **The unload-order check needs an asymmetric move.** `forget_level_chunk`'s
  two coordinates are indistinguishable under a swap for a stationary player, so
  the recorder moves 1000 blocks along `+x` alone and back, and keeps only the
  columns dropped on the **return** leg — the outbound ones are the spawn area,
  whose chunk x and z are both near zero.
* **The dimension registry only arrives with payloads because this client claims
  no known packs.** A client that claims the vanilla pack gets entries with no
  data, and a column then has no vertical window to be framed against.
* **Anonymous NBT everywhere.** Every text component and registry payload on
  this wire is binary NBT with no root name. The derive's `#[mc(nbt)]` reads the
  *named* form and is therefore unusable at this protocol;
  `packets::common::NetworkNbt` is the newtype that participates in the derive
  and reads the anonymous form.
* **The keep-alive pair is defined locally** rather than re-exported from
  `lodestone-protocol-common`, whose otherwise-identical definition is declared
  `340..=762` and refuses to encode here.
* **The entity registry is insertion order with mid-list inversions**, not
  alphabetical with additions appended. A test pins four specific inversions by
  id, because the obvious hypothesis is wrong and asserting it merely
  "not sorted" would pass for the wrong reason.

## Configuration

* Cargo feature **`v1-21-11`** on `lodestone-registry` turns the family on. No
  family is enabled by default, and the shell's default `live` feature turns on
  `v26-2` only, so this crate is invisible unless the feature is named.
  `just check-seam` proves the shell still compiles with no family at all.
* The live oracle is `scripts/live-oracles/mc-1-21-11.sh`: container
  `lodestone-mc12111` under Apple `container` (not Docker), game port
  **25604**, RCON port **25605**, password `lodestone`, image
  `eclipse-temurin:21-jdk`, 3 GB, `level-type=flat`, offline mode, secure-profile
  enforcement off, hostile and ambient spawning off. Those two port numbers are
  defined in that script and read in one place, `tests/capture_join.rs`'s
  `ERA` constant.
* `LODESTONE_REGEN=1` switches the two table tests from asserting to
  generating.

## Hosting

`server_protocol::V774ServerProtocol` implements offline login, configuration,
the Overworld join and teleport, chunk batches, and block breaking updates.
Its `src/generated/hosting-configuration.txt` contains registry, feature and
tag packets extracted from this family's committed real-server join capture.
The capture retains four registries; all retained entries have payloads, so
these entries do not require known-pack agreement. The remaining synchronized
registries still need authoritative fixtures and external-client validation.
Dimension and plains biome IDs come from that ordered registry stream.

Chunks cover y=-64 through 319 with typed heightmap arrays, uncounted palette
long arrays and inline light framing. The teleport includes its leading ID,
three velocity fields and 32-bit flags. Canonical block states must have a
unique exact inverse; unsupported states, non-plains biomes and block entities
produce explicit chunk-encoding errors. Light arrays are currently empty;
external-client lighting and broad gameplay acceptance remain unverified.

Extend the family's `server_protocol` and its `tests/server_protocol.rs` wire
controls together. When replacing the authoritative capture, re-extract
configuration packet IDs 7, 12 and 13; the fixture identity test checks the
production payloads against that capture. `tests/server_integration.rs` verifies
registry-selected login, Play, chunk receipt and a block break through the real
integrated-server loop.

## Dependencies

`lodestone-core` (codec primitives, `Reader`/`Writer`, anonymous NBT, dispatch
tables), `lodestone-macros` (the `Encode`/`Decode`/`Packet` derives and the
`#[mc(protocols)]` range), `lodestone-model` (`ClientEvent`, `Directive`,
`ClientAction`, `VersionAdapter`), `lodestone-world` (`ChunkColumn`,
`PalettedContainer`, `Heightmaps`, `ColumnLight`), `lodestone-data` (the
canonical 26.2 registries the wire is translated into),
`lodestone-protocol-common` (shared packet definitions whose declared ranges
cover 774), and `lodestone-server` (hosting protocol and checked chunk boundary).
Registration lives in
`lodestone-registry`; `cargo run -p xtask -- check-deletable 1.21.11` reports
the crate cleanly deletable, which is the property the version seam exists to
keep.

For tests: `lodestone-client` and `lodestone-registry` for integrated hosting,
`lodestone-net` (the framed connection the recorder joins with),
`lodestone-testsupport` (unique usernames, the async RCON client), `tokio`,
`serde_json` and `uuid`.
