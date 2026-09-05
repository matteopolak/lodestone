# The 1.19 era crate: one family, one protocol, chat that has to be answered

## What it is

`crates/versions/1.19` (package `lodestone-v1-19`) joins and hosts Minecraft
1.19.4 — protocol **762** — from one generated packet-id table, one generated
block-state table, one generated entity registry, and the era's own chat,
spawn and chunk-shape code. It is the fifth era crate, after
[`1.9`](./protocol-1-9-era.md), [`1.14`](./protocol-1-14-era.md),
[`1.13`](./protocol-1-13-era.md) and [`1.17`](./protocol-1-17-era.md).

The protocol number is read off the jar's own `version.json` in
`.cache/mc/1.19.4/server.jar`, which reports `"protocol_version": 762`. The
other 1.19.x releases carry 759, 760 and 761, each with a different chat shape;
none is fetched here and none is served.

This is the era where **chat stopped being a string**. A message now carries a
sender profile id, a per-sender chain index, an optional 256-byte signature,
the exact bytes that signature was taken over, and an acknowledgement window
naming what the sender had seen — and the serverbound side must send a matching
window on every message it sends, signed or not, or the server eventually
disconnects the connection. The second era-defining change is that **every
non-player entity now spawns through one packet**.

## How it works

### A singleton era, measured

`PROTOCOLS` lists one number, and that is a measurement rather than a default.
Re-derived from `minecraft-data` with named types inlined and **primitive
aliases kept** (collapsing `varint`/`i64`/`u8`/`f32` to a single token hides
every retype — the 1.9 era's warning applies here too):

| boundary | identical shapes | identity |
|---|---|---|
| 1.18.2 → 1.19.4 | 137 of 175 | **78%** |
| 1.19.4 → 1.20.6 | 113 of 201 | **56%** |

Both are below the 85% grouping threshold, so neither neighbour joins this
crate. The plan's own figures (77% and 57%) agree within the noise of a
slightly different inlining.

Dispatch is one `lodestone_core::dispatch::Table`, cached in a one-slot array
of `OnceLock`s indexed the way `ids_for` resolves a table. The array shape is
kept so that adding a second member later is an index rather than a
restructure.

### Chat: three packets in, two out, and a counter that keeps the session alive

Through 1.18 a chat message is one JSON string and a position byte. At 762 the
clientbound side is split by *what the client may trust*:

| packet | who wrote the text | signed | carries a sender id |
|---|---|---|---|
| `player_chat` | a player | optionally, over exact bytes it also carries | yes |
| `profileless_chat` | the server, on a player's behalf | no | no |
| `system_chat` | the server | no | no |

Only the first fills a `ChatAckInfo`, and only the first has a profile id for a
hide-in-chat filter to key on. `player_chat` also keeps **both** texts: the
server's decorated form is what a client displays, but a signature is hashed
over the plain form, so `raw_content` keeps the signed string verbatim even
when the decorated form is what the user sees.

The acknowledgement chain is the part that is easy to skip and expensive to
skip. The server keeps a per-connection list of pending signed messages; every
serverbound chat packet carries a last-seen update that drains it, and a client
that reads chat without ever sending any must drain it with a standalone
`message_acknowledgement`. `V762Adapter` holds a `pending_ack` counter that
`player_chat` bumps and every serverbound chat path takes.

Three differences from the modern family's otherwise similar packets, each of
which silently mis-frames a stream if inherited:

* **Components are JSON strings, not network NBT.** 1.20.3 moved them; reading
  one the modern way consumes a tag byte where a length prefix was.
* **There is no server-global message index.** The modern `player_chat` opens
  with one; this one opens with the sender UUID.
* **There is no acknowledgement checksum byte.** It is 1.21.5's addition;
  writing one appends a byte the server reads as the next packet's length.

This crate sends **unsigned** chat: zero timestamp and salt, no signature. That
is accepted by any server not running with secure-profile enforcement, which is
the only mode it can reach — signing needs a session key from an authenticated
profile, and online-mode login is not implemented for this era. The oracle
therefore sets `enforce-secure-profile=false`; with enforcement on, the server
rejects the *join*, not the message.

### One spawn packet, and why the entity table is per era

1.19.4 removed the separate mob-spawn packet the four eras below all carry. Its
head-rotation byte was folded into the object-spawn packet — inserted *before*
the object-data field, which was itself widened from a fixed `i32` to a VarInt
at the same time. Two consequences, neither of which raises anything:

* A decoder carrying the era below's field order reads the head-pitch byte as
  the first byte of the object data, then the object data's bytes as the
  velocity, and produces a spawn at a plausible position with nonsense motion.
* There is no longer a packet whose mere arrival means "this is a mob", so the
  entity kind is entirely a registry lookup.

That registry is per era and cannot be borrowed. 1.19 added `allay`, which
sorts **first** alphabetically, so id 0 is `minecraft:allay` here and
`minecraft:area_effect_cloud` in every era below — every subsequent id has
moved too. `tests/entity_types.rs` asserts that shift from the committed dump
rather than describing it.

### The vertical window became a lookup

The column has not been sixteen sections tall since 1.17. What changed at 762
is *where the window is stated*. Through 1.18 the join packet carried the
already-resolved dimension entry inline as a second NBT blob, so `min_y` and
`height` could be read straight out of it. At 762 that blob is gone: what
arrives is the whole registry (`dimension_codec`) plus the *name* of the
dimension type in use (`world_type`).

`ChunkShape::from_dimension_registry` walks
`minecraft:dimension_type` → `value` (a list of `{name, id, element}`) → the
entry whose `name` matches → `element` → `min_y` and `height`, and nothing else
infers the window. A name the registry does not carry leaves the shape alone
rather than taking the first entry, which would silently be the overworld's
window in some other dimension.

Inbound chunk columns retain their positioned block-entity records. The wire
type id remains version-specific, so the decoder derives the canonical type
from the already translated block state at the same position and preserves the
NBT payload. Pre-1.20 sign fields are reshaped into the shared two-sided sign
model before the column reaches `LoadedChunk`; dropping the records here makes
signs, containers, and other block entities disappear despite valid chunk data.

`respawn` at 762 names a dimension type but does not describe it, so the
adapter retains the registry blob the join delivered and re-resolves through
it. An adapter that keeps reading the old inline field reads a string where an
NBT compound was and desynchronises immediately.

### Hosted play

`V762ServerProtocol` is selected by `lodestone-registry` for protocol 762.
It stays in the legacy login-to-Play path: `login_success` writes the
1.19 profile-property list, but there is no Configuration phase. Its join
packet carries an overworld dimension-registry entry for `-64..319`, then the
position packet without the older trailing dismount flag.

Every hosted column supplies all 24 sections, with a block palette and a biome
palette per section, followed by the inline light framing this protocol uses.
The encoder projects a source that covers that complete vertical window;
columns with block entities or a biome other than plains are rejected until
their wire forms are implemented. The outbound state inverse comes only from
the committed 1.19.4 state table, and a missing or ambiguous canonical state
is an error rather than a substitution.

`tests/server_integration.rs` is the consumer-path control: a registry-selected
server and the 762 adapter complete login, render the fixture chunk, and apply
a block-break update over an in-memory connection. It also moves across a chunk
boundary, proving the host's four serverbound movement shapes reach the view
recentre consumer rather than merely decoding into an unused value. The host
lifts `position`, `position_look`, `look`, and status-only `flying`; position
samples update streaming, fall tracking, and the server's player snapshot,
while look-only and status-only samples preserve their narrower consumers.
The same test module carries an adapter-to-registry-host block-use assertion:
the 762 `block_place` body reaches `ServerBound::UseItemOn`, including its
hand, cursor, and prediction sequence, which is consumed by the integrated
server's placement and interaction path. The literal-byte decoder test keeps
that conversion independent of the adapter encoder and rejects an out-of-range
face before it can enter the consumer. Arm swings take the same complete route:
the hosted decoder accepts only hand ordinals zero and one, appends the shared
broadcast event, and emits the clientbound entity id plus animation byte for
other connected players. `tests/server_protocol.rs` fixes both request bodies
and the resulting clientbound bodies as literals. `tests/server_integration.rs`
then joins a sender and observer through the registry-selected host and proves
the observer receives the sender's off-hand animation event.
The two-byte hotbar-selection request likewise reaches the shared authoritative
inventory through `ServerBound::CarriedItemChanged`. Only signed values
`0..=8` are accepted: negative, out-of-range, truncated, trailing-byte, and
pre-Play forms remain ignored. The focused protocol test anchors the final
legal value as literal bytes, then sends the adapter's selection action through
the registry-selected protocol so the decoder cannot become an unconsumed
island.
`tests/server_protocol.rs` checks the packet ids present in the real 1.19.4
capture and decodes literal movement bodies with the production codecs,
including trailing-byte and unknown-id negative controls. This is not
real-client validation; a separately run 1.19.4 client remains the proof that
the complete join registry and empty-light handling are accepted outside the
in-process adapter.

### External-client acceptance

The opt-in release-client gate covers this hosted protocol as row **762**. Run it with
`just external-client-acceptance --protocol 762 --output /private/tmp/lodestone-v762` and an
external driver. Its six-stage evidence uses `configuration.mode: "login_to_play"` because this
era has no Configuration phase, and `chunk_batch_acknowledgement.mode: "unbatched"` with
`batch_count: 0` because its host sends columns without chunk-batch framing. The remaining stages
are unchanged: join, deliberate movement, one observed `start_destroy_block` result, and a
client-initiated clean disconnect, all with exact 1.19.4 client-build provenance. This gate was not
launched while this documentation was updated; protocol 762 therefore remains unverified by a
real external client until that manual run produces `report.json`.

### The nine shape deltas, and which mechanism carries each

| packet | delta | carried by |
|---|---|---|
| `login` (join) | inline dimension entry → dimension *type name*; trailing optional death location added | this crate's own struct |
| `respawn` | same two changes | this crate's own struct |
| `position` (clientbound) | trailing `dismount_vehicle` bool **removed** | this crate's own struct |
| `spawn_entity` | head-rotation byte inserted mid-struct; object data `i32` → VarInt | this crate's own struct |
| `spawn_entity_living` | **gone**; folded into `spawn_entity` | deleted |
| `entity_effect` | trailing optional NBT factor data added; `0x08` blend flag | this crate's own struct |
| `login_success` | trailing signed-property list added | this crate's own struct |
| `login_start` (serverbound) | trailing optional profile UUID added | this crate's own struct |
| `block_dig` / `block_place` / `use_item` (serverbound) | trailing block-prediction `sequence` added | plain fields |
| `player_info` | action ordinal → action **bitmask**; removal split into `player_remove` | hand-written decoder |
| clientbound `chat` | replaced by three packets | `packets::chat` |
| serverbound `chat` | replaced by `chat_message` + `chat_command` | `packets::chat` |

A **field appearing or disappearing** is what the derive's `since`/`until`
predicates express, but every delta above is at a protocol boundary rather than
inside this era, so each is carried by this crate's own definition rather than
by a predicate on a shared one. Eight shared definitions **widened** to 762
instead — the three relative-movement packets, `teleport_confirm`,
`arm_animation`, both `keep_alive`s and `resource_pack_receive`. Each is a
packet `minecraft-data` reports shape-identical from 758 to 762, and each
additionally decodes or encodes out of the committed capture, which is the
plan's one guard against inheritance-by-range.

The serverbound `chat` definition deliberately does **not** widen, and the
reason is worth stating: the message string is still first, so a widened
definition would encode an acceptable *prefix* and fail only at the server,
with the connection closing rather than a decoder erroring.

### Captures

`tests/captures/join_{1_19_4,1_18_2,1_20_6}.txt` are clientbound bytes from
real servers, and `tests/capture_join.rs` holds both the `#[ignore]`d recorders
that made them and the hermetic replays. See
[the captures' own README](../crates/versions/1.19/tests/captures/README.md)
for the format, the caps, and why the two neighbour files exist.

The era capture is not a passive recording: the recorder **sends one chat
message** once the join settles, and the server broadcasts it straight back as
`player_chat`. That is the only available check on this crate's serverbound
signing tail, because a 1.19.4 server reads the timestamp, salt, optional
signature and last-seen window off every chat packet and closes the connection
on a malformed one rather than ignoring it. The message arriving at all is the
evidence.

The replay pins values the server chose: survival on `minecraft:overworld`, the
flat preset's own floor in canonical ids at the height the server's own
registry declares (uniform bedrock at the floor, dirt one above, grass three
above, and *not* grass four above so the probe is discriminating), and the
signed message's own text, chain index and millisecond timestamp.

**The chunk buffer is longer than the sections inside it, at 762 too.** The
1.17 era measured this against 1.18.2; it is re-measured here rather than
inherited. The committed column declares a `chunkData` of 2,268 bytes across a
24-section window and leaves **23** over — one zero byte per section whose
block palette is single-valued, which is every section but the one holding the
floor. So the decoder reads exactly `section_count` sections and then requires
what remains to be all zero and no longer than the section count; only the
exact-length half of the detector is given up, and only where the wire forces
it.

### The negative controls, and what they actually measured

Run, not predicted, and the answer is the weaker of the two the earlier eras
gave.

A multi-protocol era gets its control for free: feed one member's bytes to
another member's adapter. **A singleton has no sibling to misroute against**,
so the control comes from outside — real 1.18.2 and 1.20.6 joins replayed
through the 762 adapter.

The first result came from trying to record them. **The 762 adapter cannot join
either neighbour at all**, and the reason is one byte at the end of
`login_start`: 758 reads a bare username and treats 762's presence byte as the
start of the next packet, while 766 reads a *required* 16-byte profile UUID and
rejects 762's single `false` byte outright, which a 1.20.6 server reports as a
decode failure on its own login packet. Both neighbour captures are therefore
recorded with a hand-written login, which also keeps the control free of any
dependency on another version crate.

Replaying what those logins produced:

| neighbour | errored | silent | plausible wrong events | ids 762 does not carry |
|---|---|---|---|---|
| 1.18.2 (758) | 35 | 19 | **10** | 0 |
| 1.20.6 (766) | 39 | 13 | **3** | 10 |

Two of the lower neighbour's ten are wrong in a way nothing downstream could
notice: `entity_metadata` at 758 sits where `held_item_slot` does at 762, so a
metadata update becomes a hotbar selection, and `update_time` sits where
`set_passengers` does, so a clock tick becomes a vehicle with no passengers.

The upper neighbour gives the sharpest single result. Two of its three
plausible wrong events come from an id whose **name agrees on both sides**:
`spawn_entity` is id 1 at 762 and at 766, so nothing about the routing is wrong
at all — only the shape and the entity registry are. A 1.20.6 minecart spawn
read at 762 comes out as a `spawner_minecart` at a plausible position with a
plausible velocity, with nothing red anywhere. That is precisely what the
per-era entity table and the inserted head-rotation byte exist to prevent,
demonstrated rather than described.

So the guarantee this crate offers is the **whole-stream** one — neither
neighbour's join replays cleanly through this adapter, which
`neither_neighbours_capture_replays_as_a_clean_join` asserts directly — and not
a per-packet one. The measured split is pinned so a change on either side
surfaces as a mismatch to re-derive rather than as a silently weaker control.

## How to change it

- **Adding a second protocol to this era**: there is no candidate. 1.20.6
  agrees with 1.19.4 on 56% of packet shapes and brings the configuration phase
  and data components with it, and 1.19.3 and below carry a different chat
  shape again. Should one ever appear, the steps are the ones the 1.17 era
  records: generate its id table with `cargo run -p xtask -- gen-packet-ids
  --source minecraft-data`, run the jar's data generator for its `blocks.json`
  and `registries.json`, **check those two dumps against the committed ones
  before anything else**, then add a `PROTOCOL_*` const, a `PROTOCOLS` entry,
  an `IDS_*` static, an `ids_for` arm, a `play_dispatch_table` slot, a
  `table_for` arm in each of `canonical` and `entity_types`, an oracle row in
  `scripts/live-oracles/legacy.sh`, and a recorder and replay test.
- **Never widen a `#[mc(protocols)]` range without evidence from the protocol
  it now claims.** The eight widenings here are each for a packet
  `minecraft-data` reports unchanged from 758 through 762, and each is
  additionally exercised by a committed capture.
- **Signing is not implemented, and the gap is a session key, not a wire
  shape.** `packets::chat::ChatSessionUpdate` and the signature fields on
  `ChatMessage` and `ChatCommand` are all modelled; what is missing is an
  authenticated profile to get a key from, which is the same online-mode login
  gap every legacy era has. `ChatAckInfo::verified` is constructed `false`
  here, fail-closed, and only the client driver may raise it.
- **The adapter type is called `V762Adapter`**; the folder is named for its
  Minecraft version.
- **Extending the host**: keep `V762ServerProtocol` independent from adjacent
  eras. Add a captured-byte control before changing join, section, light or
  action framing; expand the registry entry only together with a client-facing
  test that consumes the added registry data.
- Regenerating the jar dumps needs a **Java 17** container image: 1.19.4
  declares `java_version` 17 in its own jar and refuses to start under 8. It
  ships a bundler jar, so the data generator is selected through the bundler's
  own main-class property.

## Configuration

None new. The era is selected by a `v1-19` feature on `lodestone-registry`; the
registry reads `PROTOCOLS` from the crate and selects `V762ServerProtocol` for
hosting protocol 762. Oracle ports live in
[`scripts/live-oracles/legacy.sh`](../scripts/live-oracles/legacy.sh) (1.19.4
game `25596` / RCON `25597`; 1.20.6, which belongs to no family and exists only
so the upper neighbour capture can be recorded, game `25598` / RCON `25599`)
and are read from there by `tests/capture_join.rs`. The 1.19.4 row sets
`level-type=FLAT` and `enforce-secure-profile=false`; the second is not
optional, because an enforcing server rejects the join rather than the message.

Two things this era does **not** settle. `minecraft-data` models 1.19.4's
`multi_block_change` records as VarInts where every neighbouring release uses
VarLongs; this crate does not translate that packet, so the disagreement is
recorded rather than resolved. And it models `player_info`'s `update_listed`
field as a VarInt where the wire writes a boolean — the two coincide for the
only values that occur, so nothing here depends on which is right.

## Dependencies

`lodestone-core` (`Ctx`, `ProtocolRange`, `Nbt`, `dispatch::{Table, Handler,
IGNORED}`), `lodestone-macros` (`since`/`until`/`protocols`/`present_if`),
`lodestone-protocol-common` (the shared packet definitions, eight of whose
ranges this era widened), `lodestone-world` (`PalettedContainer`,
`ColumnLight`, `LightPatch`), `lodestone-data` (the canonical 26.2 block-state
registry the generated table targets, and the mob-effect names the effect
packets resolve through), `lodestone-model` (`ChatAckInfo`, `ChatSessionInfo`
and the rest of the canonical event model — the chat-signing shapes it already
carried for the modern family are what let this era report signed chat without
a new model type), and `lodestone-server` (the version-free hosted-server
seam). Recording needs Apple `container` and
[`scripts/live-oracles/legacy.sh`](../scripts/live-oracles/legacy.sh);
regenerating the block-state and entity tables additionally needs the jar's own
data generator under a Java 17 image; replay needs nothing.
