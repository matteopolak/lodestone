# Nether and End worldgen

## What it is

The composed Nether and End generators (`lodestone_worldgen::nether::NetherGenerator`,
`::end::EndGenerator`) and the engine-level differences from the Overworld that make them possible:
a selectable RNG family, two bespoke Nether biome noises, a disabled-aquifer fill, dimension-specific
cell geometry, the `minecraft:end_islands` density function, and the End's non-multi-noise biome
source. All bundled 26.2 data for both dimensions is complete; the remaining gaps are structure
families and gameplay such as the dragon fight rather than missing terrain or a disconnected
dimension source.

## How it works

### What is shared, and the one flag everything else waited on

`noise_settings/{nether,end}.json` both set `legacy_random_source: true` (the Overworld does not),
which switches the terrain-noise stack to the legacy LCG family rather than xoroshiro —
`rng::Algorithm` and the `Copy` two-variant enums `AnyRandomSource`/`AnyPositionalFactory` make this a
per-dimension constructor argument (`density::Builder::with_algorithm`) rather than a generic
parameter threaded through every stage; the Overworld's own output is unchanged and byte-identical
across the change. Both dimensions also set `aquifers_enabled: false`, which is a bypass rather than
new logic — vanilla's disabled aquifer is just: solid where `density > 0`, else the global fluid
pick at that height — and both
their noise settings feed a cell geometry derived from `size_horizontal`/`size_vertical` rather than
the Overworld's hardcoded 4-wide/8-tall assumption (the End's `2, 1` gives an **8-wide/4-tall** cell,
the transpose of the Overworld/Nether's `1, 2`).

When aquifers are enabled in another settings document, `AquiferSystem::new` carries that document's
`default_fluid` into the global fluid picker as well; the picker does not assume water for every
dimension.

This terrain selection does not choose the feature scheduler's carrier source.
Per-chunk decoration always begins with a fresh xoroshiro `WorldgenRandom`, then derives the
decoration seed and each feature stream from it. In particular, `NetherGenerator::mixed_step7_stage`
must not reuse the legacy terrain carrier: the two seed-scale draws would move every feature placement.

The Nether's mixed feature pass is `NetherGenerator::mixed_step7_stage`. It runs each source's raw
feature entries in index order, retaining a bounded synchronization boundary between the ore reader
and padded decoration grid after every entry. Only the 3×3 intersection is projected to the ore
reader; decoration spill beyond that window remains in the padded grid for the final fold. Do not
restore a split ore-then-decoration pass merely because a packet fixture initially looks closer: at
the captured first Nether chunk, correcting this order exposed 231 additional differing cells, which
identifies previously masked feature-body or input defects rather than a valid ordering exception.

Nether noise and the packet-ready terrain carrier remain 128 rows high, while the resident decoration
working window spans the dimension's full 256 rows. Top-relative placement anchors still resolve
against the 128-row generated depth; the dispatcher adjusts only those anchors when copying parsed
placement trees into the wider grid. Writes in the upper half bypass the 128-row ore view and remain
lifecycle spill, so later feature reads can observe them without turning them into packet terrain.
The same 5×5 source context carries replicated 3-D Nether biome cells into the vegetation grid, so
candidate-biome modifiers reject features whose candidate is in a different biome; ore replacement
keeps its separate block-level zoom lookup because its biome test is evaluated at the candidate block.

Both generators run vanilla's own stage order: structure starts → refs → beardifier → fill (shape,
with the disabled-aquifer fluid picker) → biome → surface → carve → structure placement. Neither
stage reads a neighbour's *terrain* product (only the starts map, a pure function of `(seed, chunk)`,
is memoised), so both are pure functions of `(seed, chunk)` and can be generated in any order on any
thread.

### Nether

Biome assignment is two-dimensional and *is* the map: the Nether's climate-parameter table
(5 rows, one per biome, derived from vanilla's registry rather than shipped as reusable JSON) has all
non-temperature/humidity channels zeroed and every row a degenerate point (no ranges), and the two
noises behind temperature and humidity (`nether/temperature`, `nether/vegetation`) are seeded
specially — `LegacyRandomSource(seed + 0)` / `(seed + 1)`, the raw world seed plus a small offset,
**not** a positional fork, regardless of the legacy-random-source flag. Both noises declare only two
octaves but a nonzero `firstOctave`, so vanilla constructs and discards a zero'th-octave noise and
skips several more before building the two it keeps — porting the octave *count* without the
skipped-draw count consumes the wrong amount of randomness and produces a plausible but wrong Nether.
The seed-derived biome-zoom salt is a separate immutable value: `NetherGenerator` computes
`nether_zoom_seed` once during construction and reuses its 64-bit digest prefix for block-biome and
mixed-decoration lookups. This cache changes cost only; it does not memoise positions or alter seed
bytes, traversal, or output order. Preserve the helper's little-endian seed input and digest prefix,
then compare generated content and packet bytes when changing it.
The lava "sea" (`sea_level 32`, `default_fluid` lava) is the disabled aquifer's global fluid picker,
not a real aquifer — and the Overworld's `-54` deep-lava threshold is unreachable at the Nether's
`min_y 0`, so it is not "an aquifer whose second fluid is lava," it is a flat fluid boundary at y=32.
Surface rules use a strict subset of the Overworld's condition types (no new condition type needed).
The `nether_cave` carver shares `CaveWorldCarver`'s codec with the Overworld's `cave` carver but
overrides thickness, Y-scale and its block-write rule — routing it through the Overworld's thickness
formula desyncs the RNG stream on the very first tunnel.

Structures: the same `structure::beardifier`/`StructureRegistry` machinery the Overworld uses,
filtered per-dimension (`StructureRegistry::new_for_biomes`) to only the sets whose biomes exist here,
plus a dimension-specific height probe over this dimension's own density field. `bastion_remnant`
places real blocks (jigsaw); `nether_fossil` places real blocks (template, and the dimension's only
adaptation-bearing structure, so it is what first made the Nether's beardifier observably non-empty);
`ruined_portal_nether` places its frame and terrain refinement. `fortress` still has no piece
generator and yields an advisory (blockless) start.

A biome resolution tie at one recorded coordinate disagrees with the real vanilla world by
construction — an exact climate-distance tie where vanilla's answer depends on the previous query on
the same thread (see `worldgen-biomes.md`'s tie-break discussion), which this engine's
demand-ordered, reorderable generation model cannot and must not reproduce. This is the one place
where matching vanilla bit-for-bit and being deterministic are incompatible, and determinism wins;
the parity gate classifies such disagreements as admissible only when the two candidate biomes are an
exact fitness tie, rather than tolerating them by threshold.

### End

The End biome source is not multi-noise: `EndBiomeSource` is a closed-form function of chunk
position (a radius-64-chunk main-island hole) and one `cache_2d(end_islands)` erosion sample,
thresholded into five constant biome ids. `EndIslandNoise` consumes its seed through the legacy
random stream before constructing its simplex sampler; its Java-style truncating division, `f32`
intermediates, and mixed-width boundary predicates are all load-bearing. `EndGenerator` carries that
source through fill, surface, materialization, and the served quart-biome grid.

At the server boundary, `EndChunkSource::column_at` maps `Shaped` to the generator's immutable
fill/surface/structure prefix and `Full` to the complete decorated column. End decoration owns a
three-by-three write window, so `EndChunkSource::packet_generation_stage` upgrades a shaped packet
request to `Full`; otherwise a source write crossing into the target could be absent from the packet.
Keep that upgrade policy explicit if the End's write radius or packet lifecycle changes.

The End has no fluid at all (its sea level and fluid-level settings make the disabled aquifer's fluid
picker return air everywhere, regardless of what `default_fluid` names) and no bedrock (its surface
rule is a same-value no-op and, unlike the Nether, contains no `vertical_gradient` construct at all —
copying the Nether's floor/roof shape here would be actively wrong). There is no carver (no bundled
End biome names one). Its structure stage samples and places End-city template pieces.
The End density and surface passes intentionally remain 128 rows (`Y=0..127`), while the resident
build grid and served column retain the full 256-row dimension window (`Y=0..255`), initialized to
air above the terrain. Structure placement runs after that widening: an End-city template can write
above the noise ceiling (the seed-42 witness at `(283,86)`, local `(12,133,0)`, is a purpur pillar),
and extraction plus spatial-batch copies must preserve those rows. Do not widen the density field or
surface loops themselves; that would change terrain evaluation rather than merely retain structure output.

The survival reference save still has no End region, but this is no longer an evidence gap:
`scripts/worldgen-oracle/EndChunkOracle.java` runs the bundled 26.2 server classes directly and emits
`crates/lodestone-worldgen/tests/support/end_chunk_jvm.txt`. The `end_gen` gate compares every block
run and quart biome for a main-island chunk, an outer-ring chunk, and a distant small-islands biome across two
seeds. Regenerate the fixture through `scripts/worldgen-oracle/run.sh EndChunkOracle`; do not replace
it with output from `EndGenerator`.

### Decoration and structures, remaining

Every biome document for both dimensions already carries its full decoration step-list wiring and
every referenced configured/placed feature is bundled. `EndGenerator` reads the fixed platform entry
from `the_end` and applies its 5×5, four-row block shape after materialization. The independent
`EndPlatformOracle.java` fixture covers those 100 writes. The generator constructs a three-by-three
decoration region before serving its centre, so outer-island, chorus, and spike writers from
neighbouring source chunks compose into the served column. Chorus plant connection state resolves
the bundled `supports_chorus_plant` block tag for its downward connection; this matters on End stone,
which is a valid support even though it is not itself another chorus block. A spike is selected only by the chunk
holding its centre; its circular block footprint is then clipped by each served column, while its
crystal remains a gameplay entity. Return gateways carry their block position, exit, and
exact-teleport flag through `EndColumn::gateways`; `ChunkColumn::from_end` turns that sidecar into a
persisted block entity. Gateway metadata is resolved from the configured feature rather than supplied
as a generator default: `EndDecoration::from_resolver` follows the End-highlands step-4 placed-feature
reference to the configured entry, and the decoration pass copies its three-coordinate `exit` and
`exact` values into `EndGateway`. A configured gateway without a parseable exit is not treated as a
return gateway, keeping delayed destination search separate from a fixed worldgen destination. The
End-filtered structure registry samples city starts from the End's own
pre-surface density field and applies intersecting template pieces before palette extraction. Its
recursive assembler keeps its ship choice at city scope, including collision-rejected branches, so
the template list cannot acquire a second ship later in the same city. The positive
`end_city_jvm.txt` capture gates one start, its nine-piece sequence, and two placed block states. The
terrain fixture deliberately stops before later writers, so it is not evidence that they were placed.
`EndChunkSource::generate` copies complete city starts and the chunk's intersecting references onto
the served `ChunkColumn`; it resolves referenced origins again for template-owned container payloads
before the column reaches packet encoding or region persistence. This source attachment is separate
from block placement so a city can remain visible while its save metadata and container sidecars are
still checked independently. Patterned black banners are template block states with a separate
component payload, so a direct source column also materializes their missing banner records. End
lifecycle replay keeps those structure records in its resident save-sidecar state, but its detached
packet snapshot filters structure-owned records at the authenticated status boundary; the external
lifecycle stream therefore carries the banner blocks without emitting banner entities.
The End's retained motion-blocking heightmap is taken from the final served
column, after intersecting city pieces and the three-by-three decoration pass.
This is the same content boundary as the three client heightmaps: a later
writer that adds a taller block must be reflected in the map sent with the
chunk. The focused external control is
`scripts/worldgen-oracle/stream-parity.sh --dimension end --cx -2 2 --cz -2 2`,
which compares terrain, biomes, heightmaps, and block entities while
deliberately excluding light.
The integrated server's `DimensionalSource` builds the Nether and End sources lazily behind the
same chunk lifecycle used by the primary dimension. A generated Nether column therefore passes
through the shared cache, the dimension-specific Anvil region path when persistence is enabled,
and the lazy dimension save registry flushes that region source on autosave and shutdown. It then
passes through lighting and the selected protocol's chunk encoder. The in-memory packet gate in
`crates/lodestone-server/tests/integrated_memory.rs` drives that connection path and checks the
Nether's 256-row wire window plus a bedrock floor marker against the external full-region oracle
at `crates/lodestone-worldgen/tests/support/nether_vanilla_oracle.txt`. End portal entry lands on
the generated fixed platform, and generated outer-island return gateways carry a destination sidecar
that the server consumes on contact. Exact exits use the stored point; delayed exits search the
destination terrain for a safe standing cell, and missing exits remain inert. Contact with a
generated or persisted portal also records its cell in the shared point index before travel, so a
return trip and a restart reuse that portal without a broad cold-terrain scan. The integrated-server
restart gate in
`integrated::tests::persistent_generated_nether_sibling_survives_portal_restart` also verifies
generated Nether edits and both dimension portal indexes survive and remain findable after reopening.
A full external-client movement replay remains outside this hermetic fixture. The remaining
worldgen-side gap is that `EmbeddedResolver` hardcodes the Overworld's documents for the default
singleplayer path, so a future resolver split must preserve each dimension's own data source.

## How to change it

- **Never assume a dimension can reuse the Overworld's fixed 4×8 cell geometry** — read it from the
  settings document (`aquifer::cell_geometry`) per dimension.
- **A biome name a dimension's generator can produce must have its carver/feature lists resolved at
  construction**, or its columns silently never carve/decorate.
- **The Nether has no fixed-biome fallback, deliberately** — an empty biome-parameter table panics
  rather than degrading to a uniform biome that looks fine in a screenshot but is wrong.
- **Do not copy the Nether's bedrock floor/roof shape into a new dimension without checking its
  surface rule for a `vertical_gradient` construct first** — the End's absence of one is the tell that
  it has no bedrock at all.
- **Any RNG draw-count change (skipped octaves, carver thickness draws, structure draws) changes the
  generated world** even when every affected value still looks plausible; the biome gates catch a
  wrong noise seeding immediately, but a wrong carver draw count is only visible in a full block
  comparison.
- **Data refresh after a version bump**: re-extract via the `#[ignore]`d gate in
  `crates/lodestone-data/tests/worldgen_dimension_data.rs`
  (`LODESTONE_REGEN=1 cargo test -p lodestone-data --test worldgen_dimension_data … -- --ignored`);
  the Nether's climate-parameter table has no jar entry to copy and is regenerated from
  `NetherParametersOracle` instead.
- **Keep End gateway metadata data-driven**: when the bundled feature layout changes, update
  `EndDecoration::gateway_config_in_step` and its resolver tests to follow the biome step's
  placed-feature reference and parse a three-integer exit. Do not restore a fallback exit for
  missing or malformed metadata; absence disables return-gateway sidecar output.
- **Apply candidate-level biome filtering after placement coordinates are chosen.** The source
  chunk's biome determines whether an End feature participates in that FEATURES pass, but the
  final placement modifier resolves the candidate block through the seed-fiddled nearby-quart
  zoom. Near outer-island biome boundaries these answers can differ; skipping the second check
  creates chorus plants or return gateways that the reference generator rejects.

## Configuration

No runtime configuration; both dimensions' data is embedded at build time by
`lodestone-server`'s `build.rs` under `assets/worldgen/` (`noise_settings/{nether,end}`,
`noise/nether/{temperature,vegetation}`, `biome_parameters/nether`, the five Nether and five End
biome documents, `density_function/{nether,end}/base_3d_noise`, `end/sloped_cheese`,
`configured_carver/nether_cave`).
The End return gateway is selected by the End-highlands placed-feature reference and its configured
feature. In the bundled data, that configured object carries `{ "exact": true, "exit": [100, 50,
0] }`; the placed object owns rarity, square spread, heightmap anchoring, and the random Y offset.
An entry without a three-integer exit is not eligible for `EndColumn::gateways` and remains available
only to delayed destination logic.

## Dependencies

`lodestone-worldgen`'s `aquifer`, `biome`, `compose`, `surface`, `dense_grid`, `interner`,
`structure::beardifier`; the End decoration path also depends on `density::Resolver` and
`serde_json::Value` to read configured and placed feature documents. `EndColumn::gateways` is
consumed by `lodestone-server::ChunkColumn::from_end` when constructing persisted block entities.
`lodestone-worldgen-core`'s `density`, `engine`, `noise::SimplexNoise`,
`rng::{Algorithm, LegacyRandomSource}`. Evidence: the Nether is verified against a real vanilla
26.2 server's own generated region files (`.cache/mc/survival/world/dimensions/minecraft/the_nether`,
seed −195764831) for both biome assignment and bedrock shell; the End terrain and fixed-platform
fixtures are captured by its bundled-server oracle harnesses (see above).
`scripts/worldgen-oracle/{NetherParametersOracle,DensityOracle}.java` for the Nether's climate table
and future `end_islands` verification. See `docs/worldgen.md` for the shared density/RNG engine,
`docs/worldgen-biomes.md` for the Overworld's own climate search and tie-break behaviour this doc's
Nether section builds on, and `docs/worldgen-structures.md` for the shared structure machinery.
