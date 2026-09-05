# The 1.14 era crate: one family, three protocols

## What it is

`crates/versions/1.14` (package `lodestone-v1-14`) serves Minecraft 1.14.4,
1.15.2 and 1.16.5 — protocols 498, 578 and 754 — from a single adapter, three
generated packet-id tables, three generated block-state tables, three
generated entity registries, and nine explicitly-carried shape deltas, rather
than three copies of a family. It is the second era crate, after
[`the 1.9 era`](./protocol-1-9-era.md), and applies the range and era-sharing
rules in [`docs/plans/multi-version-protocol-dedup.md`](./plans/multi-version-protocol-dedup.md)
to the pre-1.17 legacy gap between 1.13 and 1.17.

The folder is named `1.14` for the era's opening release. It has never been a
protocol number, and now it is not even a single protocol — ask
`VersionAdapter::supports`.

The family also provides server protocols for 498, 578 and 754. Each selector
uses its separate packet registry and chunk encoder, with its committed state
table as the sole canonical-state inverse. Hosting remains intentionally
narrower than full server behaviour: light updates and most Play actions still
need their own protocol evidence.

## How it works

### Protocol selection

`PROTOCOLS` lists all three. `adapter_for(protocol)` constructs a
`V735Adapter` that stores that protocol and, resolved once at construction,
four things keyed by it: a `&'static PacketIds` (the id of every packet the
adapter names plus that protocol's whole clientbound `ENTRIES` slice), the
`CanonicalTable` for its block-state numbering, its `EntityTypeTable`, and a
`ChunkShape` carrying both. `V735Adapter::ctx()` builds the `Ctx { version }`
every codec call reads, so a `#[mc(since)]`/`#[mc(until)]` predicate and a
`#[mc(protocols = "a..=b")]` precondition both see the negotiated protocol
rather than a constant.

The indirection is the point, and here it is not a formality: **no clientbound
play id past 7 is stable across the era.** 1.15 moved
`acknowledge_player_digging` from the end of the table to id 8, shifting 84
ids by one; 1.16 dropped `spawn_entity_weather` from id 2, shifting almost
everything back. Each generated table is its own module, and nothing outside
the `packet_ids_from!` macro may name one.

Dispatch is one `lodestone_core::dispatch::Table` per protocol, cached in a
three-slot array of `OnceLock`s indexed the same way `ids_for` resolves a
table. `spawn_entity_weather` is an `IGNORED::ranged` entry covering
`498..=578`, so 754's table does not fail construction on a stale entry and
the older two do not fail on an unlisted id.

`V498ServerProtocol`, `V578ServerProtocol` and `V754ServerProtocol` handle
their era-specific handshake and login shapes, transition directly to Play,
and emit join, initial position, chunk, block-update and Play-disconnect
packets. Each chunk encoder requires a 0..256 column, writes a named
heightmap, and rejects a non-plains biome, block entity, or canonical state
absent from that protocol's committed table rather than silently substituting.
Protocol 498 writes 256 fixed biome integers inside the length-prefixed
`chunkData` buffer after its straddling section palettes; protocol 578 writes
1,024 fixed biome integers before the buffer and also uses straddling palettes;
protocol 754 writes a length-prefixed VarInt biome array and padded palettes.
Light-update encoding and many interaction/inventory serverbound actions are
still outside this host slice and require their own protocol evidence. A
right-click against a block is the supported interaction exception: each host
decodes the shared 1.14+ `block_place` body (hand, packed target, face, three
cursor floats, `inside_block`) into `ServerBound::UseItemOn`. The three
revisions predate block-prediction sequences, so that consumer input uses zero
rather than inventing one. Literal wire bodies prove the decoder separately
for 498, 578 and 754; adapter-to-registry tests prove the matching producer and
host agree, including rejection outside Play and for an invalid face.

The host tests anchor packet ids in the committed generated tables and exercise
each differing chunk framing against the crate's independent decoder. Every
hosted protocol also has a literal reference join body and its own in-memory
client/server acceptance test: the registry selects 498, 578 or 754, the
matching adapter reaches Play, receives the fixture chunk, and observes its
block-break update. The literals keep 578's appended seed/respawn fields and
754's NBT-bearing join distinct from the codec that emits them. These tests
prove the local consumer chain from registry selection through server wire and
client state, but a live client/server acceptance capture for these host
selectors is not yet committed, so it remains a gate before calling this host
production-ready.

All three hosts also decode the four ordinary Play movement bodies. Position
and position-with-look lift to `ServerBound::PlayerMoved`; look-only lifts to
`PlayerRotated`; and the grounded-only body lifts to `PlayerStatusOnly`. The
position-bearing forms reach `lodestone_server::dispatch_play_packet`, which
recenters `ViewTracker`, moves the connection's chunk tickets, publishes the
tick anchor, and streams the newly visible chunk strip. The literal protocol
tests use negative and fractional coordinates rather than the client encoder,
and the registry-selected in-memory tests cross from chunk `(0, 0)` to `(1,
0)` and wait for that chunk to arrive. That proves the local action-to-stream
consumer chain; it does not replace the outstanding real-client gate.

### Three data sets, not one

This era's per-protocol data is not only packet ids. All three of these
produce a real-but-wrong answer when shared, never an error:

| table | 498 | 578 | 754 | first disagreement |
|---|---|---|---|---|
| block states (`canonical`) | 11,271 | 11,337 | 17,112 | state 72 (498 vs 754) |
| entity types (`entity_types`) | 102 | 103 | 108 | id 4 (bee inserted in 1.15) |
| clientbound play ids | 89 | 89 | 88 | id 8 |

Wire block-state **11214** is a lantern at 498, a bell at 578 and a prismarine
wall at 754 — and a trapped chest if left unmapped. That four-way split is the
committed probe in `tests/canonicalisation.rs`; it exists because no pair of
those answers can coincide, so the test cannot pass for the wrong reason.

The block-state and entity tables are generated from each jar's own `--reports`
dump, committed under `tests/support/`. Two mappings those dumps cannot supply
— a wall's four side properties turning from booleans into a `none`/`low`/`tall`
enum in 1.16, and the jigsaw block's `facing` becoming `orientation` — are not
read off any table. They are what vanilla itself produced when a real 1.15.2
world carrying those exact states was booted under the real 26.2 server jar and
read back over RCON; the probes, the procedure and the answers are committed
verbatim in `tests/support/state_upgrade_1_15_2_to_26_2.txt`. Without them 902
of each pre-1.16 dump's 11k states have no mapping at all.

### The nine shape deltas, and which mechanism carries each

Measured from `minecraft-data`'s `protocol.json` with named types inlined and
**primitive aliases kept**, then cross-checked against the captures. The 1.9
era's warning applies here too: collapsing `varint`/`i64`/`u8`/`f32` to the
string `"native"` hides every retype.

| packet | changed at | delta | carried by |
|---|---|---|---|
| `login` (join) | 754 | numeric dimension + level type → world-name string + two NBT blobs | second struct |
| `login` (join) | 578 | seed hash inserted, respawn-screen flag appended | `#[mc(since = 578)]` fields |
| `respawn` | 754 | numeric dimension → NBT | second struct |
| `respawn` | 578 | seed hash inserted | `#[mc(since = 578)]` field |
| login `success` | 754 | UUID string → 128 bits | second struct (the shared one widens to `47..=578`) |
| `chat` | 754 | trailing sender UUID added | `#[mc(since = 754)]` field |
| `use_entity` (×3 forms) | 754 | trailing sneaking flag added | `#[mc(since = 754)]` field |
| `abilities` (serverbound) | 754 | two trailing `f32` speeds removed | `#[mc(until = 578)]` fields |
| `update_light` | 754 | leading `trustEdges` bool added | branch on the protocol |
| `crafting_book_data` → `recipe_book` | 754 | one packet with an action selector split into two | second struct + a `RecipeBookShape` on the id table |
| `map_chunk` biomes | 578, 754 | see below | branch on the protocol |
| section long packing | 754 | straddling → padded | branch on the protocol |

The split is not stylistic. A **field appearing or disappearing** is exactly
what the derive's `since`/`until` predicates express. A **retype** cannot be an
attribute: reading sixteen raw bytes where a length-prefixed 36-character
string was sent does not fail, it eats the username too.

### Chunk and light framing, the era's real risk

Three differences live in `packets/chunk.rs`, and each desynchronises rather
than errors when taken from the wrong protocol:

* **Where the biomes are.** At 498 a full column's biomes are a 2-D 16×16
  array of big-endian `i32`s **inside** `chunkData`, after the last section —
  so the container fabricates a vertical dimension for them, the same seam
  v1-8 and v1-9 document. At 578 they left the buffer and became a bare
  1,024-entry (4×4×4 over the column) `i32` array *before* it, with no count.
  At 754 that array gained a VarInt length prefix and VarInt elements.
* **How section indices are packed.** 498 and 578 use the pre-1.16
  *straddling* layout where a value may cross a 64-bit boundary, so
  `PalettedContainer::decode` cannot serve them; 754 pads each long. The
  declared long count is checked against the straddling geometry rather than
  trusted, which is what makes a 754 column fed to the older decoder fail.
* **`update_light`'s leading `trustEdges` flag**, added at 754. One byte,
  before four VarInt masks — and a mask is what decides how many 2,048-byte
  arrays follow.

**`minecraft-data` is wrong about the first of those**, which is the single
most expensive fact in this document: its 1.14.4 `protocol.json` models
`map_chunk` with no biome field anywhere. A decoder that believes it leaves
exactly 1,024 bytes of the buffer unread, which no round-trip test can see,
because both halves agree about a field neither knows exists.

### Captures

`tests/captures/join_{1_14_4,1_15_2}.txt` are clientbound bytes from real
servers, and `tests/capture_join.rs` holds both the `#[ignore]`d recorder that
made them and the hermetic replay that consumes them. See
[`the captures' own README`](../crates/versions/1.14/tests/captures/README.md)
for the format and the caps.

Their strongest assertion is the flat preset's own floor: every decoded column
must be uniformly canonical bedrock at `y = 0` and uniformly canonical grass at
`y = 3`, with the expected ids resolved out of `lodestone_data::block_states`
rather than from this crate. That one check covers the biome placement, the
long packing and the block-state table together — all three going wrong
produce a populated but wrong world, not an error.

The negative control has a different shape from the 1.9 era's, and the
difference is worth recording. There, a misrouted packet decoded into a
plausible wrong event. Here, measured across all 28 captured packets, **no**
misroute does: the adapter's exact-decode discipline turns every one into a
trailing-bytes or truncation error, or lands it on an ignored id.
`update_health` is id 72 at 498 and 73 at 754, where 72 is `experience`; the
754 adapter rejects 498's bytes with three trailing.
`misrouting_between_protocols_is_never_a_plausible_wrong_event` holds that
line for the whole capture, so a future lenient decode cannot quietly undo it.

### External-client acceptance

The opt-in release-client gate covers this era's hosted protocol 754 as row **754**. Run it with
`just external-client-acceptance --protocol 754 --output /private/tmp/lodestone-v754` and an
external driver. The six-stage evidence records the direct login-to-Play transition in
`configuration.mode: "login_to_play"` and the unbatched initial columns in
`chunk_batch_acknowledgement.mode: "unbatched", batch_count: 0`; it then requires world join,
deliberate movement, one observed `start_destroy_block` result, and a client-initiated clean
disconnect. Provenance must identify the exact 1.16.5 client build and retain non-empty capture and
client-log artifacts. This gate was not launched while this documentation was updated, so protocol
754 remains unverified by a real external client until that manual run produces `report.json`;
protocols 498 and 578 remain outside the external-client gate.

## How to change it

- **Adding a fourth protocol to this era** (there is none — 1.13.2 is below it
  and carries light inside the chunk packet, 1.17 changes world height):
  generate its tables with `cargo run -p xtask -- gen-packet-ids --source
  minecraft-data`, run the jar's data generator for its `blocks.json` and
  `registries.json`, add a `PROTOCOL_*` const, a `PROTOCOLS` entry, an `IDS_*`
  static, an `ids_for` arm, a `play_dispatch_table` slot, a `table_for` arm in
  each of `canonical` and `entity_types`, a `Source`/`JarSource` row per
  generator, and a `MEMBERS` row with its recorder and replay test. Then record
  a capture and let the replay tell you which shapes moved. Measured here at
  **69 hand-written lines** for the second of the two versions added.
- **Never widen a `#[mc(protocols)]` range without a capture from the protocol
  it now claims.** That is the plan's one guard against inheritance-by-range,
  and the reason `lodestone-protocol-common`'s `LoginSuccess` moved from
  `47..=340` to `47..=578` in the same change as the captures.
- **The adapter type is still called `V735Adapter`** even though it serves
  three protocols and 735 is not one of them. Renaming it touches its own
  tests and `lodestone-fuzz`; worth doing, but not inside a change that also
  moves the wire.
- `minecraft-data` ships 1.14.4 and 1.15.2 under their own directories (unlike
  the 1.9 era's same-major fallbacks), so pass the real version and protocol.

## Configuration

The era is selected by the existing `v1-14` feature on `lodestone-registry`.
Joining and hosting both resolve all three protocols; the host constructor
selects 498, 578 or 754 before any packet is encoded.
Oracle ports live in
`scripts/live-oracles/legacy.sh` and are read from there by
`tests/capture_join.rs`'s `MEMBERS` table.

## Dependencies

`lodestone-core` (`Ctx`, `ProtocolRange`, `dispatch::{Table, Handler,
IGNORED}`), `lodestone-macros` (`since`/`until`/`protocols`),
`lodestone-protocol-common` (the shared packet definitions, one of whose ranges
this work widened), `lodestone-world`, `lodestone-data` (the canonical 26.2
block-state registry the generated tables target), and `lodestone-server` for
the hosting seam. Recording needs Apple
`container` and [`scripts/live-oracles/legacy.sh`](../scripts/live-oracles/legacy.sh);
regenerating the block-state and entity tables additionally needs each jar's
own data generator under `container`; replay needs nothing.
