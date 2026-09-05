# The 1.17 era crate: one family, two protocols, a world that moved

## What it is

`crates/versions/1.17` (package `lodestone-v1-17`) serves Minecraft 1.17.1
and 1.18.2 — protocols **756** and **758** — from a single adapter, two
generated packet-id tables, one generated block-state table, one generated
entity registry, and nine explicitly-carried shape deltas, rather than two
copies of a family. It is the fourth era crate, after
[`1.9`](./protocol-1-9-era.md), [`1.14`](./protocol-1-14-era.md) and
[`1.13`](./protocol-1-13-era.md).

Both protocol numbers are read off each jar's own `version.json`, not from any
dataset: 1.17.1 reports `"protocol_version": 756` (755 is 1.17, which this repo
does not fetch and this crate does not serve) and 1.18.2 reports `758`.

This is the era where **the world stopped being sixteen sections tall**. Every
era below it can hardcode a 0..256 column; here the vertical range is data,
carried in the dimension entry the join packet holds, and 1.18 moves the
overworld floor to `y = -64`. A section count taken from the wrong place does
not raise anything — it consumes the wrong number of bytes and desynchronises
the stream — which is why almost everything below is about that one number.

## How it works

### Protocol selection

`PROTOCOLS` lists both. `adapter_for(protocol)` constructs a `V756Adapter`
that stores that protocol and, resolved once at construction, four things keyed
by it: a `&'static PacketIds` (the id of every packet the adapter names plus
that protocol's whole clientbound `ENTRIES` slice), the `CanonicalTable` for
block states, the `EntityTypeTable`, and a `ChunkShape`. `V756Adapter::ctx()`
builds the `Ctx { version }` every codec call reads, so a
`#[mc(since)]`/`#[mc(until)]` predicate and a `#[mc(protocols = "a..=b")]`
precondition both see the negotiated protocol rather than a constant.

Measured against the two generated tables, this era's id drift is
**clientbound only**: 15 of the 97 shared clientbound play ids differ — the
fifteen at or above the id 1.18 gave `simulation_distance` — while all 47
shared serverbound play ids and the whole handshaking, status and login
sections agree. Every id is still selected through the id table rather than
named directly, so a future member that moves a serverbound id cannot do so
silently.

Dispatch is one `lodestone_core::dispatch::Table` per protocol, cached in a
two-slot array of `OnceLock`s indexed the same way `ids_for` resolves a table.
`simulation_distance` is an `IGNORED::ranged` entry covering `758..=758`, so
756's table does not fail construction on a stale entry and 758's does not fail
on an unlisted id.

### Hosted protocols 756 and 758

`V756ServerProtocol` is the hosted half for 1.17.1. It uses the protocol-756
table for handshake, login, join, position, chunk, block-update and break
packets, then transitions directly from login success to Play because this
revision has no configuration acknowledgement. The chunk encoder projects the
canonical y=0 through y=255 window into 756's long-array section mask,
column-wide 3-D biome array and separate section body. A column must cover
that window, contain no block entities, and contain only plains biomes; every
other source shape is an explicit encoding error. Canonical block states are
looked up through the inverse of the committed jar-backed table; missing or
ambiguous states are rejected rather than substituted.

The registry exposes protocol 756 through
`server_protocol_for_protocol(756)` and protocol 758 through
`server_protocol_for_protocol(758)`. `V758ServerProtocol` is deliberately a
separate implementation: it sends `simulation_distance` in the join packet,
requires a source covering y=-64 through y=319, sends every one of 24 sections
with its own biome container, keeps the zero long-array count for
single-valued containers, and appends the inline light framing. The in-memory
controls join each registry-selected server, read a known chunk block and
observe its block update after a break. Real 1.17.1 and 1.18.2 client sessions
remain required validation.

### External-client acceptance

The opt-in release-client gate covers both hosted rows in this era: protocol 756 (1.17.1) and
protocol 758 (1.18.2). Run either with `just external-client-acceptance --protocol 756 --output
/private/tmp/lodestone-v756` or `just external-client-acceptance --protocol 758 --output
/private/tmp/lodestone-v758`, using an external driver. Both revisions transition directly from
login to Play and send unbatched initial chunks, so their six-stage evidence records
`configuration.mode: "login_to_play"` and `chunk_batch_acknowledgement.mode: "unbatched",
batch_count: 0`; 758's per-section biome and inline-light payload do not add a batch
acknowledgement. The remaining stages require world join, deliberate movement, one observed
`start_destroy_block` result, and a client-initiated clean disconnect. Provenance must identify the
exact 1.17.1 or 1.18.2 client build and retain non-empty capture and client-log artifacts. These
gates were not launched while this documentation was updated, so both protocols remain
unverified by a real external client until their manual runs produce `report.json`.

The same hosts decode all four ordinary Play movement bodies. Position and
position-with-look become `ServerBound::PlayerMoved`; look-only becomes
`PlayerRotated`; and the grounded-only form becomes `PlayerStatusOnly`. This
is not just player state bookkeeping: the integrated server's
`dispatch_play_packet`
uses a moved position to recenter `ViewTracker`, stream the newly visible
chunk strip, move the chunk ticket, and republish the tick anchor. The
in-memory tests cross from chunk `(0, 0)` into `(1, 0)` through the real
registry-selected adapter and server and wait for that newly streamed chunk;
the protocol tests also decode literal negative/fractional wire bodies so the
server-side lift cannot be justified by a symmetric encoder round trip.

They also decode the era's `block_place` body into `ServerBound::UseItemOn`:
hand, packed position, face, three cursor floats, then `inside_block`. Both
hosted revisions use that exact shape and predate the prediction sequence, so
the host supplies sequence `0`; a client-side sequence must not be mistaken for
wire data. Literal bodies exercise a negative position, an off-hand use and an
invalid-face rejection, while a separate adapter-to-registry-host test proves
the action reaches the integrated server's existing placement consumer. The
consumer resolves the held item from its server-side inventory, rather than
trusting a client-supplied stack.

### One block-state table and one entity table, which is not what the era below needs

The 1.14 era needs three of each, because every release inserts blocks and
entities into vanilla's global palettes and renumbers everything after them.
This era needs one of each, and that is a measurement rather than a
convenience:

| table | 1.17.1 | 1.18.2 | how it was checked |
|---|---|---|---|
| `blocks.json` (`--reports`) | 898 blocks, 20,342 states | identical | the two jar dumps are **byte-identical**, MD5 `644230a6388623ab774fac03cc154a9e` |
| `minecraft:entity_type` | 113 entries | identical | both dumps committed, compared entry by entry by `both_dumps_agree_id_for_id` |

1.18 was a world-generation release: it added no block and inserted no entity.
Both claims are re-derived in the test suite rather than asserted here — the
entity check parses **both** committed dumps and fails naming the first
disagreement, and the block-state check pins the committed dump's own content
hash so a different version's dump swapped in under the same filename fails
loudly instead of silently regenerating a wrong table. `canonical::table_for`
and `entity_types::table_for` still route through the negotiated protocol, so a
third member that did renumber could not inherit either table by accident.

Two other registries in the same dumps **do** differ across the era — 88
particle ids and 1,044 sound ids move — but this crate translates neither, so
neither is generated. That is worth stating because `minecraft-data` reports
the `entity_metadata` and `world_particles` packets as changing at both era
boundaries, and the whole of that difference is the particle renumbering inside
an embedded payload this crate does not model. The nineteen entity-metadata
serializer types are unchanged across 754, 756 and 758.

### The nine shape deltas, and which mechanism carries each

Measured from `minecraft-data`'s `protocol.json` with named types inlined and
**primitive aliases kept** — the 1.9 era's warning applies here too: collapsing
`varint`/`i64`/`u8`/`f32` to the string `"native"` hides every retype. The
adjacency figures those measurements give are 79% identity at 1.16.5 → 1.17.1
(130 of 165) and 94% at 1.17.1 → 1.18.2 (156 of 166), which is exactly what
makes these two versions one era and 1.19.4 the start of the next.

| packet | changed at | delta | carried by |
|---|---|---|---|
| `login` (join) | 758 | `simulation_distance` inserted after the view distance | `#[mc(since = 758)]` field |
| `map_chunk` | 758 | section mask and column biome array removed; per-section biome containers and the whole light payload added | second function |
| `settings` (serverbound) | 756, 758 | one trailing flag at 1.17, a second at 1.18 | moved out of the shared crate; `#[mc(since = 758)]` on the second |
| `entity_effect` | 758 | effect id `i8` → VarInt | second struct |
| `remove_entity_effect` | 758 | effect id `i8` → VarInt | second struct |
| `position` (clientbound) | 756 | trailing `dismount_vehicle` bool added | plain field (every protocol here has it) |
| `spawn_position` | 756 | trailing `f32` angle added | plain field |
| `tile_entity_data` | 758 | action `u8` → VarInt | not translated; `IGNORED` |
| `multi_block_change` | 758 | packed `y` becomes signed | not translated; `IGNORED` |

The split is not stylistic. A **field appearing or disappearing** is exactly
what the derive's `since`/`until` predicates express. A **retype** cannot be an
attribute: reading one byte where a VarInt was written does not fail for any id
above 127 — it keeps the continuation byte and then reads the next field out of
the id's low bits.

1.17 also **split three action-selected packets into fourteen**: the single
title packet became `set_title_text`, `set_title_subtitle`, `set_title_time`,
`action_bar` and `clear_titles`; the combat packet became `enter_`, `end_` and
`death_combat_event`; and the world-border packet became six. Their bodies are
unchanged, so this crate carries fourteen handlers emitting the same events the
era below emits from three, and nothing that era translated is lost.

Shared `#[mc(protocols)]` ranges widened to `758` only where the measured shape
is unchanged — `keep_alive` (both directions), the serverbound `chat` and
`arm_animation`, `attach_entity`, `set_passengers`, `collect`,
`rel_entity_move`, `entity_move_look`, `entity_teleport`, `teleport_confirm`
and `resource_pack_receive`. `remove_entity_effect` stops at `756` for the
retype above, and `settings` did not widen at all. Every one of those widenings
decodes or encodes out of a committed capture from a protocol it now claims,
which is the plan's one guard against inheritance-by-range.

### Chunk framing, the era's real risk

`packets/chunk.rs` is where all the danger is, and both protocols differ from
the era below **and from each other**:

* **The vertical window is data.** `ChunkShape::from_dimension_nbt` reads
  `min_y` and `height` out of the raw named-NBT dimension blob `login` and
  `respawn` both carry, and nothing else infers them. A blob that does not
  state a usable pair leaves the shape alone rather than guessing: a section
  count is a byte count, and a wrong one desynchronises instead of erroring.
  `ChunkShape::overworld` is only the pre-join default (1.17's `y = 0..256`,
  1.18's `y = -64..320`), replaced the moment a server states its own.
* **756** replaces the single-VarInt section mask with a `varint`-counted
  `i64[]` bitset — because a column can now have more than 32 sections — drops
  the full-column flag so every `map_chunk` is a whole column, carries the
  column's biomes as one `varint`-counted VarInt array before `chunkData`, and
  leaves light to `update_light`.
* **758** drops the mask and the biome array entirely: **every** section is
  present, each carrying its own biome `PalettedContainer` after its block
  container, block entities become positioned records rather than bare NBT, and
  the light payload rides along in the same packet.
* **`update_light`'s four masks** became `varint`-counted `i64[]` bitsets at
  1.17, for the same reason the section mask did, and its two array lists
  gained their own counts. `lodestone_world::ColumnLight::decode` already
  speaks exactly that shape, so both protocols share it.

**1.18's `chunkData` buffer is longer than the sections inside it.** This is
the single most expensive fact in this document, and no round-trip against our
own encoder could have found it. Measured across three columns from one flat
world:

| sections with a single-valued block palette | declared `chunkData` | consumed by the sections | left over |
|---|---|---|---|
| 23 | 2,268 | 2,245 | **23** |
| 21 | 6,369 | 6,348 | **21** |
| 19 | 10,471 | 10,452 | **19** |

The leftover is always exactly one zero byte per section whose *block* palette
is single-valued, and always at the very end: every section parses contiguously
at its predecessor's end with a valid header, and the light payload after the
buffer parses to the packet's last byte. The server is sizing the buffer from
an estimate that over-counts a zero-width container by a byte and sending the
whole allocation, padding included. So the decoder reads exactly
`section_count` sections and then requires what remains to be **all zero and no
longer than the section count** — a misparse still leaves either nonzero bytes
or too many of them, so only the exact-length half of the detector is given up,
and only where the wire forces it.

**The single-valued palette itself is new at 1.18.** A bits-per-entry of zero
means one VarInt value for the whole container, followed by a VarInt long count
of `0`. `lodestone_world::PalettedContainer::decode` returns as soon as it
reads a zero width and never consumes that trailing count, because the family
it was written for (1.21.5+) does not write one. `packets::chunk`'s
`decode_container` therefore peeks the width byte — `Reader` is `Copy` — and
handles the zero-width case itself.

### Captures

`tests/captures/join_{1_17_1,1_18_2}.txt` are clientbound bytes from real
servers, and `tests/capture_join.rs` holds both the `#[ignore]`d recorder that
made them and the hermetic replay that consumes them. See
[`the captures' own README`](../crates/versions/1.17/tests/captures/README.md)
for the format and the caps.

The replay pins values the servers chose: survival on `minecraft:overworld`,
the flat preset's own floor (uniform canonical bedrock at the column's floor,
dirt one above, grass three above, and *not* grass four above so the probe is
discriminating), and — the era's defining assertion —
`the_two_captures_declare_different_vertical_windows`, which reads `(0, 16)`
out of the 756 capture and `(-64, 24)` out of the 758 one. A single hardcoded
sixteen-section column satisfies the first and fails the second, which is what
makes it worth asserting.

### The negative control, and what it actually measured

Run, not predicted, and the answer is the weaker of the two the earlier eras
gave.

`update_time` is clientbound id **88** at 756 and 89 at 758; id 88 at 758 is
`set_title_subtitle`. The 756 adapter reads its own id as a time update; the
758 adapter reads the same bytes as a length-prefixed JSON string, takes the
world age's own leading byte as that length, and rejects what is left over.

Over the whole 1.17.1 capture, four ids name a different packet at 758. One
errors, two land on ignored ids, and **one produces a real, well-formed, wrong
gameplay event**: id 101 is `declare_recipes` at 756 and `entity_effect` at
758, so a recipe list read as an effect becomes `MobEffectApplied` for entity
1058 — levitation, amplifier 109, 105 ticks — with nothing red anywhere. That
matches the 1.13 era's finding and not the 1.14 era's, where no misroute
produced a plausible event.

So the guarantee this crate offers is the **whole-stream** one — neither
protocol's join replays cleanly through the other's adapter, which the two
boundary tests assert directly — and not a per-packet one. The measured split
(26 agreeing ids, 1 error, 2 silent, 1 plausible) is pinned, so a change on
either side surfaces as a mismatch to re-derive rather than as a silently
weaker control.

## How to change it

- **Adding a third protocol to this era** (there is none — 1.19.4 agrees with
  1.18.2 on only 77% of packet shapes, and carries chat signing): generate its
  id table with `cargo run -p xtask -- gen-packet-ids --source minecraft-data`,
  run the jar's data generator for its `blocks.json` and `registries.json`, and
  **check those two dumps against the committed ones before anything else** —
  if either differs, the shared block-state or entity table has to become two.
  Then add a `PROTOCOL_*` const, a `PROTOCOLS` entry, an `IDS_*` static, an
  `ids_for` arm, a `play_dispatch_table` slot, a `table_for` arm in each of
  `canonical` and `entity_types`, an oracle row in
  `scripts/live-oracles/legacy.sh`, and a `MEMBERS` row with its recorder and
  replay test. Then record a capture and let the replay tell you which shapes
  moved. Measured here at **131 hand-written lines** naming 758 or 1.18.2 for
  the second of the two versions, of which 32 are code and the rest is the
  documentation that explains them; the 758-only function and struct bodies
  come to 95 lines.
- **Never widen a `#[mc(protocols)]` range without evidence from the protocol
  it now claims.** The eleven widenings here are each for a packet
  `minecraft-data` reports unchanged from 754 through 758, and each is
  additionally exercised by a committed capture.
- **The adapter type is called `V756Adapter`**, which is the era's opening
  protocol. The folder is named for its opening Minecraft version.
- Regenerating either jar dump needs a **Java 17** container image, not the
  Java 8 one every pre-1.17 oracle uses: 1.17.1 and 1.18.2 declare
  `java_version` 16 and 17 in their own jars and refuse to start under 8. 1.17.1
  ships a flat obfuscated jar whose data-generator entry point is reachable on
  the classpath; 1.18.2 ships a bundler jar, so the generator is selected
  through the bundler's own main-class property.

## Configuration

None new. The era is selected by a `v1-17` feature on `lodestone-registry`; the
client registry reads `PROTOCOLS` from the crate, and the server registry
exposes both protocols through separate implementations. Oracle ports live in
[`scripts/live-oracles/legacy.sh`](../scripts/live-oracles/legacy.sh) (1.17.1
game `25592` / RCON `25593`, 1.18.2 game `25594` / RCON `25595`) and are read
from there by `tests/capture_join.rs`'s `MEMBERS` table. Both rows use
`level-type=FLAT`; matching is case-insensitive from 1.17 on, measured by
booting each server with both spellings, deleting the generated world between
runs, and reading the resulting `level.dat` back.

One thing this era does **not** settle: `minecraft-data` names the serverbound
settings packet's 1.17 flag as if it disables text filtering and its 1.18 twin
as if it enables it — the same wire byte, described with opposite senses one
release apart — and no dump in this tree says which is right. The framing does
not depend on it and no server rejects either value, so the field is carried
through from the model as an opaque flag and named for its subject rather than
its sense. Settling it needs a server-side oracle.

## Dependencies

`lodestone-core` (`Ctx`, `ProtocolRange`, `Nbt`, `dispatch::{Table, Handler,
IGNORED}`), `lodestone-macros` (`since`/`until`/`protocols`),
`lodestone-protocol-common` (the shared packet definitions, eleven of whose
ranges this era widened), `lodestone-world` (`PalettedContainer`,
`ColumnLight`, `LightPatch`), `lodestone-data` (the canonical 26.2 block-state
registry the generated table targets, and the mob-effect names the effect
packets resolve through). Recording needs Apple `container` and
[`scripts/live-oracles/legacy.sh`](../scripts/live-oracles/legacy.sh);
regenerating the block-state and entity tables additionally needs each jar's own
data generator under a Java 17 image; replay needs nothing.
