# Structures delivery plan

## What it is

The executable unit sequence for unit group **S** of
[`worldgen-rewrite.md`](./worldgen-rewrite.md) (its U14 row): the structure engine —
placement, templates, beardifier, jigsaw and the coded piece generators — turned from the
rewrite plan's phase sketch (S0–S4) into landable units, each with its gate, its control, its
outside evidence source and its cost stated as a counter against the serve-path budget. It is the
structure-specific companion to [`worldgen-rewrite.md`](./worldgen-rewrite.md).

## Current evidence and assumptions

The following census and constraints are the evidence base for the units below:

- **The data phase (the old S2 extraction) is DONE.** Measured by `find … | wc -l`:
  34 structures, 20 structure sets, 188 template pools, 40 processor lists under
  `crates/lodestone-server/assets/worldgen/`, plus **1,212 `.nbt` templates under
  `crates/lodestone-server/assets/structure/`** — note *not* under `assets/worldgen/`, which
  is where a scoped `find` measures zero. The corpus is byte-identical to the distribution archive.
  The only Rust reader is the drift gate
  `crates/lodestone-server/tests/worldgen_structure_corpus.rs`.
- **Structure-set salted seeding is implemented.**
  `WorldgenRandom::set_large_feature_with_salt` in
  `lodestone-worldgen-core/src/rng/mod.rs` uses
  `x * 341873128712 + z * 132897987541 + seed + salt`, then reseeds the generator. The
  random-spread placement calls it. Concentric-ring placement uses its own XOROSHIRO stream and
  does not use the salted large-feature derivation.
- **The beardifier is a production fill dependency.** The structure module's beardifier receives
  adaptation-bearing starts from `structure_refs`; `OverworldGenerator::pre_ore_stage` consumes it
  through `beardifier_for` before `fill_stage`. Structure-free chunks return an empty beardifier.
- **Terrain adaptation is carried by 11 of 34 structures**, censused from the bundle
  (`python3` over `assets/worldgen/structure/*.json`): `beard_thin` — the 5 villages,
  `pillager_outpost`, `nether_fossil`; `beard_box` — `ancient_city`; `bury` — `stronghold`,
  `trail_ruins`; `encapsulate` — `trial_chambers`. **Every carrier is jigsaw except
  `stronghold` (coded) and `nether_fossil` (template, Nether-only).** This reorders the
  S3/S4 dependency — see [S3 — beardifier: production seam and composed gate](#s3--beardifier-production-seam-and-composed-gate-m).
- **Structure families are three, not two.** Bundle `type` census: `minecraft:jigsaw` × 10
  (5 villages, bastion_remnant, pillager_outpost, ancient_city, trail_ruins,
  trial_chambers); template-driven coded types (`ruined_portal` × 7, `shipwreck` × 2,
  `ocean_ruin` × 2, `end_city`, `nether_fossil`, `igloo`); and **fully-coded piece
  generators** (`mineshaft` × 2, `stronghold`, `ocean_monument`, `fortress`,
  `desert_pyramid`, `jungle_temple`, `swamp_hut`, `woodland_mansion`, `buried_treasure`).
  The rewrite plan's S2-templates/S4-jigsaw split has no home for the third family; it gets
  its own unit (S5).
- **A vanilla-authored block-and-metadata oracle already exists on disk.**
  `.cache/mc/survival/world` (seed **-195764831**, read from
  `world/data/minecraft/world_gen_settings.dat` — 26.2 does *not* keep the seed in
  `level.dat`) holds **14,499 generated overworld chunks in 29 region files**, written by the
  real vanilla 26.2 server. Its per-chunk NBT carries full `structures.starts` (piece lists
  with template names, positions, rotations, bounding boxes) and `structures.References`.
  Start census, measured by scanning every region: mineshaft 46, ocean_ruin 16,
  trial_chambers 13, ruined_portal 9, shipwreck 11, monument 2, village 2, trail_ruins 1,
  buried_treasure 2 (**102 total starts**). **Absent from the generated area**: stronghold, desert_pyramid, igloo,
  swamp_hut, jungle_temple, pillager_outpost, ancient_city, mansion. This census is the
  *world-species* precondition ledger for every gate below.
- **The persistence path serializes structure starts and references.**
  `structures_to_nbt` (`crates/lodestone-server/src/chunk_nbt.rs`) writes completed starts with
  piece metadata and chunk references, while retaining empty compounds for chunks with no
  structures.
- **There are 13 parity binaries, not 11**: 11 `*_parity.rs` under
  `crates/lodestone-worldgen/tests/` plus `overworld_gen.rs` plus
  `crates/lodestone-worldgen-parity/tests/chunk_parity.rs`. Every unit below preserves all
  13 green plus the composed `fixtures/composed_seed42.txt` gate.
- Assumed, not verified: that vanilla's start NBT encoding for each structure type
  round-trips through our NBT reader without loss. The reader is independently gated
  (`chunk_nbt_vanilla_oracle.rs` reads the same world), so treat a decode surprise as an S1
  finding, not a blocker.

## The evidence position (read before writing any gate)

There is **no JVM on this machine** (`java -version` fails). The constraint is narrower than
"no new vanilla-origin evidence, ever":

1. **The survival world is vanilla-authored evidence that already exists** — produced by the
   real server in July, bind-mounted at `.cache/mc/survival`. Reading it produces no new
   fixture; it consumes an existing outside source, exactly as
   `chunk_nbt_vanilla_oracle.rs` already does (`#[ignore]`d, "a real 26.2 world this repo
   did not write"). It is the primary oracle for this whole group.
2. **The JVM-oracle path never needed a host JVM.** `scripts/worldgen-oracle/run.sh` runs
   `eclipse-temurin:25-jdk` under Apple `container` ([Oracles and benchmarks](../oracles-and-benchmarks.md) — Docker is
   gone from every oracle path). Measured while writing this plan: `container list` shows
   the runtime **up and `lodestone-survival` running**. So new fixtures and *extending the
   oracle world's generated area* (to obtain the 8 missing structure types) appear
   available whenever a unit wants them. Each unit below is designed to land without that
   (belt), and names the container-runtime strengthening explicitly (suspenders), so the
   plan survives the runtime being down again.
3. **The cached 26.2 behavioral reference** is the record definition for every algorithm.
   Read the definition rather than inferring its order from a call site.

The placement engine and the persisted output for the same seed are independent constructions of
one physical rule: they are authored by different implementations and share nothing.

## Where structures slot into the pipeline (the S0 answer)

The reference generation-stage order is `… STRUCTURE_STARTS → STRUCTURE_REFERENCES → BIOMES →
NOISE …`: starts precede noise **because the beardifier
reads structure bounds during fill**. In our engine, fill lives inside
`OverworldGenerator::pre_ore_stage`, memoised by the staged store
(`overworld/store.rs`; slots declared in `overworld/mod.rs`, and the store's own rule is
"add a stage *above* the ones it consumes").

Concretely:

- The store has `structure_starts` (topmost) and `structure_refs` `StageSlot`s.
  `starts(C)` is a pure function of `(seed, C)` — placement math plus
  climate-sampled biome checks plus piece generation, no chunk data — so it is
  embarrassingly parallel and safely memoised. `refs(C)` consults `starts` over the
  **17×17** neighbourhood (the structure-reference stage has radius 8) and keeps the
  starts whose bounding boxes come within 12 blocks of `C` (the beardifier's own
  close-to-chunk reach, from its own per-chunk structure lookup).
- `pre_ore_stage(C)` reads `refs(C)` to build the per-chunk beardifier context before fill. This
  **inverts the terrain-first intuition** exactly as
  the rewrite plan warns; the join scheduler (U10, [Accounts, join, and chat](../accounts-and-join.md)) grows one more
  leading wavefront, serial depth +1 stage, halo +8 chunks of *starts only* (cheap — no
  block work in a start).
- **What it costs, as counters, against C_ss (the full pipeline's documented release baseline is
  853.5 ms/chunk; this unit's contribution must be measured separately):** `starts` runs exactly
  once per chunk (the store's `OnceLock` invariant, already counter-gated at 256/196
  granularity for the existing stages); per chunk it is ~20 structure-set placement draws
  plus rare piece generation on candidate chunks. `refs` is ≤289 store probes, each an
  `Arc` clone. Neither touches a block. The counter acceptance is written per unit below;
  the headline invariant is **`beardifier_evals == 0` and zero added allocations for any
  chunk with no adaptation-bearing start** — exact, not approximate.

The full ticket/status pipeline is **not** a prerequisite: the store's stage slots plus the
scheduler's dependency edges already express the ordering fact structures need. Chunk lifecycle
work is separate from this plan.

## Unit sequence

Costs use the rewrite plan's scale (S ≲ 1 session, M ≈ 1–2, L ≈ 3+). Every unit: `just
health` green, all 13 parity binaries green, composed fixture byte-identical (except where
a unit says otherwise and proves why), and no unit lands as an island — its consumer is
named in the unit.

### S1 — implemented placement, starts/refs stages, and persisted `structures` (M)

S1 is wired end-to-end: placement, staged starts/references, structure persistence, and the
`crates/lodestone-worldgen/src/structure/` module all have production consumers.

**Implemented behavior.** (a) `set_large_feature_with_salt` on `WorldgenRandom` — the five-line
formula above. (b) random-spread placement (linear + triangular spread, salt,
`frequency_reduction_method`, exclusion zones, per-set weighted structure selection) and
concentric-ring placement (stronghold rings). (c) Per-structure *start
predicates* — the biome check via the climate sampler (pure) plus any structure-specific
pre-piece draws (mineshaft's probability draw lives here) — behind the structure registry in
`crates/lodestone-worldgen/src/structure/`, where an unsupported generator records an **incomplete
start** with an empty piece list and its reason in `StructureRegistry::unsupported`
(the legible-silence pattern from `unsupported_placed_ref`, `feature/vegetation/config.rs`). It
remains visible through `OverworldGenerator::structure_starts_including_incomplete`, but is never
persisted or block-placed. (d) The two store slots and the `pre_ore` edge, whose refs build the production
beardifier context. (e) `structures_to_nbt` (`chunk_nbt.rs`) writes real `starts`/`References`
NBT and the singleplayer save path persists it.

**Gate.** For every chunk of the survival world's 29 regions: our computed start set for
seed -195764831 equals vanilla's persisted `structures.starts` — structure id, start chunk,
and, for generators with complete pieces, bounding boxes — for each structure type on the
**implemented ledger**; and our `References` equal vanilla's for those types.
Expected values are vanilla-authored; nothing round-trips through our encoder.
**World precondition, asserted not assumed**: the gate first counts starts per type in the
oracle NBT and fails if any gated type has zero (the census above says mineshaft 46 …
buried_treasure 2); a type with zero oracle instances may not sit on the ledger.

**Control, observed failing.** Re-run one gated set with its salt perturbed by +1; the gate
must report mismatches with a per-region bounding box of differing chunks. Run it, record
the failure, then remove. This is the detector-works control for "our placement equals
vanilla's" — without it, an accidentally-empty ledger passes vacuously (the
assertion-species trap), so the gate also asserts a floor on total compared starts (≥102,
the measured census total).

**Cost counter.** `structure_start_draws` per chunk (predict: ~2–6 per set × 20 sets;
calibrate the exact figure on one chunk by hand from the placement configs, per the U1
calibration rule); `refs` probes ≤289; zero heap allocations in the no-start steady path.
C_ss re-measured after landing; budget: the added median cost must be invisible next to the
ore stage's 6,566 µs — if it is not, the unit's own counters say which half.

**RNG order is the specification**: placement draws happen per set in structure-set
iteration order over the salt-seeded stream; a plausible-looking placement that draws
differently is a different world at the same seed. The gate is exact equality, so this is
enforced, not aspirational.

**Stronghold honesty**: the oracle area contains no stronghold (nearest ring starts
~1,280 blocks out; the generated area does not reach a start). The ring *math* lands in S1
gated only by vanilla's own concentric-rings placement record and generator-side structure-state
tracker, and a hand-computed first-ring fixture; its
vanilla-equality gate is a named deferred obligation until the oracle world's area is
extended under `container` (a one-command teleport session). Do not report stronghold
placement as verified until that runs.

### S2 — template engine + the template-coded family (L)

The NBT structure-template loader (blocks, palettes, entities, **jigsaw block entities** —
which S4 needs as its connection graph, so this loader is shared infrastructure), placement
semantics (vanilla's own template-placement routine: rotation, mirror, pivot, `structure_void`
transparency, water-loggable handling), processor lists (all 40 bundled documents must
parse; `rule`, `block_rot`, `block_ignore`, `capped`, … — unknown types fail loudly), and
the template-coded structures with oracle presence, in order of oracle richness:
**shipwreck (11), ruined_portal (9), ocean_ruin (16)**. Igloo, end_city, nether_fossil wait
(zero overworld oracle instances; nether_fossil and end_city belong to NE-adjacent gates).

**Gate.** For each oracle start of a gated type: piece metadata (template name, position,
rotation) equals vanilla's start NBT — this is exact and block-free; then, per piece, every
position the template *writes* (the non-`structure_void` set, derived from the template NBT
itself — jar-authored, an outside source) compares byte-equal against the vanilla region
blocks. Failure output prints the piece bounding box and per-box differing-cell counts —
location, not fraction. **World precondition**: assert the compared-position floor per
structure from the template's own block count, and assert the region NBT actually contains
the start (the world-species lesson: a fixture that cannot contain the defect measures
nothing).

**Known confounders, stated now**: ruined portals draw lava/netherrack degradation through
`block_rot`-family processors whose RNG is positional — draw order is spec; and portals
choose placement height against heightmaps we compute at generation time (fine) but
partially-buried variants read terrain that our world generates *without* ore veins and
with 3 of 11 decoration steps — so the comparison set must exclude positions where the
template preserves existing terrain. The gate computes the written-position set from the
template + processor semantics, never from "what matched".

**Control.** Force rotation by one step on one oracle shipwreck: the metadata gate and the
block gate must both fail, with boxes printed. Observed once, then removed.

**Cost counter.** `template_blocks_placed == Σ non-void positions of intersecting pieces`
per chunk, and **exactly 0** for chunks with no start — the serve-path invariant. Template
NBT parsing is per-generator-construction (interned), never per-chunk; counter:
`template_parses` stops growing after warmup.

### S3 — beardifier: production seam and composed gate (M)

The production evaluator uses the 24³ kernel
(`BEARD_KERNEL_RADIUS = 12`), rigid pieces (RIGID projection only) with
`groundLevelDelta`, jigsaw junctions, and the four `TerrainAdjustment` behaviours
(`beard_thin`, `beard_box`, `bury`, `encapsulate`), fed from S1's `refs` product.

**Production dependency:** adaptation-bearing jigsaw and coded pieces are generated through the
same starts/references path as the beardifier, so S3 is not waiting on S4 or S5 for a production
consumer. The residual proof work is broader: compare structure-positive terrain against an
outside oracle and keep the incomplete-generator ledger explicit until every placed start has a
complete piece list.

**Gate.** (a) Kernel and contribution math against the record definition:
vanilla's own beard-contribution formula, spot values hand-computed
at ~10 lattice offsets (outside-origin arithmetic, no JVM needed), including both
hypotheses at one point where dropping the ground-level-delta term gives a distinct value
— predict the value, not the sign. (b) Identity: all 13 parity binaries plus the composed
seed-42 fixture byte-identical — **with the precondition measured, not assumed**: S1's
engine computes starts for seed 42 over the fixture chunks and asserts zero
adaptation-bearing pieces intersect them (the fixture is ocean at (0,0); if this assert
ever fails the fixture seed is structure-coupled and the gate must say so loudly — the
fixture cannot be regenerated without the container oracle, so this assert is what stands
between us and silently absorbing a terrain change).

**Control, premise-checked.** Inject a synthetic rigid piece through the production
`refs → context` seam (not by calling the kernel directly — that is the hermetic trap) and
assert fill output changes exactly within the kernel's reach with the predicted sign and a
printed bounding box, and does not change one cell beyond it. Before believing it, ask
what else paints there: run the same injection with adaptation `none` and assert **zero**
change — the control's own control, since a `none` start reaching the density graph would
be a mis-filter (vanilla's own per-chunk structure lookup filters out any start whose terrain
adaptation is `none` before it ever reaches the density graph).

**Cost counter.** `beardifier_evals == 0` exact for structure-free chunks (this is the
whole serve-path budget story: the overworld C_ss sweep must be counter-identical to
pre-S3); for affected chunks, evals ≤ pieces × interpolation cells, asserted.

### S4 — jigsaw (implemented; extend its gates)

The production jigsaw engine implements start-pool resolution, per-template jigsaw block
entities as connectors (name/target/pool/final_state/joint — read from the 1,212 templates
S2's loader already parses), weighted element selection with **vanilla's own shuffle
draw order**, rotation draws, fallback pools, `max_depth`, `use_expansion_hack` (villages'
terrain matching), `project_start_to_heightmap`, `dimension_padding` and **pool aliases**
(trial_chambers binds aliases per start from its own seeded draw), plus emitted jigsaw junction
records consumed by S3's beardifier.

**What the data requires, measured not recalled**: all 188 pools and 40 processor lists
must resolve at load into typed programs with **zero unsupported element or processor
types** — gate: a loader-completeness test over the whole bundled corpus (the corpus drift
gate proves the bytes; this proves the *reader*), with the `collect_unsupported` pattern
so any residue is named. Element types to cover: `single_pool_element`,
`legacy_single_pool_element`, `list_pool_element`, `feature_pool_element` (places a
`placed_feature` — reuses the existing feature engine; some referenced features may be
among the 48/55 unimplemented types, in which case the element places nothing
**and the ledger names it** — do not let that gap hide inside S4), `empty_pool_element`.

**Gate — the gem of this group**: piece-list equality against vanilla start NBT, block-free.
For the oracle world's villages (2), trial_chambers (13) and trail_ruins (1): every expanded
piece's template name, position, rotation and depth equals vanilla's persisted list,
exactly. This gates the *entire* expansion RNG walk — selection order, shuffles, alias
draws — without comparing a single block. Then the block gate reuses S2's engine per
piece, and the **S3 composed terrain gate** compares terrain under the village
(`beard_thin`) and around the trial chambers (`encapsulate`) against vanilla
region blocks within the adaptation reach, mismatches reported by bounding box, with the
magnitude of the no-beardifier hypothesis pre-computed (run once with the leaf forced back
to 0.0 — the observed-failing control and the wrong-hypothesis prediction in one run;
record both counts).

**World precondition**: two plains villages are still thin coverage —
assert its presence, and name the other four village biomes + pillager_outpost +
ancient_city as **not exercised by the oracle area** (extend the world under `container`
to strengthen; a session teleporting to the relevant biomes suffices). Do not let a
small-sample pass generalise silently: the gate output records per-type instance counts.

**Cost counter**: expansion runs at `starts` time, once per start chunk — counter
`jigsaw_expansions == oracle start count` over the sweep, zero on other chunks; expansion
draw count per start recorded (villages ~hundreds), no per-served-chunk cost beyond S1's.

### S5 — coded piece generators (implemented; extend per-family gates)

**Oracle-backed coded families** use S4-style gates: start-piece-list equality, then
template-engine blocks where applicable, then coded-block comparison. The survival-world fixture
contains **mineshaft (46 starts — the richest gate), buried_treasure (2), and ocean_monument (2)**.

**Zero-oracle structures** — desert_pyramid, jungle_temple, swamp_hut, igloo, mansion, stronghold
(including its ring obligation), and fortress — cannot claim oracle piece-list equality until an
outside fixture contains them. Their current gates must be record-derived arithmetic and deterministic
self-consistency checks, with an explicit expected piece count or bounding box wherever the record
supplies one. A structure with an incomplete piece list remains in
`StructureRegistry::unsupported` and visible through
`OverworldGenerator::structure_starts_including_incomplete`, but is never persisted or block-placed.
The ledger, zero-oracle gates, and additional outside evidence are durable residuals. Mineshaft's
trap is RNG-heavy corridor recursion where draw order is spec; its 46-instance oracle is exactly why
it is the right first comparison family.

### Out of scope, said explicitly

- `structure_spawn_overrides` and in-structure mob spawning: **gameplay-blocked** —
  the generator parses and stores the overrides (S1 data model), and nothing
  here consumes them.
- Chest loot, spawner block entities *contents*: template placement produces the block
  entity NBT slots; wiring block entities to the wire/persistence is separate territory.
  S2 records produced-but-unshipped block entities in its ledger so the gap is measured.
- `/locate` as a command: no production command dispatch exists; S1's placement API is
  `/locate`-shaped for tests only.
- The Nether/End structures' *dimension hosting*: group NE.

## Plan boundaries

S1 absorbs the original stage-plumbing work because empty products would be an island. It uses the
survival world's persisted starts instead of live command output, S3's composed gate depends on
S4's adaptation-bearing jigsaw structures, and S5 covers the coded family omitted by a
template-versus-jigsaw split. The salted placement-seed formula and oracle-first evidence standard
apply to every unit.

## The biggest risk

**Jigsaw expansion RNG drift that the piece-list gate localises but cannot explain.** The
expansion is one long seeded walk (shuffles, alias draws, fallbacks); one mis-ordered draw
scrambles every subsequent piece, and the village oracle has exactly one instance. Budget
S4 review time around the shuffle and selection order specifically, and extend the oracle
world early (more villages = more discriminating gates) rather than after a mismatch.

## Configuration

None new at plan time. Units that add flags must record them here.

## Dependencies

`crates/lodestone-worldgen` (+ `-core` for the RNG addition), the staged store and join
scheduler (U6/U10, landed), the bundled corpus in `crates/lodestone-server/assets/`,
`.cache/mc/survival/world` as oracle, the cached 26.2 behavioral reference as record definition,
`scripts/worldgen-oracle/` + Apple `container` for strengthening fixtures. Companion:
[Worldgen engine overview](../worldgen.md),
[Structure generation](../worldgen-structures.md),
[The Nether and the End](./nether-and-end.md).
