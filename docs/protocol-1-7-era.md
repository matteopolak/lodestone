# Protocol 5 era (Minecraft 1.7.6-1.7.10)

## What it is

`lodestone-v1-7` is the client protocol crate for **protocol 5**, spoken by
Minecraft 1.7.6 through 1.7.10 — the bottom of the version ladder and the only
era that shares almost nothing with its neighbour. It implements the **joining
direction only**: there is no `ServerProtocol`, so this version can be joined
but not hosted.

## The protocol number, and where it came from

**Protocol 5.** Read off a real 1.7.10 server's own status response rather than
taken from anyone's word for it, with a negative control: the handshake was
sent claiming protocol 0, 47 and 5 in turn, and all three replies said 5, so
the server is reporting its own number rather than echoing the request.
`vendor/minecraft-data/data/pc/1.7/version.json` agrees, and adds the boundary
below: protocol 4 covers 1.7.2-1.7.5, protocol 5 covers 1.7.6-1.7.10.

`PROTOCOLS` is therefore `&[5]`, and `VersionAdapter::supports` answers for
that one number. Protocol 4 is deliberately **not** claimed: no jar and no
dataset for it is present on this machine, so admitting it would be a guess
dressed as a range. Admitting it later needs the same measurement this era
made — a shape diff against a real 1.7.2-1.7.5 server, and a join capture.

## Why it is a singleton era

`docs/plans/multi-version-protocol-dedup.md` groups adjacent versions into one
era when they agree on **85%** of packet shapes. Measured across every packet
definition in both directions and all four connection states, with every named
type inlined, protocol 5 and protocol 47 agree on **37 of 112** shapes — 33% —
and eight of those 37 are the handshake and status packets that have never
changed in any version. There is nothing adjacent to fold in.

## How it works

Standard era-crate layout: `src/packets/` per wire module, `src/generated/` for
tables, `src/adapter.rs` for the `VersionAdapter` implementation, and a
construction-time-checked dispatch table (`lodestone_core::dispatch`) in place
of a fallthrough arm — so a clientbound id is either handled, listed as
deliberately ignored with a reason, or fails table construction.

`cargo xtask connectedness` reports **clientbound decoded 50/65, emits 50/65,
decoded-but-stranded 0, serverbound encoded 19/24**, examined over 50 dispatch
arms.

### What is genuinely different, not merely older

Measured, not assumed. Each is documented where it is implemented.

| difference | where | why it bites |
|---|---|---|
| Chunk payloads are **zlib streams inside the packet body** | `packets::chunk` | Whole-connection compression does not exist here — the login state has no compression-threshold packet at all — so the inflate is per chunk packet |
| Block ids and metadata arrive in **separate arrays**, grouped by array type across the whole column, with a conditional `add` nibble array for ids above 255 | `packets::chunk` | Later eras pack one 16-bit value per block. Both groupings produce a byte-identical total length |
| Bulk chunk metadata **trails** the payload | `packets::chunk` | Protocol 47 moved it in front. A single-column packet parses under either order without erroring |
| Positions are **three separate numbers**, in three width combinations (`iii`, `ibi`, `isi`) | `packets::position` | The packed 64-bit block position arrives with protocol 47 |
| Serverbound movement carries a **`stance`**, after the feet `y` | `packets::game` | Removed at protocol 47. Getting the order wrong is completely silent — see below |
| The clientbound teleport's middle `f64` is the **eye position**, not the feet | `packets::game` | Every later protocol sends feet there |
| Entity ids are `i32` everywhere except the four spawn packets | `packets::entity` | A varint decoder consumes the wrong number of bytes |
| `keep_alive` is `i32`; food, experience and effect duration are `i16`; `entity_destroy` has a `u8` count; `custom_payload` has an `i16` length | throughout | Same class as above |
| Movement carries a `bool on_ground`, not a relative-flags byte | `packets::game` | A 47-era decoder reads `true` as "relative x" |
| Item NBT is **gzip behind an `i16` length** | `packets::slot` | Later eras use a bare optional tag |
| Custom entity name sits at data-watcher index **10**, not 2 | `entity_metadata` | |
| **Two fixed-point scales in one protocol**: 32 per block for entities, 8 for sound positions | `adapter` | The same protocol, genuinely inconsistent |
| Mob-effect ids are **one-based** | `adapter` | |
| Attribute keys are dotted camelCase, which is not a valid `Identifier` at all | `adapter` | Needs an explicit translation table, not a namespace prefix |
| Plugin channel names contain `|` and uppercase | `adapter` | Cannot be represented as an `Identifier`; these channels are in `IGNORED` |
| All minecart variants share object type 10 | `generated/entity_types` | The variant travels in entity metadata |
| Objects and mobs are numbered in **two separate id spaces** | `generated/entity_types` | An id means nothing without knowing which spawn packet carried it |

Two hazards that were **ruled out** by measurement rather than worked around:
strings are `varint(byte count) + UTF-8`, identical to 1.8; and login-success
and named-entity-spawn UUIDs are dashed 36-character strings, not the 128-bit
pair a later era sends.

### The movement chain, and why it is the era's sharpest lesson

Three defects sat on one chain and none was visible to any hermetic test,
because a protocol 5 server's response to all three is identical: it stops
accepting movement, with **no error, no disconnect, and nothing in its log**.

1. The serverbound movement packets order their four doubles `x`, `y`,
   `stance`, `z` — the stance **after** the feet. Both orders encode to the
   same length and round-trip perfectly.
2. The clientbound teleport's middle double is the eye position. Reading it as
   feet puts the player 1.62 blocks in the air on every teleport.
3. The teleport must be **confirmed** by echoing a matching serverbound
   `position_look` back. There is no teleport id and no confirmation packet
   until protocol 340; until the echo arrives the server holds the player.

The three interact: because the echo derives its own stance, a wrong reading of
(2) makes (3) carry a stance the server refuses, and a wrong order in (1) makes
the echo never match. The server's held-player branch returns before the stance
range check, which is why nothing complains.

The discriminator, measured over a 320-block walk on both arms:

| outcome | broken | fixed |
|---|---|---|
| server re-sends its own position | 65-70 times | once |
| chunk columns loaded | 445 (the join burst, and no more) | 759 |
| chunk columns unloaded behind the player | 0 | 420 |

Confirmed a third way: with the transposition the server saved the player at
the spawn point on logout, having discarded all 1,600 movement packets.

`tests/live_movement.rs` is the gate that keeps those apart.
`tests/movement.rs` pins both field orders at **byte offsets**, because a
struct compared against itself cannot see a transposition.

### The one concept with no canonical equivalent

`PlayerListEntry.uuid` is required by the version-free model, and **protocol
5's player list carries no UUID, no profile and no skin** — just
`(name, online, i16 ping)`.

Three options existed. A nil UUID collides every entry. Dropping the packet
makes the player list an island. What this crate does instead is derive the
**offline-mode UUID**: a version-3 UUID over the *bare* bytes of
`OfflinePlayer:<name>`, with no namespace — which `Uuid::new_v3` cannot express,
since it always prepends one, hence the direct `md-5` dependency.

That choice is checkable, and is checked: a real server sends the same
derivation in its own `login_success`, and `tests/capture_join.rs` asserts the
two agree. **It is wrong against an online-mode server**, where the real
profile UUID is unrelated to the name. Nothing in this era's wire can fix that;
resolving it needs either a profile lookup the client does not do or a decision
to leave the field empty.

## How to change it

- **The adapter** is `src/adapter.rs`: `CLIENTBOUND` lists every translated
  packet and `IGNORED` every deliberately untranslated one with its reason.
  Both entries must be spelled literally — `cargo xtask connectedness` reads
  `Handler::new(` as a text anchor and a helper function defeats it.
- **Tables** in `src/generated/` are regenerated, never edited:
  `LODESTONE_REGEN=1 cargo test -p lodestone-v1-7 --test entity_types` rebuilds
  the entity-type tables from the committed wire transcript, reproducing the
  file byte for byte including its provenance notes.
- **Admitting protocol 4** means measuring shape identity against a real
  1.7.2-1.7.5 server and adding a join capture; the crate would then declare
  `&[4, 5]` and nothing else would need to change, since nothing keys off the
  folder or feature name.
- **Removing the era** is deleting `crates/versions/1.7/` plus its dependency
  and feature lines in `lodestone-registry`.

### Gotchas

- The era is invisible unless its feature is named: no family is on by default
  in `lodestone-registry`, and the shell's default `live` feature enables only
  `v26-2`.
- A vanilla server uses single-column `map_chunk` for **nothing but chunk
  unloads**; every loaded column arrives in `map_chunk_bulk`. So no vanilla
  capture can exercise the single-column loading path, and `tests/chunk.rs`
  builds it by hand.
- A chunk unload is **not** an empty payload. Its 12 compressed bytes inflate
  to a 256-byte biome footer, because ground-up implies a footer whether or not
  a section is present.
- `mc_1_8`'s physics profile is what this era maps to. Protocol 5 pre-dates the
  1.9 input-pipeline rewrite, so 1.8's input model is the algorithm this era
  actually ran, not an approximation of it.
- The oracle's ports are **25602/25603**, not the 25600/25601 the "next two
  free" rule would suggest — `scripts/live-oracles/lovelier.sh` already
  publishes those.

## Configuration

| knob | where | effect |
|---|---|---|
| feature `v1-7` | `lodestone-registry` | Registers the family. Off by default, like every family |
| `LODESTONE_REGEN=1` | `tests/entity_types.rs` | Regenerates the entity-type tables from the committed transcript |
| `./scripts/live-oracles/legacy.sh 1.7.10` | — | Boots the oracle on `:25602`, RCON `:25603`, container `lodestone-mc1710`, `eclipse-temurin:8-jdk`, flat quiet overworld |

## Dependencies

- `lodestone-canonical` — the pre-Flattening `(id << 4) | meta` translation,
  shared with the 1.8 and 1.9 eras. The one substantial piece of work this era
  did **not** have to repeat.
- `lodestone-protocol-common` — the measured-identical packet definitions, each
  carrying its own `#[mc(protocols = "…")]` range. Founding this era widened
  exactly one: `LoginSuccess`, from `47..=578` to `5..=578`. The range is
  enforced at decode, so the mistake surfaced as a runtime refusal rather than
  as a silent mis-parse.
- `lodestone-core`, `lodestone-macros`, `lodestone-model`, `lodestone-world`,
  `lodestone-data` — the usual seam.
- `flate2` for the per-packet chunk inflate, and `md-5` for the offline-mode
  UUID derivation described above.

## Evidence

Provenance for everything ported, since no first-party source for this protocol
exists:

| subject | outside source |
|---|---|
| protocol number | a real server's status reply, with a control ruling out echo; `minecraft-data`'s `version.json` agrees |
| packet ids and shapes | `minecraft-data` 1.7 as bootstrap, then a recorded real join as the authority (`tests/captures/`) |
| entity type ids | a wire transcript: each name summoned over RCON, the id read from the resulting spawn packet, accepted only when its fixed-point position matched the summon's. `minecraft-data` is the cross-check |
| item ids | `minecraft-data` 1.7's `items.json`, cross-checked against ids in a recorded container packet |
| block ids and metadata | `minecraft-data` 1.7's `blocks.json` for the wire values; expected canonical states come from `lodestone_data::block_states`, the jar-derived 26.2 registry |
| chunk array grouping | the biome footer's *position* in a real bulk packet — both groupings give the same total length, so length cannot discriminate |
| nibble parity | four wool blocks at adjacent x with metadata 14, 1, 5 and 11, chosen so no value equals its byte-partner, read back off a real server; `minecraft-data`'s variation list agrees on the colours independently |
| movement field order | the server's behaviour over a 320-block walk, three independent ways (position corrections, columns streamed, saved logout position) |
| the eye-position reading | an RCON teleport to an exact y of 80.0 producing 81.62 on the wire, plus the server's own login log |
| chunk unload framing | 420 recorded unloads from one walk; twelve bytes of one of them are inlined in `tests/chunk.rs` |
