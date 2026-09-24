# Multi-version protocol

## What it is

The durable plan for every supported release from 1.7.10 through 26.2: family-per-wire-era with
range extension inside an era, shared canonicalisation, capture-led evidence, and a strict
join-versus-host split. The canonical-state foundation, multi-protocol seam, and legacy
canonicalisation bridges are implemented; the feature ledger below records extension work and
acceptance gates rather than commit history.

[`multi-version-protocol-dedup.md`](./multi-version-protocol-dedup.md)'s duplication measurement and
era-grouping guidance supersede the older family map and unit scoping. Use its 85% threshold and
independent wire fixtures before grouping an era. The historical 5,139–6,201 hand-written-line range
is a sizing baseline only; rerun the instrument before using it for a decision.

## Current evidence and shipped boundaries

Family support, protocol membership, and hosting capability are derived from the registry tables and
their tests. The join table and the host table are intentionally separate: a joining adapter does not
imply a hosted protocol. Ask the registry or `VersionAdapter::supports`; never infer either fact from
a crate, feature, folder, or release label.

`cargo xtask connectedness` is the current connectedness authority. It reports decoded, emitted,
connected, ignored-only, and decoded-but-stranded packet paths for each enabled family. Run it before
quoting a count: changing packet routes alters every number. A decoded packet that reaches no client
event, directive, world write, or sink is stranded even when a decoder test passes.

The last committed snapshot is retained only as a denominator/control reference, not as a current
claim: v1-8 decoded and emitted 21/74 clientbound packets and encoded 17/26 serverbound packets;
v1-9 was 22/80, 22/80, and 20/33; v1-14 was 17/92, 17/92, and 21/48. v26-2 decoded 116/141
clientbound packets, emitted 114/141, encoded 54/69 serverbound packets, decoded 62/69 serverbound
packets, connected 21/69, and had 41 ignored-only arms. The same snapshot reported zero
decoded-but-stranded clientbound paths. Re-run the tool before reporting any of these values.

Serverbound work is a two-file join. The version crate decodes a semantic `ServerBound` value and
`lodestone-server` consumes that value into authoritative state. Audit both sides; decoding into an
ignored arm is not connectivity. The same rule applies to clientbound paths: route the result to a
consumer and prove that consumer can affect a rendered or otherwise observable result.

The canonical-state crate, protocol-at-construction seam, legacy canonicalisation bridges, generated
version table, version-family scaffold, shape review, isolation check, deletability check, and
feature conformance tooling are already part of the architecture. Extend them rather than recreating
a local equivalent.

The release cache, generated reports, vendored `minecraft-data`, captured worlds, and oracle scripts
are evidence inputs, not implementation state. Use a release report where it exists, use
`minecraft-data` only for the covered historical releases, and use captures for releases without an
appropriate report. The family scaffold withholds registry support until its generated shape review is
discharged.

`xtask conformance` requires the family, release, protocol, and source information appropriate to the
family. It cannot substitute a private target directory for its checks, and workspace-wide
connectedness can expose an unrelated reachable-root failure. Family isolation and deletability are
fixed: the fuzz crate's per-family dependencies are optional, so `check-isolation` and
`check-deletable` pass for the legacy families. The remaining combined-conformance blocker is the
workspace-wide `check-connected` census, not a family failure. Run the structural family checks first,
then report that workspace-level blocker separately rather than attributing it to the family.

## Join versus host: two different sets, stated per version

Conflating these is the single most likely way this plan goes wrong, so it gets its own
section. The authority is `crates/lodestone-registry/src/lib.rs`: `FAMILIES` (join — a
`VersionAdapter`) and `SERVER_FAMILIES` (host — a `lodestone_server::ServerProtocol`) are
deliberately **two tables**, and the registry's own doc says why: "a family can have a
`VersionAdapter` (so the client can *join* that version) and no `ServerProtocol` (so we
cannot *host* it)". Today `SERVER_FAMILIES` has exactly one entry, v26-2. The implemented
multi-protocol seam constructs adapters *with* the negotiated protocol — `Family` carries
`protocols: &'static [i32]` (borrowed from each family crate's own `PROTOCOLS` const,
never restated) alongside `make: fn(i32) -> Box<dyn VersionAdapter>`, and a registry
drift-guard test asserts each family's slice agrees with its `supports`. v26-2 was
deliberately left `|_protocol|` — single-protocol, because it is the canonical space and
the only `ServerProtocol`.

The family ledger is scoped to joining. Hosting is a distinct phase and remains decomposed below
because some prerequisites are shared state-machine substrate rather than per-family work.

### The hosting blockers, located

1. **The configuration-phase handshake is hardwired into the connection state machine,
   not version-gated.** Handled inside `serve_connection_inner`
   (`crates/lodestone-server/src/server.rs`) — grep `ServerBound::LoginAcknowledged`:
   `LoginAcknowledged` → `State::Configuration` → `begin_configuration()`,
   and `ServerBound::ConfigurationFinished` → `State::Play` → `begin_play()`. A pre-1.20.2
   client sends **neither packet** — they do not exist on its wire; after Login Success it
   transitions straight to Play — so a legacy host connection stalls in Login forever.
   And the fix cannot live inside a family: `ServerProtocol::decode`
   (`crates/lodestone-server/src/protocol.rs`) is strictly one-packet-in, one-`ServerBound`-out, so a family cannot
   synthesize the two transitions from packets that never arrive. This is **one-off
   substrate** (legacy login transition), touching two choke-point files once, after which no per-family work
   recurs.
2. **A pre-1.13 host needs a flattening inverse; a pre-1.14 host needs light in-chunk.**
   Flattening is not symmetric: modern states must be *lowered* to `(id, meta)`, and the
   lossy cases are decisions, not transforms (legacy state inverse). Pre-1.14 clients expect light inside
   the chunk packet rather than as separate packets, so the host must fabricate or carry
   light at encode time (legacy chunk lighting).
3. **v26-2's own hosting is only 21/69 connected** (the two-file join above). Widening that
   is per-packet work on the existing family, orthogonal to this epic's units, but any
   "host version X" claim inherits whatever the shared `server.rs` loop actually consumes.
4. **CLOSED — `resource_pack_push/pop` is now handled in both Configuration and Play.**
   The configuration handler in `crates/versions/26.2/src/adapter/connection.rs` has decode arms for both
   `configuration::clientbound::RESOURCE_PACK_PUSH` and `RESOURCE_PACK_POP`, each carrying a
   comment explaining that required packs can arrive during Configuration, alongside the pre-existing
   Play-state arms. Both decode into `ClientEvent::ResourcePackPushed`/`Popped` the same as the
   Play arms, and the serverbound response encoder was already state-aware. This blocker is
   gone: `v1-20-6`'s configuration-phase machinery can imitate v26-2's without inheriting the
   hole. Grep `RESOURCE_PACK` in `connection.rs` to re-confirm — four arms (two Configuration,
   two Play) is the signal this stays closed.

### Per-version split

Join cost is the family map in the next section (U-units). Host cost, per version, is the
v26-2 baseline **plus** the rows below. "H" units are defined after the phase-1 unit list.

| version(s) | join unit | host needs beyond a per-family `server_protocol.rs` + `SERVER_FAMILIES` entry (first legacy host-shaped) |
|---|---|---|
| 26.2 | done | baseline (21/69 connected and growing — blocker 3) |
| 1.21.11, 1.20.5–1.20.6 | `v1-21-11`, `v1-20-6` | registry-sync/component **down**-conversion; config phase exists on these wires, so no legacy login transition |
| 1.19.4 | `v1-19` | legacy login transition (no config phase); host-side chat-signing enforcement decisions |
| 1.17.1–1.18.2 | `v1-17` | legacy login transition; height/biome down-conversion |
| 1.14.4–1.16.5 | `v1-14`, 1.14 canonicalisation | legacy login transition; state-id down-mapping (inverse of 1.14 canonicalisation's table) |
| 1.13.2 | `v1-13` | legacy login transition; legacy chunk lighting (light in-chunk) |
| 1.7.10–1.12.2 | `v1-7`, `v1-8`, `v1-9`, evidence capture | legacy login transition; legacy state inverse (flattening inverse, lossy); legacy chunk lighting; item component→NBT lowering |

**Value ranking for phase 2:** hosting 1.8.9/1.12.2 clients (legacy friends joining a LAN
world) is the plausible demand; hosting 1.15.2 clients is not. So phase 2, when it opens,
starts at legacy login transition + one pre-1.13 family, not at the version nearest 26.2. Phase 2 opens after
[The integrated and dedicated server](../dedicated-server.md)'s migration lands — `lodestone-server` currently has live agents
mid-migration, which is itself a reason legacy login transition is the *only* server-side edit this plan
schedules early.

## The decision: family-per-wire-era, range extension inside an era

**Not fifteen new crates.** The unit of work is a *family crate per wire era*, and inside
an era, additional versions are per-protocol generated tables plus branch points — a
change to that family only, invisible to the registry.

Evidence, not preference:

1. `docs/roadmap/protocol.md` measured the irreducible cost of a new family in
   `adapter.rs` and `chunk.rs`, and
   recorded that the `xtask new-version` cloning experiment produced "a 1.12.2 client
   wearing 1.16 packet IDs" — now structurally mitigated by the `SHAPE_REVIEW.toml` gate,
   but the lesson stands: cloning each family would multiply near-duplicate wire code.
   `docs/plans/multi-version-protocol-dedup.md` measured the smallest real family (v1-14) at
   5,139 hand-written lines via `cargo xtask codegen-ratio`; the *qualitative* argument (a clone
   is near-duplicate wire code) still holds, but the historical magnitude is not a current basis
   for sizing.
2. Adjacent-version deltas inside an era are small and *table-shaped* (packet id
   renumbering, a handful of shape changes), which is exactly what the generated
   `packet_ids.rs` per version already expresses — `xtask gen-packet-ids --source
   minecraft-data` parses any covered version's `protocol.json` today.
3. Widening a family costs the registry nothing: `adapter_for_protocol` delegates to each
   family's `supports`. Adding a whole new family costs one optional dep line, one feature
   line, one `FAMILIES` entry (verified against `crates/lodestone-registry/src/lib.rs`).
   **So the fifth family is cheap at the registry while the crate cost remains material — the
   seam scales; what does not scale without the multi-protocol seam is multi-protocol families.**
4. The era boundaries below are the epic's own expensive discontinuities (Flattening,
   light split, dynamic height, 3-D biomes, chat signing, configuration phase,
   components). Crossing one inside a single crate is where the wearing-wrong-packet-IDs
   failure lives; staying inside one is cheap.

**The resulting family map** (protocol numbers from the derived table,
`crates/lodestone-registry/src/generated/version_table.rs` — never hand-derived).
**Superseded for grouping decisions**: apply
[`multi-version-protocol-dedup.md`](./multi-version-protocol-dedup.md)'s era-grouping and
range-validation criteria before extending these families. This table is retained for the detailed
family constraints below.

| family | protocols | versions | status |
|---|---|---|---|
| v1-7 | 5 | 1.7.10 | capture-led, last in sequence; no `minecraft-data`, cached jar, or modern wire shortcut |
| v1-8 | 47 | 1.8.9 | implemented legacy canonicalisation bridge |
| v1-9 | 110, 210, 316, 340 | 1.9.4–1.12.2 | grouped family with four protocol tables and a canonical-state bridge |
| v1-13 | 404 | 1.13.2 | native-block-model boundary |
| v1-14 | 498, 578, 754 | 1.14.4–1.16.5 | grouped family with a chunk-biome branch and direct name/properties canonicalisation bridge |
| v1-17 | 756, 758 | 1.17.1, 1.18.2 | split only if the chunk paths fail the grouping threshold |
| v1-19 | 762 | 1.19.4 | signing state-machine family |
| v1-20-6 | 766 | 1.20.5–1.20.6 | configuration phase and item components |
| v1-21-11 | 774 | 1.21.11 | closest join family to the canonical host; remains separate to keep hosting simple |
| v26-2 | 776 | 26.2 | done, canonical host family |

Seven new crates instead of eleven; four multi-protocol groupings, each inside one era.

**The negotiated-protocol seam is implemented**; see
[`multi-protocol-seam.md`](../multi-protocol-seam.md):
`Family::make` is now `fn(i32) -> Box<dyn VersionAdapter>`, each family exposes
`PROTOCOLS` + `adapter_for(protocol)`, and the registry's drift-guard test constructs
every family at every protocol it claims. It touched neither `net.rs` nor `app.rs`, as
predicted — the shell already passed the negotiated protocol to `adapter_for_protocol`.
The grouped `v1-9`, `v1-14`, and `v1-17` families are unblocked on this axis.

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
ways ([The integrated and dedicated server](../dedicated-server.md)), so ask what *produces* a serverbound action as well as
what consumes a clientbound event — `SetFlying` was encoded by four adapters with zero
producers.

## Canonicalisation: the layer that is the actual work

The architecture is fixed on one canonical internal version (26.2), a
translation layer per protocol. The survey's largest finding — **legacy families skipping
the translation while their suites stay green** — is now down from two live instances to
**zero**: v1-8 now canonicalises raw `(id << 4) | meta` before it enters the
palette, avoiding the former bedrock-as-lava failure; v1-14 likewise canonicalises
its 1.16.5-space flat ids before they enter the mesher and collision
that consume 26.2 ids — bedrock meshed as birch sapling. Any plan that adds a new family
should still canonicalise from day one rather than repeat this defect an eighth time.

**What exists:** `lodestone-canonical` holds the shared implementation and v1-9 re-exports it.
`flattening.rs` (API) sits over `generated/flattening.rs`
(9,076 lines): 4,095 `id:meta` slots, 1,663 resolved, 2,400
no-entry, 32 `RequiresAdditionalContext`; provenance is a reflective dump of the **real
1.13.2 jar's own state-conversion registry**, regenerated via `LODESTONE_REGEN=1` against
`tests/support/flattening_1_13_2_jvm.txt` ([Registries](../registries.md)). Plus
`canonical.rs`: 1.13-name → 26.2-state bridge with the rename pass,
`waterlogged` defaulting, and an `Unmapped` drift-guard variant
([Registries](../registries.md)). Item-side
flattening: **zero item content in the crate** — recorded but not
built; that scope is item canonicalisation's, intact.

**Generalisation, two regimes with the Flattening as the boundary:**

- **Pre-1.13 families (`v1-7`, `v1-8`, `v1-9`):** all speak `id:meta`. The dumped table is
  the 1.13.2 reference conversion table, which upgrades *1.12.2-space* ids; older versions' id space is a
  strict subset (ids were only added), so the same table serves all four — per-version
  difference is which slots are populated, which the existing `NoTableEntry` outcome
  already expresses. This forced a doctrine call: the roadmap records v1-8 was
  *deliberately* denied v1-9's table to preserve per-crate deletability. **Decided and
  executed: `flattening` + `canonical` live in `lodestone-canonical`, and v1-8 is its second
  consumer.** Deletability applies
  to *families*; shared game data already has a precedent crate (`lodestone-data`, an
  earlier extraction), and the alternative is four copies of a 9k-line generated table
  drifting independently. Deleting a family remains folder + dep line + feature line
  (`xtask check-deletable` verifies exactly this).
- **Post-1.13 families (`v1-13`, `v1-14`, `v1-17`, `v1-19`, `v1-20-6`, `v1-21-11`):** each speaks its own
  version's block-state id space. The mapping is per-version: dump each version's
  `blocks.json` by running **its own jar's** data generator (the jars are in `.cache/mc/`;
  the generator ships in the server jar from 1.13 onward — verify per jar as part of each
  unit, do not assume the invocation is uniform), then resolve name+properties → 26.2
  state id through a **DFU-walk oracle against the 26.2 jar**, keyed by the source
  `data_version` from the version table. The 26.2 jar contains every fixer from every
  older data-version transition — so this replaces `v1-9`'s hand-written
  rename pass with the same outside-our-code provenance the flattening table already has.
  New program in `oracle-java/` (pattern:
  `crates/lodestone-data/tests/{collision_shapes,hardness}.rs` generate-or-assert +
  `LODESTONE_REGEN=1`). minecraft-data is a cross-check only; the v1-9 cross-check
  measured 91.8% agreement with all 137 disagreements resolving in the jar's favour.

**The 1.12.2→1.13.2 boundary, costed explicitly:** for inbound blocks it is *already
mostly paid* — the table is built and wired. Remaining boundary cost:
(a) the 32 context-dependent slots (need TileEntity/neighbour context; currently
resolve-or-air); (b) **item flattening, entirely unbuilt** — the ~300-entry item table
needs the same reflective dump before any pre-1.13 family can decode inventories into
canonical items; (c) the whole outbound direction (26.2 → id:meta is lossy; phase 2, legacy state inverse).
New cost at this boundary for `v1-13` itself is *low* — 1.13.2 is the first version whose
block model matches ours natively; its chunk format and command tree are ordinary era
work, not Flattening work.

**Items pre-1.20.5 (families through `v1-19`):** NBT → data components, both directions
lossy per the epic. Inbound scope: map the NBT the client actually renders (display name,
lore, enchantments, damage) and preserve-the-rest; full fidelity is not required to join.

## Data sources, per version

Authority order per child issues: real oracle jar (strongest) → jar's generated reports →
minecraft-data (cross-check only, never authority). Protocol/DataVersion numbers: always the
derived table, never hand-written. The lesson
binds every registry-sync unit here: a generator file is authoritative about
registry *contents*, not about which registries are *sent* — the sent-list comes from the
jar (derive the authoritative ordered sent-registry list from each release's wire behaviour,
not from `registries.json`).

| versions | packet shapes | ids/registries/states | jar on disk? |
|---|---|---|---|
| 1.7.10 | **captures only** — no minecraft-data, nothing else exists | captures + the jar itself | **no — fetch** (`xtask version-table --fetch-missing`) |
| 1.8.9–1.12.2 | minecraft-data `protocol.json`, cross-checked by live capture | 1.13.2 DFU flattening table (blocks); item dump pending (item canonicalisation) | 1.8.9, 1.12.2 yes; **1.9.4/1.10.2/1.11.2 no — fetch** |
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
  [Oracles and benchmarks](../oracles-and-benchmarks.md).
- **Concurrency budget**: ≈1.1–1.3 GB resident per running oracle, 16 GB box, many agents
  live — **run one legacy oracle at a time, stop it when the gate passes**. Oracles are
  not repo state; scripts recreate them.
- **Capture once, commit, replay forever.** Per version, one scripted session (login →
  join → chunk load → RCON-driven `setblock`/`fill`/`summon` at *known coordinates* →
  disconnect) recorded to `crates/versions/<fam>/tests/support/<ver>_join_capture.bin`
  and committed. The always-run test decodes the committed capture and asserts against the
  RCON-known values — expected values originate outside our code, satisfying the evidence
  standard without a running server. Live `#[ignore]`d gates re-run the capture script and
  are needed only for serverbound effects (dig a block, observe the update come back),
  which a replay structurally cannot exercise.
- **The host direction inverts the oracle**: the external artifact is the **real release
  client jar** of that version (`xtask fetch-assets` fetches client jars), joining *our*
  server. Our encoder cannot grade itself; the reference client rendering the world is the
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
`--source minecraft-data` for a legacy family), and rejects `--target-dir`. Isolation and
deletability now pass; the remaining combined-command blocker is the unrelated workspace-wide
`check-connected` census, so report that separately from family-specific conformance.

**evidence capture — pay the standing evidence debt (dispatch first, smallest unit).**
Run `scripts/live-oracles/legacy-1.12.sh`; against it, verify `multi_block_change`'s
`horizontalPos` nibble order, currently sourced from external wire docs only — flagged in
`crates/versions/1.9/src/adapter.rs` and `tests/block_updates.rs`. Method: RCON `fill` an
asymmetric pattern (different extents in x and z, e.g. 3×1) inside one chunk in one tick,
capture the packet, assert decoded relative coords match the RCON coordinates. **Negative
control:** decode the same capture with the nibbles swapped and require the assertion to
fail — if it doesn't, the pattern was symmetric and the control is vacuous. Owns:
`crates/versions/1.9/tests/block_updates.rs` (new live test), the captured fixture.
Consumer: the existing decode arm. Also promote the capture to the committed-fixture
pattern above.

**Canonical-state foundation — implemented: `crates/lodestone-canonical`.** The dispatch-time decision
resolved as a new shared crate, not a `lodestone-data` module. v1-9 re-exports through it;
the flattening drift-guard suite and `flattening_1_13_2_jvm.txt` moved with it;
[Registries](../registries.md) documents the crate. Item canonicalisation was **not** part
of it — the crate has zero item content, confirmed by a source-tree content audit, so item
canonicalisation's scope
is intact.

**Multi-protocol seam — implemented.** See "The seam line" above for the
landed shape (`protocols` slice + `make: fn(i32)`, registry drift-guard, v26-2
deliberately single-protocol) and `docs/multi-protocol-seam.md` for the full record. The
per-protocol-table negative control ("a family constructed for protocol A must select
A's table when B is in its set") becomes exercisable only when the first *grouped* family
exists — it is part of the `v1-9` family's gate, not a retroactive gate for the multi-protocol seam.

**1.8 canonicalisation retrofit — implemented in `v1-8`.** One gate variation worth recording for the benefit of 1.14 canonicalisation: instead
of the planned live-oracle RCON gate, evidence came from a **real 1.8.9-written world
save** — `tests/support/real_1_8_9_section_save.txt`, extracted by
`oracle/extract_real_section.py` — so expected values still originate outside our code,
at lower cost than a live container. The reverted-fix measurement (the gate's own failure
output): 1.8 stone `1:0` had been decoding as `minecraft:spruce_planks`, bedrock `7:0` as
**lava**. The `v1-8` canonicalisation gate is the durable record.

**1.14 canonicalisation retrofit — implemented in `v1-14`.** Same shape as 1.8 canonicalisation, different mechanism than originally
planned: this landed as a **direct name/properties bridge**, not the DFU-walk-against-the-
26.2-jar oracle this entry originally specified. Both sides' `(name, properties)` come
straight from release's own data-generator tool, invoked with its reports flag — the
1.16.5 side run fresh against `.cache/mc/1.16.5/server.jar` under Apple `container`
(`tests/support/blocks_1_16_5_jar.json`, committed), the 26.2 side already available via
`lodestone_data::block_states` — matched through a reverse index exactly like
`lodestone-canonical::canonical`'s pre-Flattening bridge: direct match, then a 3-entry
rename table, then two generic single-property fallbacks (`waterlogged=false`,
`powered=false`, each confirmed against the decompiled 26.2 source) and a cauldron
identity split. Zero unmapped states across the full 17,112-state corpus — no DFU/NBT walk
was needed because 1.16.5's report already gives real names/properties per state, unlike
the pre-1.13 `id:meta` table this pattern was modelled on. **A real DFU-walk oracle
remains unbuilt**; whether it is ever needed depends on whether a future post-1.13 family
(from `v1-14` through `v1-21-11`) hits a rename this direct-bridge technique cannot resolve — try the cheaper
direct-bridge pattern first and only reach for a DFU walk if that leaves unmapped states.
The reverted-fix measurement (the gate's own failure output): 1.16.5 bedrock (wire state
33) had been decoding as `minecraft:birch_sapling`, `minecraft:diamond_block` (wire state
3355) as `minecraft:warped_shelf`.

**item canonicalisation — both regimes.** Pre-1.13 item flattening (the unbuilt
~300-entry reflective dump, same pattern as blocks) + legacy-NBT → component mapping for
the render-relevant subset. Owns: the shared canonical crate's item module, its oracle
program, its dump fixture. Consumer: each family's inventory decode arms (wiring lands
with each family's unit; this unit ships the table + API and **one** consumer — v1-9's
inventory path — so it is not an island on day one). Gate: v1-9 live inventory test
asserting a chest item placed by RCON with known id:damage decodes to the canonical item
+ components. Negative control: an id:damage pair absent from the table must decode to
the explicit `Unmapped` variant, not to air/default — and prove the detector fires by
feeding a known-bad pair. Blocked by: nothing — canonical-state foundation landed; dispatchable now.

**`v1-9` family (1.9.4–1.12.2).** The era family carries four protocol tables; its
range-validation criteria live in `docs/plans/multi-version-protocol-dedup.md`.
Re-check that plan before dispatching this unit. The `v1-9` family is the first grouped
consumer of the multi-protocol seam's machinery; four generated `packet_ids` tables; pre-1.13
canonicalisation from day one via canonical-state foundation (never the v1-8 raw-palette shape). Scope: the
roadmap's irreducible protocol core (join flow, chunk, entity, block updates, movement)
× the era's specifics (offhand slot, attack cooldown as data, reshaped entity metadata —
index tables per version from minecraft-data cross-checked by capture; **never hand-count
an index**; run an oracle dump when in doubt). Owns: `crates/versions/1.9/` entirely +
registry 2-liner (brokered). Jars must be fetched first (three missing). Gate:
per-version committed-capture replay + one live join gate per protocol against the
parameterized oracle; negative control: the wrong-protocol handshake must be refused by
`supports`. Vacuous if: captures are generated by our own encoder — they must come from
the real server. The `v1-9` family also owns the multi-protocol seam's deferred negative control: an adapter constructed for
protocol A must select A's `packet_ids` table when B is in the family's set, asserted on
an id that differs between them. Blocked by: oracle script, jar fetch (canonical-state foundation and multi-protocol seam landed).

**`v1-13` family (1.13.2).** The boundary anchor: first native-block-model
family, new chunk format, command tree (reuse `lodestone-command`, the existing substrate).
State mapping via the first *small* conversion walk (1631 → 4903). Owns: `crates/versions/1.13/`
+ registry 2-liner. Gate/control: as the `v1-9` family, jar already on disk. Blocked by: 1.14 canonicalisation's oracle
pattern (not multi-protocol seam — single protocol).

**`v1-14` family (1.14.4–1.16.5).** The dedup plan's range-validation criteria govern the
intra-family branches; re-check that plan before dispatching.
Light out of the chunk packet
(1.14), biome array into it (1.15) — the intra-family branch is confined to `chunk.rs`.
Owns: `crates/versions/1.14/` + registry 2-liner. Blocked by: the `v1-13` family pattern (multi-protocol seam implemented).

**`v1-17` family (1.17.1–1.18.2).** Dynamic world height + 1.18
section-scoped paletted biomes. **Check what the chunk store and mesher assume about
section count before writing wire code** — if the store hardcodes 16 sections this unit
gains a prerequisite outside protocol land and must say so rather than absorb it. The
grouping most likely to split into two crates; the tell is `chunk.rs` sharing under 50%
between the two protocols. Owns: `crates/versions/1.17/` + registry 2-liner. Blocked by:
`v1-14` family (multi-protocol seam landed).

**`v1-19` family (1.19.4).** Chat signing: scope is *joining* — decode the
session/signature packets, send unsigned chat where the oracle permits
(`enforce-secure-profile=false` on our own oracle; document that joining strict servers
may need the full signature chain and leave that as a named follow-up, not silent scope
creep). Owns: `crates/versions/1.19/` + registry 2-liner. Blocked by: the `v1-13` family pattern.

**`v1-20-6` (1.20.5–1.20.6) and `v1-21-11` (1.21.11).** Two units, one
briefing: configuration-phase state machine (`v1-20-6` — the login flow structurally differs;
v26-2's own configuration handling is the reference implementation to imitate, not import —
**and v26-2's own Configuration-state `resource_pack` gap must be fixed first, or the
imitation copies the hole**) and components-era items (both; `v1-21-11` items are near-26.2).
`v1-21-11` is the cheapest new family in the set — closest wire to v26-2 — and its packet shapes
can come from its own jar's report if present. Owns: `crates/versions/1.20.6/`,
`crates/versions/1.21.11/` + registry 2-liners. Blocked by: the `v1-13` family pattern; `v1-20-6` also by item canonicalisation and
the v26-2 resource-pack decode gap.

**`v1-7` family (1.7.10), last in sequence.** No minecraft-data, no cached
jar, pre-compression pre-UUID wire; every shape from captures against a fetched real jar.
Budget it as the most expensive single family (the epic agrees) and do not let it block
anything — nothing depends on it. Owns: `crates/versions/1.7/` + registry 2-liner.

**Era-family naming.** Folder names use the era-start Minecraft version and package/feature names
use the corresponding current family label, such as `crates/versions/1.14` and
`lodestone-v1-14`; protocol numbers remain wire data, not family names.

## Units of work — phase 2 (host)

Phase 2 opens after the server-ECS migration lands. **Exception: legacy login transition is the substrate that
should land as soon as the `lodestone-server` choke-point calendar allows**, because it is
tiny, independently gateable with fakes, and every legacy host unit is blocked on it.

**legacy login transition — version-gate the login→play transition.** Add a defaulted
`ServerProtocol::has_configuration_phase(&self) -> bool { true }`; in the connection
loop, when it answers false, run the Configuration→Play sequence immediately after
`login_success` instead of waiting for `LoginAcknowledged`/`ConfigurationFinished` (which
pre-1.20.2 wires cannot send — the transition lives in `serve_connection_inner`
(`crates/lodestone-server/src/server.rs`), decode contract in `ServerProtocol::decode`
(`crates/lodestone-server/src/protocol.rs`)). Owns: `crates/lodestone-server/src/protocol.rs` (trait),
`crates/lodestone-server/src/server.rs` (transition) — **both are live-agent choke
points; the orchestrator schedules this as a solo slot.** Gate: a fake protocol with
`has_configuration_phase() == false` reaches Play and receives chunks without either ack
packet. Negative control: the default-true fake must **not** reach Play without them.
External evidence: none at this layer — this is a state machine against our own trait;
the external oracle arrives with first legacy host's real client. Said plainly rather than inventing a
round trip.

**serverbound connectivity (v26-2).** Ongoing per-packet work
on the existing family; each arm is the two-file join (variant in
`v26-2/src/server_protocol.rs`, consumer arm in `server.rs`). **Blocked on gameplay, not
protocol, for most arms** — a prior survey established the majority strand because the
gameplay behind them (recipe book, beacons, command blocks, jigsaw, trades, …) is
unimplemented, so "wire up the arm" is not the unit of work and counting these against
protocol coverage inflates the plan. Not a unit of this epic — tracked per-packet
elsewhere — but every host-version claim inherits it, so it is named here to stop it
being rediscovered.

**legacy state inverse — flattening inverse (26.2 state → `id:meta`).** Extends the canonical-state foundation crate. The resolvable
direction is mechanical: invert the JVM-dumped forward table (this is *not* the
`decode(encode(x))` trap — the forward table's provenance is the 1.13.2 jar, outside our
code). The lossy remainder — modern states with no pre-1.13 representation — is a
**decision table, hand-curated, reviewed case by case**, and it cannot be externally
evidenced: no oracle knows what waxed copper "should" look like to a 1.12.2 client. What
*can* be evidenced externally: (a) round-trip on the 1,663 resolved slots,
`inverse(forward(id:meta)) == id:meta`; (b) first legacy host's real-client gate rendering a world
containing sampled lossy states without disconnect or visual holes. Owns: the shared
canonical crate's inverse module + decision-table fixture. Blocked by: nothing on this
axis (canonical-state foundation landed) — only phase-2 scheduling.

**legacy chunk lighting — pre-1.14 light-in-chunk fabrication.** Pre-1.14 clients expect light nibble arrays
inside the chunk packet. **Open question the unit must establish first, not assume: what
light does the v26-2 host currently compute or send?** If the server has no real lighting
engine, the fabricator's scope is "full-bright plausible light", explicitly labelled.
Owns: the hosting family's `server_protocol.rs` encode path (per-family, after first legacy host's first
instance). External evidence: the real legacy client renders the world non-black — a
screenshot-level gate against the reference client, which is the consumer.

**first legacy host (recommend v1-9).** The canonical bridge and legacy state inverse both live
there. A `server_protocol.rs` in the family crate + one `SERVER_FAMILIES` entry.
Owns: `crates/versions/1.9/src/server_protocol.rs`, registry 2-liner (brokered). Gate —
the strongest external oracle in this plan: **the real release 1.12.2 client** (fetched
via `xtask fetch-assets`) joins our hosted world and a scripted probe verifies chunks
render and a placed block appears at known coordinates. Negative control: a
wrong-protocol client must be refused at handshake with a version-mismatch message, not a
stall. Blocked by: legacy login transition, legacy state inverse, legacy chunk lighting (for its light), and the server-ECS migration settling.

## Order

```
phase 1 (join):
implemented foundations: canonical-state crate, negotiated-protocol seam, 1.8 canonicalisation,
      and the 1.14 direct name/properties bridge
open now, disjoint files, dispatchable in parallel:
evidence capture (evidence debt — smallest)   item canonicalisation (items)
then:
`v1-13` (protocol 404) ─ `v1-14` (protocols 498/578/754) ─ `v1-17` (protocols 756/758)
     `v1-19` (protocol 762)   `v1-20-6` (protocol 766; needs item canonicalisation + the configuration resource-pack path)   `v1-21-11` (protocol 774)
`v1-9` (protocols 110/210/316/340) — needs oracle script + jar fetch, nothing else now
`v1-7` (protocol 5; 1.7.10) — last, depends on nothing, nothing depends on it

phase 2 (host), after server-ECS lands:
legacy login transition (state-machine gate; may land early in a brokered solo slot)
legacy state inverse (flattening inverse, extends the canonical-state foundation crate) ─┬─ first legacy host (v1-9, real-client gate)
legacy chunk lighting (light fabricator) ───────────────────────┘
then per-family: server_protocol.rs + SERVER_FAMILIES entry each
```

Open and parallelizable now: evidence capture, item canonicalisation (disjoint files; the 1.14 bridge
is already available). The registry 2-liners and
the workspace-member lines are the only cross-unit file contention; broker them. Legacy login transition
contends with the live `lodestone-server` agents and is scheduled by the orchestrator,
not grabbed.

## Risks

1. **Canonical-state integrity is a standing risk.** Every joinable family must canonicalise before
   meshing or collision, and every family ledger entry retains canonical output plus a raw-value
   negative control.
2. **The negotiated-protocol seam needs its drift guard.** A new family bypassing `adapter_for` or restating
   its protocol list in the registry would reopen it; the drift-guard test is the
   detector.
3. **Evidence supply is the long pole, not code.** Four jars must be fetched; per-jar data
   generators and packet reports must be *verified per jar, not assumed*; captures must
   come from real servers (the `decode(encode(x))` trap); the box affords one live oracle
   at a time. Mitigation: the capture-once/replay-forever pattern makes live time a
   one-shot cost per version; evidence capture proves the whole capture loop on the family that already
   exists.
4. **Host-direction lossiness has no oracle.** legacy state inverse's degradation choices are decisions;
   the only external check is a real legacy client tolerating the result. Budget review
   time for the decision table and do not let a round-trip test impersonate evidence.
5. **Counts are instruments, not documentation.** Connectedness, ignored-arm, and source-line
   counts change with each route. Re-run `cargo xtask connectedness` and the relevant measurement
   tool before quoting a value.

Secondary: the `v1-17` grouping may split (tell named in the `v1-17` family); chat signing scope creep
(named in the `v1-19` family); the 32 context-dependent flattening slots are consciously deferred, not
forgotten.
