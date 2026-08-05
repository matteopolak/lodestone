# Multi-version protocol: the dispatch plan for epic #343

## What it is

The implementation plan for epic #343 (every major version 1.7.10 through 26.2, children
#344–#358): family-per-wire-era with range extension inside an era, a shared canonicalisation
crate every legacy family maps through, a capture-once oracle strategy, and the
join-versus-host split stated per version with the hosting blockers located in the tree.
Re-verified 2026-08-05 at `d197d555`, and again the same day at `e2508e3` — by which point
**units U1, U2 and U3 had already landed** (`3ba959a`, `02b8053`, `fa75f38`); every
"X exists / X is missing" claim below was re-checked rather than copied forward.

## Verified ground truth, 2026-08-05, second pass at `e2508e3`

Re-verified per CLAUDE.md rule 2. The previous revision of this plan (written 2026-08-04,
`1df63a6`) was already stale in five figures within one day — v770 clientbound 113→114,
serverbound-connected 15→17, decode-to-`Ignored` 45→43, and two crate line counts. Nothing
in it was wrong when written. **That is the general property of numbers in this repo, not a
one-off: treat every figure in this document as a snapshot labelled with its sha, and re-run
the instrument rather than citing the snapshot.** The `d197d555` revision then rotted
*within the same day*: three of its own units landed (U1–U3), v770's connectedness moved
again (decoded 114→116, connected 17→21, `Ignored` 43→41), and every server-side line
number it cited drifted. Anchor by grep pattern; treat cited line numbers as hints.

**What exists.** Four family crates under `crates/protocol/`, all implementing
`VersionAdapter` (trait at `crates/lodestone-model/src/adapter.rs:697`, still). Since U2
(`02b8053`), `supports` tests membership in the family's own `PROTOCOLS` const, and
adapters are constructed *with* the negotiated protocol — see the seam section below.

| crate | protocol | version | lines (at `d197d555`) | canonicalises to 26.2 state? |
|---|---|---|---|---|
| v47 | 47 | 1.8.9 | 3,856 | **Yes since `fa75f38` (U3)** — chunk decode now maps `(blockId << 4) \| meta` through `lodestone-canonical`; the reverted-fix measurement (stone→spruce_planks, bedrock→**lava**) is in `docs/protocol-47-canonicalisation.md`. The 2-D→3-D biome fabrication seam remains |
| v340 | 340 | 1.12.2 | 14,128 | **Yes** — `flattening.rs` + `canonical.rs` |
| v735 | 754 | 1.16.5 | 4,237 | **No** — `packets/chunk.rs:17` stores "a single flat block-state id" in **1.16.5's** id space; zero references to `canonical` or `flattening` in the crate |
| v770 | 776 | 26.2 | 16,290 | native (it *is* the canonical space) |

**Connectedness, measured this revision** (`cargo xtask connectedness` on the working
tree over `e2508e3`, exit 0 read from a captured file; other agents had uncommitted
server-crate edits live, so this is a *sample* in CLAUDE.md's sense). **Do not re-quote
these — re-run the command**; both previous revisions' figures rotted within a day:

```
v47   clientbound decoded 21/74;  emits 21/74;  serverbound encoded 17/26
v340  clientbound decoded 22/80;  emits 22/80;  serverbound encoded 20/33
v735  clientbound decoded 17/92;  emits 17/92;  serverbound encoded 21/48
v770  clientbound decoded 116/141; emits 114/141; serverbound encoded 54/69;
      serverbound decoded 62/69, connected 21/69; decodes-to-Ignored-only 41
```

The instrument now also prints **`decoded-but-stranded` per family (0 everywhere at this
pass)** — the direct detector for the sharp corollary that an arm which decodes and drops
on the floor is *worse* than `Ignored`, because a naive connectedness read counts it as
connected. Any family unit below whose report shows a non-zero stranded count has shipped
that defect, whatever its other numbers say.

For each legacy family the tool itself prints the host-side verdict:

> serverbound decode: not applicable (no src/server_protocol.rs — family does not
> implement ServerProtocol, so it cannot host)

**Quote the instrument, not prose, when asked which families can host** — the join/host
split has been hand-derived wrongly more than once, and the tool now states it per family.

**The serverbound two-file join, spelled out.** Serverbound decode lives in
`crates/protocol/v770/src/server_protocol.rs`; consumption lives in
`crates/lodestone-server/src/server.rs`, whose `ServerBound::Ignored => {}` arms sit at
lines 1497 and 2768 this pass (they were 1346/2316 one revision ago — grep for the arm,
do not trust the number). A variant that decodes but lands in `Ignored` is stranded
exactly as an unhandled clientbound packet would be — hence 62/69 *decoded* but only
21/69 *connected*. The 41-strong `Ignored` list includes `CONFIGURATION_ACKNOWLEDGED` and
`RESOURCE_PACK`; note carefully that those are **not** the hosting blockers below —
`CONFIGURATION_ACKNOWLEDGED` is Play-state configuration *re-entry*, a different packet
from the login-time handshake. Auditing hosting completeness is always this two-file join,
never a one-file scan.

**Already implemented, issues still open** (`git log --grep`; the sixteen issues remain
OPEN — per the standing rule, check the log before dispatching any child):

- #343 groundwork: `faeb692` — the derived sixteen-row version table,
  `crates/lodestone-registry/src/generated/version_table.rs` (16 `Entry` rows, protocol +
  DataVersion + per-field provenance; `docs/version-table.md`). `d0cd8d6` — the
  pre-Flattening id:meta table from the 1.13.2 jar's own DataFixerUpper. The epic's
  "change the CLAUDE.md scope line with the first non-770 family" instruction is already
  satisfied (`07e3d83`).
- **This plan's own U1, U2 and U3 landed between its two same-day revisions:** U1 =
  `3ba959a` (`crates/lodestone-canonical` extracted, v340 re-exports through it,
  `docs/canonical-block-states.md`); U2 = `02b8053` (protocol-at-construction seam,
  `docs/multi-protocol-seam.md`); U3 = `fa75f38` + `c033f1f` + `7f5512a` (v47
  canonicalisation, `docs/protocol-47-canonicalisation.md`). Details at each unit below.
- #345 (1.8.9): `53b906a` four v47 decode arms; `0a3e00f` outbound death-screen/spectate.
- #349 (1.12.2): `35d4401` decode arms; `714209b` `block_change`/`multi_block_change`
  decoded *into canonical states* via the flattening bridge.
- #353 (1.16.5): partial outbound via `0a3e00f`; the issue's "v735 may be 1.16.1" caveat
  is resolved — `supports` says 754, which is 1.16.5.

**Assets on disk.** `.cache/mc/` holds 12 of the 16 target versions — missing only
**1.7.10, 1.9.4, 1.10.2, 1.11.2** (`xtask version-table --fetch-missing` exists to fetch
them). 1.8.9 / 1.12.2 / 1.16.5 are full booted installs with worlds. Only 26.2 has
decompiled source and `generated/` reports. `vendor/minecraft-data` is vendored
(1.8 → 1.21.x; nothing for 1.7.10 or 26.x). `scripts/live-oracles/legacy-1.12.sh` exists
(game :25568 / RCON :25569, `eclipse-temurin:8-jdk`, ports disjoint from the 26.2 oracles)
— but per the roadmap it has **never been run for the v340 decode work**; that is evidence
debt, unit U0 below.

**Scaffolding that did not exist when the family map was first drawn:** `xtask
new-version` now generates a family skeleton and **withholds registry support until a
generated `SHAPE_REVIEW.toml` is discharged** — a structural mitigation for the "1.12.2
client wearing 1.16 packet IDs" failure the roadmap recorded. `xtask conformance` runs
packet-id, registry, isolation, deletability, test and clippy checks per family, and
`xtask check-deletable <vNNN>` simulates deleting a family and reports the fallout. Every
family unit below should use all three.

**`conformance` does not take `--family <vNNN>` alone**, which this document claimed in two
places and U3 found unrunnable. It also needs `--minecraft` and `--protocol`, plus
`--source minecraft-data` for a legacy family, and it **rejects `--target-dir`** — so it
cannot be pointed at a private build directory the way every other command here is. It then
bails at the pre-existing `lodestone-fuzz` isolation failure (that crate has non-optional
deps on all four families), so its later stages have to be run separately. U3 did exactly
that: clippy 0 errors, and `check-deletable v47` naming `lodestone-canonical` nowhere.
**Run it, read its `--help`, and expect to split it — do not transcribe an invocation from
this document.**

## Join versus host: two different sets, stated per version

Conflating these is the single most likely way this plan goes wrong, so it gets its own
section. The authority is `crates/lodestone-registry/src/lib.rs`: `FAMILIES` (join — a
`VersionAdapter`) and `SERVER_FAMILIES` (host — a `lodestone_server::ServerProtocol`) are
deliberately **two tables**, and the registry's own doc says why: "a family can have a
`VersionAdapter` (so the client can *join* that version) and no `ServerProtocol` (so we
cannot *host* it)". Today `SERVER_FAMILIES` has exactly one entry, v770. Since U2 landed
(`02b8053`), adapters are constructed *with* the negotiated protocol — `Family` carries
`protocols: &'static [i32]` (borrowed from each family crate's own `PROTOCOLS` const,
never restated) alongside `make: fn(i32) -> Box<dyn VersionAdapter>`, and a registry
drift-guard test asserts each family's slice agrees with its `supports`. v770 was
deliberately left `|_protocol|` — single-protocol, because it is the canonical space and
the only `ServerProtocol`.

**This epic's fifteen child issues are scoped to the join direction.** Hosting is phase 2,
but phase 2 is decomposed concretely below (H0–H4) rather than waved at, because two of
its blockers are substrate that can land early, and one was located precisely during this
revision's verification pass.

### The hosting blockers, located

1. **The configuration-phase handshake is hardwired into the connection state machine,
   not version-gated.** `crates/lodestone-server/src/server.rs:1318-1330` this pass —
   grep `ServerBound::LoginAcknowledged`, the number drifts with the live server work:
   `LoginAcknowledged` → `State::Configuration` → `begin_configuration()`,
   and `ServerBound::ConfigurationFinished` → `State::Play` → `begin_play()`. A pre-1.20.2
   client sends **neither packet** — they do not exist on its wire; after Login Success it
   transitions straight to Play — so a legacy host connection stalls in Login forever.
   And the fix cannot live inside a family: `ServerProtocol::decode`
   (`crates/lodestone-server/src/protocol.rs:619` this pass, formerly :521 — same drift
   warning) is strictly one-packet-in, one-`ServerBound`-out, so a family cannot
   synthesize the two transitions from packets that never arrive. This is **one-off
   substrate** (H0), touching two choke-point files once, after which no per-family work
   recurs.
2. **A pre-1.13 host needs a flattening inverse; a pre-1.14 host needs light in-chunk.**
   Flattening is not symmetric: modern states must be *lowered* to `(id, meta)`, and the
   lossy cases are decisions, not transforms (H2). Pre-1.14 clients expect light inside
   the chunk packet rather than as separate packets, so the host must fabricate or carry
   light at encode time (H3).
3. **v770's own hosting is only 21/69 connected** (the two-file join above). Widening that
   is per-packet work on the existing family, orthogonal to this epic's units, but any
   "host version X" claim inherits whatever the shared `server.rs` loop actually consumes.
4. **`resource_pack_push/pop` is handled in Play state only, not Configuration** (#294).
   Re-verified at `e2508e3`, still open: the v770 adapter's Configuration-state
   clientbound arms (`adapter.rs:2831-2900` this pass) cover ten packet ids with no
   `RESOURCE_PACK_PUSH/POP`; the
   Play arms exist at `adapter.rs:4495/4517`, and the serverbound *response* encoder is
   already state-aware (`adapter.rs:4699` picks
   `configuration::serverbound::RESOURCE_PACK`). So #294 is a decode-side gap in the
   **join** direction against real 26.2 servers, and it matters here because v766's
   configuration-phase machinery (U11) will be built by imitating v770's — fix #294 first
   or the copy inherits the hole. The patch belongs to #294, not this epic: add the two
   Configuration-state decode arms delegating to the same handlers Play uses.

### Per-version split

Join cost is the family map in the next section (U-units). Host cost, per version, is the
v770 baseline **plus** the rows below. "H" units are defined after the phase-1 unit list.

| version(s) | join unit | host needs beyond a per-family `server_protocol.rs` + `SERVER_FAMILIES` entry (H4-shaped) |
|---|---|---|
| 26.2 | done | baseline (21/69 connected and growing — blocker 3) |
| 1.21.11, 1.20.6 | U11 | registry-sync/component **down**-conversion; config phase exists on these wires, so no H0 |
| 1.19.4 | U10 | H0 (no config phase); host-side chat-signing enforcement decisions |
| 1.17.1–1.18.2 | U9 | H0; height/biome down-conversion |
| 1.14.4–1.16.5 | U8, U4 | H0; state-id down-mapping (inverse of U4's table) |
| 1.13.2 | U7 | H0; H3 (light in-chunk) |
| 1.7.10–1.12.2 | U13, U3, U6, U0 | H0; H2 (flattening inverse, lossy); H3; item component→NBT lowering |

**Value ranking for phase 2:** hosting 1.8.9/1.12.2 clients (legacy friends joining a LAN
world) is the plausible demand; hosting 1.15.2 clients is not. So phase 2, when it opens,
starts at H0 + one pre-1.13 family, not at the version nearest 26.2. Phase 2 opens after
`docs/server-ecs.md`'s migration lands — `lodestone-server` currently has live agents
mid-migration, which is itself a reason H0 is the *only* server-side edit this plan
schedules early.

## The decision: family-per-wire-era, range extension inside an era

**Not fifteen new crates.** The unit of work is a *family crate per wire era*, and inside
an era, additional versions are per-protocol generated tables plus branch points — a
change to that family only, invisible to the registry.

Evidence, not preference:

1. `docs/roadmap/protocol.md` (§#306) measured the irreducible cost of a new family at
   **~900 hand-written lines** (line 539) concentrated in `adapter.rs` and `chunk.rs`, and
   recorded that the `xtask new-version` cloning experiment produced "a 1.12.2 client
   wearing 1.16 packet IDs" — now structurally mitigated by the `SHAPE_REVIEW.toml` gate,
   but the lesson stands: fifteen clones would be ~13k lines of near-duplicate wire code.
2. Adjacent-version deltas inside an era are small and *table-shaped* (packet id
   renumbering, a handful of shape changes), which is exactly what the generated
   `packet_ids.rs` per version already expresses — `xtask gen-packet-ids --source
   minecraft-data` parses any covered version's `protocol.json` today.
3. Widening a family costs the registry nothing: `adapter_for_protocol` delegates to each
   family's `supports`. Adding a whole new family costs one optional dep line, one feature
   line, one `FAMILIES` entry (verified against `crates/lodestone-registry/src/lib.rs`).
   **So the fifth family is cheap at the registry and ~900 lines at the crate — the seam
   scales; what does not scale without U2 is multi-protocol families.**
4. The era boundaries below are the epic's own expensive discontinuities (Flattening,
   light split, dynamic height, 3-D biomes, chat signing, configuration phase,
   components). Crossing one inside a single crate is where the wearing-wrong-packet-IDs
   failure lives; staying inside one is cheap.

**The resulting family map** (protocol numbers from the derived table,
`crates/lodestone-registry/src/generated/version_table.rs` — never hand-derived):

| family | protocols | versions | issues | status |
|---|---|---|---|---|
| v5 | 5 | 1.7.10 | #344 | new, **last** — no minecraft-data, no cached jar, pre-compression wire |
| v47 | 47 | 1.8.9 | #345 | exists; canonicalisation retrofit landed (U3, `fa75f38`) |
| v110 | 110, 210, 316 | 1.9.4, 1.10.2, 1.11.2 | #346–#348 | new, one crate, three protocol tables |
| v340 | 340 | 1.12.2 | #349 | exists; donated the canonical bridge to `lodestone-canonical` (U1) |
| v404 | 404 | 1.13.2 | #350 | new — the Flattening-boundary anchor |
| v498 | 498, 578 | 1.14.4, 1.15.2 | #351–#352 | new, one crate (1.15 chunk-biome branch) |
| v735 | 754 | 1.16.5 | #353 | exists; needs canonicalisation retrofit (U4); rename to v754 optional (U12) |
| v756 | 756, 758 | 1.17.1, 1.18.2 | #354–#355 | new, one crate (1.18 section-biome branch — the riskiest grouping, split if the chunk paths stop sharing) |
| v762 | 762 | 1.19.4 | #356 | new — chat-signing state machine |
| v766 | 766 | 1.20.6 | #357 | new — configuration phase + item components; **fix #294 first** |
| v774 | 774 | 1.21.11 | #358 | new — nearest neighbour to v770; still its own crate (v770 is the canonical space *and* the only `ServerProtocol`; keeping it single-protocol keeps the hosting seam simple) |
| v770 | 776 | 26.2 | — | done, canonical |

Seven new crates instead of eleven; four multi-protocol groupings, each inside one era.

**The seam change this required (U2) has landed** (`02b8053`, `docs/multi-protocol-seam.md`):
`Family::make` is now `fn(i32) -> Box<dyn VersionAdapter>`, each family exposes
`PROTOCOLS` + `adapter_for(protocol)`, and the registry's drift-guard test constructs
every family at every protocol it claims. It touched neither `net.rs` nor `app.rs`, as
predicted — the shell already passed the negotiated protocol to `adapter_for_protocol`.
The grouped families (U6, U8, U9) are unblocked on this axis.

### The seam line: what belongs in `VersionAdapter` versus a family

The question every new method will raise, answered once. **The seam speaks canonical
26.2-space types only** — `ClientEvent`/`ClientAction`, canonical block-state ids (now via
`lodestone-canonical`), canonical items — and **the protocol number crosses the seam
exactly once, at construction**, never again as a method parameter. A method belongs in
`VersionAdapter` iff all three hold:

1. a version-free consumer (shell, model, renderer) needs it — the family is not calling
   itself through the trait;
2. **every** family can implement it meaningfully — a method most families stub as a
   no-op is family logic leaked upward, and the tell is a defaulted body that only one
   impl overrides plus a caller that version-checks before calling;
3. its signature names no wire shape, packet id, or per-version id space.

The tests that catch a wrong-side placement, in order of arrival: **`just check-seam`**
fails if the method forces shell code to know a version (the failure mode is
architectural, nothing else sees it); a **tree-wide grep for `lodestone_v` under
`crates/lodestone-shell/src/`** must stay empty outside the registry — any hit is a
family being reached around the trait, which is the symptom of a *missing* seam method
(per this plan's own rule, a gap in `VersionAdapter` is a defect in the seam, not a
reason to route around it); and the registry drift-guard catches a family whose claimed
protocols and constructed adapters disagree. Direction matters too: the seam runs both
ways (`docs/singleplayer.md`), so ask what *produces* a serverbound action as well as
what consumes a clientbound event — `SetFlying` was encoded by four adapters with zero
producers.

## Canonicalisation: the layer that is the actual work

Epic #343 already decided the architecture: one canonical internal version (26.2), a
translation layer per protocol. The survey's largest finding — **legacy families skipping
the translation while their suites stay green** — is now down from two live instances to
one: v47 was fixed by U3 (`fa75f38`; it had been parking raw `(id << 4) | meta` in the
palette, and its bottom bedrock layer meshed as lava), while **v735 still parks
1.16.5-space flat ids** feeding a mesher and collision that consume 26.2 ids. Any plan
that adds families before U4 lands replicates the defect seven more times.

**What exists (extracted to `crates/lodestone-canonical` by U1, `3ba959a`; v340 was the
donor and re-exports through it):** `flattening.rs` (API) over `generated/flattening.rs`
(9,076 lines): 4,095 `id:meta` slots, 1,663 resolved, 2,400
no-entry, 32 `RequiresAdditionalContext`; provenance is a reflective dump of the **real
1.13.2 jar's own DataFixerUpper**, regenerated via `LODESTONE_REGEN=1` against
`tests/support/flattening_1_13_2_jvm.txt` (`docs/protocol-340-flattening-table.md`). Plus
`canonical.rs`: 1.13-name → 26.2-state bridge with the rename pass,
`waterlogged` defaulting, and an `Unmapped` drift-guard variant
(`docs/protocol-340-canonical-bridge.md`, `docs/canonical-block-states.md`). Item-side
flattening: **zero item content in the crate** (counted at `e2508e3`) — recorded but not
built; that scope is U5's, intact.

**Generalisation, two regimes with the Flattening as the boundary:**

- **Pre-1.13 families (v5, v47, v110, v340):** all speak `id:meta`. The dumped table is
  the 1.13.2 DataFixer's, which upgrades *1.12.2-space* ids; older versions' id space is a
  strict subset (ids were only added), so the same table serves all four — per-version
  difference is which slots are populated, which the existing `NoTableEntry` outcome
  already expresses. This forced a doctrine call: the roadmap records v47 was
  *deliberately* denied v340's table to preserve per-crate deletability. **Decided and
  executed: `flattening` + `canonical` extracted into `lodestone-canonical` (U1,
  `3ba959a`), and v47 became its second consumer (U3).** Deletability applies
  to *families*; shared game data already has a precedent crate (`lodestone-data`, the
  #361 extraction), and the alternative is four copies of a 9k-line generated table
  drifting independently. Deleting a family remains folder + dep line + feature line
  (`xtask check-deletable` verifies exactly this).
- **Post-1.13 families (v404, v498, v735, v756, v762, v766, v774):** each speaks its own
  version's block-state id space. The mapping is per-version: dump each version's
  `blocks.json` by running **its own jar's** data generator (the jars are in `.cache/mc/`;
  the generator ships in the server jar from 1.13 onward — verify per jar as part of each
  unit, do not assume the invocation is uniform), then resolve name+properties → 26.2
  state id through a **DFU-walk oracle against the 26.2 jar**, keyed by the source
  `data_version` from the version table. The 26.2 jar contains every fixer from every
  older DataVersion — DataFixerUpper's contract — so this replaces v340's hand-written
  rename pass with the same outside-our-code provenance the flattening table already has.
  New program in `oracle-java/` (pattern:
  `crates/lodestone-data/tests/{collision_shapes,hardness}.rs` generate-or-assert +
  `LODESTONE_REGEN=1`). minecraft-data is a cross-check only; the v340 cross-check
  measured 91.8% agreement with all 137 disagreements resolving in the jar's favour.

**The 1.12.2→1.13.2 boundary, costed explicitly:** for inbound blocks it is *already
mostly paid* — `d0cd8d6`/`714209b` built and wired the table. Remaining boundary cost:
(a) the 32 context-dependent slots (need TileEntity/neighbour context; currently
resolve-or-air); (b) **item flattening, entirely unbuilt** — the ~300-entry item table
needs the same reflective dump before any pre-1.13 family can decode inventories into
canonical items; (c) the whole outbound direction (26.2 → id:meta is lossy; phase 2, H2).
New cost at this boundary for v404 itself is *low* — 1.13.2 is the first version whose
block model matches ours natively; its chunk format and command tree are ordinary era
work, not Flattening work.

**Items pre-1.20.5 (all families except v774):** NBT → data components, both directions
lossy per the epic. Inbound scope: map the NBT the client actually renders (display name,
lore, enchantments, damage) and preserve-the-rest; full fidelity is not required to join.

## Data sources, per version

Authority order per child issues: real oracle jar (strongest) → jar's generated reports →
minecraft-data (cross-check only, never authority). Protocol/DataVersion numbers: always
the derived table, never hand-written (epic first task, satisfied by `faeb692`). The #275
lesson binds every registry-sync unit here: a generator file is authoritative about
registry *contents*, not about which registries are *sent* — the sent-list comes from the
jar (`RegistryDataLoader.SYNCHRONIZED_REGISTRIES` for 26.2; establish the per-version
equivalent from each era's jar, not from `registries.json`).

| versions | packet shapes | ids/registries/states | jar on disk? |
|---|---|---|---|
| 1.7.10 | **captures only** — no minecraft-data, nothing else exists | captures + the jar itself | **no — fetch** (`xtask version-table --fetch-missing`) |
| 1.8.9–1.12.2 | minecraft-data `protocol.json`, cross-checked by live capture | 1.13.2 DFU flattening table (blocks); item dump TBD (U5) | 1.8.9, 1.12.2 yes; **1.9.4/1.10.2/1.11.2 no — fetch** |
| 1.13.2–1.20.4 era | minecraft-data, cross-checked by live capture (no packet report in these jars) | own jar's `blocks.json`/`registries.json` + 26.2 DFU walk | yes |
| 1.20.6, 1.21.11 | own jar's generated packet report where present (report added in the 1.20.5 era — verify per jar), else minecraft-data + capture | own jar's reports + 26.2 DFU walk | yes |

For frozen historical protocols minecraft-data will never rot the way its 26.2 data did —
its weakness is *current* versions — which is why it is genuinely useful here and still
never the authority.

## Oracle strategy: three JDK images, one live oracle at a time, captures committed

The "15 versions × a container each" fear dissolves on inspection: the oracle image is a
stock JDK; the version lives in the bind-mounted jar (`legacy-1.12.sh` already works this
way — `eclipse-temurin:8-jdk` + `.cache/mc/1.12.2`, game :25568 / RCON :25569). So:

- **Three base images total**: temurin 8 (≤1.16.5), temurin 17 (1.17.1–1.20.x), temurin
  21 (1.21.11) — exact split verified per jar's `version.json` `java_version` when each
  script is written, not assumed.
- **One parameterized script**, `scripts/live-oracles/legacy.sh <version>`, generalizing
  `legacy-1.12.sh`: per-version port pairs carved out of a documented range disjoint from
  :25565/:25568-9/:25570-1/:25580-1, `--memory 3g`, the three traps from
  `docs/oracle-runtimes.md`.
- **Concurrency budget**: ≈1.1–1.3 GB resident per running oracle, 16 GB box, many agents
  live — **run one legacy oracle at a time, stop it when the gate passes**. Oracles are
  not repo state; scripts recreate them.
- **Capture once, commit, replay forever.** Per version, one scripted session (login →
  join → chunk load → RCON-driven `setblock`/`fill`/`summon` at *known coordinates* →
  disconnect) recorded to `crates/protocol/<fam>/tests/support/<ver>_join_capture.bin`
  and committed. The always-run test decodes the committed capture and asserts against the
  RCON-known values — expected values originate outside our code, satisfying the evidence
  standard without a running server. Live `#[ignore]`d gates re-run the capture script and
  are needed only for serverbound effects (dig a block, observe the update come back),
  which a replay structurally cannot exercise.
- **The host direction inverts the oracle**: the external artifact is the **real Mojang
  client jar** of that version (`xtask fetch-assets` fetches client jars), joining *our*
  server. Our encoder cannot grade itself; the vanilla client rendering the world is the
  only oracle for lossy down-conversion.

## Units of work — phase 1 (join)

Each unit: scope, files owned (no two dispatchable-in-parallel units share a file), the
consumer that proves it is not an island, the gate with its negative control, and what
would make the gate vacuous. Choke-point note: **no phase-1 unit touches `app.rs`,
`sim.rs`, `net.rs`, `gpu.rs`, or `lodestone-server/src/lib.rs`** — the registry seam is
the whole reason. Units adding a `FAMILIES`/feature line touch
`crates/lodestone-registry/src/lib.rs` and its `Cargo.toml` (2 lines each); those two
files are the one collision surface, so the orchestrator serializes just those edits.
Every family unit finishes by running `cargo xtask connectedness` and `cargo xtask
conformance` and quoting the output in its report — never a remembered number. See the
`conformance` note above: its real invocation needs `--minecraft` and `--protocol` (plus
`--source minecraft-data` for a legacy family), rejects `--target-dir`, and bails early on
`lodestone-fuzz`'s pre-existing isolation failure, so the later stages run separately.

**U0 — pay the standing evidence debt (dispatch first, smallest unit).**
Run `scripts/live-oracles/legacy-1.12.sh`; against it, verify `multi_block_change`'s
`horizontalPos` nibble order, currently sourced from external wire docs only — flagged in
`crates/protocol/v340/src/adapter.rs` and `tests/block_updates.rs`. Method: RCON `fill` an
asymmetric pattern (different extents in x and z, e.g. 3×1) inside one chunk in one tick,
capture the packet, assert decoded relative coords match the RCON coordinates. **Negative
control:** decode the same capture with the nibbles swapped and require the assertion to
fail — if it doesn't, the pattern was symmetric and the control is vacuous. Owns:
`crates/protocol/v340/tests/block_updates.rs` (new live test), the captured fixture.
Consumer: the existing decode arm. Also promote the capture to the committed-fixture
pattern above.

**U1 — LANDED (`3ba959a`): `crates/lodestone-canonical`.** The dispatch-time decision
resolved as a new shared crate, not a `lodestone-data` module. v340 re-exports through it;
the flattening drift-guard suite and `flattening_1_13_2_jvm.txt` moved with it;
`docs/canonical-block-states.md` is the crate doc. Item canonicalisation was **not** part
of it — the crate has zero item content (verified by count at `e2508e3`), so U5's scope
is intact.

**U2 — LANDED (`02b8053`): multi-protocol seam.** See "The seam line" above for the
landed shape (`protocols` slice + `make: fn(i32)`, registry drift-guard, v770
deliberately single-protocol) and `docs/multi-protocol-seam.md` for the full record. The
per-protocol-table negative control ("a family constructed for protocol A must select
A's table when B is in its set") becomes exercisable only when the first *grouped* family
exists — it is part of U6's gate, not retroactively U2's.

**U3 — LANDED (`fa75f38`, doc `c033f1f`, fixture generator `7f5512a`): v47
canonicalisation retrofit.** One gate variation worth recording for U4's benefit: instead
of the planned live-oracle RCON gate, evidence came from a **real 1.8.9-written world
save** — `tests/support/real_1_8_9_section_save.txt`, extracted by
`oracle/extract_real_section.py` — so expected values still originate outside our code,
at lower cost than a live container. The reverted-fix measurement (the gate's own failure
output): 1.8 stone `1:0` had been decoding as `minecraft:spruce_planks`, bedrock `7:0` as
**lava**. Full record: `docs/protocol-47-canonicalisation.md`.

**U4 — v735 canonicalisation retrofit.** Same shape as U3, different mechanism: 1.16.5
state ids → 26.2 via the DFU-walk table (first consumer of the post-1.13 mapping oracle).
Owns: `crates/protocol/v735/src/` chunk/block paths, the 1.16.5 mapping table + its
`oracle-java/` dump program. Gate/control: as U3's *original* two-armed shape, against
the 1.16.5 install already in `.cache/mc/` (or a real 1.16.5-written save, the cheaper
variant U3 validated). Establishes the pattern U6–U11 reuse. Blocked by: nothing — **with
U1–U3 landed, this is the front of the queue** and the last live instance of the
wrong-id-space defect.

**U5 — item canonicalisation, both regimes.** Pre-1.13 item flattening (the unbuilt
~300-entry reflective dump, same pattern as blocks) + legacy-NBT → component mapping for
the render-relevant subset. Owns: the shared canonical crate's item module, its oracle
program, its dump fixture. Consumer: each family's inventory decode arms (wiring lands
with each family's unit; this unit ships the table + API and **one** consumer — v340's
inventory path — so it is not an island on day one). Gate: v340 live inventory test
asserting a chest item placed by RCON with known id:damage decodes to the canonical item
+ components. Negative control: an id:damage pair absent from the table must decode to
the explicit `Unmapped` variant, not to air/default — and prove the detector fires by
feeding a known-bad pair. Blocked by: nothing — U1 landed; dispatchable now.

**U6 — family v110 (1.9.4, 1.10.2, 1.11.2), issues #346–#348.** First new crate
(scaffolded via `xtask new-version`, `SHAPE_REVIEW.toml` discharged before registry
support); first consumer of U2's machinery; three generated `packet_ids` tables; pre-1.13
canonicalisation from day one via U1 (never the v47 raw-palette shape). Scope: the
roadmap's ~900-line irreducible core (join flow, chunk, entity, block updates, movement)
× the era's specifics (offhand slot, attack cooldown as data, reshaped entity metadata —
index tables per version from minecraft-data cross-checked by capture; **never hand-count
an index**; run an oracle dump when in doubt). Owns: `crates/protocol/v110/` entirely +
registry 2-liner (brokered). Jars must be fetched first (three missing). Gate:
per-version committed-capture replay + one live join gate per protocol against the
parameterized oracle; negative control: the wrong-protocol handshake must be refused by
`supports`. Vacuous if: captures are generated by our own encoder — they must come from
the real server. U6 also owns U2's deferred negative control: an adapter constructed for
protocol A must select A's `packet_ids` table when B is in the family's set, asserted on
an id that differs between them. Blocked by: oracle script, jar fetch (U1 and U2 landed).

**U7 — family v404 (1.13.2), issue #350.** The boundary anchor: first native-block-model
family, new chunk format, command tree (reuse `lodestone-command`, the #118 substrate).
State mapping via the first *small* DFU walk (1631 → 4903). Owns: `crates/protocol/v404/`
+ registry 2-liner. Gate/control: as U6, jar already on disk. Blocked by: U4's oracle
pattern (not U2 — single protocol).

**U8 — family v498 (1.14.4, 1.15.2), issues #351–#352.** Light out of the chunk packet
(1.14), biome array into it (1.15) — the intra-family branch is confined to `chunk.rs`.
Owns: `crates/protocol/v498/` + registry 2-liner. Blocked by: U7 (pattern; U2 landed).

**U9 — family v756 (1.17.1, 1.18.2), issues #354–#355.** Dynamic world height + 1.18
section-scoped paletted biomes. **Check what the chunk store and mesher assume about
section count before writing wire code** — if the store hardcodes 16 sections this unit
gains a prerequisite outside protocol land and must say so rather than absorb it. The
grouping most likely to split into two crates; the tell is `chunk.rs` sharing under 50%
between the two protocols. Owns: `crates/protocol/v756/` + registry 2-liner. Blocked by:
U8 (U2 landed).

**U10 — family v762 (1.19.4), issue #356.** Chat signing: scope is *joining* — decode the
session/signature packets, send unsigned chat where the oracle permits
(`enforce-secure-profile=false` on our own oracle; document that joining strict servers
may need the full signature chain and leave that as a named follow-up, not silent scope
creep). Owns: `crates/protocol/v762/` + registry 2-liner. Blocked by: U7 pattern.

**U11 — family v766 (1.20.6, #357) and family v774 (1.21.11, #358).** Two units, one
briefing: configuration-phase state machine (v766 — the login flow structurally differs;
v770's own configuration handling is the reference implementation to imitate, not import —
**and #294's Configuration-state `resource_pack` gap must be fixed in v770 first, or the
imitation copies the hole**) and components-era items (both; v774 items are near-26.2).
v774 is the cheapest new family in the set — closest wire to v770 — and its packet shapes
can come from its own jar's report if present. Owns: `crates/protocol/v766/`,
`crates/protocol/v774/` + registry 2-liners. Blocked by: U7 pattern; v766 also by U5 and
#294.

**U13 — family v5 (1.7.10), issue #344 — last, eyes open.** No minecraft-data, no cached
jar, pre-compression pre-UUID wire; every shape from captures against a fetched real jar.
Budget it as the most expensive single family (the epic agrees) and do not let it block
anything — nothing depends on it. Owns: `crates/protocol/v5/` + registry 2-liner.

**U12 (optional, anytime) — rename v735 → v754** per `docs/protocol-crate-naming.md`
(family named for its lowest protocol). Cheap but repo-wide: grep for the old crate path
*including doctests* and run `cargo test --workspace`, not just check — the #361
extraction's exact trap.

## Units of work — phase 2 (host)

Phase 2 opens after the server-ECS migration lands. **Exception: H0 is substrate that
should land as soon as the `lodestone-server` choke-point calendar allows**, because it is
tiny, independently gateable with fakes, and every legacy host unit is blocked on it.

**H0 — version-gate the login→play transition.** Add a defaulted
`ServerProtocol::has_configuration_phase(&self) -> bool { true }`; in the connection
loop, when it answers false, run the Configuration→Play sequence immediately after
`login_success` instead of waiting for `LoginAcknowledged`/`ConfigurationFinished` (which
pre-1.20.2 wires cannot send — located at `server.rs:1199-1207`, decode contract at
`protocol.rs:521`). Owns: `crates/lodestone-server/src/protocol.rs` (trait),
`crates/lodestone-server/src/server.rs` (transition) — **both are live-agent choke
points; the orchestrator schedules this as a solo slot.** Gate: a fake protocol with
`has_configuration_phase() == false` reaches Play and receives chunks without either ack
packet. Negative control: the default-true fake must **not** reach Play without them.
External evidence: none at this layer — this is a state machine against our own trait;
the external oracle arrives with H4's real client. Said plainly rather than inventing a
round trip.

**H1 — v770 serverbound connectivity (the 41 `Ignored` arms).** Ongoing per-packet work
on the existing family; each arm is the two-file join (variant in
`v770/src/server_protocol.rs`, consumer arm in `server.rs`). **Blocked on gameplay, not
protocol, for most arms** — a prior survey established the majority strand because the
gameplay behind them (recipe book, beacons, command blocks, jigsaw, trades, …) is
unimplemented, so "wire up the arm" is not the unit of work and counting these against
protocol coverage inflates the plan. Not a unit of this epic — tracked per-packet
elsewhere — but every host-version claim inherits it, so it is named here to stop it
being rediscovered.

**H2 — flattening inverse (26.2 state → `id:meta`).** Extends U1's crate. The resolvable
direction is mechanical: invert the JVM-dumped forward table (this is *not* the
`decode(encode(x))` trap — the forward table's provenance is the 1.13.2 jar, outside our
code). The lossy remainder — modern states with no pre-1.13 representation — is a
**decision table, hand-curated, reviewed case by case**, and it cannot be externally
evidenced: no oracle knows what waxed copper "should" look like to a 1.12.2 client. What
*can* be evidenced externally: (a) round-trip on the 1,663 resolved slots,
`inverse(forward(id:meta)) == id:meta`; (b) H4's real-client gate rendering a world
containing sampled lossy states without disconnect or visual holes. Owns: the shared
canonical crate's inverse module + decision-table fixture. Blocked by: nothing on this
axis (U1 landed) — only phase-2 scheduling.

**H3 — pre-1.14 light-in-chunk fabrication.** Pre-1.14 clients expect light nibble arrays
inside the chunk packet. **Open question the unit must establish first, not assume: what
light does the v770 host currently compute or send?** If the server has no real lighting
engine, the fabricator's scope is "full-bright plausible light", explicitly labelled.
Owns: the hosting family's `server_protocol.rs` encode path (per-family, after H4's first
instance). External evidence: the real legacy client renders the world non-black — a
screenshot-level gate against the vanilla client, which is the consumer.

**H4 — first legacy `ServerProtocol` (recommend v340: the canonical bridge and H2 both
live there).** A `server_protocol.rs` in the family crate + one `SERVER_FAMILIES` entry.
Owns: `crates/protocol/v340/src/server_protocol.rs`, registry 2-liner (brokered). Gate —
the strongest external oracle in this plan: **the real Mojang 1.12.2 client** (fetched
via `xtask fetch-assets`) joins our hosted world and a scripted probe verifies chunks
render and a placed block appears at known coordinates. Negative control: a
wrong-protocol client must be refused at handshake with a version-mismatch message, not a
stall. Blocked by: H0, H2, H3 (for its light), and the server-ECS migration settling.

## Order

```
phase 1 (join):
DONE: U1 (canonical crate, 3ba959a)  U2 (multi-protocol seam, 02b8053)  U3 (v47, fa75f38)
open now, disjoint files, dispatchable in parallel:
U0 (evidence debt — smallest)   U4 (v735 retrofit + DFU-walk pattern)   U5 (items)
then:
U4 ─ U7 (v404: 1.13.2) ─ U8 (v498) ─ U9 (v756)
     U10 (v762)   U11 (v766 needs U5 + #294; v774)
U6 (v110: 1.9–1.11) — needs oracle script + jar fetch, nothing else now
U13 (v5: 1.7.10) — last, depends on nothing, nothing depends on it

phase 2 (host), after server-ECS lands:
H0 (state-machine gate; may land early in a brokered solo slot)
H2 (flattening inverse, extends U1's crate) ─┬─ H4 (first legacy host, v340, real-client gate)
H3 (light fabricator) ───────────────────────┘
then per-family: server_protocol.rs + SERVER_FAMILIES entry each
```

Open and parallelizable now: U0, U4, U5 (disjoint files). The registry 2-liners and
the workspace-member lines are the only cross-unit file contention; broker them. H0
contends with the live `lodestone-server` agents and is scheduled by the orchestrator,
not grabbed.

## Risks

1. **The existing families are quietly wrong, and the pattern is contagious — half
   retired.** v47 is fixed (U3, `fa75f38`; its bottom layer had been lava). **v735 is the
   one live instance left** and stays the imitation hazard until U4 lands; every family
   unit's briefing still names canonical-output as the acceptance bar with the raw-value
   negative control.
2. **The seam change (U2) — retired.** Landed at `02b8053` with `check-seam` green and
   the drift-guard in place. Residual: a new family bypassing `adapter_for` / restating
   its protocol list in the registry would reopen it; the drift-guard test is the
   detector.
3. **Evidence supply is the long pole, not code.** Four jars must be fetched; per-jar data
   generators and packet reports must be *verified per jar, not assumed*; captures must
   come from real servers (the `decode(encode(x))` trap); the box affords one live oracle
   at a time. Mitigation: the capture-once/replay-forever pattern makes live time a
   one-shot cost per version; U0 proves the whole capture loop on the family that already
   exists.
4. **Host-direction lossiness has no oracle.** H2's degradation choices are decisions;
   the only external check is a real legacy client tolerating the result. Budget review
   time for the decision table and do not let a round-trip test impersonate evidence.
5. **Every number in this document ages.** Connectedness figures and the `Ignored` list
   were re-measured on the working tree over `e2508e3`; line counts remain from
   `d197d555`; both prior revisions' figures rotted within a day of being written. Re-run
   `cargo xtask connectedness` before quoting anything here.

Secondary: the v756 grouping may split (tell named in U9); chat signing scope creep
(named in U10); the 32 context-dependent flattening slots are consciously deferred, not
forgotten.
