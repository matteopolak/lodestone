# Structures: the group S issue tree

## What it is

The executable unit sequence for unit group **S** of
[`worldgen-rewrite.md`](./worldgen-rewrite.md) (its U14 row): the structure engine —
placement, templates, beardifier, jigsaw and the coded piece generators — turned from the
rewrite plan's phase sketch (S0–S4) into landable units, each with its gate, its control, its
outside evidence source and its cost stated as a counter against the serve-path budget.
Written 2026-08-08 against `HEAD` `5f37fb83`; refines and partially supersedes issue **#514**
(see [Relationship to the parent issue](#relationship-to-the-parent-issue)).

## What was verified vs assumed (2026-08-08)

Every load-bearing claim below was re-measured for this plan, because the rewrite plan's
inventory was stale in exactly this area (corrected in the same commit as this file):

- **The data phase (the old S2 extraction) is DONE.** Measured by `find … | wc -l`:
  34 structures, 20 structure sets, 188 template pools, 40 processor lists under
  `crates/lodestone-server/assets/worldgen/`, plus **1,212 `.nbt` templates under
  `crates/lodestone-server/assets/structure/`** — note *not* under `assets/worldgen/`, which
  is where a scoped `find` measures zero. Landed `6c6c0e10`, byte-identical to the
  jar (`worldgen-structure-corpus.md`). The only Rust reader is the drift gate
  `crates/lodestone-server/tests/worldgen_structure_corpus.rs`.
- **`WorldgenRandom.setLargeFeatureWithSalt` is absent**: `/usr/bin/grep -rn
  "large_feature_with_salt" --include='*.rs' crates/` → 0 hits.
  `lodestone-worldgen-core/src/rng/mod.rs` has `set_decoration_seed`,
  `set_feature_seed`, `set_large_feature_seed` and nothing else. The record
  definition, read from `.cache/mc/26.2/src/.../WorldgenRandom.setLargeFeatureWithSalt` (not from a call
  site): `long result = x * 341873128712L + z * 132897987541L + seed + blend; setSeed(result)`
  — the fourth parameter is the structure set's `salt` (the decompiler names it `blend`).
- **The beardifier is a constant-zero leaf**: `Density::Beardifier` parses in
  `Builder::build_object` (`crates/lodestone-worldgen-core/src/density/mod.rs`) and evaluates `0.0` in `Density::compute` (same file).
- **Terrain adaptation is carried by 11 of 34 structures**, censused from the bundle
  (`python3` over `assets/worldgen/structure/*.json`): `beard_thin` — the 5 villages,
  `pillager_outpost`, `nether_fossil`; `beard_box` — `ancient_city`; `bury` — `stronghold`,
  `trail_ruins`; `encapsulate` — `trial_chambers`. **Every carrier is jigsaw except
  `stronghold` (coded) and `nether_fossil` (template, Nether-only).** This reorders the
  S3/S4 dependency — see [S3](#s3--beardifier-the-engine-seam-then-the-composed-gate).
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
  Start census, measured by scanning every region: mineshaft 29, ocean_ruin 14,
  trial_chambers 8, ruined_portal 7, shipwreck 7, monument 1, village 1, trail_ruins 1,
  buried_treasure 1. **Absent from the generated area**: stronghold, desert_pyramid, igloo,
  swamp_hut, jungle_temple, pillager_outpost, ancient_city, mansion. This census is the
  *world-species* precondition ledger for every gate below.
- **The persistence path already ships an empty `structures` compound**:
  `structures_to_nbt` (`crates/lodestone-server/src/chunk_nbt.rs`) writes
  `structures{References:{}, starts:{}}` — the same "populated empty" shape as the empty
  heightmap NBT (census §10). Filling it is S1's cheapest production consumer.
- **There are 13 parity binaries, not 11**: 11 `*_parity.rs` under
  `crates/lodestone-worldgen/tests/` plus `overworld_gen.rs` plus
  `crates/lodestone-worldgen-parity/tests/chunk_parity.rs`. Every unit below preserves all
  13 green plus the composed `fixtures/composed_seed42.txt` gate.
- Assumed, not verified: that vanilla's start NBT encoding for each structure type
  round-trips through our NBT reader without loss. The reader is independently gated
  (`chunk_nbt_vanilla_oracle.rs` reads the same world), so treat a decode surprise as an S1
  finding, not a blocker.

## The evidence position (read before writing any gate)

There is **no JVM on this machine** (`java -version` fails; CLAUDE.md corrected at
`789c6869`). But the constraint is narrower than "no new vanilla-origin evidence, ever":

1. **The survival world is vanilla-authored evidence that already exists** — produced by the
   real server in July, bind-mounted at `.cache/mc/survival`. Reading it produces no new
   fixture; it consumes an existing outside source, exactly as
   `chunk_nbt_vanilla_oracle.rs` already does (`#[ignore]`d, "a real 26.2 world this repo
   did not write"). It is the primary oracle for this whole group.
2. **The JVM-oracle path never needed a host JVM.** `scripts/worldgen-oracle/run.sh` runs
   `eclipse-temurin:25-jdk` under Apple `container` (`docs/oracle-runtimes.md` — Docker is
   gone from every oracle path). Measured while writing this plan: `container list` shows
   the runtime **up and `lodestone-survival` running**. So new fixtures and *extending the
   oracle world's generated area* (to obtain the 8 missing structure types) appear
   available whenever a unit wants them. Each unit below is designed to land without that
   (belt), and names the container-runtime strengthening explicitly (suspenders), so the
   plan survives the runtime being down again.
3. **The decompiled 26.2 source** under `.cache/mc/26.2/src/` is the record definition for
   every algorithm. Read the definition, not a call-site summary — this repo has shipped a
   backwards transcription that way.

Two independent constructions of one physical rule is the worked precedent (DESIGN.md
§12.117): here the two arms are *our placement engine* and *vanilla's own persisted output
for the same seed* — authored by different implementations, sharing nothing.

## Where structures slot into the pipeline (the S0 answer)

Vanilla's order (`chunk/status/ChunkStatus.java`) is `… STRUCTURE_STARTS →
STRUCTURE_REFERENCES → BIOMES → NOISE …`: starts precede noise **because the beardifier
reads structure bounds during fill**. In our engine, fill lives inside
`OverworldGenerator::pre_ore_stage`, memoised by the staged store
(`overworld/store.rs`; slots declared in `overworld/mod.rs`, and the store's own rule is
"add a stage *above* the ones it consumes").

Concretely:

- Two new `StageSlot`s on the store entry: `structure_starts` (topmost) and
  `structure_refs`. `starts(C)` is a pure function of `(seed, C)` — placement math plus
  climate-sampled biome checks plus piece generation, no chunk data — so it is
  embarrassingly parallel and safely memoised. `refs(C)` consults `starts` over the
  **17×17** neighbourhood (`ChunkStatus` gives STRUCTURE_REFERENCES radius 8) and keeps the
  starts whose bounding boxes come within 12 blocks of `C` (the beardifier's
  `isCloseToChunk(chunkPos, 12)` reach, `Beardifier.java:forStructuresInChunk`).
- `pre_ore_stage(C)` gains one upstream edge: it reads `refs(C)` to build the per-chunk
  beardifier context before fill. This **inverts the terrain-first intuition** exactly as
  the rewrite plan warns; the join scheduler (U10, `docs/join-scheduler.md`) grows one more
  leading wavefront, serial depth +1 stage, halo +8 chunks of *starts only* (cheap — no
  block work in a start).
- **What it costs, as counters, against C_ss (15.14 ms/column today vs the ≤1.0 ms goal —
  do not mix in the 61 ms cold figure; the two are unreconciled):** `starts` runs exactly
  once per chunk (the store's `OnceLock` invariant, already counter-gated at 256/196
  granularity for the existing stages); per chunk it is ~20 structure-set placement draws
  plus rare piece generation on candidate chunks. `refs` is ≤289 store probes, each an
  `Arc` clone. Neither touches a block. The counter acceptance is written per unit below;
  the headline invariant is **`beardifier_evals == 0` and zero added allocations for any
  chunk with no adaptation-bearing start** — exact, not approximate.

The full vanilla ticket/status pipeline is **not** a prerequisite and is not
scheduled here: the store's stage slots plus the scheduler's dependency edges already
express the one ordering fact structures need. The chunk-lifecycle issue remains open for
reasons unrelated to this group.

## Unit sequence

Costs use the rewrite plan's scale (S ≲ 1 session, M ≈ 1–2, L ≈ 3+). Every unit: `just
health` green, all 13 parity binaries green, composed fixture byte-identical (except where
a unit says otherwise and proves why), and no unit lands as an island — its consumer is
named in the unit.

### S1 — placement, starts/refs stages, and the persisted `structures` compound (M)

The old S0+S1 merged deliberately: stage plumbing alone, with empty products, is an island
by construction.

**Contents.** (a) `set_large_feature_with_salt` on `WorldgenRandom` — the five-line formula
above. (b) `RandomSpreadStructurePlacement` (linear + triangular spread, salt,
`frequency_reduction_method`, exclusion zones, per-set weighted structure selection) and
`ConcentricRingsStructurePlacement` (stronghold rings). (c) Per-structure *start
predicates* — the biome check via the climate sampler (pure) plus any structure-specific
pre-piece draws (mineshaft's probability draw lives here) — behind a registry in a new
`crates/lodestone-worldgen/src/structure/` module, where a structure whose generator has
not landed yet yields **no start** and is *named* in a `collect_unsupported`-style ledger
(the legible-silence pattern from `unsupported_placed_ref`, `feature/vegetation/config.rs`), never silently
skipped. (d) The two store slots and the `pre_ore` edge, with the beardifier context built
and handed to a still-constant-zero leaf — bit-identical output, proven. (e) The
production consumers: `structures_to_nbt`'s (`chunk_nbt.rs`) empty compound becomes real
`starts`/`References` NBT, and the singleplayer save path persists it.

**Gate.** For every chunk of the survival world's 29 regions: our computed start set for
seed -195764831 equals vanilla's persisted `structures.starts` — structure id, start chunk,
and (once S2+/S5 land pieces) bounding boxes — for each structure type on the
**implemented ledger**; and our `References` equal vanilla's for those types.
Expected values are vanilla-authored; nothing round-trips through our encoder.
**World precondition, asserted not assumed**: the gate first counts starts per type in the
oracle NBT and fails if any gated type has zero (the census above says mineshaft 29 …
buried_treasure 1); a type with zero oracle instances may not sit on the ledger.

**Control, observed failing.** Re-run one gated set with its salt perturbed by +1; the gate
must report mismatches with a per-region bounding box of differing chunks. Run it, record
the failure, then remove. This is the detector-works control for "our placement equals
vanilla's" — without it, an accidentally-empty ledger passes vacuously (the
assertion-species trap), so the gate also asserts a floor on total compared starts (≥69,
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
gated only by the record definition (`ConcentricRingsStructurePlacement.java` +
`ChunkGeneratorStructureState`) and a hand-computed first-ring fixture; its
vanilla-equality gate is a named deferred obligation until the oracle world's area is
extended under `container` (a one-command teleport session). Do not report stronghold
placement as verified until that runs.

### S2 — template engine + the template-coded family (L)

The NBT structure-template loader (blocks, palettes, entities, **jigsaw block entities** —
which S4 needs as its connection graph, so this loader is shared infrastructure), placement
semantics (`StructureTemplate.placeInWorld`: rotation, mirror, pivot, `structure_void`
transparency, water-loggable handling), processor lists (all 40 bundled documents must
parse; `rule`, `block_rot`, `block_ignore`, `capped`, … — unknown types fail loudly), and
the template-coded structures with oracle presence, in order of oracle richness:
**shipwreck (7), ruined_portal (7), ocean_ruin (14)**. Igloo, end_city, nether_fossil wait
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

### S3 — beardifier: the engine seam now, the composed gate after S4 (M)

Replace the constant-zero leaf with the real evaluator: the 24³ kernel
(`BEARD_KERNEL_RADIUS = 12`), rigid pieces (RIGID projection only) with
`groundLevelDelta`, jigsaw junctions, and the four `TerrainAdjustment` behaviours
(`beard_thin`, `beard_box`, `bury`, `encapsulate`), fed from S1's `refs` product.

**The dependency inversion found while planning (differs from #514's S3-before-S4
reading):** every adaptation carrier is jigsaw except stronghold and nether_fossil, and
none of S2's overworld family carries adaptation at all. So S3 can land its *seam and
kernel* now, but its structure-positive composed gate — real vanilla terrain flattened
under a real structure — is only satisfiable after S4 (village, trail_ruins,
trial_chambers) or group NE (nether_fossil). The plan records that as an explicit deferred
obligation inside S4's gate list rather than pretending S3 is fully proven at landing.

**Gate at S3 landing.** (a) Kernel and contribution math against the record definition:
`computeBeardContribution` spot values hand-computed from `Beardifier.java`'s own formula
at ~10 lattice offsets (outside-origin arithmetic, no JVM needed), including both
hypotheses at one point where dropping the `groundLevelDelta` term gives a distinct value
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
be a mis-filter (vanilla filters `terrainAdaptation() != NONE` in
`forStructuresInChunk`).

**Cost counter.** `beardifier_evals == 0` exact for structure-free chunks (this is the
whole serve-path budget story: the overworld C_ss sweep must be counter-identical to
pre-S3); for affected chunks, evals ≤ pieces × interpolation cells, asserted.

### S4 — jigsaw (L, the largest single piece)

`JigsawPlacement.addPieces` semantics: start pool resolution, per-template jigsaw block
entities as connectors (name/target/pool/final_state/joint — read from the 1,212 templates
S2's loader already parses), weighted element selection with **vanilla's `Util.shuffle`
draw order**, rotation draws, fallback pools, `max_depth`, `use_expansion_hack` (villages'
terrain matching), `project_start_to_heightmap`, `dimension_padding` and **pool aliases**
(trial_chambers binds aliases per start from its own seeded draw), plus emitted
`JigsawJunction`s (which S3's beardifier consumes — landing S4 *activates* S3's deferred
gate).

**What the data requires, measured not recalled**: all 188 pools and 40 processor lists
must resolve at load into typed programs with **zero unsupported element or processor
types** — gate: a loader-completeness test over the whole bundled corpus (the corpus drift
gate proves the bytes; this proves the *reader*), with the `collect_unsupported` pattern
so any residue is named. Element types to cover: `single_pool_element`,
`legacy_single_pool_element`, `list_pool_element`, `feature_pool_element` (places a
`placed_feature` — reuses the existing feature engine; note some referenced features may
be among the 48/55 unimplemented types (the missing-feature-types issue), in which case the element places nothing
**and the ledger names it** — do not let that gap hide inside S4), `empty_pool_element`.

**Gate — the gem of this group**: piece-list equality against vanilla start NBT, block-free.
For the oracle world's village (1), trial_chambers (8) and trail_ruins (1): every expanded
piece's template name, position, rotation and depth equals vanilla's persisted list,
exactly. This gates the *entire* expansion RNG walk — selection order, shuffles, alias
draws — without comparing a single block. Then the block gate reuses S2's engine per
piece, and **S3's deferred composed gate runs here**: terrain under the village
(`beard_thin`) and around the trial chambers (`encapsulate`) compares against vanilla
region blocks within the adaptation reach, mismatches reported by bounding box, with the
magnitude of the no-beardifier hypothesis pre-computed (run once with the leaf forced back
to 0.0 — the observed-failing control and the wrong-hypothesis prediction in one run;
record both counts).

**World precondition**: one village (plains, at one start chunk) is thin coverage —
assert its presence, and name the other four village biomes + pillager_outpost +
ancient_city as **not exercised by the oracle area** (extend the world under `container`
to strengthen; a session teleporting to the relevant biomes suffices). Do not let a
1-instance pass generalise silently: the gate output records per-type instance counts.

**Cost counter**: expansion runs at `starts` time, once per start chunk — counter
`jigsaw_expansions == oracle start count` over the sweep, zero on other chunks; expansion
draw count per start recorded (villages ~hundreds), no per-served-chunk cost beyond S1's.

### S5 — coded piece generators, ordered by oracle presence (L, decomposable per structure)

Each is a bespoke port gated like S4 (start piece-list equality, then S2-engine blocks
where template-based, then coded-block comparison): **mineshaft (29 oracle starts — first,
richest gate), buried_treasure (1), ocean_monument (1)**; then the zero-oracle tail —
desert_pyramid, jungle_temple, swamp_hut, igloo, mansion, stronghold (+ its S1 ring
obligation), fortress (zero starts even in the Nether oracle area) — each **blocked on
evidence, not on code**: the unit rule is *no landing without an oracle-positive*, and the
acquisition path (extend the survival world under `container`) is named per structure.
Mineshaft's trap: its pieces are RNG-heavy corridor recursion where draw order is spec;
its 29-instance oracle is exactly what makes it the right first port.

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

## Relationship to the parent issue

This plan **refines** the parent issue (keep it as the group parent; file S1–S5 as sub-issues
from these units): it keeps S0's contract but merges it into S1's landing (stage plumbing
alone is an island); replaces its "/locate dumps" oracle with the survival world's
persisted starts (no live server needed); corrects S2/S3/S4's implied independence with
the measured adaptation census (S3's composed gate depends on S4); and adds S5 for the
coded family its S2/S4 dichotomy has no home for. The parent issue's S1-blocker analysis
(`setLargeFeatureWithSalt`) and its evidence-standard paragraphs stand.

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
`.cache/mc/survival/world` as oracle, `.cache/mc/26.2/src/` as record definition,
`scripts/worldgen-oracle/` + Apple `container` for strengthening fixtures. Companion:
[`worldgen-gap-census.md`](../worldgen-gap-census.md) §7,
[`worldgen-structure-corpus.md`](../worldgen-structure-corpus.md),
[`plans/nether-and-end.md`](./nether-and-end.md).
