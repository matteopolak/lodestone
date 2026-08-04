# Multi-version protocol: the dispatch plan for epic #343

## What it is

The implementation plan for epic #343 (join servers from 1.7.10 through 26.2, children
#344–#358): family-per-wire-era with range extension inside an era, a shared canonicalisation
crate every legacy family maps through, and a capture-once oracle strategy. Written 2026-08-04
from a verified survey of the tree; every "X exists / X is missing" claim below was re-checked
that day, not copied from an older document.

## Verified ground truth, 2026-08-04

Re-verified per CLAUDE.md rule 2 — several of the epic's own premises were already stale.

**What exists.** Four family crates under `crates/protocol/`, all implementing
`VersionAdapter` (trait at `crates/lodestone-model/src/adapter.rs:697`), each with a
one-liner `supports`: `protocol == PROTOCOL`.

| crate | protocol | version | lines | canonicalises to 26.2 state? |
|---|---|---|---|---|
| v47 | 47 | 1.8.9 | 3,856 | **No — stores raw `(id << 4) \| meta` as the palette entry** |
| v340 | 340 | 1.12.2 | 14,079 | **Yes** — `flattening.rs` + `canonical.rs` |
| v735 | 754 | 1.16.5 | 4,237 | **No — leaves 1.16.5-space state ids** |
| v770 | 776 | 26.2 | 15,626 | native (it *is* the canonical space) |

Connectedness, measured this session (`cargo xtask connectedness`, supersedes the figures in
the epic and in `docs/roadmap/protocol.md`): v47 21/74 clientbound decoded, v340 22/80,
v735 17/92, v770 113/141 clientbound and 60/69 serverbound decoded (15/69 connected, 45
decode-to-`Ignored`). The epic's "serverbound decode is 0/69" line, repeated in every child
issue, is stale; `docs/roadmap/protocol.md` retracts its own "completely zero" at lines
231–238.

**Already implemented, issues still open** (`git log --grep`, all sixteen issues are OPEN):

- #343 groundwork: `faeb692` — the derived sixteen-row version table,
  `crates/lodestone-registry/src/generated/version_table.rs` (protocol + DataVersion +
  provenance per version; see `docs/version-table.md`). `d0cd8d6` — the pre-Flattening
  id:meta table from the 1.13.2 jar's own DataFixerUpper.
- #345 (1.8.9): `53b906a` four new v47 decode arms; `0a3e00f` outbound death-screen/spectate.
- #349 (1.12.2): `35d4401` decode arms; `714209b` `block_change`/`multi_block_change` decoded
  *into canonical states* via the flattening bridge.
- #353 (1.16.5): partial outbound via `0a3e00f`; the issue's "v735 may be 1.16.1" caveat is
  resolved — `supports` says 754, which is 1.16.5.

**Assets on disk.** `.cache/mc/` holds `server.jar` for 12 of the 15 target versions —
missing only **1.7.10, 1.9.4, 1.10.2, 1.11.2**. 1.8.9 / 1.12.2 / 1.16.5 are full booted
installs with worlds. Only 26.2 has decompiled source and `generated/` reports.
`vendor/minecraft-data` is vendored (1.8 → 1.21.x; nothing for 1.7.10 or 26.x).
`scripts/live-oracles/legacy-1.12.sh` exists and runs vanilla 1.12.2 under Apple `container`
(game :25568 / RCON :25569, ports disjoint from the 26.2 oracles) — but per the roadmap it
has **never been run for the v340 decode work**; that is evidence debt, unit U0 below.

**Server side.** Only v770 implements `ServerProtocol`; `lodestone-registry` keeps `FAMILIES`
and `SERVER_FAMILIES` as separate tables. The server's ECS decision (`docs/server-ecs.md`,
`f0d22a1`) is decided but unimplemented. This plan scopes **hosting legacy clients (the lossy
outbound direction) out to a phase 2** — every unit below is the inbound direction (our
client joins a legacy server), which is where the epic's own value ranking points.

## The decision: family-per-wire-era, range extension inside an era

**Not fifteen new crates.** The unit of work is a *family crate per wire era*, and inside an
era, additional versions are per-protocol generated tables plus branch points — a change to
that family only, invisible to the registry.

Evidence, not preference:

1. `docs/roadmap/protocol.md` (§#306) measured the irreducible cost of a new family at
   **~900 hand-written lines** concentrated in `adapter.rs` and `chunk.rs`, and recorded that
   the `xtask new-version` cloning experiment produced "a 1.12.2 client wearing 1.16 packet
   IDs". Fifteen clones would be ~13k lines of near-duplicate wire code with no shared
   packet-definition layer to keep them honest.
2. Adjacent-version deltas inside an era are small and *table-shaped* (packet id renumbering,
   a handful of shape changes), which is exactly what the generated `packet_ids.rs` per
   version already expresses — `xtask gen-packet-ids --source minecraft-data` parses any
   version's `protocol.json` today (`xtask/src/lib.rs:251–296`).
3. Widening a family costs the registry nothing: `adapter_for_protocol` delegates to each
   family's `supports`, and v47 already answers for two release names under one protocol.
   Adding a whole new family costs one optional dep line, one feature line, one `FAMILIES`
   entry (`crates/lodestone-registry/src/lib.rs:1–40`).
4. The era boundaries below are the places the epic itself ranks as the expensive
   discontinuities (Flattening, light split, dynamic height, 3-D biomes, chat signing,
   configuration phase, components). Crossing one inside a single crate is where the
   "1.12.2 client wearing 1.16 packet IDs" failure mode lives; staying inside one is cheap.

**The resulting family map** (protocol numbers from the derived table,
`crates/lodestone-registry/src/generated/version_table.rs` — never hand-derived):

| family | protocols | versions | issues | status |
|---|---|---|---|---|
| v5 | 5 | 1.7.10 | #344 | new, **last** — no minecraft-data, no cached jar, pre-compression wire |
| v47 | 47 | 1.8.9 | #345 | exists; needs canonicalisation retrofit (U3) |
| v110 | 110, 210, 316 | 1.9.4, 1.10.2, 1.11.2 | #346–#348 | new, one crate, three protocol tables |
| v340 | 340 | 1.12.2 | #349 | exists; the canonical-bridge donor |
| v404 | 404 | 1.13.2 | #350 | new — the Flattening-boundary anchor |
| v498 | 498, 578 | 1.14.4, 1.15.2 | #351–#352 | new, one crate (1.15 chunk-biome branch) |
| v735 | 754 | 1.16.5 | #353 | exists; needs canonicalisation retrofit (U4); rename to v754 optional (U12) |
| v756 | 756, 758 | 1.17.1, 1.18.2 | #354–#355 | new, one crate (1.18 section-biome branch — the riskiest grouping, split if the chunk paths stop sharing) |
| v762 | 762 | 1.19.4 | #356 | new — chat-signing state machine |
| v766 | 766 | 1.20.6 | #357 | new — configuration phase + item components |
| v774 | 774 | 1.21.11 | #358 | new — nearest neighbour to v770; still its own crate (v770 is the canonical space *and* the only `ServerProtocol`; keeping it single-protocol keeps the hosting seam simple) |
| v770 | 776 | 26.2 | — | done, canonical |

Seven new crates instead of eleven; four multi-protocol groupings, each inside one era.

**The seam change this requires (U2):** today no adapter stores the negotiated protocol —
`supports` is the only place the number appears. A multi-protocol family must be
*constructed with* the negotiated protocol and select its per-protocol `packet_ids` table at
runtime. That is a signature change on the family-construction path in
`crates/lodestone-model/src/adapter.rs` and the `FAMILIES` entries in
`crates/lodestone-registry/src/lib.rs`, mechanical for the four existing families. It does
**not** touch `net.rs`/`app.rs` (the shell already passes the negotiated protocol to
`adapter_for_protocol` — the seam's whole design, `docs/singleplayer.md:38`). Do U2 before
any multi-protocol family exists; retrofitting it after v110 is built single-protocol is the
expensive order.

## Canonicalisation: the layer that is the actual work

Epic #343 already decided the architecture: one canonical internal version (26.2), a
translation layer per protocol. The survey's largest finding is that **two of the three
existing legacy families skip the translation**: v47 parks raw `(id << 4) | meta` in the
palette and v735 parks 1.16.5-space state ids — both feed the mesher and collision, which
consume 26.2 ids, so both families "work" while rendering and colliding wrongly. Any plan
that adds families before fixing this replicates the defect seven more times.

**What exists (v340, the donor):** `flattening.rs` — 142-line API over a 9,076-line
generated table: 4,095 `id:meta` slots, 1,663 resolved, 2,400 no-entry, 32
`RequiresAdditionalContext`, 1 out-of-bounds; provenance is a reflective dump of the **real
1.13.2 jar's own DataFixerUpper**, regenerated via `LODESTONE_REGEN=1` against
`tests/support/flattening_1_13_2_jvm.txt` (`docs/protocol-340-flattening-table.md`). Plus
`canonical.rs` (557 lines): 1.13-name → 26.2-state bridge with the rename pass
(`mob_spawner`→`spawner`, `grass`→`short_grass`, …), `waterlogged` defaulting, and an
`Unmapped` drift-guard variant (`docs/protocol-340-canonical-bridge.md`). Item-side
flattening (DFU class `aah`, 302 entries) is recorded but **not built**.

**Generalisation, two regimes with the Flattening as the boundary:**

- **Pre-1.13 families (v5, v47, v110, v340):** all speak `id:meta`. The dumped table is the
  1.13.2 DataFixer's, which upgrades *1.12.2-space* ids; older versions' id space is a strict
  subset (ids were only added), so the same table serves all four — per-version difference is
  which slots are populated, which the existing `NoTableEntry` outcome already expresses.
  This forces a doctrine call: the roadmap records v47 was *deliberately* denied v340's table
  to preserve per-crate deletability. **Decision: extract `flattening` + `canonical` into a
  shared crate, U1 below.** Deletability applies to *families*; shared game data already has
  a precedent crate (`lodestone-data`, the #361 extraction), and the alternative is four
  copies of a 9k-line generated table drifting independently. Deleting a family remains
  folder + dep line + feature line.
- **Post-1.13 families (v404, v498, v735, v756, v762, v766, v774):** each speaks its own
  version's block-state id space. The mapping is per-version: dump each version's
  `blocks.json` by running **its own jar's** data generator (the jars are already in
  `.cache/mc/`; the generator ships in the server jar from 1.13 onward — verify per jar as
  part of each unit, do not assume the invocation is uniform), then resolve
  name+properties → 26.2 state id through a **DFU-walk oracle against the 26.2 jar**, keyed
  by the source `data_version` from the version table. The 26.2 jar contains every fixer
  from every older DataVersion — that is DataFixerUpper's contract — so this replaces
  v340's hand-written rename pass with the same outside-our-code provenance the flattening
  table already has. New program in `oracle-java/` (pattern:
  `crates/lodestone-data/tests/{collision_shapes,hardness}.rs` generate-or-assert +
  `LODESTONE_REGEN=1`). minecraft-data is a cross-check only, per every child issue; the
  v340 cross-check measured 91.8% agreement with all 137 disagreements resolving in the
  jar's favour.

**The 1.12.2→1.13.2 boundary, costed explicitly:** for inbound blocks it is *already mostly
paid* — `d0cd8d6`/`714209b` built and wired the table. Remaining boundary cost: (a) the 32
context-dependent slots (need TileEntity/neighbour context; currently resolve-or-air);
(b) **item flattening, entirely unbuilt** — 302 `aah` entries need the same reflective dump
before any pre-1.13 family can decode inventories into canonical items; (c) the whole
outbound direction (26.2 → id:meta is lossy; phase 2). New cost at this boundary for v404
itself is *low* — 1.13.2 is the first version whose block model matches ours natively; its
chunk format and command tree are ordinary era work, not Flattening work.

**Items pre-1.20.5 (all families except v774):** NBT → data components, both directions
lossy per the epic. Inbound scope: map the NBT the client actually renders (display name,
lore, enchantments, damage) and preserve-the-rest; full fidelity is not required to join.

## Data sources, per version

Authority order per child issues: real oracle jar (strongest) → jar's generated reports →
minecraft-data (cross-check only, never authority). Protocol/DataVersion numbers: always the
derived table, never hand-written (epic first task, satisfied by `faeb692`).

| versions | packet shapes | ids/registries/states | jar on disk? |
|---|---|---|---|
| 1.7.10 | **captures only** — no minecraft-data, nothing else exists | captures + the jar itself | **no — fetch via manifest** (`xtask version-table --fetch-missing` machinery) |
| 1.8.9–1.12.2 | minecraft-data `protocol.json`, cross-checked by live capture | 1.13.2 DFU flattening table (blocks); item dump TBD (U5) | 1.8.9, 1.12.2 yes; **1.9.4/1.10.2/1.11.2 no — fetch** |
| 1.13.2–1.20.4 era | minecraft-data, cross-checked by live capture (no packet report in these jars) | own jar's `blocks.json`/`registries.json` + 26.2 DFU walk | yes |
| 1.20.6, 1.21.11 | own jar's generated packet report where present (report added in the 1.20.5 era — verify per jar), else minecraft-data + capture | own jar's reports + 26.2 DFU walk | yes |

## Oracle strategy: three JDK images, one live oracle at a time, captures committed

The "15 versions × a container each" fear dissolves on inspection: the oracle image is a
stock JDK; the version lives in the bind-mounted jar (`legacy-1.12.sh` already works this
way — `eclipse-temurin:8-jdk` + `.cache/mc/1.12.2`). So:

- **Three base images total**: temurin 8 (≤1.16.5), temurin 17 (1.17.1–1.20.x), temurin 21
  (1.21.11) — exact split verified per jar's `version.json` `java_version` when each script
  is written, not assumed.
- **One parameterized script**, `scripts/live-oracles/legacy.sh <version>`, generalizing
  `legacy-1.12.sh`: per-version port pairs carved out of a documented range disjoint from
  :25565/:25570-1/:25580-1, `--memory 3g`, the three traps from `docs/oracle-runtimes.md`
  (no `127.0.0.1` publishes; `--platform linux/arm64` on pulls; memory flag mandatory).
- **Concurrency budget**: ≈1.1–1.3 GB resident per running oracle on Apple `container`, 16 GB
  box, ~10 agents live — **run one legacy oracle at a time, stop it when the gate passes**.
  Oracles are not repo state; scripts recreate them.
- **Capture once, commit, replay forever.** Per version, one scripted session (login → join →
  chunk load → RCON-driven `setblock`/`fill`/`summon` at *known coordinates* → disconnect)
  recorded to `crates/protocol/<fam>/tests/support/<ver>_join_capture.bin` and committed.
  The always-run test decodes the committed capture and asserts against the RCON-known
  values — expected values originate outside our code, satisfying the evidence standard
  without a running server. Live `#[ignore]`d gates re-run the capture script itself and are
  needed only for serverbound effects (dig a block, observe the update come back), which a
  replay structurally cannot exercise.

## Units of work

Each unit: scope, files owned (no two dispatchable-in-parallel units share a file), the
consumer that proves it is not an island, the gate with its negative control, and what would
make the gate vacuous. Choke-point note: **no unit below touches `app.rs`, `sim.rs`,
`net.rs`, `gpu.rs`, or `lodestone-server/src/lib.rs`** — the registry seam is the whole
reason. Units adding a `FAMILIES`/feature line touch `crates/lodestone-registry/src/lib.rs`
and its `Cargo.toml` (2 lines each); those two files are the one collision surface, so the
orchestrator serializes just those edits across units.

**U0 — pay the standing evidence debt (dispatch first, smallest unit).**
Run `scripts/live-oracles/legacy-1.12.sh`; against it, verify `multi_block_change`'s
`horizontalPos` nibble order, which is currently sourced from external wire docs only —
flagged at `crates/protocol/v340/src/adapter.rs:687–703` and
`tests/block_updates.rs:21–26`. Method: RCON `fill` an asymmetric pattern (different extents
in x and z) inside one chunk in one tick, capture the packet, assert decoded relative coords
match the RCON coordinates. **Negative control:** decode the same capture with the nibbles
swapped and require the assertion to fail — if it doesn't, the pattern was symmetric and the
control is vacuous; use extents like 3×1. Owns: `crates/protocol/v340/tests/block_updates.rs`
(new live test), the captured fixture. Consumer: the existing decode arm. Also promote the
capture to the committed-fixture pattern above.

**U1 — extract the canonical bridge into `crates/lodestone-canonical`.**
Move `flattening.rs` + generated table + `canonical.rs` out of v340 into a new shared crate
(or into `lodestone-data` if the crate-count objection wins — decide at dispatch; the #361
extraction is the precedent either way). v340 re-exports through it. Owns: the new crate,
`crates/protocol/v340/src/{flattening,canonical}.rs` (deletion/forwarding), v340
`Cargo.toml`; workspace `Cargo.toml` member line (orchestrator-brokered). Consumer: v340's
existing decode arms — unchanged call sites, new import path. Gate: v340's `flattening.rs`
and `live_canonical.rs` suites green, **plus** `cargo test --workspace` for the doctest trap
(CLAUDE.md: after any module move, grep the moved code for the old crate path — `check`
never sees doctests). Negative control: perturb one table entry, `LODESTONE_REGEN`
drift-guard must fail. Vacuous if: the drift test compares the file to itself rather than to
the JVM dump.

**U2 — multi-protocol seam.** Adapter construction takes the negotiated protocol;
`supports` may answer a set; per-protocol `packet_ids` module selection inside a family.
Owns: `crates/lodestone-model/src/adapter.rs` (trait + docs), the four families'
constructor impls, `crates/lodestone-registry/src/lib.rs` `FAMILIES` entries. Consumer:
`adapter_for_protocol` — already passes the protocol; nothing above the registry changes.
Gate: `just check-seam` (the shell must still compile with **no** family — this is the unit
most able to break the seam) + existing per-family suites. Negative control:
`supports(protocol+1)` must be false for a single-protocol family; a family constructed for
protocol A must select A's table when B is also in its set (assert on a packet id that
differs between A and B — if none differs, the grouping was free and the control needs a
different pair). Blocks: U6, U8, U9. **Do this before any multi-protocol family.**

**U3 — v47 canonicalisation retrofit.** Replace raw `(id << 4) | meta` palette entries with
canonical 26.2 state ids via U1's crate. Owns: `crates/protocol/v47/src/` (chunk + block
paths), v47 `Cargo.toml`. Consumer: the mesher/collision, which already consume the palette —
this unit changes what flows through an existing pipe, so it cannot be an island. Gate:
1.8.9 oracle (U-oracle script), RCON-place a block whose canonical id provably differs from
its packed legacy encoding, assert the decoded palette holds the 26.2 id;
committed-capture replay as the always-run form. **Negative control: assert the raw packed
value fails the same assertion** — and pick the block so raw ≠ canonical (verify the
inequality against `lodestone-data` first; a coincidental equality makes the control
vacuous). Blocked by: U1.

**U4 — v735 canonicalisation retrofit.** Same shape as U3, different mechanism: 1.16.5
state ids → 26.2 via the DFU-walk table (first consumer of the post-1.13 mapping oracle).
Owns: `crates/protocol/v735/src/` chunk/block paths, the 1.16.5 mapping table + its
`oracle-java/` dump program. Gate/control: as U3, against the 1.16.5 install already in
`.cache/mc/`. Establishes the pattern U6–U11 reuse. Blocked by: nothing (can run parallel
to U1/U3 — different id regime, different files).

**U5 — item canonicalisation, both regimes.** Pre-1.13 item flattening (the unbuilt 302-entry
`aah` dump, same reflective pattern) + legacy-NBT → component mapping for the render-relevant
subset. Owns: the shared canonical crate's item module, its oracle program, its dump fixture.
Consumer: each family's inventory decode arms (wiring lands with each family's unit; this
unit ships the table + API and **one** consumer — v340's inventory path — so it is not an
island on day one). Gate: v340 `live_interaction`/`inventory` asserting a chest item placed
by RCON with known id:damage decodes to the canonical item + components. Negative control:
an id:damage pair absent from the table must decode to the explicit `Unmapped` variant, not
to air/default — assert the variant, and prove the detector fires by feeding a known-bad pair.
Blocked by: U1.

**U6 — family v110 (1.9.4, 1.10.2, 1.11.2), issues #346–#348.** First new crate; first
consumer of U2's multi-protocol machinery; three generated `packet_ids` tables; pre-1.13
canonicalisation from day one via U1 (never the v47 raw-palette shape). Scope: the roadmap's
~900-line irreducible core (join flow, chunk, entity, block updates, movement) × the era's
specifics (offhand slot, attack cooldown as data, reshaped entity metadata — metadata index
tables per version from minecraft-data cross-checked by capture; **never hand-count an
index**, per CLAUDE.md run an oracle dump when in doubt). Owns: `crates/protocol/v110/`
entirely + registry 2-liner (brokered). Jars must be fetched first (three missing).
Gate: per-version committed-capture replay + one live join gate per protocol against the
parameterized oracle; negative control: the wrong-protocol handshake must be refused by
`supports`. Vacuous if: captures are generated by our own encoder (`decode(encode(x))`) —
they must come from the real server. Blocked by: U1, U2, oracle script, jar fetch.

**U7 — family v404 (1.13.2), issue #350.** The boundary anchor: first native-block-model
family, new chunk format, command tree (reuse `lodestone-command`, the #118 substrate).
State mapping via the first *small* DFU walk (1631 → 4903). Owns: `crates/protocol/v404/` +
registry 2-liner. Gate/control: as U6, jar already on disk. Blocked by: U4's oracle pattern
(not U2 — single protocol).

**U8 — family v498 (1.14.4, 1.15.2), issues #351–#352.** Light out of the chunk packet
(1.14), biome array into it (1.15) — the intra-family branch is confined to `chunk.rs`.
Owns: `crates/protocol/v498/` + registry 2-liner. Blocked by: U2, U7 (pattern).

**U9 — family v756 (1.17.1, 1.18.2), issues #354–#355.** Dynamic world height + 1.18
section-scoped paletted biomes. **Check what the chunk store and mesher assume about section
count before writing wire code** — the issue's own warning; if the store hardcodes 16
sections this unit gains a prerequisite outside protocol land and must say so rather than
absorb it. The grouping most likely to split into two crates; the tell is `chunk.rs` sharing
under 50% between the two protocols. Owns: `crates/protocol/v756/` + registry 2-liner.
Blocked by: U2, U8.

**U10 — family v762 (1.19.4), issue #356.** Chat signing: scope is *joining* — decode the
session/signature packets, send unsigned chat where the oracle permits
(`enforce-secure-profile=false` on our own oracle; document that joining strict servers may
need the full signature chain and leave that as a named follow-up, not silent scope creep).
Owns: `crates/protocol/v762/` + registry 2-liner. Blocked by: U7 pattern.

**U11 — family v766 (1.20.6, #357) and family v774 (1.21.11, #358).** Two units, one
briefing: configuration-phase state machine (v766 — the login flow structurally differs;
v770's own configuration handling is the reference implementation to imitate, not import)
and components-era items (both; v774 items are near-26.2). v774 is the cheapest new family
in the set — closest wire to v770 — and its packet shapes can come from its own jar's report
if present. Owns: `crates/protocol/v766/`, `crates/protocol/v774/` + registry 2-liners.
Blocked by: U7 pattern; v766 also by U5.

**U13 — family v5 (1.7.10), issue #344 — last, eyes open.** No minecraft-data, no cached
jar, pre-compression pre-UUID wire; every shape from captures against a fetched real jar.
Budget it as the most expensive single family (the epic agrees) and do not let it block
anything — nothing depends on it. Owns: `crates/protocol/v5/` + registry 2-liner.

**U12 (optional, anytime) — rename v735 → v754** per `docs/protocol-crate-naming.md`'s
recommendation (family named for its lowest protocol). Cheap but repo-wide: grep for the
old crate path *including doctests* and run `cargo test --workspace`, not just check — the
#361 extraction's exact trap.

## Order

```
U0 (evidence debt)          — now, independent, smallest
U1 (shared canonical crate) ─┬─ U3 (v47 retrofit)
                             ├─ U5 (items) ──────────┐
U2 (multi-protocol seam) ────┼─ U6 (v110: 1.9–1.11)  │
U4 (v735 retrofit, DFU walk pattern) ─ U7 (v404: 1.13.2) ─ U8 (v498) ─ U9 (v756)
                                       U10 (v762)  U11 (v766 needs U5; v774)
U13 (v5: 1.7.10) — last, depends on nothing, nothing depends on it
```

Parallelizable from day one: U0, U1, U2, U4 (disjoint files). The registry 2-liners and the
workspace-member lines are the only cross-unit file contention; broker them.

## Risks

1. **The existing families are quietly wrong, and the pattern is contagious.** v47 and v735
   decode into non-canonical id spaces feeding the mesher and collision. If U3/U4 land after
   new families are built by imitation, the defect replicates seven times. Mitigation: U3/U4
   early; every family unit's briefing names canonical-output as the acceptance bar with the
   raw-value negative control.
2. **The seam change (U2) is cross-cutting and order-sensitive.** Built late, v110/v498/v756
   get built single-protocol and reworked. Built carelessly, it breaks `check-seam` —
   the one health check whose failure mode is architectural. Mitigation: U2 before any
   grouped family; `just check-seam` is its gate.
3. **Evidence supply is the long pole, not code.** Four jars must be fetched; per-jar data
   generators and packet reports must be *verified per jar, not assumed*; captures must come
   from real servers (the `decode(encode(x))` trap); the box affords one live oracle at a
   time. Mitigation: the capture-once/replay-forever pattern makes live time a one-shot cost
   per version; U0 proves the whole capture loop on the family that already exists.

Secondary: the v756 grouping may split (tell named in U9); chat signing scope creep (named
in U10); the 32 context-dependent flattening slots and outbound/hosting are consciously
deferred, not forgotten — phase 2 begins when `docs/server-ecs.md`'s world lands.
