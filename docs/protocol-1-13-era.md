# The 1.13 era crate: one family, one protocol, two breaks

## What it is

`crates/versions/1.13` (package `lodestone-v1-13`) serves Minecraft 1.13.2 —
protocol **404** — from one adapter, one generated packet-id table, one
generated block-state table, two generated entity tables and one hosted
protocol implementation. It is the third era crate, after [`the 1.9 era`](./protocol-1-9-era.md) and
[`the 1.14 era`](./protocol-1-14-era.md), and the first with exactly one
member.

That is deliberate, and it is the point. 1.13 is the Flattening: numeric
`(block id, metadata)` pairs give way to flat block-state ids, item ids stop
carrying damage, and the whole id space is renumbered. Measured against
`minecraft-data` with named types inlined and primitive aliases kept, 1.13.2
agrees with 1.12.2 on 104 of 125 shared packet shapes and with 1.14.4 on 114
of 142 — 72% and 73% once each side's additions are counted, both below the
~88% that makes a run of releases one era. It neighbours a discontinuity on
each side, and the two committed neighbour captures make that falsifiable
rather than merely asserted.

The protocol number is read off the server itself: a 1.13.2 server started by
`scripts/live-oracles/legacy.sh 1.13.2` answers a server-list ping with
`{"name":"1.13.2","protocol":404}`.

## How it works

### Protocol selection

`PROTOCOLS` is `[404]`. `adapter_for(protocol)` constructs a `V404Adapter`
that resolves, once at construction, the `&'static PacketIds`, the
`CanonicalTable` for block states, the `EntityTypeTable` for entity ids, and a
`ChunkShape`. The indirection is kept rather than inlined even with one
member: it is what makes adding a second protocol a table plus an `ids_for`
arm instead of an edit to every send site, and `ids_for`/`table_for` panic for
anything outside `PROTOCOLS` rather than answering with a neighbour's
numbering.

Dispatch is one `lodestone_core::dispatch::Table`, built from the protocol's
own 86-entry clientbound `ENTRIES`, a `CLIENTBOUND` handler list and an
`IGNORED` list. Construction fails, by name, if any id is neither handled nor
ignored. Two `IGNORED` entries name no later packet at all and say so:
`minecraft:bed` (removed in 1.14, when sleeping became an entity-metadata
pose) and `minecraft:entity` (an abstract base packet no real server sends).

### What breaks at each side

| difference | 1.12.2 | **1.13.2** | 1.14.4 |
|---|---|---|---|
| chunk palette entry | `(id << 4) \| meta` | flat state id | flat state id |
| light | inside `map_chunk` | **inside `map_chunk`** | `update_light` |
| chunk heightmaps | absent | **absent** | inline NBT |
| section block count | absent | **absent** | leading `i16` |
| column biomes | 256 bytes, buffer tail | **256 big-endian `i32`, buffer tail** | 256 `i32` (1.15: 1024, before the buffer) |
| section long packing | straddling | **straddling** | straddling (1.16: padded) |
| packed `position` | `x,y,z` | **`x,y,z`** | `x,z,y` |
| slot | `i16` id + damage | **`present` bool + flat VarInt id** | same as 1.13 |
| `spawn_entity` type | object id space, `i8` | **object id space, `i8`** | unified registry, VarInt |
| `spawn_entity_living` type | mob id space | **unified registry** | unified registry |
| entity-metadata types | 0..12 | **0..15** | 0..18 |
| join/respawn difficulty byte | present | **present** | removed |
| recipe books | 1 | **2** | 4 |

1.13.2 is the only protocol in this repo that is **post-Flattening and still
carries light inside the chunk packet**, which is why neither neighbouring
era's chunk decoder can serve it.

### Hosted position confirmation and movement

Protocol 404's clientbound position carries a teleport id, and the host now
decodes `teleport_confirm` into `ServerBound::TeleportationAccepted`. That
activates the shared pending-id gate: movement stays blocked until the matching
reply arrives. `position`, `position_look`, `look`, and `flying` are decoded
separately so a pose, rotation-only change, or grounded-only change reaches its
own shared-server consumer. The in-memory control joins through the real 404
adapter, confirms its initial placement, then crosses into chunk `(1, 0)`;
arrival of that column proves the acknowledgement and movement chain reached
view streaming. A real 1.13.2 client session remains the external wire
compatibility gate.

The same host also lifts the non-breaking `block_dig` statuses rather than
only its mining phases: drop-one, drop-stack, release-use, and off-hand swap
become the server's inventory/drop, use-release, and hand-swap inputs. The
404 adapter already emits those wire bodies, so this decode is the link that
makes those input paths reach their existing consumers instead of disappearing
after encoding. Statuses outside the model remain ignored.

The packed `position` row is the widest single difference from the era above:
fifteen of the twenty-eight packets whose shape changes between 1.13.2 and
1.14.4 change *only* because they carry a position. That is why
`lodestone-protocol-common`'s pre-1.14 `Position` and the two packets that
embed it widened from `47..=340` to `47..=404` rather than this crate keeping
its own copy.

### Two entity id spaces, which is not what the Flattening looks like

The most expensive finding here, and the one no dataset in the tree states.
1.13 unified the entity **registry** — one alphabetical table of 95 entries
where 1.12 kept a mob table and an object table. It did **not** unify the two
*wire* id spaces:

* `spawn_entity_living` carries a VarInt into the unified registry;
* `spawn_entity` carries a **signed byte into the pre-1.13 object id space**,
  unchanged from 1.12.

Measured against a real 1.13.2 server: it spawns `armor_stand` through
`spawn_entity` with type id **78**, and id 78 in the unified registry is
`vex`; it spawns `boat` with id **1**, where the unified registry has
`armor_stand`. An adapter that resolved an object spawn through the unified
table would name a real, wrong entity for every object on the wire, with
nothing red anywhere. 1.14 is where that field widened to a VarInt and started
indexing the unified registry.

Every minecart variant shares object id **10**; the variant travels in
`spawn_entity`'s own `object_data` field, which a type id alone cannot
recover, so the generated table names the family and stops there.

The object table is generated from the wire transcript alone
(`tests/captures/entity_types_1_13_2.txt`) because no dataset covers it
usably. Its coverage is partial — 23 ids, only entities that can be summoned
and that spawn through `spawn_entity` — and an uncovered id resolves to
`None`, which the adapter reports rather than guessing at.

### Where the data comes from, and what it got wrong

| table | source | authority |
|---|---|---|
| packet ids (86 clientbound, 43 serverbound) | `minecraft-data` 1.13.2 | cross-check, checked by the join capture |
| block states (8,599) | the jar's own `--reports` `blocks.json` | authority |
| unified entity registry (95) | `minecraft-data` 1.13.2 | cross-check, checked id-by-id on the wire |
| object id space (23) | the wire transcript | authority |
| entity name spellings (144) | the jar's own language file | authority |

1.13.2's data generator emits block, item and command reports and **no
registry dump** — that provider arrived in 1.14 — which is why the entity side
needs an oracle the 1.14 era's does not.

`minecraft-data`'s 1.13.2 entity list is wrong in four separate ways, all
caught here rather than shipped:

* it holds 123 rows for a 95-entry registry, 28 of them stale pre-1.13
  *object* rows carried forward from 1.12's second id space, so a naive read
  gives a table where id 1 is both `armor_stand` and `boat`;
* three names are not identifiers at all — `iron_golem}`, `fireworks_rocket`
  and `commandblock_minecart` — each rejected outright by vanilla's own
  `/summon` ("Unknown entity: minecraft:fireworks_rocket");
* several object rows carry names no version uses (`area_effect cloud`,
  `eye_of ender`, `armorstand`), which is why the object table is not built
  from them at all.

The generator now fails on any name the jar's own language file does not ship.

### Captures, and the two era-boundary controls

`tests/captures/join_1_13_2.txt` is a real 1.13.2 join recorded through this
crate's own adapter. `join_1_12_2.txt` and `join_1_14_4.txt` are real joins
against the two **neighbouring** protocols, recorded with a hand-written
handshake and no adapter at all — a version crate may not depend on a sibling,
and does not need to: a handshake is four fields in all three protocols and
login `set_compression`/`success` are ids 3 and 2 in all three. See
[`the captures' own README`](../crates/versions/1.13/tests/captures/README.md).

The replay pins values the server chose. Its strongest assertion is the flat
preset's own floor — every decoded column uniformly canonical bedrock at
`y = 0`, dirt at `y = 1`, grass at `y = 3`, with the expected ids resolved out
of `lodestone_data::block_states` rather than from this crate — plus sky light
`0` inside that floor and `15` above it. Together those cover the inline light
arrays, the 256-int biome tail, the straddling long packing and the
block-state table at once; every one of them going wrong produces a populated
but wrong world, not an error.

### Hosted path

`V404ServerProtocol` moves directly from login success to Play and uses login
compression. It sends the era's fixed-shape join and position packets, then a
full y=0 through y=255 column. Each present chunk section uses the committed
straddling palette layout followed by block and sky light; the full-column
biome tail contains 256 big-endian integers.

Outbound state ids are the unique reverse of this family's committed generated
table, not a generic legacy conversion. A canonical state with no table entry,
or more than one wire entry, is rejected rather than replaced. The byte-level
control uses the jar report's dandelion state id (1111), while the in-memory
client/server control reaches Play, observes that state in a chunk, breaks it,
and observes the air update. A live protocol-404 client session against this
host remains the external compatibility check.

The hosted protocol also supplies the server's connection watchdog: it sends a clientbound
keep-alive and lifts only an exact serverbound echo to `ServerBound::KeepAlive`. Protocol 404 uses
a fixed signed `i64` id in both directions. Its server-protocol control pins an eight-byte body and
proves a trailing byte is not accepted as an acknowledgement; it is not an external-client
validation.

The hosted decoder also turns serverbound `settings` into
`ServerBound::ClientInformationChanged`, allowing its signed-byte requested view distance to resize
the server's chunk window. The remaining locale, VarInt chat mode, colour flag, skin-parts byte,
and VarInt main hand are decoded to preserve the protocol-404 boundary but have no host-side
consumer. A literal-body control fixes this six-field layout and rejects a trailing byte.

### The negative control, and what it actually measured

The 1.14 era found that **no** misroute between its protocols produces a
plausible wrong event: every one errors or lands on an ignored id. **That is
not true here.** Measured across the two neighbour captures, a misroute emits
a real, well-formed, wrong gameplay event **7 times out of 25** across the
lower boundary and **7 times out of 50** across the upper one:

* a 1.12.2 `abilities` body read as 404's `open_sign_entity` becomes a
  `SignEditorOpened` at block `(62771, 819, 20827340)`;
* a 1.12.2 `update_health` read as `entity_velocity` becomes a velocity for
  entity 65;
* a 1.14.4 **`map_chunk`** read as 404's `keep_alive` becomes
  `KeepAlive { id: 4294967298 }`, because the keep-alive arm reads eight bytes
  off the front of a 30-kilobyte column and stops.

The reason is structural rather than a defect in any one arm: most of the
packets this crate translates are short, fixed-width and unvalidated beyond
their length, so a body of the right size decodes into whichever struct the id
selects. The 1.14 era's stronger property came from its packets happening to
differ in length at the ids that collide.

So the guarantee this crate offers is the **whole-stream** one — a
neighbour's join never comes out as a clean join, which the two boundary tests
assert directly — and not a per-packet one. The measured split is asserted, so
a change on either side surfaces as a mismatch to re-derive rather than as a
silently weaker control.

## How to change it

- **Adding a second protocol to this era** (there is none; 1.13.0/1.13.1 speak
  393/401 and are not fetched): generate its id table with `cargo run -p xtask
  -- gen-packet-ids --source minecraft-data`, run the jar's data generator for
  its `blocks.json`, add a `PROTOCOL_*` const, a `PROTOCOLS` entry, an `IDS_*`
  static, an `ids_for` arm, a `play_dispatch_table` slot, a `table_for` arm in
  each of `canonical` and `entity_types`, and an oracle row in
  `scripts/live-oracles/legacy.sh`. Then record a capture and let the replay
  tell you which shapes moved.
- **Never widen a `#[mc(protocols)]` range without evidence from the protocol
  it now claims.** Four ranges widened to `47..=404` here — `ClientboundChat`,
  `SpawnPosition`, `BlockDig` and `PlayerAbilities` — each for a packet
  `minecraft-data` reports unchanged between 1.12.2 and 1.13.2, and the first
  two additionally decode out of the committed capture.
- **The adapter type is called `V404Adapter`**, which for once is the protocol
  it speaks. The folder is still named for the Minecraft version.
- Regenerating the entity oracle needs a live server and takes about ninety
  seconds; it is `#[ignore]`d. Two hazards are recorded in the test's own
  docs: driving `kill @e` over RCON while a 1.13.2 server ticks entities
  crashes it outright, and a recorder that merely drains the socket is
  disconnected partway through for not answering keep-alives — silently, since
  a closed socket simply stops producing spawn packets.
- **Hosting** lives in `src/server_protocol.rs`. Its outbound state lookup must
  remain the reverse of `generated_canonical::STATE_TO_CANONICAL`; preserve the
  ambiguous-or-absent rejection. Add both a byte-level control in
  `tests/server_protocol.rs` and a visible client/server assertion in
  `tests/server_integration.rs` when extending the hosted packet path.

## Configuration

None new. The era is selected by a `v1-13` feature on `lodestone-registry`;
the registry selects its adapter and its protocol-404 host. Oracle ports live in
`scripts/live-oracles/legacy.sh` (game `25590`, RCON `25591`) and are read
from there by `tests/capture_join.rs` and `tests/entity_types.rs`. That row
gives 1.13.2 a flat, peaceful, spawn-free world: `level-type=FLAT` is still
the right spelling despite 1.13's namespacing sweep — measured, by booting
with each and reading the resulting `level.dat` generator name back — and the
no-natural-spawn properties are what the entity oracle needs to correlate one
summon with one spawn packet.

## Dependencies

`lodestone-core` (`Ctx`, `ProtocolRange`, `dispatch::{Table, Handler,
IGNORED}`), `lodestone-macros` (`since`/`until`/`protocols`),
`lodestone-protocol-common` (the shared packet definitions, four of whose
ranges this era widened), `lodestone-world`, `lodestone-data` (the canonical
26.2 block-state registry the generated table targets), and `lodestone-server`
(the version-free host seam). The in-memory hosted control also uses
`lodestone-client`. Recording needs Apple
`container` and [`scripts/live-oracles/legacy.sh`](../scripts/live-oracles/legacy.sh);
regenerating the block-state table additionally needs the jar's own data
generator under `container`, and the two bridging rules in it need a real 26.2
server to upgrade a 1.13.2 world; replay needs nothing.
