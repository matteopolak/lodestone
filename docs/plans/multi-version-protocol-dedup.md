# Multi-version protocol dedup: wire eras, a shared packet library, and version-ranged definitions

## What it is

A staged plan to stop paying the full per-family cost for each of the twelve protocol versions
still to come. Today every `crates/versions/<family>` crate is a near-copy of its neighbour — measured
here, at `16b72257`, as **0 byte-identical files but 54–61 of ~80 packet structs identical, 65–70%
of adapter dispatch-arm lines identical or near-identical between adjacent legacy families, and 42%
of test-function lines near-duplicated** — while the mechanism that would let one definition serve a
range of protocols (`#[mc(since = N)]`/`#[mc(until = N)]` in `lodestone-macros`) is implemented,
tested, and used by **zero** production packets. The recommended design is: one crate per *wire era*
(grouped where adjacent versions agree on ≥85% of packet shapes), a version-free
`lodestone-protocol-common` crate holding every era-stable packet **with its lift/lower function and
its protocol range**, per-protocol *generated* tables inside the era crate, and a data-driven
dispatch table with an enumerated ignore list replacing the `_ =>` island factory. It keeps
`cargo check -p lodestone-shell --no-default-features` green at every stage and migrates the four
existing families incrementally.

Every number below is a snapshot at `16b72257` (2026-09-01) with the command that produced it,
taken on the shared working tree — so a *sample* in CLAUDE.md's sense. `main` advanced four
commits to `08016db6` while the measurements ran; `git log 16b72257..08016db6 -- crates/protocol
crates/lodestone-{registry,canonical,macros,core,model} xtask` is empty, so none of them touched a
measured subject. Re-run the instrument before quoting; the previous plan in this directory rotted
three times in one day.

## Ground truth, measured

### Sizes

`find crates/versions/<fam> -name '*.rs' | xargs wc -l`, split by directory; hand-written figures are
`cargo run -p xtask -- codegen-ratio` (exit 0, read from a captured file):

| family | protocol | `src/` total | of which `generated/` | hand-written (`codegen-ratio`) | `tests/` |
|---|---|---|---|---|---|
| v1-8 | 47 (1.8.9) | 7,262 | 1,061 | 6,201 | 6,734 |
| v1-9 | 340 (1.12.2) | 7,791 | 1,814 | 5,977 | 6,728 |
| v1-14 | **754** (1.16.5) | 23,239 | 18,100 (17,136 is `STATE_TO_CANONICAL`) | 5,139 | 5,109 |
| v26-2 | **776** (26.2) | 30,643 | 1,591 | 29,052 | 43,915 |

Two corrections to the record this plan starts from. The roadmap's *"~900 irreducible hand-written
lines per family"* is stale by a factor of five — the smallest family is 5,139 hand-written lines
today. And the families are not dormant: `git log --since='60 days ago' -- crates/versions/<fam>/src`
counts 22 (v1-8), 29 (v1-9), 17 (v1-14) and 220 (v26-2) commits, so a migration has to land beside live
edits, not in a quiet tree.

### What is already shared, and the one shared mechanism nobody uses

Read from each family's `Cargo.toml` and `lib.rs`, and from the crates named:

- **Codec primitives are already shared.** `lodestone_core::{Reader, Writer, Encode, Decode, Ctx,
  Packet, read_network_nbt, write_network_nbt, encode_body, decode_body, decode_body_exact}` — no
  family hand-rolls a varint, string or NBT reader (`grep -rl "fn read_varint\|fn write_varint"
  crates/protocol` returns nothing). The "duplicated codec helpers" hypothesis in the brief is false
  for primitives; `docs/protocol-adapter-prologue.md` records the last four helpers moving.
- **Derives are shared.** `lodestone-macros` provides `Encode`/`Decode`/`Packet` with `#[mc(varint)]`,
  `#[mc(nbt)]`, `#[mc(fixed = N)]`, `#[mc(present_if = …)]`, `#[mc(decode_context = …)]`, bit-packing.
- **`#[mc(since = N)]` and `#[mc(until = N)]` exist and are dead.** `lodestone_macros`'s
  `version_condition` emits `ctx.version >= since && ctx.version <= until` around a field; every
  `Encode`/`Decode` already receives `Ctx { version }`; the behaviour is pinned by
  `lodestone-macros/tests/derives.rs::since_until_change_wire_bytes_and_round_trip_by_version` and
  `fixed_respects_since_until_predicates`. `grep -rn "mc(since\|mc(until" crates/versions/*/src`
  finds **zero** uses. Every family instead declares `const CTX: Ctx = Ctx { version: PROTOCOL }` and
  copies the struct. This is the single most important finding: the version-range mechanism the
  design needs is already in the tree, unexercised — the classic "routed around X that exists".
- **The canonical model is shared.** `lodestone_model::{ClientEvent (134 variants), ClientAction,
  Directive, VersionAdapter}`; families produce 63 / 65 / 58 / 131 distinct `ClientEvent` variants
  (`grep -o "ClientEvent::[A-Za-z]*" … | sort -u | wc -l`).
- **World containers are shared.** `lodestone_world::PalettedContainer::{decode, from_values}`,
  `Heightmaps::decode`, `ColumnLight::decode` are called from every family's `chunk.rs`; each family
  owns only the *framing* around them.
- **Pre-Flattening block canonicalisation is shared** (`lodestone-canonical`: `flattening` +
  `canonical`, 9,076 generated lines from the 1.13.2 jar's DataFixerUpper), consumed by v1-8 and v1-9.
  Item flattening: zero content in that crate (`grep -ci item` on all three source files → 0).
- **Test helpers are partially shared.** `lodestone_testsupport::assert_emits_set` is used by 26
  family test files; the join-flow *driver* (encode/decode/round-trip helpers, the login choreography
  walk) is copied per family — see the test figures below.

### The seam today

`lodestone_registry::Family { label, protocols: &'static [i32], make: fn(i32) -> Box<dyn
VersionAdapter> }` in `FAMILIES`, plus `SERVER_FAMILIES` (hosting, one entry: v26-2) and
`PHYSICS_FAMILIES`. Each family exposes `PROTOCOLS` and `adapter_for(protocol)`
(`docs/multi-protocol-seam.md`); `VersionAdapter::supports` tests membership. The shell reaches
families only through `adapter_for_protocol`, `server_protocol_for_protocol`,
`physics_profile_for_protocol`, `supported_protocols` and `compiled_families` (`grep -rn` over
`crates/lodestone-shell/src`; the only two `lodestone_v26_2` mentions there are inside comments).
`cargo check -p lodestone-shell --no-default-features` compiles the registry with every family
feature off — `lodestone-server` and `lodestone-physics` are *required* registry deps precisely so
the two lookup functions still exist and answer `None`/default in that build.

The isolation lint (`xtask`'s `check-isolation`) classifies a crate as a *version crate* by path —
`package_is_version_crate` is `relative.starts_with("crates/protocol")` — and makes two edges fatal:
version → version, and shared → version unless optional. `package_is_version_registry` is the one
metadata-keyed exemption. **Consequence for every design below: a crate placed under
`crates/versions/` cannot be depended on by another family, and a shared crate must live outside
that directory.** `check-deletable <fam>` measures the manifest lines a deletion leaves behind.

### Duplication, four ways

All scripts were run from the workspace root on the working tree at `16b72257`; each is short enough
to re-derive from its description, and stage 0 lands them as an `xtask` command so the numbers are
re-runnable rather than re-typed.

**1. Whole files.** `md5` of every same-relative-path file across the six family pairs: **0 identical
files** in any pair. `diff -u` line-level similarity (unchanged lines / larger file) for the shared
paths, adjacent pairs:

| path | v1-8→v1-9 | v1-9→v1-14 | v1-14→v26-2 |
|---|---|---|---|
| `src/adapter.rs` | 1,591/2,849 (56%) | 1,467/2,849 (51%) | n/a — v26-2's is a directory module |
| `src/packets/player_info.rs` | 324/342 (95%) | 317/342 (93%) | 106/600 |
| `src/packets/slot.rs` | 115/117 | 83/117 | — |
| `src/packets/window.rs` | 144/148 | 121/147 | — |
| `src/packets/login.rs` | 94/97 | 82/99 | 38/130 |
| `src/packets/position.rs` | 92/99 | 76/99 | — |
| `src/packets/status.rs` / `handshake.rs` | 43/45, 24/26 | 43/45, 24/26 | —, 14/26 |
| `src/packets/game.rs` | 360/737 | 469/737 | 154/1,543 |
| `src/packets/entity.rs` | 183/418 | 228/321 | 21/266 |
| `src/packets/chunk.rs` | 193/480 | 144/480 | 65/415 |
| `src/packets/metadata.rs` | 105/230 | 180/296 | 2/3,743 |
| `src/generated/packet_ids.rs` | 281/690 | 560/841 | 248/1,370 |
| `tests/join_flow.rs` | 819/1,025 | 652/1,005 | 173/849 |
| `tests/window.rs` / `inventory.rs` | 375/387, 292/384 | —, 323/379 | — |

Reading: the legacy trio share most of their *packet definitions* and half their *adapter*; `chunk.rs`
and `metadata.rs` are the genuinely version-specific modules; v1-14→v26-2 shares almost nothing
textually because v26-2 was rewritten into a directory module with a different naming scheme, **not**
because the wire diverged that much (see the minecraft-data table below).

**2. Packet structs/enums under `src/packets/`** (same name, whitespace-and-doc-stripped body
compared):

| pair | same-named | identical body | examples of the *differing* ones |
|---|---|---|---|
| v1-8 vs v1-9 | 77 | **54** | `JoinGame` (`dimension: i8`→`i32`), `EntityTeleport` (fixed-point `i32`→`f64`), `KeepAlive` (varint→`i64`), `SpawnObject`, `MetadataValue`, `Settings` |
| v1-9 vs v1-14 | 79 | **61** | `JoinGame` (dimension codec NBT), `Slot` (numeric id→varint+present bool), `LoginSuccess` (string→UUID), `ChunkShape`, `OpenWindow` |
| v1-8 vs v1-14 | 71 | 44 | — |
| v1-14 vs v26-2 | 10 | 3 | naming scheme differs (`UpdateHealth` vs `SetHealth`), so this pair is unmeasurable textually |

Totals: v1-8 82, v1-9 87, v1-14 82, v26-2 117 packet types.

**3. Adapter dispatch arms.** Each legacy `handle_play` is an `if packet_id ==
play::clientbound::X { … }` chain; arms were extracted by packet name and compared by token
`SequenceMatcher` ratio:

| pair | arms | common names | identical | ≥0.85 | ≥0.6 | <0.6 | reusable share of arm-lines |
|---|---|---|---|---|---|---|---|
| v1-8 (1,415 arm-lines) vs v1-9 | 59 / 62 | 52 | 30 (761 lines) | 8 (164) | 9 (259) | 5 (89) | **65%** |
| v1-9 (1,545) vs v1-14 | 62 / 54 | 50 | 37 (749) | 10 (328) | 2 (47) | 1 (32) | **70%** |
| v1-8 vs v1-14 | 59 / 54 | 44 | 19 (427) | 12 (292) | 7 (204) | 6 (126) | 51% |

The v1-9→v1-14 row crosses the Flattening, the light split, 3-D biomes and the long-packing change at
once and *still* shares 70% of translation logic — because those discontinuities live in `chunk.rs`,
`slot.rs` and `metadata.rs`, not in the sixty other packets. A representative identical arm is
`UPDATE_HEALTH` (decode `UpdateHealth`, emit `ClientEvent::HealthChanged` — byte-identical in v1-8
and v1-9 down to the comment); a representative divergent arm is `ENTITY_TELEPORT` (v1-8 divides by
`FIXED_POINT_SCALE`, v1-9 passes `f64` through — the 1.9 boundary).

**4. Functions** (all `fn` items under `src/` excluding `generated/`, bodies normalised):
body-identical across **all three** legacy families: 38 functions / 360 lines (`begin_login`,
`handle_login`, `game_mode`, `json_reason_text`, `chat_kind`, `face_ordinal`, `skin_parts_bits`,
`decode_optional_nbt`, the `player_info` readers …). Near-duplicate (≥0.85) share of function-body
lines: src 20% (v1-8/v1-9), 32% (v1-9/v1-14), 3% (v1-14/v26-2); **tests 42%** (v1-8/v1-9: 2,225 of 5,270
lines), 32% (v1-9/v1-14), 2% (v1-14/v26-2).

The whole-fn number understates sharing because `handle_play` is one 1,400–1,500-line function per
family; the per-arm table is the honest granularity.

### What the wire itself says

`vendor/minecraft-data/data/pc/<ver>/protocol.json` for the fifteen covered target versions
(`dataPaths.json` aliases resolved; each packet definition compared with every referenced named type
inlined, so a `slot` or `entityMetadata` change propagates into every packet carrying one):

| target | packets | identical to previous target | new | gone | identical |
|---|---|---|---|---|---|
| 1.7.10 | 100 | — | | | |
| 1.8.9 | 112 | 33 | 12 | 0 | **29%** |
| 1.9.4 | 118 | 82 | 12 | 6 | 69% |
| 1.10.2 | 118 | 115 | 0 | 0 | **97%** |
| 1.11.2 | 118 | 114 | 0 | 0 | **97%** |
| 1.12.2 | 125 | 113 | 7 | 0 | **90%** |
| 1.13.2 | 143 | 103 | 18 | 0 | 72% |
| 1.14.4 | 153 | 112 | 11 | 1 | 73% |
| 1.15.2 | 153 | 147 | 0 | 0 | **96%** |
| 1.16.5 | 154 | 136 | 3 | 2 | **88%** |
| 1.17.1 | 165 | 130 | 17 | 6 | 79% |
| 1.18.2 | 166 | 156 | 1 | 0 | **94%** |
| 1.19.4 | 175 | 135 | 15 | 6 | 77% |
| 1.20.6 | 201 | 114 | 30 | 4 | 57% |
| 1.21.11 | 225 | 143 | 29 | 5 | 64% |
| 26.2 | — | no minecraft-data | | | *unmeasured* |

This is an upper bound on shape identity (minecraft-data leaves chunk-section bytes, metadata
payloads and some NBT opaque), and minecraft-data is cross-check-grade — but for frozen protocols it
is stable, and the *shape* of the curve is what matters: a handful of large steps (1.8, 1.9, 1.13,
1.14, 1.17, 1.19, 1.20.5) separated by runs of ≥88% identity. Those runs are the eras.

One namespace problem this exposes: legacy tables are generated from minecraft-data names
(`minecraft:update_health`, `minecraft:named_entity_spawn`) and v26-2's from Mojang's report
(`minecraft:set_health`, `minecraft:add_entity`). `play::clientbound::ENTRIES` names agree v1-8∩v1-9
66/72, v1-9∩v1-14 75/77, but v1-14∩v26-2 **7/88**. Any cross-era key must pick one namespace.

### Connectedness now

`cargo run -p xtask -- connectedness` (exit 0, captured):

```
v1-8   clientbound decoded 59/74;  serverbound encoded 21/26;  stranded 0
v1-9  clientbound decoded 62/80;  serverbound encoded 24/33;  stranded 0
v1-14  clientbound decoded 54/92;  serverbound encoded 25/48;  stranded 0
v26-2  clientbound decoded 141/141; emits 139/141; serverbound encoded 68/69;
      serverbound decoded 66/69, connected 47/69; decodes-to-Ignored-only 19
```

The legacy gaps (15, 18, 38 undecoded packets) are mostly packets v26-2 *does* implement — the
"backport" the owner describes is real: v26-2 carries decoders for scoreboard, boss bar, particles,
sound, advancements and more that the legacy wires also have in some shape.

### Oracle assets on disk, and who uses them

`ls .cache/mc/` at `16b72257`: server jars for **12 of the 16 targets** — 1.8.9, 1.12.2, 1.13.2,
1.14.4, 1.15.2, 1.16.5, 1.17.1, 1.18.2, 1.19.4, 1.20.6, 1.21.11, 26.2 — plus 1.20.1. Missing:
**1.7.10, 1.9.4, 1.10.2, 1.11.2**. Four of them are full, booted server directories with a generated
`world/` (1.8.9, 1.12.2, 1.16.5, 26.2); 1.8.9 and 1.12.2 also carry `client.jar` and an asset index.

Who references each (`grep -rl` over `crates xtask scripts docs`): 1.8.9 — v1-8's `live_*` gates
(targeting `:25566`, a container named `lodestone-mc189` that no script under `scripts/live-oracles/`
creates) and `tests/support/real_1_8_9_section_save.txt`, extracted from that world; 1.12.2 —
`scripts/live-oracles/legacy-1.12.sh` and v1-9's `live_canonical`; 1.13.2 — the flattening dump and
v1-9's particle ids; 1.16.5 — v1-14's `live_chunk` (`:25573`, again no script creates it); **1.14.4,
1.15.2, 1.17.1, 1.18.2, 1.19.4, 1.20.1, 1.20.6, 1.21.11 — referenced by nothing.** They were fetched
by `xtask version-table` for protocol numbers and no protocol test has learned they exist. That is
eight future families' outside oracle sitting on disk unreferenced, and twelve chances to route around
it; stage 0 below makes the census a committed file so the next agent reads it instead of re-fetching.

Runtime: the host has no `java` (`java -version` → "Unable to locate a Java Runtime"), and at
`16b72257` `container list` fails with *"Ensure container system service has been started with
`container system start`"*. Every oracle step in this plan therefore begins with `container system
start`; `docs/oracle-runtimes.md` is the authority. Only one legacy oracle script exists
(`legacy-1.12.sh`); v1-8 and v1-14's gates target containers with no script, which is evidence debt
this plan schedules (stage 0).

## The version seam, and the constraint every design must satisfy

Three things are load-bearing and must survive unchanged:

1. **`just check-seam`** — the shell compiles with no family. Any shared crate must therefore be
   *required* by nothing the shell links unless it is itself version-free and feature-free.
2. **Folder deletability** — `check-deletable <fam>` must keep reporting only manifest lines. A
   shared crate that a family depends on is fine (version → shared is the allowed direction); a
   family another family depends on is fatal.
3. **The registry shape** — `Family { protocols, make: fn(i32) }` already supports multi-protocol
   crates; nothing above the registry changes in this plan.

## Naming: what `vNNN` denotes, and the recommendation

**Superseded by the executed decision.** The owner chose neither the exact-protocol nor the
lowest-protocol-of-an-era scheme this section analyses: package/feature suffixes (`v1-8`, `v1-9`,
`v1-14`, `v26-2`) and the `crates/versions/<version>` folders they now live under (`1.8`, `1.9`,
`1.14`, `26.2`) are both named for each family's *era-start Minecraft version*, not a protocol
number at all — "no one really cares what the protocol number is from a glance." The rename
already landed; `docs/multi-protocol-seam.md`'s "What the `vNNN` suffix denotes" section describes
the current scheme. The analysis below is kept as the record of what was considered and why a
protocol-number-derived name was rejected.

`docs/protocol-crate-naming.md` already established that two rules are in use — `v1-8`/`v1-9` name the
protocol they implement, `v1-14`/`v26-2` name the *lowest protocol of a planned era* (1.16 = 735,
1.21.5 = 770) while implementing 754 and 776 — and recommended exact-protocol names. That
recommendation was written for a one-crate-per-version plan. Under this plan crates are eras, so
"the protocol" is a set, and the only rule stable under *adding later versions to an era* is:

**Name an era crate for the lowest protocol it implements.** Adding a newer version never changes the
name; only absorbing an *older* one does, which happens exactly once here (v26-2 taking 774, if the
owner chooses that grouping — see open decisions). Never derive a protocol from the folder; the
registry already reads `PROTOCOLS` from the crate, which is what makes the name cosmetic.

Under that rule the end state is: `v5` (1.7.10), `v1-8` (1.8.9), **`v110`** (1.9.4–1.12.2, absorbing
v1-9), `v404` (1.13.2), **`v498`** (1.14.4–1.16.5, absorbing v1-14), `v756` (1.17.1–1.18.2), `v762`
(1.19.4), `v766` (1.20.6), `v774` (1.21.11), **`v776`** (26.2, today's v26-2). Two renames of existing
crates, and both happen as a *merge* (v1-9 → v110, v1-14 → v498) rather than a bare rename, so the
cost is absorbed by the stage that merges them.

Rename cost for v26-2 → v776, measured: 83 of v26-2's test files name `V770Adapter`; the crate is a
required dependency of `crates/plugins/lodestone-server-brand`, a dev-dependency of
`lodestone-server`, an optional dependency of `lodestone-fuzz` and `lodestone-registry`, named in
`web/Cargo.toml`, `xtask` (`DEFAULT_PACKET_IDS_OUT`, `connectedness`), `CLAUDE.md`, `DESIGN.md`,
`HANDOFF.md` and ~15 docs. It is a mechanical `git mv` plus a grep-driven sweep, but it collides with
the most-edited crate in the tree (220 commits in 60 days). **Recommendation: defer the v26-2 rename to
the stage that first makes it multi-protocol (stage 6), and never do it incidentally.** The legacy
merges rename for free.

## Where sharing genuinely breaks

Each discontinuity, the protocol set it partitions, and which *module* carries it — so the era
layering respects the break instead of fighting it:

| discontinuity | partitions | lives in | shared how |
|---|---|---|---|
| 1.7→1.8: UUID login, compression, `map_chunk_bulk` | {5} vs {47+} | login choreography, `chunk.rs` | own era; 29% shape identity, nothing to inherit |
| 1.9: fixed-point → `f64` positions, offhand, attack cooldown, metadata type table reshaped, new entity ids | {47} vs {110+} | `entity.rs`, `metadata.rs`, entity registries | era boundary; 30 arms still identical across it |
| **1.13 Flattening**: `id:meta` → block states; item numeric ids → names | {5, 47, 110–340} vs {404+} | `chunk.rs`, `slot.rs`, block/item canonicalisation | pre: `lodestone-canonical` (blocks; items unbuilt). post: one generated `STATE_TO_CANONICAL` per protocol |
| 1.14: light leaves the chunk packet (`update_light`); 1.15: 1024-entry 3-D biome array | {404} vs {498+} | `chunk.rs` only | era boundary at 1.14; 1.15 is an in-era `chunk.rs` branch |
| 1.16: non-straddling long packing | {≤578} vs {754+} | `lodestone_world::LongArrayFraming` | already a shared parameter |
| **1.17 dynamic height**; 1.18 per-section paletted biomes | {≤754} vs {756+}; {756} vs {758+} | `ChunkShape` from the dimension registry, `chunk.rs` | v26-2 already derives shape from `registry_data`; v1-14 hardcodes 16 sections |
| **1.19 chat signing** | {≤758} vs {762+} | `adapter/chat.rs` (623 lines in v26-2), `lodestone-game`'s signature cache | shared from v26-2 downward to 762 |
| **1.20.2 configuration phase** | {≤762} vs {766+} | `adapter/connection.rs` (751), `packets/configuration.rs`, `packets/registry.rs` (822) | shared from v26-2 downward to 766; login state machine differs structurally below |
| **1.20.5 data components** | {≤762} vs {766+} | `adapter/inventory.rs` (3,265), `packets/metadata.rs` slot codec | shared from v26-2 downward to 766; NBT-item eras need the NBT→component lift (unbuilt) |

Two things the table makes explicit. First, **the expensive breaks are confined to three modules**
(`chunk.rs`, `slot.rs`/inventory, `metadata.rs`) plus the login state machine; the other ~60 packets
per era cross most boundaries unchanged. Second, **the modern discontinuities are already
implemented — in v26-2 only.** Chat signing, the configuration phase, registry sync and components are
each a self-contained v26-2 module today; 1.19.4, 1.20.6 and 1.21.11 need them *as they are*, with
`since`/`until` at the edges. That is the backport the owner asked about, and it is a hoist, not a
rewrite.

## The design

### Recommended: eras + `lodestone-protocol-common` + ranged packets + dispatch tables

Four pieces.

**1. `crates/lodestone-protocol-common` — the shared packet library (new, version-free by path).**
Depends on `lodestone-core`, `lodestone-macros`, `lodestone-model`, `lodestone-world`,
`lodestone-canonical`, `lodestone-data`; on no family. One module per packet
(`src/packets/update_health.rs`, `src/packets/player_info.rs`, …) so concurrent agents never share a
file. Each module holds:

- the packet struct(s), using `#[mc(since = N)]`/`#[mc(until = N)]` for in-range field deltas and
  **separate structs where a field's type changes** (`EntityTeleportFixedPoint` for 47,
  `EntityTeleport` for 110+ — an attribute cannot retype a field, and pretending otherwise is how a
  1.8 arm ends up dividing a double by 32);
- a **container-level protocol range**, `#[mc(protocols = "47..=754")]`, surfaced as
  `Packet::PROTOCOLS` (new macro feature; today the derive carries only `NAME`/`STATE`/`BOUND`);
- the lift (`fn lift(&self, session: &mut SessionState) -> Vec<Directive>`) and, for serverbound
  packets, the lower (`fn lower(action: &ClientAction, …) -> Option<Self>`), i.e. the arm body that
  is identical today, next to the struct it decodes;
- the shared adapter state those arms need, extracted from the three identical copies:
  `MovementSendState` + `select_move_packet`, `PendingTabComplete`, the entity-kind memo, the
  vehicle/passenger graph, `ChunkShape` + `FallbackTally` reporting.

Packet keys are **Mojang 26.2 names** (`minecraft:set_health`); `xtask gen-packet-ids --source
minecraft-data` gains an alias column so a legacy table also exports the canonical name. This is the
one namespace decision the minecraft-data/Mojang split forces, and 26.2's names are the ones the
canonical model already speaks.

**2. `crates/versions/<era>` — one crate per wire era.** Owns: the per-protocol *generated* tables
(`generated/packet_ids_<protocol>.rs`, entity/item/sound/particle registries, and for post-1.13
protocols a run-length `STATE_TO_CANONICAL` — v1-14's 17,112-entry table is **232 consecutive runs**,
so ~250 lines instead of 17k, checked against the flat form by the existing generate-or-assert gate);
the era-specific packets that no other era shares (`chunk.rs` framing, `metadata.rs` type table,
`slot.rs`, login choreography); the **dispatch table**; the `VersionAdapter` impl; its own tests and
captures. It re-exports the common packets it uses. `PROTOCOLS` lists every protocol in the era;
`adapter_for(protocol)` selects that protocol's id table at construction — the seam that already
exists.

**3. Data-driven dispatch with an enumerated ignore list.** `handle_play` stops being an if-chain and
becomes, per era, `static CLIENTBOUND: &[(&str, Handler)]` keyed by canonical packet name plus
`static IGNORED: &[(&str, &str)]` — name and *reason* (`"no canonical event; server-only debug"`).
`adapter_for(protocol)` builds an `id → Handler` array from that protocol's generated table and
**fails construction** if a handler's `Packet::PROTOCOLS` excludes the negotiated protocol, if a
handler names a packet the table lacks, or if a table id is neither handled nor listed as ignored.
The `_ =>` arm disappears. This is the anti-island guard: a packet the wire has and the family does
not translate is a construction-time error with a name, not a silent drop; `connectedness` remains
the reporting instrument, and its per-family denominators stop moving by accident.

**4. Shared test driver.** Extend `lodestone-testsupport` with the join-flow walker (handshake →
login → play, asserting on `Directive`s) and the capture-replay harness, parameterised by era
adapter and protocol. Fixtures stay per protocol and come from outside (captures, hand-decoded spec
bytes, jar dumps) — the driver is shared, the expected values are not.

**How a new version is added.** *In-era* (1.10.2, 1.11.2, 1.15.2, 1.18.2): generate its tables, add
the protocol to `PROTOCOLS`, widen or narrow the `since`/`until` bounds on the handful of packets that
changed (3, 0+, 6 and 10 packets respectively by the minecraft-data table), capture one join against
its oracle, commit the capture, and the construction-time check tells you which names are unbound.
*New era* (1.13.2, 1.19.4, …): scaffold the era crate with `xtask new-version` *without* `--from`
copying packets — the scaffold becomes tables + dispatch skeleton + `pub use` of every common packet
whose range covers the protocol, and `SHAPE_REVIEW.toml` lists only packets minecraft-data says
changed, which is the review a human actually has to do.

**Guards against the island hazards CLAUDE.md names.**

- *Defaulted trait + wrapper*: no packet handling goes through a trait with a default. Handlers are
  table entries; `VersionAdapter`'s defaulted census methods are unchanged and remain the seam's
  business, not the era's.
- *Inheritance by range*: a struct whose range is too wide silently serves a protocol it was never
  checked against. Two controls: the construction-time check rejects a handler outside its declared
  range (negative control: bind `SetHealth` at protocol 5 and require the error), and **every range
  widening must land with a capture from that protocol's oracle** decoding through it — a widened
  bound with no capture is the review comment to write.
- *Dead shared code after a family deletion*: `check-deletable` cannot see a common packet whose last
  consumer left. Add a `protocol-common-consumers` census (extend `xtask islands`, which already
  scans for zero-caller functions) that fails on a common packet no era re-exports.
- *Copy-forward through the scaffold*: `scaffold_new_version`'s `copy_tree_with_substitutions` is
  the mechanism that produced "a 1.12.2 client wearing 1.16 packet IDs". Stage 5 removes the
  packet-copy from it; until then the tool must refuse `--from` for a target outside the source's era.

### The alternatives, and how each fails

| option | buys | costs | fails how |
|---|---|---|---|
| **Shared primitives + per-version tables only** (status quo plus tables) | nothing new — primitives are already shared | — | leaves the 54–61 identical structs and 30–37 identical arms copied; measured, this is where the duplication *is* |
| **Diff-from-previous inheritance chain** (v1-9 depends on v1-8 and overrides) | matches the owner's mental model exactly | fatal under `check-isolation` (version → version); deleting v1-8 deletes 1.12.2; a chain of ten crates | the defaulted-method island: a version inherits an arm that decodes the *previous* wire, compiles, tests green against its own encoder, and mis-parses the first real packet — CLAUDE.md's wrapper hazard with a protocol number on it |
| **Declarative packet tables + one generic codec** (minecraft-data-style schema) | zero Rust per packet for the ~60 simple ones | a schema interpreter and a second source of truth | Mojang's `packets.json` has ids only, no shapes; minecraft-data has no 26.2 and no 1.7.10, and leaves chunk sections, metadata payloads and NBT opaque — precisely the three modules where versions differ. The generic codec covers the easy 70% and abandons you at the hard 30% |
| **Macro-generated families** | mechanical | a macro that hides the diff between versions | the diff is the review; hiding it reproduces the `new-version` incident with less to read |
| **Trait-with-default-impl layering** (`trait EraPackets { fn set_health(...) { default } }`) | idiomatic | one trait per era, each impl a wrapper | *is* the island generator CLAUDE.md records — a default that is right for one era compiles for all of them |
| **Recommended** (above) | one definition per packet per range; deltas as attributes; eras as crates; tables generated | a new shared crate; a container-range macro feature; the v26-2 hoist | a range widened without a capture — guarded by the construction-time check and the capture rule; and file contention on the common crate, mitigated by one-module-per-packet |

The recommended design is the inheritance chain re-expressed so that "the earliest version that has
it" becomes the `since` bound on a shared definition, which the isolation lint permits and the
construction-time check makes falsifiable.

## Stages

**Measured status, not a claim from when this plan was written.** Re-derive it rather than trusting
this block; the commands are given so you can. Stages 0-3 have landed:

| stage | evidence |
|---|---|
| 0 instruments | `cargo xtask protocol-dup`, `connectedness` both run and report |
| 1 macro + builder | `#[mc(protocols)]` and `lodestone_core::dispatch` are consumed, not shelved |
| 2 `lodestone-protocol-common` | ~1,500 lines; v1-8/v1-9/v1-14 `pub use` from it (15/21/16 references each) |
| 3 dispatch tables | v1-8/v1-9/v1-14 carry 59/62/54 `Handler::new` entries and 20/32/52 `IGNORED` ones |
| 4 first era crate | `lodestone_v1_9::PROTOCOLS` is `[110, 210, 316, 340]`, with a committed join capture per new protocol under `crates/versions/1.9/tests/captures/` |
| 7 second era crate | `lodestone_v1_14::PROTOCOLS` is `[498, 578, 754]`, with a committed join capture per new protocol under `crates/versions/1.14/tests/captures/` |
| 8 (first era) 1.13.2 | `lodestone_v1_13::PROTOCOLS` is `[404]`, with a join capture and **two neighbour captures** under `crates/versions/1.13/tests/captures/` |
| 8 (second era) 1.17.1/1.18.2 | `lodestone_v1_17::PROTOCOLS` is `[756, 758]`, with a committed join capture per protocol under `crates/versions/1.17/tests/captures/` |

**Stage 4's measured cost, which is what this plan asked it to produce.** Diffed against
`455a87d6`, excluding the three generated id tables (1,971 lines) and the three captured-byte
files (157 lines):

| bucket | lines added |
|---|---|
| era-crate source (`crates/versions/1.9/src`, excluding `generated/`) | 870 |
| shared-crate source (`lodestone-core`'s dispatch, `lodestone-protocol-common`, one v1-14 call site) | 94 |
| oracle script | 44 |
| tests (in-file plus `tests/`) | 931 |
| **hand-written total** | **1,939** |

So ~1,008 hand-written *source* lines bought three protocols, about 336 each, against the
~5.5k-per-version the copy-forward scheme charged. The number the plan named as its falsifier is
the marginal one: adding 1.10.2 to a crate that already serves 1.9.4 and 1.12.2 costs a generated
table, a capture, and roughly **twenty** hand-written lines (a protocol const, a `PROTOCOLS`
entry, an `IDS_*` static, an `ids_for` arm, a dispatch slot, a `minecraft_versions` entry, and a
`MEMBERS` row with its recorder and replay test) — well inside the ~500-line threshold, so the
`since`/`until` mechanism is carrying the delta.

Two corrections Stage 4 forces on the estimate above. First, the split between predicate and
second struct is not free-form: a field **appearing or disappearing** is a `since`/`until`
predicate on the shared definition, but a field **retype** must be a second struct, and four of
this era's eight deltas are retypes. Second, tests are roughly half the hand-written cost and are
*not* copies — the capture harness is shared across the three protocols, so a fourth would add
about a dozen test lines, not another 300.

See [`docs/protocol-1-9-era.md`](../protocol-1-9-era.md) for the era crate's own documentation.

**Stage 7's measured cost, for comparison.** Diffed against `d9a926b1`, excluding the
six generated tables (24,569 lines), the two captured-byte files (64 lines) and the four
committed jar dumps (239,381 lines):

| bucket | lines added |
|---|---|
| era-crate source (`crates/versions/1.14/src`, excluding `generated/`) | 1,524 |
| shared-crate source (one `lodestone-protocol-common` range widening) | 10 |
| oracle script and the committed world-upgrade transcript | 69 |
| tests (in-file plus `tests/`) and the captures' README | 1,364 |
| **hand-written total** | **2,967** |

So ~1,534 hand-written *source* lines bought two protocols, about 767 each — higher than
stage 4's 336 because this era needed three *kinds* of per-protocol data rather than one
(ids, block states, entity types) and because its chunk framing genuinely differs three
ways, not because the mechanism failed. The marginal figure is the one the plan named as
its falsifier, and it holds: every hand-written line naming 578 or 1.15.2 counts **69**,
against the ~500-line threshold. Two further corrections to the record. First, tests are
again about half the cost and again are not copies. Second, a per-version *generated*
data set is larger than stage 4 implied — 12k lines per protocol here, because a
post-Flattening era needs its own block-state table — but it is generated from that
version's own jar, so it costs review, not authorship.

See [`docs/protocol-1-14-era.md`](../protocol-1-14-era.md) for that era crate's own
documentation.

**Stage 8's first era, 1.13.2, and the two things it corrects.** Diffed against
`4fcc999d`, excluding the five generated tables (9,744 lines), the committed jar dump
(83,529) and the captured bytes (240):

| bucket | lines added |
|---|---|
| era-crate source (`crates/versions/1.13/src`, excluding `generated/`) | 5,531 |
| shared-crate source (four `lodestone-protocol-common` range widenings) plus registry and workspace wiring | 98 |
| tests (`tests/*.rs`) | 2,270 |
| oracle script and the two committed oracle transcripts | 226 |
| **hand-written total** | **8,125** |

A **singleton era has no marginal figure**, so the plan's ~500-line falsifier does
not apply to it; the comparable number is the whole-family one, and at 5,531
source lines it is below `codegen-ratio`'s reading for v1-14 (6,257) and roughly
double the ~2–2.5k this plan projected for 1.13.2. The gap is not the mechanism
failing: it is that a new era crate pays for a whole adapter, and the projection
was made before Stage 7 measured what a post-Flattening era actually costs.

Two corrections to the record this stage forces. First, **`minecraft-data` is
wrong about 1.13.2's entity list four separate ways** — 28 stale pre-1.13 object
rows mixed into a 95-entry unified registry, and three names that are not
identifiers at all (`iron_golem}`, `fireworks_rocket`, `commandblock_minecart`),
each rejected outright by vanilla's own `/summon`. Second, and more expensive:
**1.13 unified the entity registry but not the two wire id spaces.** At 404
`spawn_entity` still carries the pre-1.13 *object* numbering and only
`spawn_entity_living` uses the unified one, so an adapter that resolves an object
spawn through the unified table names a real, wrong entity for every object on
the wire. Both were caught by a wire oracle, not by a dataset.

The negative control also came out differently from Stage 7's, and the honest
answer is the weaker one: measured across two real neighbour captures, a misroute
across either era boundary **does** produce a plausible wrong event — 7 of 25
across the lower boundary and 7 of 50 across the upper. The guarantee this crate
can offer is the whole-stream one, not the per-packet one.

See [`docs/protocol-1-13-era.md`](../protocol-1-13-era.md) for that era crate's
own documentation.

**Stage 8's second era, 1.17.1 and 1.18.2, and the two things it settles.**
Diffed against `e90d4391`, excluding the four generated tables (22,310 lines),
the three committed jar dumps (230,669) and the captured bytes (96):

| bucket | lines added |
|---|---|
| era-crate source (`crates/versions/1.17/src`, excluding `generated/`) | 6,229 |
| shared-crate source (eleven `lodestone-protocol-common` range widenings) plus registry and workspace wiring | 99 |
| tests (`tests/*.rs`) plus the captures' README | 1,549 |
| oracle script | 45 |
| **hand-written total** | **7,922** |

The marginal figure — the one the plan named as its falsifier — is **131
hand-written lines naming 758 or 1.18.2**, of which 32 are code and the rest
the documentation that explains them; the function and struct bodies that
exist only for 758 come to 95 lines. Either reading is well inside the
~500-line threshold, on an era where the second version changes the chunk
packet's whole shape. That is the strongest confirmation the mechanism has
had: 1.9's ~20 and 1.14's 69 were both added on top of an unchanged chunk
framing.

The whole-era figure repeats Stage 8's first lesson rather than contradicting
it. At 6,229 source lines this era is *above* 1.13's 5,531 and 1.14's 6,257 is
its nearest neighbour — founding an era costs roughly a full family however
the crate is arranged, and the saving is entirely in the second and subsequent
version. Two era-specific reasons it came in high: the era needed a whole
second chunk decoder rather than a branch (1.18 removes the section mask and
the column biome array and folds the light payload in), and 1.17 split three
action-selected packets into fourteen, so the same events now come from
fourteen handlers instead of three.

Two corrections to the record this era forces. First, **a per-protocol data
set is not always per protocol**: the two jars' `--reports` `blocks.json` dumps
are byte-identical and their entity registries agree id for id, so this era
needs *one* of each where 1.14 needs three. Both claims are re-derived from the
committed dumps by tests rather than asserted, and `table_for` still routes
through the negotiated protocol. Second, and more expensive: **1.18's
`chunkData` buffer is longer than the sections inside it**, by exactly one zero
byte per section whose block palette is single-valued (measured at 23, 21 and
19 bytes of padding across three columns). The exact-length `ensure_empty` every
other protocol here can assert is therefore false at 758, and no round-trip
against our own encoder could have found it — only a capture could.

The negative control came out matching the 1.13 era's answer, not the 1.14
era's: across the 1.17.1 capture, four ids name a different packet at 758 —
one errors, two land on ignored ids, and one emits a real, well-formed, wrong
event (`declare_recipes` read as `entity_effect` becomes a levitation
`MobEffectApplied`). The guarantee this crate offers is the whole-stream one.

See [`docs/protocol-1-17-era.md`](../protocol-1-17-era.md) for that era crate's
own documentation.

**Stage 8's third era, 1.19.4, and the two things it settles.** Diffed
against `8ff165e2`, excluding the two generated tables (24,852 lines), the
committed jar dumps (252,350), the generated neighbour-name table (246) and
the captured bytes (221):

| bucket | lines added |
|---|---|
| era-crate source (`crates/versions/1.19/src`, excluding `generated/`) | 6,811 |
| shared-crate source (eight `lodestone-protocol-common` range widenings) plus registry and workspace wiring | 72 |
| tests (`tests/*.rs`) plus the captures' README | 2,035 |
| oracle script rows | 36 |
| **hand-written total** | **8,954** |

A **singleton era has no marginal figure**, so the plan's ~500-line falsifier
does not apply; the comparable number is the whole-family one. At 6,811 source
lines it is the largest era yet — above 1.17's 6,229 and 1.13's 5,531 — and the
projection for it here was ~2-2.5k. The gap is not the mechanism failing. It is
that "77% + chat signing hoisted from v26-2" assumed a hoist, and stage 6 has
not run: v26-2's chat module is 776's shape, which differs from 762's in three
ways that each silently mis-frame a stream (components are JSON strings not
network NBT, there is no server-global message index, there is no
acknowledgement checksum byte). So this era wrote its own chat rather than
sharing one, and the same is true of its `player_info` decoder, which 1.19.3
changed from an action ordinal to an action bitmask. **Every projection below
that says "hoisted from v26-2" should be read as conditional on stage 6 having
actually run.**

Two corrections to the record this era forces. First, **a shared definition can
be dangerous to widen in a direction the derive cannot detect.** The serverbound
`chat` packet still begins with the message string at 762, so a widened range
would encode an acceptable *prefix* and fail only at the server, with the
connection closing rather than a decoder erroring — the one case where the
`protocols` guard's loud failure is strictly better than the wire's own. Eight
other definitions widened; this one deliberately did not.

Second, **the login handshake can itself be the era boundary.** The 762 adapter
cannot join either neighbour at all, over one byte at the end of `login_start`:
758 reads a bare username and treats 762's presence byte as the next packet's
start, and 766 reads a *required* 16-byte profile UUID. That made the neighbour
captures impossible to record through the adapter, so they use a hand-written
login — and it is the strongest whole-stream boundary statement any era has
produced.

The negative control came out matching the 1.13 era's answer, not the 1.14
era's, and more strongly. Across the lower neighbour's 64 packets, 35 error, 19
go silent and **10 produce a real, well-formed, wrong gameplay event**. The
upper neighbour gives the sharpest single result: two of its three plausible
wrong events come from an id whose *name agrees on both sides* —
`spawn_entity` is id 1 at 762 and at 766 — so nothing about the routing is
wrong, only the shape and the entity registry, and a 1.20.6 minecart spawn
comes out as a spawner minecart at a plausible position with a plausible
velocity. The guarantee this crate offers is the whole-stream one.

See [`docs/protocol-1-19-era.md`](../protocol-1-19-era.md) for that era crate's
own documentation.

**Stage 8's last era, 1.7.10, and the three things it settles.** Diffed
against `1ead3edc`, excluding the generated tables (983 lines):

| bucket | lines added |
|---|---|
| era-crate source (`crates/versions/1.7/src`, excluding `generated/`) | 5,772 |
| shared-crate source (one `lodestone-protocol-common` range widening) plus registry and workspace wiring | 38 |
| tests (`tests/*.rs`) plus the captures and their README | 2,421 |
| oracle script row | 25 |
| **hand-written total** | **8,256** |

The projection for this era was ~3.5-4.5k and it came in at 5,772, so the
projection was low by about a third — but this is the **closest** any singleton
era has come, and it is the only one below 1.17's 6,229, 1.19's 6,811 and
1.20.6's 7,437. That is worth stating plainly because the reason is not the
mechanism: 1.7.10 has the *least* to inherit of any era on the ladder and still
cost less than three eras that had far more, which says the founding cost of a
singleton is dominated by writing an adapter at all rather than by how much of
the neighbour it can reuse. The one substantial thing it did inherit —
`lodestone-canonical`'s pre-Flattening `(id << 4) | meta` table, shared with the
1.8 and 1.9 eras — is exactly the kind of reuse this plan is about, and it is
also the single largest module this era did not have to write.

Three corrections to the record this era forces.

First, **the identity figure holds and the era-grouping threshold is nowhere
near being met.** Measured independently of this plan's own table — every
packet definition in both directions and all four states, named types inlined —
protocol 5 and protocol 47 agree on **37 of 112** shapes, 33%, against an
85% threshold. Eight of the 37 are the handshake and status packets that have
never changed in any version, so the real figure is lower still. This plan's
29% and that 33% are the same conclusion counted from opposite ends.

Second, **`decode(encode(x)) == x` is not merely weak here, it is actively
misleading, and so is a length check.** This era's defining hazard is a field
order inside a run of same-typed fields, and three of them landed in one
chain: the serverbound movement packets' `stance` sits after the feet rather
than between `x` and `y`; the clientbound teleport's middle `f64` is the eye
position, not the feet; and the teleport has to be confirmed by echoing a
matching `position_look` back. Every one of the three encodes to a body of
exactly the right length, round-trips perfectly, and replays a recorded join
without complaint. A protocol 5 server's response to all three is the same and
is completely silent: it holds the player and discards movement, with no error,
no disconnect and nothing in its log. The measured discriminator over a
320-block walk was 65-70 server position corrections versus 1, zero chunk
unloads versus 420, and — read a third way, out of the server's own saved
player data on logout — the player still at the spawn point having discarded
1,600 movement packets. **The lesson generalises past this era: a plan that
measures shape identity by comparing field *types* cannot see this class at
all, because the types agree.** Nothing but a live server found it, and the
guard that keeps it found reads the encoded body at byte offsets rather than
through the struct that produced it.

Third, **the shared-definition range guard earned its keep in the direction
nobody was worried about.** Founding this era widened exactly one definition,
`LoginSuccess`, from `47..=578` to `5..=578`. The mistake was the *missing*
widening, not a wrong one, and because the range is enforced at decode it
surfaced as a loud runtime refusal naming both the protocol and the declared
range — which is how it was found at all, since the era's login flow was
otherwise complete and compiled clean.

The negative control this era offers is unusual: with only 33% shape identity
there is no plausible-wrong-event arm to measure, because a 47-era adapter
pointed at a protocol 5 stream does not get far enough to produce one. The
whole-stream boundary statement is stronger than any per-packet one, and it is
the same statement the login handshake makes at 762.

See [`docs/protocol-1-7-era.md`](../protocol-1-7-era.md) for that era crate's
own documentation, including the one concept protocol 5 has that has no
canonical equivalent — its player list carries no profile UUID at all — and
what this crate does about it.

`cargo xtask connectedness` currently reports v1-7 50/65, v1-8 59/74, v1-9 62/80,
v1-13 51/86, v1-14 54/92, v1-17 65/103, v1-19 67/111, v1-20-6 61/122, v26-2 141/141
clientbound, with zero decoded-but-stranded in every family.

**v26-2 has not migrated and that is deliberate** — it is Stage 6, explicitly deferred and not a
blocker. It still dispatches through the if-arm chain with no `Handler::new` and no `IGNORED` list.
Since it decodes 141/141, the fallthrough strands nothing today; the cost is that a *future* packet
added to v26-2 has no construction-time check to catch it.

An earlier note in this plan said Stage 1 shipped "with no `crates/versions/*` family converted to
use either yet". That was true when written and is now stale — the check above is one grep.

### Measured cost, and where the estimate was wrong

Several eras have now landed, and the numbers separate two cases the plan treated as one.

| era | protocols | era-crate source | marginal cost per added version |
|---|---|---|---|
| 1.9 | 110, 210, 316, 340 | ~1,008 | ~20 lines |
| 1.14 | 498, 578, 754 | 1,524 | 69 lines |
| 1.13 | 404 (singleton) | 5,531 | n/a |
| 1.17 | 756, 758 | 6,229 | 131 lines |
| 1.19 | 762 (singleton) | 6,811 | n/a |
| 1.20.6 | 766 (singleton as shipped) | 7,437 | n/a |
| 1.7 | 5 (singleton) | 5,772 | n/a |

The 1.17 figure is the **strongest** reading the marginal claim has had, not the weakest: 1.9's
~20 and 1.14's 69 were both on top of chunk framing that did not change across their era, while
1.18 rewrote it — the chunk buffer gained a trailing zero byte per single-valued-palette section,
so the exact-length assertion every other protocol here relies on is simply false at 758. A
framing rewrite still cost 131 lines against a ~500-line threshold.

**The mechanism works, and the falsifier is passed** — adding a version to an era that already
exists costs 20-70 hand-written lines against a ~500-line threshold, versus ~5.5k for a
copy-forward. That is the claim this plan was built on and it holds.

**Founding a new era is a different cost, and the estimate was about half of it.** A singleton
era pays for a whole adapter and shares nothing: 1.13 came in at 5,531 source lines against a
projected ~2-2.5k. `codegen-ratio` puts it beside 1.14's 6,257, i.e. a new era costs roughly what
an established multi-protocol one does. So the payoff is entirely in the *second and subsequent*
version of an era, and a version that neighbours a break on both sides — as 1.13.2 does, at 72%
and 73% — carries close to full freight however the crate is arranged.

1.20.6 is the fourth reading of that founding cost and the largest yet: **7,437 hand-written
lines against a projected ~2-3k**, so the projection is out by a factor of roughly 2.5-3.5 here
as well. The trend across the four singletons and near-singletons is monotone — 5,531, 6,229,
6,811, 7,437 — and the reason is not accumulated sloppiness: each newer era carries more
mechanism than the last. 1.20.6 pays for a configuration phase no era below has, an item model
made of length-implied components that cannot be skipped past, and a dimension registry resolved
by index rather than carried inline. Founding an era should be budgeted at what an established
multi-protocol era costs, plus whatever the era's own breaks are, and the estimate in this plan
should not be used for a fifth one.

1.20.6's era width is worth stating separately, because the crate is a singleton *as shipped* and
not *as measured*. Its lower boundary is real (54% against 762, 80% against 765, both under the
85% threshold) but its upper boundary is not: 766 and 767 agree on 204 of 226 shapes, **90%**, so
Minecraft 1.21 and 1.21.1 belong to this era and are simply not implemented. That is the cheapest
remaining marginal-cost datapoint in the tree — an era already founded, a neighbour already
measured as inside it, and 22 differing shapes to write.

Read that as guidance about ordering, not about whether to continue: fold versions into existing
eras first, and treat each new era as its own decision.

Every stage leaves `just health` green; each names what proves it. Sizes are hand-written lines,
estimated from the measurements above. **Prerequisites for family #5 are marked ★.**

**Stage 0 — instruments and evidence debt (small, ~300 lines of xtask + scripts).** Land the four
duplication measurements as `cargo xtask protocol-dup` (file, struct, arm, fn; and the minecraft-data
adjacency table), so every later stage quotes a re-run. Commit the `.cache/mc` oracle census to
`docs/oracle-assets.md` naming which versions have a jar, a world, a client, and *which test uses
which* — the eight unreferenced jars go in a table with an empty consumer column, deliberately.
Write `scripts/live-oracles/legacy.sh <version>` generalising `legacy-1.12.sh` (v1-8's and v1-14's
gates currently target containers no script creates), with `container system start` as its first
line. Proof: the xtask prints the tables in this doc within noise; `legacy.sh 1.8.9` and `1.16.5`
bring up the servers v1-8's and v1-14's `#[ignore]`d gates already expect, and those gates pass.

**Stage 1 ★ — the macro and the dispatch builder (medium, ~600 lines + tests).** Add
`#[mc(protocols = "a..=b")]` → `Packet::PROTOCOLS`; add `lodestone_core::dispatch::{Table,
Handler, IGNORED}` with the construction-time checks; extend `gen-packet-ids` to emit the canonical
(Mojang) name alongside a minecraft-data name. Proof: derive tests for the range (a bound that
excludes the `Ctx` version must fail encode with a named error, not produce bytes); the three
negative controls for the builder, each observed failing; `check-seam` green (this touches nothing
the shell links differently).

**Stage 2 ★ — create `lodestone-protocol-common` and move what is already identical (large,
~2,500 lines moved, ~200 written).** Move the 54 structs identical v1-8/v1-9 and the 61 identical
v1-9/v1-14 (union ≈ 65 packet types), the 38 identical functions, and the shared adapter state, each
with its measured range (`47..=754` for most). v1-8, v1-9 and v1-14 switch to `pub use`. Proof: every
existing golden-byte and round-trip test in the three families passes unchanged (they are the
byte-identity guard); `check-deletable` for each family reports the same manifest-line count as
before; `connectedness` prints identical figures. This is the stage most exposed to concurrent
edits — do it one packet module at a time, each its own commit.

**Stage 3 ★ — dispatch tables in the legacy families (medium, per family ~400 lines rewritten,
parallelisable).** Convert each `handle_play` chain to the table + `IGNORED` list. The 15 / 18 / 38
currently-undecoded packets per family become named `IGNORED` entries with reasons — some of which
will read "v26-2 has this; backport", which is the honest backlog. Proof: `connectedness` unchanged;
the new per-family test asserts every clientbound id in that protocol's table is handled-or-ignored;
the `_ =>` arm is gone (grep).

**Stage 4 ★ — first in-era addition: 1.9.4/1.10.2/1.11.2 into v1-9's crate, renamed `v110`
(medium; this is family #5).** Fetch the three jars (`xtask version-table --fetch-missing`), generate
three id tables, add the three protocols to `PROTOCOLS`, split the handful of 1.9-boundary structs
(fixed-point → `f64`, offhand, metadata table) with `since`/`until`, capture one join per protocol
against `legacy.sh <version>`, commit the captures. **Sequencing note on evidence:** these are three of
the four versions with *no jar on disk today* and minecraft-data as their only shape source; their
gates carry less confidence than 1.13.2+'s until the captures exist, which is why the captures are
part of the stage, not a follow-up. Proof: the U2 negative control finally exercisable — an adapter
constructed for 110 must select 110's id for a packet whose id differs at 340; per-protocol replay
gates; 1.12.2's own gates unchanged. **This stage is the calibration for the payoff estimate** —
record its hand-written line count.

**Stage 5 — `new-version` stops copying (small, ~200 lines).** The scaffold emits tables, the
dispatch skeleton with every in-range common packet pre-registered, and a `SHAPE_REVIEW.toml` seeded
from the minecraft-data adjacency diff. Proof: scaffolding 1.13.2 produces a crate whose only
hand-written files are the era-specific three plus the adapter, and whose review list has 40 entries,
not 143.

**Stage 6 — the v26-2 hoist (large, incremental; ~4–6k lines moved over many commits; not a
prerequisite for family #5).** Move v26-2's era-stable packets and their lifts into common with
ranges: player_info, scoreboard, boss bar, particles, sound, advancements, entity events, then the
three modern modules (`chat.rs` → range `762..`, `connection.rs`/`configuration.rs`/`registry.rs` →
`766..`, `inventory.rs` components → `766..`). Rename v26-2 → v776 in the same window if the owner
takes the 1.21.11 grouping (open decision 2). Proof: v26-2's 43,915 test lines are the guard; per
module, `connectedness` for v26-2 must not move; a legacy family gains a backported arm only with a
capture from its own oracle (this is where the eight unreferenced jars finally get consumers).

**Stage 7 — merge v1-14 into an era crate with 1.14.4/1.15.2 (medium). Landed.** The crate
keeps its `1.14` folder name (decision 2 renamed the scheme to era-start Minecraft versions
before this stage ran, so there was no rename to do). Three things the estimate missed, all
found by doing it: the branch is *three* chunk differences, not one — the biome array moves
twice and the section long packing changes at 754; a post-Flattening era needs a
**per-protocol block-state table** and a per-protocol entity registry, not just id tables,
because the palettes hold 11,271/11,337/17,112 states and 102/103/108 entities; and
`minecraft-data` models 1.14.4's `map_chunk` with no biome field at all, so the captures were
load-bearing rather than confirmatory. Proof: 1.16.5's canonicalisation gate unchanged, new
captures for 498 and 578 from the jars already on disk, and a replay that lands the flat
preset's floor in canonical ids.

**Stage 8 — remaining eras, in the order the existing plan's value ranking gives** (1.13.2,
1.17.1/1.18.2 and 1.19.4 landed; then 1.20.6, 1.21.11, 1.7.10 last). Each is stage-5 scaffold + era-specific three
modules + captures. 1.7.10 is the outlier: 29% identity with 1.8.9, and no jar-derived source of
any kind, so captures are its authority — budget it last and alone. (The "no minecraft-data" this
paragraph originally claimed was wrong, and contradicted this plan's own shape table above, which
reads 1.7.10 out of that dataset: `vendor/minecraft-data/data/pc/1.7/` exists and carries 17 files.
It is still cross-check grade, never an authority.)

Hosting (a per-era `ServerProtocol`, v26-2's is 9,350 lines) is out of scope here; the existing
plan's H0–H4 stand, and the same hoist logic applies to `server_protocol.rs` when phase 2 opens.

## Payoff

Today a family costs ~5.1–6.2k hand-written source lines plus ~5–6.7k test lines, and the measured
copy fraction says 30–35% of the source and ~40% of the tests are near-copies of a neighbour — the
copy is not even the majority of the cost, which is why "just copy less" was never going to fix it.
What changes is that *in-era* versions stop being families at all.

Projected hand-written lines per remaining version, after stages 1–5 (calibrate against stage 4's
real count before quoting further):

| version | today (copy-forward) | after | basis |
|---|---|---|---|
| 1.10.2, 1.11.2 | ~5.5k + 6k tests each | **~0.2–0.4k** each + captures | 97% identical to 1.9.4 (0–3 packets) |
| 1.15.2, 1.18.2 | same | ~0.3–0.5k each | 96% / 94% (6 / 10 packets + a `chunk.rs` branch) |
| 1.9.4, 1.14.4, 1.17.1 | same | ~1.5–2.5k each | 69% / 73% / 79% — new era adjacent to an existing one; ~35–40 changed packets + one or two era modules |
| 1.13.2 | same | ~2–2.5k | 72%; first native-state era, own `chunk.rs`, command tree from `lodestone-command` |
| 1.19.4 | same | ~2–2.5k projected; **6,811 measured** | 77%; the projection assumed chat signing hoisted from v26-2, and stage 6 has not run, so this era wrote its own |
| 1.20.6, 1.21.11 | same | ~2–3k each (≈1.5k if stage 6 is done first) | 57% / 64%; config phase + components hoisted from v26-2 |
| 1.7.10 | same | ~3.5–4.5k projected; **5,772 measured** | 29%, captures the authority |
| **twelve versions** | **~66k src + ~70k tests** | **~20–25k src + ~15–20k tests** | |

Roughly a 3× reduction in hand-written source and 4× in tests, concentrated where the owner said it
would be: in-era versions become cheap (tables, a few attributes, a capture), and the modern eras stop
re-implementing what v26-2 already has. The number that would falsify this is stage 4's actual count:
if adding 1.10.2 to `v110` costs more than ~500 hand-written lines, the `since`/`until` mechanism is
not carrying the delta and the design needs the type-changing structs split earlier than planned.

## Open decisions for the owner

1. **Era grouping threshold.** This plan groups where adjacent targets agree on ≥85% of packet shapes
   (minecraft-data, types inlined), giving ten era crates for sixteen versions. The previous plan's
   map (twelve crates: `v110` with three protocols, `v498` with two, `v756` with two, v1-9 and v1-14
   separate) is superseded by this one; the two agree on which versions group and differ on merging
   v1-9 and v1-14 into their eras. Decide whether 1.13.2 joins `v498` (73% — below threshold, and it
   carries light-in-chunk) — this plan says no.
2. **Does 26.2's crate absorb 1.21.11?** 1.21.11↔26.2 is *unmeasured* (no minecraft-data for 26.2).
   The measurement that settles it: run Mojang's `--reports` on `.cache/mc/1.21.11/server.jar` under
   `container` and diff `packets.json` ids against 26.2's, then diff the decompiled packet classes
   for the shared ids. Below 85% it is its own `v774` crate consuming common; above, v26-2 becomes
   `v774` (lowest protocol) — the one rename this plan cannot avoid deciding.
3. **Whether `lodestone-protocol-common` is one crate or two** (`-common` for `47..=754` legacy shapes
   and `-modern` for the v26-2 hoist). One crate is simpler; two keep the v26-2 hoist off the legacy
   files' commit history. Recommendation: one, one-module-per-packet.
4. **Item canonicalisation** (pre-1.13 numeric ids; pre-1.20.5 NBT → components) is unbuilt in both
   regimes and blocks inventories for every legacy era. It is orthogonal to this dedup and the
   previous plan's U5 still describes it; decide whether it lands before or after stage 4.
5. **This plan does not edit `docs/plans/multi-version-protocol.md`** (read-only task). Its family
   map, U6/U8/U9 scoping and "~900 irreducible lines" figure are superseded here; its
   canonicalisation regimes, oracle strategy and H0–H4 hosting units stand. Someone with write access
   should mark that doc's status lines in the commit that lands stage 0.

## Configuration

None new. Era selection stays a `lodestone-registry` feature per crate; `LODESTONE_V<N>_PORT`
overrides remain per gate until `legacy.sh` standardises them.

## Dependencies

`lodestone-core`, `lodestone-macros` (the `since`/`until` machinery and the new container range),
`lodestone-model`, `lodestone-world`, `lodestone-canonical`, `lodestone-data`, `lodestone-registry`,
`lodestone-testsupport`; `xtask` (`gen-packet-ids`, `connectedness`, `check-isolation`,
`check-deletable`, `islands`, `new-version`); Apple `container` for every oracle; `.cache/mc/<ver>`
jars (12 of 16 present) and `vendor/minecraft-data` (cross-check only).

## Decisions taken

The four open decisions above are now settled. Recorded here so a later stage does not
re-litigate them.

**1. Era grouping threshold: ≥85%, as proposed.** Ten era crates for sixteen versions. 1.13.2
keeps its own crate — 73% is below threshold and it carries light-in-chunk, so folding it into
`v498`'s era would put a real discontinuity inside one crate.

**2. Crate naming: the lowest *Minecraft version* an era serves, not its protocol number.** The
owner's reasoning: nobody reads a protocol number at a glance, and the crate name's job is to say
which game an era covers. Directory and package names use an underscore, since a Rust package name
cannot carry a dot:

| today | becomes | serves |
|---|---|---|
| `v1-8` | `v1_8` | 1.8.9 |
| `v1-9` | `v1_9` | 1.9.4 · 1.10.2 · 1.11.2 · 1.12.2 |
| — | `v1_13` | 1.13.2 |
| `v1-14` | `v1_14` | 1.14.4 · 1.15.2 · 1.16.5 |
| — | `v1_17` | 1.17.1 · 1.18.2 |
| — | `v1_19` | 1.19.4 |
| — | `v1_20` | 1.20.6 |
| `v26-2` | `v1_21` or `v26_2` | 1.21.11 and/or 26.2 — see decision 2b |
| — | `v1_7` | 1.7.10 |

This replaces the plan's earlier "lowest protocol served" suggestion (`v110`, `v498`, `v774`)
everywhere it appears above; read those names as the Minecraft-version equivalents. It also
resolves the inconsistency that motivated the question: `v1-14` never spoke protocol 735, and no
scheme keyed on a protocol number survives a folder name that is not the protocol.

**2b. Settled: `v26-2` becomes `26.2`.** The owner's call. It is what that crate serves today, and
naming it for a version it does not yet implement would be a claim rather than a description. If
1.21.11 later proves to share ≥85% of its packet shapes and joins the same era crate, the rename to
`1.21` at that point is cheaper than carrying a speculative name until then — and the measurement
that settles the grouping (Mojang's reports on `.cache/mc/1.21.11/server.jar`, `packets.json` ids
diffed against 26.2's) remains worth taking before stage 6 regardless, because it decides whether a
crate is added or extended.

The full mapping, now fully decided. Directory names carry the dot; package names cannot, so they
use a hyphen:

| today | directory | package |
|---|---|---|
| `crates/versions/1.8` | `crates/versions/1.8` | `lodestone-v1-8` |
| `crates/versions/1.9` | `crates/versions/1.9` | `lodestone-v1-9` |
| `crates/versions/1.14` | `crates/versions/1.14` | `lodestone-v1-14` |
| `crates/versions/26.2` | `crates/versions/26.2` | `lodestone-v26-2` |

**Execution note.** This is one atomic change, not four. It touches ~172 files that name the old
path, including **91 hardcoded occurrences in `xtask/src/lib.rs`**, the workspace glob
`crates/versions/*`, the isolation lint that treats that directory as version-crate space, and
`check-deletable`/`new-version`/`conformance`. It must land when no sweep is mid-flight: measured
mid-session, the collision set with running agents was 19 files across `lodestone-game`,
`lodestone-shell/tests` and `lodestone-entity` alone.

*(Superseded: this section previously recorded 2b as open pending measurement.)*
 Run Mojang's `--reports` on `.cache/mc/1.21.11/server.jar` under `container`, diff
`packets.json` ids against 26.2's, then diff the decompiled packet classes for the shared ids.
Below 85% they are two crates (`v1_21` and `v26_2`); at or above, one crate named `v1_21`. Take
this measurement before stage 6, since it decides whether that stage carries a rename.

**3. One `lodestone-protocol-common` crate, not a legacy/modern split.** A split would need a
boundary drawn at a version, and the whole point of a container-level protocol range is that the
boundary is per packet rather than per crate. If one crate later proves unwieldy, splitting it is
mechanical; merging two is not.

**4. Item canonicalisation happens *after* stage 4.** It is unbuilt under both the old and new
regimes, so it is not a regression either way, and stage 4 is the calibration measurement for the
whole payoff estimate — putting unrelated new work in front of it would delay the one number that
can falsify this plan.
