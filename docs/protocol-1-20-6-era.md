# The 1.20.6 era crate: a join with a configuration phase, and items made of components

## What it is

`crates/versions/1.20.6` (package `lodestone-v1-20-6`) serves Minecraft 1.20.5
and 1.20.6 — both protocol **766** — from a single adapter, one generated
packet-id table, one generated block-state table, one generated entity
registry, and the era's own configuration-phase, item-component and chunk-shape
code. It is the sixth era crate, after [`1.9`](./protocol-1-9-era.md),
[`1.14`](./protocol-1-14-era.md), [`1.13`](./protocol-1-13-era.md),
[`1.17`](./protocol-1-17-era.md) and [`1.19`](./protocol-1-19-era.md).

The protocol number comes from two independent sources that agree: the jar's own
`version.json` in `.cache/mc/1.20.6/server.jar` reports
`"protocol_version": 766`, and `minecraft-data`'s `protocolVersions.json` lists
766 for both 1.20.5 and 1.20.6. One wire version covering two Minecraft releases
is why this crate's `minecraft_versions()` returns two strings for one number.

Two breaks land inside this era and both reshape the join:

* **A connection now has a configuration phase.** Login no longer ends in Play.
  The client acknowledges login, the server sends its registries, feature flags
  and tags in a state of its own, and Play begins only after both sides exchange
  a finish-configuration packet. Every era below goes straight from login
  success into Play, with the registries carried inline in the join packet.
* **An item stack is a component map.** A stack on the wire is a count, an item
  id and two component lists — the ones to add and the ones to remove — where
  every era below carries an id, a count, a damage/metadata short and an
  optional NBT compound.

## How it works

### A singleton crate inside a wider era, measured

`PROTOCOLS` lists one number. Unlike every era crate before it, that is **not**
because the measurement says the era is one protocol wide.

Re-derived from `minecraft-data` with named types inlined recursively and
**primitive aliases kept** (collapsing `varint`/`i64`/`u8`/`f32` to a single
token hides every retype, and this era retypes several — a metadata amplifier
went from a signed byte to a varint with no other change):

| boundary | identical shapes | identity |
|---|---|---|
| 1.19.4 → 1.20.6 | 119 of 220 | **54%** |
| 1.20.4 → 1.20.6 | 177 of 220 | **80%** |
| 1.20.6 → 1.21 | 204 of 226 | **90%** |

The grouping threshold for one crate to serve two protocols is 85% agreement.
So the era's **lower** boundary is real — both readings below it are under the
threshold — and its **upper** boundary is not: protocol 767 (Minecraft 1.21 and
1.21.1) is inside the same wire era by that measure and is the natural second
member of this crate. `PROTOCOLS` lists what is implemented and checked against
real bytes, which is 766 alone; the measurement is recorded here and in
`adapter::PROTOCOLS`'s own doc so the gap is a stated decision rather than an
unexamined default. The next protocol past 767 is 1.21.11's 774, which agrees
with its own predecessor on 66% and is a separate era.

`adapter_for` already selects the id table from the negotiated protocol rather
than naming a generated module, so adding 767 is a table entry and an arm
rather than a restructure.

### The configuration phase, and what depends on it

`handle_configuration` is the whole of the phase. Four packets matter:

| packet | what the adapter does |
|---|---|
| `registry_data` | records `minecraft:dimension_type`'s entries, in order |
| `select_known_packs` | answers with an **empty** list |
| `keep_alive` / `ping` | echoes, because the phase can last arbitrarily long |
| `finish_configuration` | acknowledges, then `SetState(Play)` |

The empty known-packs reply is load-bearing, not politeness. That packet offers
to *elide* registry payloads for any data pack the client claims to already
have. Claiming none is what makes the dimension registry arrive with its `min_y`
and `height` values inside it — and those two values are the only way to frame a
column. Claiming the vanilla core pack saves a few kilobytes and leaves every
column unframeable, with nothing logged anywhere.

Everything else the phase carries (tags, feature flags, resource-pack pushes,
cookies) is passed over. Unlike the play state, this phase has no dispatch table
with an enumerated ignore list, because the packets that matter are the four
above and `finish_configuration` is matched explicitly rather than by
fallthrough.

The phase is also not a login-time detour. `start_configuration` can pull a
*playing* connection back into it at any time — a resource-pack change, a
datapack reload — so the play dispatch table handles it, replies with
`configuration_acknowledged` and returns `SetState(Configuration)`. A client
that treats configuration as something it left behind reads the next
`registry_data` as a play packet.

### Where the vertical window comes from

The join packet does **not** name its dimension. It carries a
`SpawnInfo` whose first field is a varint **index into the dimension-type
registry** the configuration phase delivered. The eras below either carry the
resolved dimension entry inline (through 1.18) or name it with a string (1.19),
so this is a third mechanism, not a variation on either:

1. configuration `registry_data` for `minecraft:dimension_type` →
   `DimensionRegistry::adopt`, keeping only each entry's `id`, `min_y` and
   `height`;
2. join or respawn → `ChunkShape::from_dimension_index`, which returns `None`
   rather than guessing when the registry has no such index, when the entry
   arrived with no payload, or when `height` is not a positive multiple of 16.

Guessing a height is the one thing that must not happen: a section count is a
byte count, so a wrong one consumes the wrong number of bytes and produces a
populated but wrong column instead of an error. `ChunkShape::overworld` is the
pre-join fallback (`min_y` -64, 24 sections, which is what this era's own jar
declares for `minecraft:overworld`), and it exists only so a column arriving
before the registry does not panic.

`respawn` carries the same `SpawnInfo`, so a respawn into a dimension of a
different height re-resolves the shape rather than inheriting a stale one.

### Item components, and why an unknown one is a hard error

`packets::slot::Slot` models the era's stack: a varint count, and when non-zero,
a varint item id, a count of components to add, a count of components to remove,
then those components. A component's payload has **no length prefix** — its
width is implied by its type id — so a component this crate does not model
cannot be skipped. `read_component_payload` therefore errors by name
(`Error::InvalidEnumVariant`) rather than guessing a width, because the
alternative is silently desynchronising every byte after it.

The 56-entry component id table comes from the jar's own registry report,
cross-checked against `minecraft-data`'s identical id-to-name mapping.

### The metadata serializer table

31 entries, renumbered at this era: the armadillo-state and wolf-variant
serializers were inserted, moving everything after them. A wrong number does not
fail loudly — it reads the next field's bytes as some other type and either
succeeds with nonsense or reports a corrupted stream several fields later. Three
serializers (particle, particle list, and optional global position) are refused
**by name** rather than approximated, for the same reason the component table
refuses an unknown id.

`handle_play_entity_metadata` reports only index `0`, the shared entity flags
byte. Every other index at this protocol is claimed by more than one entity
category with the same serializer, and the adapter has no id-to-category map to
tell them apart; surfacing one anyway would put an arrow's crit bit where a
player's using-item bit belongs. The whole entry list is still decoded, so an
unmodelled serializer fails rather than desynchronising.

### Chunk-batch pacing

`chunk_batch_finished` must be answered with `chunk_batch_received` carrying a
columns-per-tick rate. A server that receives no reply throttles chunk delivery
to its floor, so a client that ignores the packet loads the world at a trickle
with nothing logged anywhere. The rate this client asks for is a request, not a
measurement.

### Two disconnect shapes at one protocol

The login-state disconnect carries a **JSON string**; the configuration- and
play-state ones carry a component in **anonymous NBT**. The adapter keeps two
functions (`json_reason_text`, `nbt_reason_text`) rather than one that sniffs
the payload: the connection state decides the form, and a sniff would silently
accept the wrong one.

### Evidence: what is checked against what

The jar for this version ships **no machine-readable packet report** — its data
generator emits block, item, command and registry reports and no packet report —
so the packet ids come from `minecraft-data`, which is a cross-check-grade
source rather than an authority. The authority is a recorded join:
`tests/capture_join.rs` drives a real server through this crate's own adapter and
commits every packet it received to `tests/captures/join_1_20_6.txt`.

| claim | what checks it |
|---|---|
| the join choreography works | the recorder asserts it reached Configuration *and* Play |
| the dimension registry arrives with payloads | replay decodes it and asserts every entry has one |
| the column is framed right | the decoded column parses to the packet's last byte |
| the block-state bridge works | the flat floor reads back as canonical 26.2 bedrock/dirt/grass |
| `unload_chunk` is (z, x) | see below |
| chat round-trips | the recorder sends one and the server broadcasts it back |
| the metadata table is right | every recorded body decodes to its `0xff` terminator |
| every clientbound id is accounted for | `dispatch::Table::build`, with a negative control |

`unload_chunk` deserves its own note. Its two coordinates are plain
big-endian ints with **z first**, and a square view distance makes a swapped
pair invisible: every column a stationary player's server drops has `|x|` and
`|z|` in the same range. The recorder therefore RCON-teleports the joined player
1000 blocks along **+x only** and back, so the far columns it then drops have a
large chunk x and a near-zero chunk z. `unload_chunk_reads_z_before_x` rejects
any body that puts the x displacement in `chunk_z`.

The block-state and entity-type tables are generated from the jar's own reports
and pinned by an FNV-1a content hash on the committed dump, with a `#[ignore]`d
drift guard that regenerates under `LODESTONE_REGEN=1`.

`cargo xtask connectedness` reports this family at **61/122 clientbound
decoded, 60/122 emitting, 0 decoded-but-stranded, 31/58 serverbound encoded**.
The 61 that decode nothing are enumerated in `adapter::IGNORED` with a reason
each, so the dispatch table refuses to build if a packet is dropped by
omission. The commonest reason is a missing 766 registry table — item ids, sound
ids and attribute ids all name registry entries this crate cannot yet resolve
into canonical keys, which is what keeps `window_items`, `set_slot`,
`open_window`, `entity_equipment` and the sound packets out.

## How to change it

* **Adding 767 to this era.** Generate its id table
  (`cargo run -p xtask -- gen-packet-ids --version 1.21 --protocol 767 --source
  minecraft-data`), add a `packet_ids_from!` static and an `ids_for` arm, extend
  `PROTOCOLS`, widen the `#[mc(protocols = ...)]` range on each packet whose
  shape the adjacency table says is unchanged, and record a second capture. The
  22 shapes the measurement says differ are the work; the rest is a table.
* **Wiring one of the 61 ignored packets.** Move its `IGNORED` entry to
  `CLIENTBOUND` and write the handler. Spell the row as a literal
  `Handler::new(` with the packet name beside it: `cargo xtask connectedness`
  anchors on exactly that text, and a helper function that builds the row leaves
  the instrument reporting zero arms examined while the table is correct.
* **Adding a component type.** Extend `read_component_payload`'s table in
  `packets/slot.rs`. Never add a default arm: the payload widths are
  type-implied, so a wrong guess desynchronises the stream.
* **Anything text-shaped.** Use `packets::common::NetworkNbt`, not the derive's
  `#[mc(nbt)]`. That attribute reads the *named* NBT form; every text component
  and registry payload at this protocol is the anonymous form, a tag byte
  followed immediately by its payload.
* **Re-recording the capture.** Start the oracle
  (`./scripts/live-oracles/legacy.sh 1.20.6`), then
  `cargo test -p lodestone-v1-20-6 --test capture_join -- --ignored --nocapture
  record_1_20_6`. The recorder needs RCON as well as the game port; both come up
  with the container.
* **Do not derive the protocol from the folder name.** The folder is `1.20.6`,
  the package suffix `v1-20-6`, the feature `v1-20-6`, and the protocol 766. Ask
  `VersionAdapter::supports` or `PROTOCOLS`.

## Configuration

| knob | where | effect |
|---|---|---|
| `v1-20-6` feature | `lodestone-registry` | compiles this family in and registers its adapter. Off by default, like every family |
| `LODESTONE_REGEN=1` | environment, with `--ignored` | rewrites the committed generated tables from their dumps instead of asserting against them |
| oracle ports 25598 / 25599 | `scripts/live-oracles/legacy.sh` | game and RCON for the 1.20.6 container (`lodestone-mc1206`) |

The crate itself reads no environment variable and has no runtime
configuration: the negotiated protocol is the only per-connection input, and it
is resolved once at construction.

Hosting is not configurable because it is not implemented: only `v26-2`
implements `ServerProtocol`, so this family can join a 766 server and cannot be
one.

## Dependencies

Only the six crates every version family may depend on: `lodestone-core`
(codecs, NBT, dispatch table), `lodestone-model` (the version-free
`VersionAdapter`, `ClientEvent` and `Directive` vocabulary), `lodestone-macros`
(the `Encode`/`Decode`/`Packet` derives), `lodestone-protocol-common` (one
shared packet, the brand payload), `lodestone-data` (the canonical 26.2
block-state, block-entity and mob-effect registries) and `lodestone-world`
(paletted column storage and light). Tests additionally use `lodestone-net`,
`lodestone-testsupport` (RCON), `tokio` and `serde_json`.

Nothing else depends on this crate except `lodestone-registry`, through one
optional feature-gated edge, so the whole era can be removed by deleting the
folder and three manifest lines — `cargo xtask check-deletable 1.20.6` reports
which.

The 1.20.6 era shares **no** `lodestone-protocol-common` definitions beyond the
brand payload. Every other shared definition there is range-capped at 762 or
below, and none of those ranges was widened for this era: at 766 the resource-pack
reply is keyed by UUID, `settings` gained two fields and moved into the
configuration state, `abilities` lost its two speed hints, and the movement
packets are the only genuinely unchanged group — a group too small to be worth
the inheritance-by-range hazard.
