# Proving vegetation reaches blocks: the vegetal-decoration census

## What it is

The instrumentation and the two gates that answer "did vegetal decoration
actually put grass, flowers and trees into a served chunk, and *how many*?" —
`lodestone_worldgen::feature::vegetation::census` plus
`plains_grass_patch_attempt_count_matches_the_placement_json` and
`vegetation_reaches_real_blocks_over_a_production_sweep` in
`crates/lodestone-server/src/worldgen_data.rs`. It exists because the vegetation
placer's blanket rule is "an unmodelled feature type degrades to a silent no-op",
which makes *every* quantity of vegetation — including zero — look identical from
the outside. This is issue #478's other half: the diagnosis was that vegetation
was missing, and the finding was that it is not, but that nothing in the tree
could have told you either way.

## Why it was needed

`crates/lodestone-worldgen/src/feature/vegetation.rs` cannot panic on data it
does not model — a trimmed or third-party datapack naming an unimplemented
feature must still generate a world. 26.2's own vanilla data reaches such types
constantly: `multiface_growth` (glow lichen) is in 55 of the 66 bundled biomes,
and over a 136-chunk sweep **372,842** terminal dispatches landed in the
unmodelled bucket against **87,133** that reached a placer this engine
implements. So "some feature did nothing" is the normal case, and cannot be the
signal.

Two gates already existed and neither could see a blackout:

| existing gate | what it proves | why it cannot see zero blocks |
|---|---|---|
| `worldgen_data::tests::vegetation_placer_gaps_are_named_not_silent` | every biome's declared step resolves, and every unimplemented placer is named in `KNOWN_VEGETATION_GAPS` | it runs **resolve**, never the placer |
| `lodestone-worldgen/tests/vegetation_parity.rs` | the placer matches a real 26.2 dump block-for-block at four fixtures | it drives `apply_vegetal_decoration_step` directly over a **fixture** resolver, not `OverworldGenerator::column` over the bundled assets |

And this exact regression had already shipped once: the absolute-vs-local
`VegGrid` coordinate bug (see that struct's own doc comment) reached **zero**
blocks in every served chunk with the whole suite green. The gate that caught it,
`diagnostic_vegetation_counts_over_plains_sweep`, was later deleted — but the doc
comment naming it survived to `074b5e9`, so the repo carried a written record of
a regression with nothing watching for its return, and the reference read as
coverage on inspection.

## How it works

### The census

`vegetation::census` is a **thread-local** `VegCensus` the placer writes on every
terminal dispatch and every grid write. Thread-local, not global, on purpose:
`OverworldGenerator` is shared across threads by
`chunk::generate_columns_parallel` and `cargo test` runs multi-threaded, so a
process-global counter would let a gate read another test's work — the *duration*
species of vacuous test. Call `census::reset()`, generate, then
`census::snapshot()`.

The fields that matter:

- `unsupported: BTreeMap<String, usize>` — unmodelled dispatches **keyed by
  reason**. This is the loud part: a newly-unimplemented type appears as a named,
  counted row instead of as a slightly emptier world.
- `block_predicate_filter_in` — the last exactly-predictable boundary in a
  vanilla pipeline. See below.
- `simple_block` / `tree` / `block_column` — terminal dispatches that reached a
  modelled placer.
- `writes` / `writes_rejected` — grid writes that landed, and those dropped as
  spill outside the grid's footprint.

`LODESTONE_VEG_STRICT=1` turns an unmodelled dispatch into a panic naming the
reason — a debugging switch for "which type am I missing here", never a shipping
mode, because vanilla data reaches unmodelled types in nearly every biome.

### Why `block_predicate_filter_in` is the boundary to assert on

Every 26.2 overworld vegetation `placed_feature` ends in at least one
terrain-dependent filter — measured: of the 262 bundled placed features, the only
three with no filter anywhere are `end_spike`, `freeze_top_layer` and
`void_start_platform`. So no *terminal* count can be predicted from JSON alone.
Everything upstream of the filter can: `count` and `noise_threshold_count`
multiply by a JSON constant; `in_square`, `biome` and `random_offset` are each
exactly position-preserving; `heightmap` yields exactly one position for any
column that is not entirely air.

For `patch_grass_plain` that gives an exact product:

| modifier | positions out, per position in |
|---|---|
| `noise_threshold_count` (`below_noise: 5`, `above_noise: 10`) | **5 or 10** |
| `in_square` | 1 |
| `heightmap: WORLD_SURFACE_WG` | 1 |
| `biome` | 1 |
| `count: 32` | **32** |
| `random_offset` | 1 |
| `block_predicate_filter` | not predictable |

So the count arriving at the filter is `n * 32` for `n ∈ {5, 10}` — `160` or
`320`, and nothing else. **Measured: 320.** The gate admits both branch values
and deliberately does *not* compute which one applies: predicting the
`Biome.BIOME_INFO_NOISE` branch with our own noise implementation would make the
expected value originate inside the code under test.

### The wrong hypotheses this excludes

Each is computed from the same JSON, not merely listed:

| hypothesis | predicted value | inside `{160, 320}`? |
|---|---|---|
| correct | `n * 32` | yes |
| `count` silently dropped from the pipeline | `n` = 5 or 10 | no |
| `count` read from the wrong field | 1 | no |
| `noise_threshold_count` dropped | 32 | no |
| both dropped | 1 | no |

The dropped-modifier row is the one that matters. `parse_placed_feature_doc`
builds its pipeline with `.filter_map(VegPlacement::try_parse)`, so an
unrecognised modifier `type` is **removed from the pipeline** rather than making
the feature inert — a silent 32× under-placement that no `cargo check` and no
"is it non-zero" assertion can see. Its control,
`grass_patch_attempt_count_control_fires_when_the_count_modifier_is_removed`,
strips `count` from the bundled JSON and observes the count collapse to **10**,
confirming the gate measures the modifier rather than a constant.

### The production-seam gate

`vegetation_reaches_real_blocks_over_a_production_sweep` runs the whole composed
pipeline — bundled data, real generated terrain, the 3×3 driver, the fold back
into a `GeneratedColumn` — over a fixed stride-9 5×5 lattice from chunk
`(-40, -40)` at seed 42. The lattice is stated as a *rule*, not a set of
coordinates picked after seeing which ones had trees; stride 9 so no two centres
share a 3×3 neighbourhood.

Measured healthy total: **3269** vegetation blocks. The floor is 300 — ~11× under
that, and ~3× above what a single dropped modifier (a measured 32× cut) would
leave. Failure prints the **per-biome breakdown**, not one aggregate, because an
aggregate cannot distinguish "uniformly thin" from "one biome contributes
everything". A separate assertion requires at least one `*_log`: grass and
flowers are `simple_block` and would carry the total alone, so without it the
`ConfiguredFeature::Tree` path — trunk placer, foliage placer, leaf-distance
update — could reach zero blocks unnoticed, which is the specific symptom #478
reported.

**Negative control, observed:** with the `self.vegetation_stage(cx, cz, world)`
call removed from `OverworldGenerator::column`, the sweep reports **0** blocks and
the gate fails.

## How to change it

- **Adding a census field:** add it to `VegCensus`, bump it via
  `census_bump(|c| ...)` at the site, and document what a gate can conclude from
  it. Do not make it a `static` — see the thread-local rationale above.
- **Implementing a new feature type:** its reason string disappears from
  `census.unsupported`, which will fail
  `vegetation_placer_gaps_are_named_not_silent` in the "listed but no longer
  measured" direction. Prune the `KNOWN_VEGETATION_GAPS` entry; do not leave it.
- **Changing the floor:** re-derive it, do not nudge it. Raise
  `VEGETATION_FLOOR` until the assertion fires, read the real total out of the
  failure message, then set the floor from that — the number in the doc comment
  is required to be an observed value.
- **The lattice coordinates are load-bearing.** Changing them after seeing a
  failure is cherry-picking. If the lattice needs to move, state the new rule
  first and re-derive the floor.
- **`block_predicate_filter_in` is only exact for a single-source run of a single
  placed feature.** Summed over a mixed-biome sweep it is a diagnostic, not a
  prediction.

## Configuration

| name | effect |
|---|---|
| `LODESTONE_VEG_STRICT` | any value but `0`/empty: panic on an unmodelled vegetal-decoration terminal dispatch, naming the reason and position. Read once per process. |
| `LODESTONE_VEG_SINGLE_SOURCE_DEBUG` | pre-existing: skip the 8 neighbours in `vegetation_stage`, matching `VegetationOracle.java`'s `SINGLE` mode. |

## Dependencies

- `lodestone_worldgen::feature::vegetation` — the placer being censused.
- `lodestone_worldgen::compose::build_biome_vegetation` — resolves each biome's
  `VEGETAL_DECORATION` step.
- `lodestone_server::worldgen_data::EmbeddedResolver` — the bundled 26.2
  `placed_feature`/`configured_feature`/`biome`/`tags/block` corpus the gates read
  their predicted constants out of.
- `scripts/worldgen-oracle/VegetationOracle.java` and
  `crates/lodestone-worldgen/tests/vegetation_parity.rs` — the block-for-block
  parity evidence these gates sit *alongside*, not a substitute for. Note that
  oracle is self-authored and has already been wrong in a plausible-looking way
  (see that test file's "A real bug in the oracle itself").
