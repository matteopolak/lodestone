# The Nether and the End: the group NE issue tree

## What it is

The executable unit sequence for unit group **NE** of
[`worldgen-rewrite.md`](./worldgen-rewrite.md) (its U13 row): Nether and End terrain —
legacy RNG wiring, the bespoke noise instantiations, the `nether_cave` carver, the
disabled-aquifer fluid picker, the Nether multi-noise biome source, the `end_islands`
density type, the End's own biome source, End cell geometry, and the serve seam that keeps any of
it from being an island. Written 2026-08-08 against `HEAD` `5f37fb83`. Engine work only:
**the data phase is complete** (verified below), and portals/dimension travel are
gameplay, out of scope — a Nether generator is oracle-testable with no portal existing.

## What was verified vs assumed (2026-08-08)

- **All 7 `noise_settings` are bundled** (`ls assets/worldgen/noise_settings/` — amplified,
  caves, end, floating_islands, large_biomes, nether, overworld), **all 63 noises**, and
  **`biome_parameters/nether.json`** (the list the worldgen-data-census issue paid a bespoke JVM oracle for). The
  rewrite plan's "6 of 7 missing / 2 of 63 noises missing / nether parameter list missing"
  was true when written and is now wrong — corrected in its inventory in this same commit.
  **There is no `[data]` item left for either dimension**
  ([`worldgen-dimensions.md`](../worldgen-dimensions.md)).
- **Key settings, read from the bundled documents, not recalled**:
  `nether.json` — min_y 0, height 128, sea_level 32, `aquifers_enabled: false`,
  `legacy_random_source: true`, `ore_veins_enabled: false`, size_horizontal 1 /
  size_vertical 2 (**the same 4×8 cell as the overworld — cell-geometry plumbing is
  End-only**). `end.json` — min_y 0, height 128, sea_level 0, size 2/1 (**8-wide/4-tall
  cells** vs the engine's constants), legacy true, aquifers false.
- **`Density::build_object` (`crates/lodestone-worldgen-core/src/density/mod.rs`) panics on `minecraft:end_islands`** (the `other =>` arm of the
  builder). Two live trigger paths, per
  the worldgen-data-census issue's correction: inline in `end.json` **and** inside the already-bundled
  `density_function/end/sloped_cheese.json`. It is the only DF type used anywhere in
  vanilla's worldgen data that we do not implement (plan census).
- **`CarverConfig::parse` (`crates/lodestone-worldgen/src/carver/mod.rs`) panics on `nether_cave`** (`unsupported carver type`); the config
  document is bundled and unreferenced by any overworld biome, so the panic is latent.
- **`Resolver::biome_parameters(&self)` takes no dimension argument** — verified at
  `crates/lodestone-worldgen-core/src/density/mod.rs` (default method returning
  `Value::Array(vec![])`), with `EmbeddedResolver::biome_parameters`
  (`crates/lodestone-server/src/worldgen_data.rs`) hardcoding
  `biome_parameters/overworld`. So the Nether list is structurally unreachable today —
  the census is right. **But the fix need not widen the trait**: see NE4 — a per-dimension
  resolver *value* (a wrapper overriding `biome_parameters` and the settings path) reaches
  the same end with no signature change and no churn across the trait's other
  implementors. Widening is the fallback if the wrapper turns out to leak dimension
  decisions into call sites.
- **A vanilla-authored Nether block-and-biome oracle exists on disk**:
  `.cache/mc/survival/world/dimensions/minecraft/the_nether/region/` — **4 region files,
  2,444 generated chunks**, written by the real vanilla 26.2 server at seed **-195764831**
  (read from `world/data/minecraft/world_gen_settings.dat`). Biome census, measured by
  region scan: nether_wastes in 487 chunks, crimson_forest 327, soul_sand_valley 255,
  basalt_deltas 172, **warped_forest 0** — a world-species limit every gate below must
  assert, not discover. Structure starts there (bastion 2, ruined_portal 2,
  nether_fossil 22) belong to group S. **There are zero End region files in any cached
  world** — the End's composed gates need evidence acquisition (below).
- **The oracle runtime is Apple `container`, not Docker, and it is currently up**
  (`container list` shows `lodestone-survival` running; `docs/oracle-runtimes.md`). New
  JVM fixtures (`scripts/worldgen-oracle/run.sh`, `eclipse-temurin:25-jdk`) and extending
  the survival world (including making the server generate End chunks) appear available.
  Units below land without that (belt) and name the strengthening (suspenders).
- **13 parity binaries** (11 `*_parity.rs` + `overworld_gen.rs` +
  `lodestone-worldgen-parity`'s composed gate) define "the overworld did not move";
  every NE unit runs all of them — NE touches shared code (`density::Builder`, carver
  parse, resolver seam), so overworld byte-identity is each landing's first gate.
- Assumed, not verified: that vanilla's Nether biome storage in the region files is
  vertically uniform per column (its climate points are y-independent in 1.18+ lineage).
  **NE4 asserts this from the oracle data before relying on it** — if false, NE4 inherits
  a dependency on 3-D biome sampling (the 3-D-biome-sampling issue's U11) and must say so.

## The evidence problem, and its shape here

No JVM on the machine (`789c6869`). The two dimensions differ sharply:

- **The Nether has a block-level outside oracle on disk** (the 2,444 chunks above), so
  every Nether unit funnels into one composed gate, NE5, whose expected values vanilla
  wrote. Unit-level gates lean on committed dumps (the RNG/noise primitives are already
  JVM-proven — `rng_java.txt`, `noise_java.txt`) plus record definitions for the *wiring*,
  and NE5 is the discriminator that catches a wiring lie.
- **The End has no block oracle anywhere**, so its units gate on record-definition
  transcription + arithmetic invariants at landing, with the composed gate a **named
  deferred obligation** until End regions are generated in the oracle world (one
  `container` session; the acquisition step is part of NE7's issue, not hand-waved).

The confounder NE5 must design around: vanilla's Nether chunks are fully decorated, and we
run 3 of 11 decoration steps, have 48/55 feature types unimplemented (the missing-feature-types issue), and skip
`scattered_ore` (nether gold/quartz placement). A whole-chunk byte-equality gate is
therefore structurally red. The gate must **classify, localise and predict** instead —
see NE5.

## Unit sequence

### NE1 — `legacy_random_source`: the algorithm switch (M)

The gating item: both dimensions set it true; the engine hardcodes the Xoroshiro branch.
Exactly as the algorithm-switch issue sizes it: `Builder`'s concrete
`XoroshiroPositionalFactory` field **and** `Builder::positional_factory`'s concrete return
type become polymorphic (enum over the two factories — both already exist and are
JVM-proven), plus threading the flag from the `noise_settings` document.

**The sequencing decision the algorithm-switch issue asked for, taken here**: land NE1 against the *current*
interpreter now, and accept that U4 (the flattened-engine rewiring issue, re-aimed, not started) re-does the wiring
inside the flattened engine. Rationale: U4 has no landing date, group NE is
schedule-visible, the wiring is small, and the redo is mechanical once the enum exists.
Record the redo obligation in that rewiring issue.

**Gate.** (a) Overworld first: all 13 parity binaries + composed fixture byte-identical
(the flag is false there; any drift means the polymorphism itself moved a draw).
(b) Wiring: a probe test constructs a Builder from `nether.json` and asserts the legacy
arm was taken (a counter/marker on the factory, not a doc-comment), and the equivalent
xoroshiro assert for `overworld.json` — fork on `#[cfg(test)]`-visible state, so the
interception is assertable. (c) The discriminating evidence is NE5's oracle comparison —
stated in the unit so nobody reports NE1 "verified" from (a)+(b) alone.
**Strengthening**: a `NoiseOracle` legacy-arm dump under `container` (available now).

**Cost counter.** Factory selection happens at generator construction, never per block:
the density-eval and allocation counters over a C_ss sweep must be **identical to the
digit** pre/post (the harness's 905,459-exact precedent is the standard).

### NE2 — the two bespoke Nether noises + `NormalNoise`/`BlendedNoise` legacy init (M)

Record definitions: vanilla's own per-noise wiring visitor (`nether/temperature` and
`nether/vegetation` instantiate via a legacy-nether-biome `NormalNoise` constructor seeded from
a legacy random source at `seed + 0` and `seed + 1` — **raw world seed, not a positional
fork**, which is exactly why these two were once the only missing noises);
that same legacy-nether-biome constructor uses the older initialization path; vanilla's own
noise-wiring "wrap new" step
(`BlendedNoise` legacy arm: a legacy-instance constructor vs a hash-of-name constructor).
`PerlinNoise::new_legacy` exists but is private and blended-noise-only — this unit opens
the `NormalNoise` legacy path.

**Gate.** Unit level: octave seeding asserted against hand-derived expectations computed
with the already-JVM-proven `LegacyRandomSource` (the chain of custody: primitive proven
by committed dump → wiring pinned by construction-order assert → composed value proven by
NE4/NE5's oracle). Control: swap `seed + 1` for `seed + 0` in the test's expectation and
observe the assert fail (detector-works, run once). **Strengthening**: extend
`NoiseOracle` with the two nether noises under `container`.

**Cost**: instantiation-time only; same counter-identity requirement as NE1.

### NE3 — disabled aquifer, lava sea, and the `nether_cave` carver (M)

(a) `aquifers_enabled: false` → vanilla's own disabled-aquifer semantics: solid where
`density > 0`, else the global fluid picker — for the Nether every position resolves to
lava at y=32 (from vanilla's own per-generator fluid-picker construction; **not** the overworld
aquifer with lava as second fluid — the worldgen-data-census issue's audit correction stands: `min(-54, 32)` is
unreachable at min_y 0). The flag is currently unread; the overworld path must be
bit-identical after the branch lands (parity suite). (b) `nether_cave` carver replacing
the `CarverConfig::parse` (`carver/mod.rs`) panic — record definition: the 26.2 carver sources (cave variant
with lava replacement below the carve liquid level and the Nether replaceable set); its
config document is bundled.

**Gate.** The lava-sea invariant is exactly predictable from the record definition and
checkable against the oracle *today, independent of every other unit*: in vanilla's 2,444
Nether chunks, open (non-solid) cells at y < 32 are lava, at y ≥ 32 air (excluding
decoration-placed fluids — classify by block). Assert the same invariant on our output,
and assert the *counts* agree per chunk column with mismatch bounding boxes. Carver gate:
carve-shape agreement rides NE5 (a carver-only comparison is impossible to isolate from a
final-state chunk); unit-level, the config parse of all 4 bundled carver documents with
zero panics, plus the overworld parity suite for the untouched arms.

**Control**: run the lava invariant with sea_level misread as 63 (the overworld value) —
predicted failure count ≫ 0, observed once. **Cost**: no overworld change (counters
identical); Nether fill cost recorded by the same stage counters, reported not gated.

### NE4 — the Nether biome source behind a per-dimension resolver view (M)

A `DimensionResolver` wrapper (or equal mechanism) that supplies
`biome_parameters/nether.json` and `noise_settings/nether` to a second generator instance
— the smallest change that makes the worldgen-data-census issue's oracle-bought parameter list reachable. Trait
widening only if the wrapper leaks.

**Gate — the strongest one in this group.** Per-quart biome equality against the biome
palettes vanilla persisted in the 2,444-chunk oracle: for every generated Nether chunk,
our multi-noise assignment (through NE2's legacy noises) equals vanilla's stored biome,
reported as mismatch count **by location with bounding boxes**, target 0. Preconditions
asserted, not assumed: (a) chunk-count floor 2,444; (b) per-biome floors from the measured
census (487/327/255/172); (c) **vertical uniformity of vanilla's stored Nether biomes** —
if this assert fails, stop and re-plan against the 3-D-biome-sampling issue; (d) warped_forest recorded as
unexercised (world species) — the gate output must name it, and the strengthening is one
`container` session extending the world into a warped forest.

**Control**: perturb one climate channel's parameter by one ulp-scale step and observe
mismatches appear (detector-works); magnitude predicted: boundary quarts flip first, so
mismatches must hug biome borders — a mismatch in a region interior indicts the noises
(NE2), not the search. **Cost**: biome sampling reuses the memoised store layer (U9);
counter: climate searches per chunk equal the overworld's figure.

### NE5 — the composed Nether generator on the oracle, classified and localised (L)

Instantiate the full Nether pipeline (settings → router → fill → surface → carve →
the 3 implemented decoration steps) and run the block-level comparison against all 2,444
vanilla chunks. **Design, honouring the confounder:**

- Classify every position into: base-terrain class (netherrack/basalt/blackstone/soul
  classes vs air vs lava — surface rules + fill + carve territory, ours to get right) vs
  decoration-attributable blocks (the missing-feature-types issue's absent feature types: fungi, glowstone,
  ore blobs, etc. — enumerated from the bundled biome JSONs' step arrays, an outside
  source, not from "whatever mismatched").
- **Predict both hypotheses before running**: correct-wiring hypothesis — base-terrain
  mismatches near zero and *localised to decoration-disturbed cells* (a placed huge
  fungus converts terrain cells); wrong-RNG hypothesis (NE1/NE2 wrong) — shape mismatch on
  the order of half the volume. Then run the wrong arm deliberately (force Xoroshiro) as
  the observed-failing control and record its measured count next to the real run's. That
  pair of numbers is the unit's headline evidence — the §12.117 shape: two independent
  constructions, differing-cell counts, bounding boxes, and a signed/classified verdict.
- Failure output prints per-chunk mismatch counts and 3-D bounding boxes (location, not
  fraction — a uniform 2% and a localised blob must read differently).

**Consumer**: NE6 — this unit's generator must not terminate in its own gate.
**Cost**: Nether C_ss measured with the U1 counters and recorded (its own budget line;
no overworld impact, counter-asserted).

### NE6 — the serve seam: host the Nether without portals (M)

The island rule applied: a generator consumed only by its gate is orphaned by this repo's
own census definitions. Smallest production consumer: let the integrated server host
`minecraft:the_nether` as the served dimension behind a world/config option (the shape
`world_gen_settings.dat` already enumerates), including the Anvil path seam —
`RegionChunkSource::new` (`region_source.rs`) hardcodes `dimensions/minecraft/overworld` (this plan's census row).
**Scope boundary**: this is *hosting one dimension*, deliberately not the portal/dimension-travel issue's
multi-dimension travel (portals, respawn plumbing — gameplay). Gate: join the hosted
Nether from the real client; a served-chunk screenshot with sky/fog aside — the on-screen
change is Nether terrain in the world rect, and the serve-path counters show the Nether
generator's stages ran (not the overworld's). Persistence round-trip: save/load through
the dimension-correct region path.

### NE7 — `minecraft:end_islands` + End evidence acquisition (M)

The DF type behind the `:916` panic. Record definition:
`DensityFunctions.EndIslandDensityFunction` — transcribed in
[`worldgen-dimensions.md`](../worldgen-dimensions.md); **seeding never consults
`legacy_random_source`** (`new LegacyRandomSource(seed)` unconditionally, in its
constructor), so this unit is independent of NE1 and could land any time
(the algorithm-switch issue's correction). `SimplexNoise::new` is generic over `RandomSource` and takes a
`LegacyRandomSource` directly.

**Gate.** (a) Hand-computed expected values from the transcription at ~8 sample points
(origin cell, an island-edge cell, a far cell), computed outside the implementation
(spreadsheet-style arithmetic from the record definition, committed as literals with the
derivation in comments) — predict values, both for the correct form and for one plausible
mis-transcription (e.g. the `i/j` vs `x/z` scaling swap) so the gate discriminates.
(b) Invariants: range clamp exactly `[-0.84375, 0.84375]`-derived bounds, radial decay
sign, 8-block lattice periodicity of the underlying draw structure. (c) Loading
`end.json` and `end/sloped_cheese.json` stops panicking — both trigger paths exercised.
**Deferred obligation, named**: a `DensityOracle` end_islands dump and End region
generation in the oracle world (one `container` session) upgrade this to outside-origin;
until then the unit's record is "transcription + arithmetic, no vanilla-origin values",
stated in the landing message.

### NE8 — the End's own biome source, End cell geometry, and the End generator (M)

(a) the End's own biome source — not multi-noise; four thresholds over the erosion router slot
(which holds `cache2d(end_islands)`), record vanilla's own biome-lookup routine for it, ~a page.
(b) Cell geometry: End cells are 8×4 (`size 2/1` × 4, vanilla's own cell-height/cell-width accessors) against
the engine's 4×8 constants; `NoiseChunkSampler::new` already takes the dims as parameters
— thread them from the settings. Gate by counter: cells-per-chunk and corner-evaluation
counts must land on the 8×4 prediction, with the 4×8 hypothesis's counts precomputed as
the wrong-arm value. (c) Instantiate the End pipeline behind NE6's seam (host-the-End
option). Composed gate: deferred to the End regions NE7's acquisition step produces; at
landing, gates are the counter predictions, the biome-threshold record transcription, and
overworld/Nether parity untouched.

### Out of scope, said explicitly

Portals and dimension switch (gameplay; NE6 hosts, it does not travel). The dragon
fight, respawn, gateways beyond the worldgen-placed `end_gateway_return` (the worldgen-data-census issue's audit:
the other paths are gameplay). Fortress/bastion/end_city/nether_fossil generation
(group S — see [`structures.md`](./structures.md); nether_fossil is also S3's
beard_thin oracle once both groups land). `scattered_ore` and the other absent feature
types — NE5 *classifies around* them and must not quietly absorb them. Ore veins
(`ore_veins_enabled` is false in both dimensions; U15 is overworld work).

## The biggest risk

**NE5's classification becoming a tolerance in disguise.** The composed gate must exclude
decoration-attributable mismatches to be green at all, and every exclusion is a place a
real terrain bug can hide. The defences are structural: the exclusion set is derived from
the bundled biome JSONs' feature lists (outside source), never from observed mismatches;
the wrong-RNG control's measured count is recorded next to every green run; and as the missing-feature-types issue
closes feature types, the exclusion set must shrink monotonically — a ratchet, asserted,
so the gate converges on byte-equality instead of ossifying at "classified green".

## Configuration

None new at plan time; NE6 adds the hosted-dimension option and must record it here and in
its own doc.

## Dependencies

`crates/lodestone-worldgen{,-core}` (Builder, carver, noise, rng), the staged store (U6),
`crates/lodestone-server` (`worldgen_data.rs` resolver seam, `region_source.rs` NE6 seam),
bundled data under `assets/worldgen/`, `.cache/mc/survival/world` (Nether oracle),
`.cache/mc/26.2/src/` (record definitions), `scripts/worldgen-oracle/` + Apple `container`
(strengthening). Companions: [`worldgen-dimensions.md`](../worldgen-dimensions.md) (the
engine-gap report this plan sequences), [`worldgen-gap-census.md`](../worldgen.md)
§1, and the worldgen-data-census, algorithm-switch, 3-D-biome-sampling, missing-feature-types
and portal/dimension-travel issues named above.
