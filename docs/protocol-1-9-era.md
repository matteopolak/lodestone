# The 1.9 era crate: one family, four protocols

## What it is

`crates/versions/1.9` (package `lodestone-v1-9`) is the first *era* crate in this repo: one
family serving Minecraft 1.9.4, 1.10.2, 1.11.2 and 1.12.2 — protocols 110, 210, 316 and 340 —
from a single adapter, four generated packet-id tables, and nine explicitly-carried shape
deltas, rather than four copies of a family. It demonstrates the range and
per-protocol-table rules in
[`docs/plans/multi-version-protocol-dedup.md`](./plans/multi-version-protocol-dedup.md).

## How it works

### Protocol selection

`PROTOCOLS` lists all four. `adapter_for(protocol)` constructs a `V340Adapter` that stores that
protocol and, resolved once at construction, a `&'static PacketIds` — a struct holding the id of
every packet the adapter names plus that protocol's whole clientbound `ENTRIES` slice.
`V340Adapter::ctx()` builds the `Ctx { version }` every codec call reads, so a
`#[mc(since)]`/`#[mc(until)]` predicate and a `#[mc(protocols = "a..=b")]` precondition both see
the negotiated protocol rather than a constant.

The indirection is the point. Each generated table is its own module, so a
`play::serverbound::BLOCK_DIG` path can only ever mean one protocol's id; nothing outside the
`packet_ids_from!` macro may name a generated module. 1.12 inserted four clientbound and three
serverbound packets into the middle of the tables, shifting everything above them: `update_health`
is id **62** at 110/210/316 and **65** at 340. The committed test for that is
`update_health_dispatches_on_each_protocols_own_id`, and it was watched failing first — with one
shared 340 table, a protocol-110 adapter handed id 62 emitted `EntityVelocity { entity_id: 65 }`.
No error, a plausible event, the wrong packet.

### Hosted protocols 110, 210, 316 and 340

`V110ServerProtocol`, `V210ServerProtocol`, `V316ServerProtocol` and `V340ServerProtocol` are the
server halves for 1.9.4, 1.10.2, 1.11.2 and 1.12.2. The registry exposes them through
`server_protocol_for_protocol(110)`, `server_protocol_for_protocol(210)`,
`server_protocol_for_protocol(316)` and `server_protocol_for_protocol(340)`. All legacy logins
move directly to Play after login success: these wire revisions have no configuration
acknowledgement packets. Protocols 110, 210 and 316 select their own packet-id tables: their
captured position packet is id 46, while protocol 340 uses id 47.

The play baseline emits a join and absolute position packet, then encodes y=0 through y=255 columns as legacy
`map_chunk` packets. Each canonical block-state id goes through `lodestone_canonical::inverse`,
which returns an exact `(old_id << 4) | meta` representative or an error. The checked chunk path
propagates that error to the server rather than silently using air. The direct block-update helper
uses the same conversion before constructing its packed position and legacy state payload.

The encoder accepts any canonical column spanning y=0 through y=255 and projects precisely that
window; rows below or above it are not sent. It rejects only a source that does not cover the
whole legacy window. Protocols 110 and 210 accept only old block IDs 0 through 212 and 255, and
protocol 316 accepts only IDs 0 through 234 and 255, the sets recorded in their committed registry
data; later ranges remain explicit encoding errors. Its serverbound break decoder accepts the three
break statuses and six faces, so a player can act on the streamed blocks. The in-memory controls join each
registry-selected server through the family adapter, observe a known chunk block, and verify its
block update after breaking it. Live 1.9.4, 1.10.2, 1.11.2 and 1.12.2 client sessions remain the
required real-client validation.

All four hosted protocols now also carry the server's keep-alive watchdog. Each server protocol
emits the clientbound challenge and converts the exact serverbound echo into `ServerBound::KeepAlive`,
which lets `lodestone-server` clear its connection-local pending challenge. Protocols 110, 210, and
316 use a signed VarInt id; 340 widened it to a fixed signed `i64`. The wire controls use literal
body bytes, each protocol's generated ids, and a trailing-byte rejection rather than a codec
round-trip. They do not substitute for the four external-client sessions.

Each hosted table also decodes its serverbound `settings` id into
`ServerBound::ClientInformationChanged`. The shared server consumes its signed-byte view distance
to resize the connection's streamed chunk window; locale, chat visibility, colours, skin parts,
and main hand establish the full wire boundary but have no hosted consumer. The settings id is 4
in all four tables, and the body is the same through 110, 210, 316, and 340: locale, distance,
VarInt chat mode, colours, skin parts, and VarInt main hand. Literal-body controls check every
table and reject trailing bytes.

Dispatch is one `lodestone_core::dispatch::Table` per protocol, cached in a four-slot array of
`OnceLock`s indexed the same way `ids_for` resolves a table. A handler or `IGNORED` entry may
declare a `ProtocolRange`; `Table::build` skips one whose range excludes the protocol it is
building for and demands one whose range includes it, which is what lets a single handler list
serve tables with different contents.

### The nine shape deltas, and which mechanism carries each

Measured from `minecraft-data`'s `protocol.json` with named types inlined. **Do not collapse the
primitive aliases when re-deriving this** — `varint`, `i64`, `u8` and `f32` all resolve to the
string `"native"`, and inlining that away hides every retype. A first pass that did so reported
six changes and missed `keep_alive` entirely.

| packet | changed at | delta | carried by |
|---|---|---|---|
| `resource_pack_receive` | 210 | leading pack-hash string dropped | `#[mc(until = 110)]` field |
| `named_sound_effect`, `sound_effect` | 210 | pitch `u8` → `f32` | second struct |
| `collect` | 316 | stack-size VarInt added | `#[mc(since = 316)]` field |
| `spawn_entity_living` | 316 | mob type `u8` → VarInt | second struct |
| `title` | 316 | action-bar inserted as action `2` | normalised in the arm |
| `block_place` | 316 | cursor `i8`×3 → `f32`×3 | second struct |
| `keep_alive` (both directions) | 340 | id VarInt → `i64` | second struct |
| entity-metadata type table | 340 | NBT joins as type 13 | version gate in the codec |

The split is not stylistic. A **field appearing or disappearing** is exactly what the derive's
`since`/`until` predicates express, and those deltas live on the shared definition in
`lodestone-protocol-common`, whose ranges widen to `110..=754`. A **retype** cannot be an
attribute: reading eight bytes where one to five were sent does not fail, it consumes the next
packet's header, and every later packet in the stream is then garbage. Those get a second struct
with its own `#[mc(protocols)]` range and an explicit branch on `self.protocol`.

The metadata gate is a real check rather than a comment: encoding an NBT metadata value below
340 errors, and decoding type 13 below 340 errors instead of consuming the rest of the packet as
a plausible NBT read.

### Exact inversion of legacy block-state ids

`lodestone-canonical::inverse` is the shared reverse lookup for a canonical 26.2 state that must
be represented in pre-1.13 `(old_id << 4) | meta` form. It scans the forward
`canonical::resolve` result once, keeps the minimum packed representative when aliases exist,
and indexes the result with a `OnceLock` table spanning the canonical state registry. Only exact
forward resolutions enter the table: missing legacy entries, context-dependent entries,
out-of-bounds pairs, and bridge-unmapped states are excluded. A canonical state outside the
resulting image returns `InverseError::Unsupported`; the reverse lookup never substitutes air or
silently chooses a nearby state.

### Captures

`tests/captures/join_{1_9_4,1_10_2,1_11_2}.txt` are clientbound bytes from real servers, and
`tests/capture_join.rs` holds both the `#[ignore]`d recorder that made them and the hermetic
replay that consumes them. See [`the captures' own README`](../crates/versions/1.9/tests/captures/README.md)
for the format and the caps. They are the authority these three protocols otherwise lack: no
Mojang data generator exists before 1.13, and `minecraft-data` is a cross-check, not a source of
truth.

They found a defect on first use. `play_world_border` had picked up `play_title`'s new action
renumbering — both arms opened with the same three lines — and a real action-3 body decoded with
38 trailing bytes. Nothing else in the suite could see it.

They also fix the pre-1.10 sound pitch scale by measurement. Asking a 1.9.4 server for pitch 1.5
and 0.5 put **94** and **31** on the wire: `pitch * 63`, truncated. A scale of 62 would give
93/31 and 64 would give 96/32, so the *pair* separates all three where neither value does alone.
1.10.2 and 1.11.2 put `3fc00000` and `3f000000` — exact floats — which is the committed
differential: the byte era must never reproduce 1.5 exactly, the float era must always.

## How to change it

- **Adding a fifth protocol to this era** (there is none left; this is the shape for the next
  era): generate its table with `cargo run -p xtask -- gen-packet-ids --source minecraft-data`,
  add a `PROTOCOL_*` const, a `PROTOCOLS` entry, an `IDS_*` static, an `ids_for` arm, a
  `play_dispatch_table` slot, a `minecraft_versions` entry, and a `MEMBERS` row plus a recorder
  and a replay test. Then record a capture and let the replay tell you which shapes moved.
- **Never widen a `#[mc(protocols)]` range without a capture from the protocol it now claims.**
  That is the plan's one guard against inheritance-by-range, and the reason the three captures
  are part of this work rather than a follow-up.
- **The adapter type is still called `V340Adapter`** even though it serves four protocols.
  Renaming it touches ~150 references across the family's own tests and `lodestone-fuzz`; it is
  worth doing, but not inside a change that also moves the wire.
- `minecraft-data` ships 1.10.2's shapes under its `1.10` directory and 1.11.2's under `1.11`;
  `gen-packet-ids` resolves that same-major fallback itself, so pass the real version and
  protocol.
- The hosted types are deliberately `V110ServerProtocol`, `V210ServerProtocol`,
  `V316ServerProtocol` and `V340ServerProtocol`, not an era-wide server adapter. Add a separate host
  implementation and an explicit `ServerFamily` predicate before exposing another protocol: the
  family adapter's broader `PROTOCOLS` list is not evidence that its server packet layouts match.

## Configuration

None new. The era is selected by the existing `v1-9` feature on `lodestone-registry`; the
client adapter reads `PROTOCOLS` from the crate, while the server registry exposes protocols 110,
210, 316 and 340. Oracle ports live in `scripts/live-oracles/legacy.sh` and are read from there by
`tests/capture_join.rs`'s `MEMBERS` table.

## Dependencies

`lodestone-core` (`Ctx`, `ProtocolRange`, `dispatch::{Table, Handler, IGNORED}`),
`lodestone-macros` (`since`/`until`/`protocols`), `lodestone-protocol-common` (the shared packet
definitions shared by this era), `lodestone-canonical`, `lodestone-world`,
`lodestone-data`, and `lodestone-server` (the version-free hosting trait and chunk column).
Recording needs Apple `container` and
[`scripts/live-oracles/legacy.sh`](../scripts/live-oracles/legacy.sh); replay needs nothing.
