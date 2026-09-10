# Decoration: features, vegetation, ores and generation-time mob spawns

## What it is

Everything that runs after terrain shape and biome assignment to make a chunk look inhabited:
the `GenerationStep.Decoration` driver and its placement-modifier interpreter, the vegetation engine
(grass, flowers, trees), ore-vein placement and allocation, and the one-shot animal spawn vanilla
performs at chunk generation. All four are interpreters over the same kind of data (`configured_feature`
/ `placed_feature` / per-biome step lists) and share one seeding discipline: one `set_decoration_seed`
per chunk, then `set_feature_seed(decoration_seed, index, step)` per feature, so a feature's `(step,
index)` pair — never a flattened running count — is what isolates its RNG stream from its neighbours'.

## How it works

The decoration driver first uses section-biome containers to decide which
globally ordered placed features receive a random stream. A `biome` placement
modifier is a second, narrower gate: it checks the candidate's exact
three-dimensional biome cell against the biome memberships of that placed
feature. These two decisions cannot be collapsed. In particular, a cave feature
can be eligible because one section contains its cave biome while an above-ground
candidate from the same source column must still be rejected. The gate is
fail-closed when production biome cells are available but the placed feature has
no registry identity; compact unit fixtures without biome sources retain their
unconstrained form.

### Decoration steps and feature types

`compose::DecorationCatalog` resolves biome `features` arrays over the driven steps —
`RAW_GENERATION`, `LAKES`, `LOCAL_MODIFICATIONS`, `UNDERGROUND_STRUCTURES`, `SURFACE_STRUCTURES`,
`UNDERGROUND_DECORATION`, `FLUID_SPRINGS`, `VEGETAL_DECORATION` — into `(step, index, PlacedRef)`
triples in step order. `TOP_LAYER_MODIFICATION` is a separate engine with its own docs
(freeze/snow in `worldgen-biomes.md`); underground ores are selected by the unified FEATURES
dispatcher described below. `STRONGHOLDS` has zero entries across every bundled biome and is not
driven.

`compose::build_decoration_catalog` builds the globally ordered feature graph once from the biome
source's first-occurrence order. For each decorating source, the unified FEATURES dispatcher unions section
biomes from the source's 3×3 chunk neighbourhood, selects those graph entries, and retains their
global indices even when earlier entries are not selected. This matters for underground biomes:
their features can run in a chunk whose surface biome is different. The compiled-server
selection fixture records the sulfur-cave control: the `{plains, sulfur_caves}`
union selects `sulfur_spike_cluster` and `sulfur_spike` at step-7 indices 2 and 3, not local
indices 0 and 1. `decoration_selection_jvm.txt` preserves that external control
alongside the server-worldgen test.

The source order is not a complete-registry enumeration. The Overworld climate
table yields 55 distinct biomes in first-occurrence order; its external seed-42
control places `seagrass_warm` at step-9 index 97 and predicts square `(13,8)`
followed by body offsets `(4,3)` and `(3,4)`. Enumerating all bundled biome
documents instead gives index 104, a deliberately retained failing-control
value. The catalog must therefore use the biome source's possible-biome order
and use biome membership only when selecting which already-indexed features run.

The catalog also materializes each placed feature's eligible-biome map once and
shares it by `Arc` with replay contexts and vegetation grids. This map is immutable
after generator construction; rebuilding its `String` keys and sets for every
served chunk changes no admission or RNG decision and only adds avoidable heap
traffic.

`UNDERGROUND_ORES` is dispatched through the same catalog rather than treated as an ore-only
list. `select_ores` emits the configured ore entries, while `select_step6_disks` emits disk
features to the existing `VegGrid` placement interpreter, and
`select_step6_non_ore` emits the modeled underwater-magma entries; all three walks count every
raw step entry so the feature seed index remains global. The disk placement data is still authoritative: its
heightmap, water and biome modifiers run before the configured disk body, which preserves the
count/radius/target behavior for sand, gravel and clay disks without duplicating a second disk
implementation in the ore engine.

Underwater magma uses the same step-6 interleave and its placement modifiers, then scans down from
a water origin to the first non-water floor. Each candidate receives its probability draw before
the body checks that the target is non-fluid and enclosed by a non-visible floor and four horizontal
faces. Those five visibility checks use `lodestone-data`'s generated six-direction face-occlusion
table; the worldgen interner binds each local state to its canonical state id when it is first
interned, so the candidate loop performs only numeric lookups. States outside the canonical table
remain visible, preventing an unmeasured shape from being treated as a closed enclosure. The
direction mapping is explicit: below→up, north→south, south→north, west→east, east→west.

Each synthetic 3x3 source pass resets the random wrapper's Gaussian cache before reseeding that source. This keeps cached paired draws local to one source, matching independent source wrappers; sharing the cache would leak a prior source's spare Gaussian into the next feature stream.

The production Overworld dispatcher completes its nine sources in x-major,
z-fastest order: `(-1,-1), (-1,0), (-1,1), (0,-1), (0,0), (0,1),
(1,-1), (1,0), (1,1)`. This is the order observed in the external feature
trace and is shared by production replay and parity admission. Placement reads
and replacement checks observe earlier cross-chunk writes, so changing this
order changes generated blocks even when every individual source's random
stream is unchanged.

Placement modifiers (count, in_square, heightmap, biome, rarity_filter,
surface_water_depth_filter, noise_threshold_count, random_offset, block_predicate_filter,
height_range, and the list fan-out `Positions::List`/`count_on_every_layer`/`fixed_placement` need)
compose as a depth-first flat-map, exactly reproducing vanilla's `Stream` pipeline's draw order.
Configured-feature bodies (`simple_block`, `tree`, `random_selector`/`simple_random_selector`,
`underwater_magma`, `speleothem`, `speleothem_cluster`, `geode`, and the vegetation-specific ones below) each reproduce their placement body's exact draw
sequence. An unmodelled feature type or placement modifier degrades to a silent, RNG-free no-op
(`ConfiguredFeature::Unsupported`) rather than a panic — the census resolves every bundled biome's
step list at generator construction time, including biomes nobody has tested yet, so a hard failure
on one unmodelled type would break every biome's world generation, not just the untested one.
The End-specific families sit outside this common interpreter but are production-connected through
`EndGenerator`'s three-by-three decoration region: the fixed platform, outer islands, chorus plants,
return gateways, and spike blocks. An `end_spike` configuration with a non-empty `spikes` list now
keeps each explicit center, radius, height and cage flag; the empty list retains the seed-derived
ten-spike fallback. Their independent feature fixtures cover the platform, island,
chorus, and gateway shapes. The independent feature-order fixture also pins the global per-step
indices: `end_gateway_return` is step-4 index 0 and `end_spike` is step-4 index 1, so
`EndDecoration` derives each feature seed from the dimension-wide order rather than its local
biome position. The whole-column End terrain fixture deliberately stops before later writers, so
it is not evidence for decoration scheduling or cross-source order. Several rarer
single-use types remain unmodelled and are tracked by name in
`lodestone_server::worldgen_data::KNOWN_VEGETATION_GAPS`; update that set whenever a type lands so a
regression (or a fixed gap that should be pruned) is loud rather than silent.

The geode body receives the raw world seed separately from its feature stream. It builds its
normal-noise field from that seed without consuming the placement RNG, then visits its closed
generation box with X as the innermost coordinate. Layer providers, cracks, invalid-block aborts,
crystal direction order and waterlogged crystal states all retain their conditional draws. Geodes
can write sixteen blocks away from an origin, so the Overworld decoration grid keeps a `[-32, 48)`
local footprint; the ordinary Nether vegetation footprint remains eight blocks wide. Nested placed
features and structure-pool feature elements must forward the same world seed rather than silently
substituting zero. The accepted lifecycle manifest is the composed output gate; feature-local tests
cover parsing, protected blocks and boundary spill but are not substitutes for that packet digest.

Fossils resolve two parallel structure-template arrays and two processor chains. One rotation draw
and one shared index select the bone template and matching ore overlay; a ten-way vertical jitter
then settles the transformed footprint against the ocean-floor heightmap. The eight transformed
bounding-box corners admit a placement only when the configured empty-corner limit allows it. Both
templates are applied through the live dense grid so processor reads see earlier writes and the
overlay can replace the selected fossil's blocks without creating a second placement surface.

Ice spikes use a dedicated configured-feature body rather than the generic block placer. The body
settles an air candidate onto a snow block, consumes the height/width draws, writes the tapered
packed-ice layers into air or the resolved replacement-tag closure, and then fills the narrow support
pillar until protected terrain. The `ice_spike` entry is selected from the `ice_spikes` biome's
surface-structure step, so the parser, catalog, dispatcher, and feature-local external fixture all
cover the production path together.

Large dripstone uses a dedicated cave-column body. It scans from an empty or water-filled origin to
the nearest valid stone edges, caps the configured radius range by cave height, then grows and
embeds paired tapered cones before writing their exposed sections as dripstone blocks. The
`large_dripstone` entry is selected by `dripstone_caves`' underground-decoration step; constant-
provider geometry and the bundled asset/catalog path are covered independently.

Simple-block state providers include fixed, weighted, threshold-noise, noise and dual-noise forms.
The two noise forms construct their deterministic fields from the bundled seed and octave data at
selection time; dual noise first selects the fast-field frequency, then selects the output state.
Simple-block placement dispatches through a finite target-state survival table. Ordinary vegetation
uses `supports_vegetation`; dry vegetation, azalea, crimson roots, small dripleaf and soul fire use
their own resolved floor tags; mushrooms, lily pads, ceiling plants, carpets, leaf litter and fire
read their respective local support geometry. Full blocks (`melon`, `pumpkin`, `tuff`) and potent
sulfur have no survival floor gate. The tag sets are resolved once and bound as state-id bitsets per
decoration pass, so this fidelity does not reintroduce string-set work into each placement attempt.
Nether forest roots, fungi and sprouts have their own support family: nylium and soul soil are valid
for all three, mycelium is additionally valid for fungi, and warped roots follow the same support
closure as sprouts. Ordinary vegetation keeps its narrower floor tag. Huge fungi use the generated
source depth even when the Nether receiving window is wider, treat liquid cells as replaceable, and
only roll their broad-stem variant for the natural (not planted) configuration.
Block columns also accept weighted nested height providers and randomized integer state properties,
which covers the hanging cave-vine records; bamboo uses the configured floor tag, stalk states and
optional podzol disk.
The hanging-vine environment scan now evaluates its downward sturdy-face target against the
resolved per-state support facts. Air candidates therefore continue upward until they reach a
ceiling with the required face, while unsupported target directions retain the parser's safe
permissive fallback.

The single speleothem feature resolves its anchor-holder tag at construction, chooses an upward or
downward point from the two adjacent anchor candidates, then writes its base patch before its one-
or two-segment pointed state. Its horizontal patch branches and their nested direction draws occur
whether or not a candidate cell can be replaced; that draw order is part of the feature's result.
It runs through the same 3×3 per-source decoration driver as every other configured feature, so an
anchor patch from a source at a chunk edge may legitimately write into its neighbouring chunk. The
external `speleothem_feature_jvm.txt` fixture places at the east edge and asserts the western spill,
the tag-backed cinnabar-to-sulfur replacement, and the generated pointed-state properties together.

The cave-wide cluster form samples its height, wetness, density, and two radii before scanning each
column in rectangular order. Every accepted air-or-water column consumes its water, ceiling, floor,
collision, and merge draws in that order before it writes base layers and upward or downward pointed
segments. `speleothem_cluster_jvm.txt` keeps an external compact cave fixture whose mixed base,
frustum, tip, and paired-direction results catch a plausible single-column or non-Gaussian port.

The Nether's mixed step does not split ore from its neighbouring entries. Its scheduler applies
each source's raw indices in order and transfers final writes across the bounded ore/decoration
adapter boundary before the next entry reads. The padded decoration grid retains edge spill for
the final fold while the ore adapter observes only its 3×3 read window; treating those as two
independent completed stages would conceal order-sensitive body or input defects. Its production
consumer control withholds only non-ore step-7/step-9 bodies while preserving their raw slots, so
the returned dense columns prove the same mixed dispatcher carries a non-ore write end to end.

### Vegetation

`feature/vegetation/` places grass, flowers and trees over a real 3×3 neighbourhood (a tree or patch
straddling a chunk edge genuinely spills into whichever chunk generates it, matching vanilla's own
cross-chunk decoration spill) via `VegGrid`, a mutable chunk-local block field seeded from the
composed terrain-prefix grid and folded back afterward. Trunk placers cover straight, forking, dark-oak
2×2, giant/mega-jungle, fancy, cherry and mangrove's upwards-branching shape; foliage placers cover
the vanilla equivalents plus cherry's hanging-leaves pass and mangrove's dart-throw scatter; a
mangrove root placer and a fallen-tree feature (stump + horizontal log, sharing decorator machinery
with standing trees) are also modelled. Huge red and brown mushrooms use their configured ground
tags, a four-to-six-block stem height with a one-in-twelve doubled branch, and their distinct directional
cap layouts (brown's cornerless square versus red's three plus-shaped rim layers and smaller filled top).
Their preflight clearance accepts air and leaves, while the cap/stem writes use the dedicated mushroom
replacement set, so a valid canopy can be replaced without allowing a solid obstruction to produce a
partial tree. Brown's clearance reserves its configured radius above the fourth stem layer; red's
preflight clearance remains stem-column-only before its cap is written. Every reachable overworld biome's
tree content is now covered by a real placer. Multiface
growth writes complete directional state, checks its support,
and performs its one seeded outward spread; the external single-source and 3×3 fixtures compare that
layout exactly. The fixed-seed `vegetation_mushroom_fields_neg1_0_jvm.txt` and
`vegetation_mushroom_fields_5_5_jvm.txt` external captures exercise the production mushroom-fields
selector; their composed 3×3 replays contain both cap variants and their stems, so the selector path
cannot regress while feature-local geometry tests remain green. The
Nether's `huge_fungus` body is also modelled: crimson and warped fungi retain their variable
height, rare broad stem, probabilistic hat/decor blocks, and wart-hat hanging vines, while the
two source biomes' feature-list indices remain unchanged. The
remaining named gaps degrade individually rather than disabling the whole tree.

Sculk cursor movement uses an explicit 18-offset order: X advances fastest,
then Y, with Z as the outer coordinate, while the zero offset and cube corners
are omitted. That order is part of the seeded shuffle contract, so changing it
changes which reachable vein is updated even when the bounded random values do
not change.

The three Nether basalt-deltas records at step 4 use the same scheduler as the later vegetation
records and retain their raw `(step, index)` identities. The delta writes a floor-held contents
patch only when all horizontal and lower neighbours are occupied and the upper neighbour is air;
its optional rim is decided before both patch radii are sampled. Small and large column records
sample their height, select either the dense or sparse attempt count, then consume X, unit-range Y,
and Z coordinates for every attempt in that order. The Y value is always zero but its draw advances
the feature stream; a reach draw occurs only for an attempt inside the height diamond. They count as
`other_feature` in the decoration census. The captured wrapper-stream controls in
`feature::vegetation::features::tests` cover the delta's shifted patch/rim shape and the columns'
conditional reach order.

The step-7 netherrack replacement blobs have a separate captured single-invocation control: an
origin at `(0,20,0)` finds its target at y 18, samples radii `(7,3,7)`, and emits 300 cells under a
fixed xoroshiro stream. Its no-target companion confirms that the three radius draws are not consumed
when the descending target search fails. Keep this body control distinct from the placed-feature
modifier stream: a correct replacement shape does not prove that production chose the same origin.

Root systems scan upward for a valid nested feature site, scatter the root-column replacement only
after that nested placement succeeds, then independently scatter hanging roots from supported ceilings.
Coral tree, claw and mushroom forms share the tagged coral state choice and water gate but retain
their distinct trunk, branched and hollow-shell geometries.

Tree `place_on_ground` decorators use the lowest trunk/root positions as their horizontal bounds,
expand that box by the configured radius and vertical height, and try the configured state provider
above a solid-rendering ground block. The support check uses the exact canonical block-state
capability, while the separate no-leaves heightmap scan continues to use motion blocking. Each try
consumes one inclusive x, y and z draw before any candidate checks, including rejected candidates;
this is important for the paired 96-try and 150-try leaf-litter decorators used by mixed oak trees.
The provider is evaluated only after the air, solid-rendering ground and no-leaves heightmap checks
pass, so a rejected candidate consumes no provider-specific draws.

The direct compiled-server maps for root systems and all three coral forms are exact, including
their blocked-origin and dry-water controls. Their real-biome composition captures remain an
explicit production gap: `warm_ocean` currently diverges on seagrass and multiface writes, and
`lush_caves` remains pending an exact composed replay. Keep `coral_*` and `root_system` in
`KNOWN_VEGETATION_GAPS` until the ignored composed-fixture gate in
`vegetation_parity.rs` passes; a successful feature-local map is not permission to remove that
end-to-end ledger entry.

Vegetation-patch configurations may name block tags in `replaceable`, including
`#minecraft:moss_replaceable`. The parser expands those tags recursively through
`resolve_block_set`; the waterlogged variant uses the same path. Treating the field as literal block
IDs silently makes patches skip terrain such as deepslate, so preserve tag expansion when extending
the configuration parser.

Placement is off block-state strings only at the edges: tag-membership questions are answered by
fixed bitsets indexed by `StateId`,
exact and never needing to grow since a `StateId` is a `u16`. A bit above the interner's watermark
(minted *during* the current decoration pass — a rewritten leaf's `distance=N` state, for instance)
falls back to the pre-bitset string path, which is a correctness requirement, not a slow path: an
unexamined id would answer every tag query `false`, which changes what decorates where. Two derived
per-position values (`distance=N` leaf rewrite, `waterlogged` fix-up) are memoised `id -> id`
lookups rather than re-derived per call.

### Ore allocation

`feature/mod.rs`'s ore engine (`UNDERGROUND_ORES`) is the same placement-modifier/positions shape as
vegetation, composed into `column()` over the real vanilla 3×3 `blockStateWriteRadius(1)` driver. Each
source selects ore-capable entries from the global decoration catalog using every section biome in
that source chunk; the retained global step index, not a biome document's local array offset,
seeds that ore. The same nine sources write the result, but their terrain and
heightmap probes use a 5×5 read context: a blob at an outer source edge can
inspect the real neighbour column rather than a clamped substitute. Those probes
read `OCEAN_FLOOR_WG` from the completed pre-ore grid, after carving and structure
placement; the earlier fill height is only for biome and surface selection. `OrePositions::{None, One, Repeat}` replaces a
per-attempt-allocated `Vec<BlockPos>`, matching vegetation's `Positions` shape; per-blob scratch (the
sphere-fill table and its visited-bitset) is taken from and returned to thread-local free lists rather
than allocated fresh per ore blob. **A recycled visited-bitset must be cleared before resize, not
just resized** — `Vec::resize` only zeroes newly-added elements, so a buffer recycled from a larger
blob can carry stale set bits into a smaller one, which makes the placer skip a position it must
place at — a dropped ore, not a slow one. RNG draw order and count are unaffected by any of the
allocation work above; the surface stage (see `worldgen-biomes.md`), not the ore engine, is where
worldgen's remaining string-classification cost actually lives.

An ore configuration with more than eight replacement targets bypasses the compact target cache and
evaluates every rule in declaration order. Later targets therefore remain reachable, and each matching
target still receives its own air-exposure decision draw before the next rule is considered.

`overworld_ore_ne_250_neg250_oracle.txt` preserves a packet-derived boundary
control for that separation. Two independent frozen-world packet exports agree
on twenty non-copper cells at chunk `(250,-250)`'s positive-Z edge. Including
the neighbouring chunk containers in this source's selection reintroduces
copper at all twenty cells; keeping those containers only as the 3×3 driver's
read/write context restores the external states.

### Generation-time mob spawns

Vanilla's `ChunkStatus.SPAWN` step places one weighted-species animal pack, once, the moment a chunk
first generates — `spawn_stage::spawn_candidates_for_chunk` is the pure, version-free pick (one
species from the biome's `spawners.creature` list, one pack, one position), riding on
`GeneratedColumn::spawn_candidates`. It is deliberately **not** light-aware: `lodestone-worldgen` has
no light engine, so the raw candidates are re-validated server-side
(`natural_spawn::validate_generation_spawns`) against the same per-species `SpawnRule` and real
column light the tick-driven spawn cycle already uses, before anything is actually spawned. This is
genuinely one-shot: `ChunkColumn::generation_spawns` is populated only in `ChunkColumn::from_generated`,
which only runs on a true disk-miss, so a reloaded chunk never re-proposes candidates, and any mob
that does spawn is covered by the same entity persistence every other mob uses — no bespoke
persistence was needed. Known scope cuts: only the mob-simulation's fixed initial snapshot area gets
generation-time spawns (chunks streamed in later as a player walks do not, matching the existing
tick-driven spawner's own scope), one pick per chunk rather than vanilla's bounded retry loop, and a
group's wander clamps to its own chunk rather than reading a neighbour.

## How to change it, and the gotchas

- **Adding a feature type or placement modifier is a fixed three-edit shape**: a variant, a parse
  arm, a body. The parse function's catch-all is the island factory — a variant added without an arm
  silently becomes `Unsupported`.
- **Never delete or renumber a step-list entry to "clean it up".** Every entry's raw array position
  feeds `set_feature_seed`; removing one shifts every later feature's seed and changes the whole
  chunk downstream of it. An unmodelled type stays in the list as `Unsupported` for exactly this
  reason.
- **A height-scan default direction is load-bearing.** The ocean-floor height scan answers "topmost
  non-motion-blocking" using a deny-list of known exceptions (matching vanilla's `blocksMotion`) —
  anything unlisted counts as solid ground, so extending the list (not narrowing it) is the only safe
  way to fix a plant that floats or double-places.
- **Height scans read the currently-mutating grid on purpose**, which is what lets a later feature in
  the same pass see an earlier one's writes — exactly like vanilla. A wrong predicate here compounds
  rather than merely repeating, so verify against a *second* placement on the same column, not just
  the first.
- **Adding a modifier or fan-out that can produce genuinely different positions must get its own
  `Positions`/`OrePositions` variant.** `Repeat(pos, n)` means the same position n times; smuggling a
  real fan-out into it silently changes what a whole class of placements does.
- **`LODESTONE_VEG_STRICT=1`** turns any unmodelled dispatch into a named panic instead of a silent
  no-op — use it when developing a new placer so a missing arm surfaces immediately rather than as a
  quietly-wrong world.

## Configuration

None beyond the debug/test escape hatch `LODESTONE_VEG_STRICT=1` (panic on unmodelled dispatch).
Everything else is read from bundled `configured_feature`/`placed_feature`/`biome` JSON through
`Resolver`.

## Dependencies

`lodestone-worldgen-core`'s `rng`/`density`/`counters`; `lodestone-worldgen`'s `compose` (tag
resolution and per-biome step-list parsing, shared verbatim between the ore and vegetation engines),
`feature::region_view` (the 3×3 read/write routing both drivers share), `interner`
(`StateId`/`StateInterner`). `lodestone_entity::spawn` (`SpawnConditions`/`MobCategory`) and
`lodestone-server`'s `natural_spawn` for generation-time spawn validation — see
`docs/natural-mob-spawning.md` and `docs/biome-spawners.md` for the tick-driven spawn cycle this
reuses. Verified against vanilla via `scripts/worldgen-oracle/VegetationOracle.java` and the ore
engine's JVM fixtures under each crate's `tests/support/`.
