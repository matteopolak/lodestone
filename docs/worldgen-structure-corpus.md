# The bundled 26.2 structure corpus (worldgen phase S-data)

## What it is

The complete vanilla 26.2 structure dataset, extracted verbatim from the server
jar into `crates/lodestone-server/assets/`, plus the drift gate that keeps it
byte-identical to the jar. 1606 files, 4.42 MiB: 34 structures, 20 structure
sets, 188 template pools, 40 processor lists, 7 world presets, 9 flat presets, 4
noise settings, 92 worldgen tags, and 1212 NBT structure templates. This is
phase **S-data** of [`plans/worldgen-rewrite.md`](./plans/worldgen-rewrite.md)
(issue #484) and it is data only — no placement, no jigsaw, no beardifier.

It exists because structures were blocked twice over: unbuilt in the engine *and*
unbundled as data. Every later structure phase (S1 placement, S2 templates, S3
beardifier, S4 jigsaw) reads from this corpus, so nothing could start until it
landed.

## How it works

### Extraction

`scripts/extract-worldgen-structures.py` opens
`.cache/mc/26.2/versions/26.2/server-26.2.jar` and **byte-copies** zip entries
into `assets/`. The JSON is never parsed and re-serialised, so what ships is what
Mojang shipped. No JVM, no container, no `oracle-java` program: this is all
datapack data, and unzipping it is strictly more authoritative than booting a
server and asking a program to describe it (the same reasoning as
`scripts/extract-damage-types.py`).

| jar path | bundled at | n |
|---|---|---|
| `data/minecraft/worldgen/structure/` | `assets/worldgen/structure/` | 34 |
| `data/minecraft/worldgen/structure_set/` | `assets/worldgen/structure_set/` | 20 |
| `data/minecraft/worldgen/template_pool/` | `assets/worldgen/template_pool/` | 188 |
| `data/minecraft/worldgen/processor_list/` | `assets/worldgen/processor_list/` | 40 |
| `data/minecraft/worldgen/world_preset/` | `assets/worldgen/world_preset/` | 7 |
| `data/minecraft/worldgen/flat_level_generator_preset/` | same name | 9 |
| `data/minecraft/worldgen/noise_settings/` | `assets/worldgen/noise_settings/` | 4 of 7 |
| `data/minecraft/tags/worldgen/` | `assets/worldgen/tags/worldgen/` | 92 |
| `data/minecraft/structure/**.nbt` | `assets/structure/` | 1212 |

`build.rs` already walks `assets/worldgen/` for `*.json` and turns each path into
an `EMBEDDED_WORLDGEN` lookup id, so the 394 new JSON files are addressable with
no build change: `structure/igloo`, `template_pool/village/plains/town_centers`,
`tags/worldgen/biome/has_structure/village_plains`. The NBT templates sit outside
`assets/worldgen/` — under `assets/structure/`, mirroring the jar — and are *not*
in that table, because `build.rs` collects JSON only. Whoever implements S2 picks
their own embedding for them.

### The drift gate

`crates/lodestone-server/tests/worldgen_structure_corpus.rs`, anchored on
`tests/support/worldgen_structure_corpus.txt`: for all 1606 files, the SHA-256
and byte length **of the jar entry**, not of the bundled copy. Regenerating the
manifest requires the jar, which is not repo state, so the manifest is an
external anchor — a hand-edited asset fails the gate and the manifest cannot be
re-derived *from the assets* to hide the edit. Nothing here compares two things
we produced.

A hash manifest rather than a committed verbatim dump (the
`damage_types_jar.txt` pattern) because a dump would duplicate 4.42 MiB; hashes
cost 130 KB and fail just as loudly.

Three drift directions, because re-hashing listed files cannot see an *addition*
— which is how a partial or polluted extraction hides:

| test | catches |
|---|---|
| `bundled_corpus_is_byte_identical_to_the_jar_manifest` | content edits; also asserts the 4,635,950-byte total, so a truncated extraction with correct filenames fails |
| `manifest_covers_the_bundled_tree_exactly` | files added or removed |
| `corpus_counts_match_the_jar_enumeration` | the per-registry enumeration, and that the manifest's own `# counts` header agrees with its rows |
| `manifest_matches_a_fresh_jar_extraction` (`#[ignore]`d) | that the manifest still traces to the jar |

Verified by deliberate perturbation, not description: a same-length character
edit in `structure/igloo.json`, a flipped byte in `structure/igloo/top.nbt`, and
one stray unlisted file each fail with exit 101 and name the offending path.

### Closure checks — is the corpus usable?

Byte-identity says the files are right; it says nothing about whether they
resolve. Five cross-registry joins, each with a predicted magnitude rather than a
sign:

- 20 structure sets reference 34 structures, and **every** structure is named by
  some set (both directions — a structure no set places can never generate).
- All 34 structures state `biomes` as a tag; all 34 resolve into
  `tags/worldgen/biome/has_structure/`.
- 1134 pool `location` references resolve to NBT, 757 `processors` to processor
  lists, 188 `fallback` to pools.
- 10 jigsaw `start_pool` references resolve.
- 16 preset `settings` and 20 `structure_overrides` references resolve.

## How to change it

**Never hand-edit an asset under `assets/worldgen/` or `assets/structure/`.** They
are jar bytes; the gate will catch you and the fix is always to re-extract.

After a version bump: `just regen-worldgen-structures` (re-extracts assets *and*
manifest together, then runs the gate), then update `EXPECTED_COUNTS` and
`EXPECTED_TOTAL_BYTES` in the test to the new enumeration — deliberately
hard-coded so a count change is a decision someone makes, not something that
silently follows the data.

### Gotchas, all measured

- **The outer jar is a bundler.** `.cache/mc/26.2/server.jar` contains none of
  these paths; searching it returns zero hits and looks exactly like "26.2 ships
  no structure data". Always
  `.cache/mc/26.2/versions/26.2/server-26.2.jar`.
- **The NBT set cannot be derived from template pools.** Pools reference 989
  distinct templates; 1212 exist. **224 are named from Java code, never from a
  pool** — all of `end_city/*` (`EndCityPieces`),
  `ancient_city/city_center/*`, and others. Bundling only pool-referenced
  templates omits them, and the omission surfaces only when a structure fails to
  place.
- **Vanilla 26.2 ships a dangling pool reference.**
  `template_pool/ancient_city/walls/no_corners.json` names
  `minecraft:ancient_city/walls/intact_horizontal_wall_stairs_5`; only `_1`..`_4`
  exist in the jar. A gate asserting that *every* pool reference resolves fails
  against the authoritative source. It is carried as a one-entry whitelist whose
  premise is itself checked — that the file really is absent, really is
  referenced, and that `_1`..`_4` really do exist — so the whitelist cannot
  outlive the defect it excuses.
- **`tags/worldgen/` is required and the plan's inventory table does not name
  it.** Every one of the 34 structures states its biome filter as a tag
  reference, with zero inline biome lists, so without the 68 bundled worldgen
  biome tags no structure's biome filter resolves and placement stays blocked on
  data. Found by following what the structure documents reference, not from the
  audit table.
- **`structure_overrides` names structure *sets* directly**
  (`"minecraft:villages"`), not tags. Resolving those 20 references against
  `tags/worldgen/structure/` reads as obviously right and fails on the first
  file. The 20 structure tags are consumed elsewhere.
- **Two template pools list the same element more than once.**
  `bastion/treasure/extensions/{large,small}_pool.json` each repeat
  `bastion/treasure/extensions/empty` (3× and 2×) with separate weights, so there
  are 1134 `location` references across 1131 distinct values. A
  distinct-set count is a *different quantity* than the jar census measures, and
  predicting the wrong one is what caught this.
- **`multi_noise_biome_source_parameter_list` has 2 files in the jar** (nether,
  overworld). The bundle's `biome_parameters/overworld_temperature.json` has no
  jar counterpart under that registry — do not assume that directory mirrors it.

### Cross-unit boundary

`noise_settings/{nether,end}.json`, the nether multi-noise parameter list and 2
absent noises belong to the concurrent **Nether/End** unit, not here. This phase
took the other four noise settings — `amplified`, `caves`, `floating_islands`,
`large_biomes` — and touched none of theirs.

`every_preset_reference_resolves` originally carried that gap as an allowance
bounded to exactly `{nether, end}`, written to report itself as *deletable* once
that unit landed rather than persist quietly as cover for a real gap. It landed
within the session, so the allowance is gone and full closure is required: the
test asserts both files are present by name, and a control asserts the presets
really do reference both, so their absence fails loudly here instead of being
tolerated. The 16 preset `settings` references now all resolve against bundled
data.

The nether multi-noise parameter list is still that unit's, and nothing in this
corpus references it — `world_preset`'s `preset` key points at it, which is why
that key is deliberately *not* one of the closure-checked reference kinds.

## Configuration

- `LODESTONE_REGEN` — no effect on the corpus gate itself; the refresh path is
  `just regen-worldgen-structures`, which re-extracts rather than regenerating a
  Rust table. The `#[ignore]`d jar-trace test errors if `LODESTONE_REGEN` is set,
  rather than silently not writing.
- The extractor takes optional `JAR`, `ASSETS_DIR`, `MANIFEST` positional
  arguments; all three default into the repo, and the `#[ignore]`d test uses them
  to extract into a scratch directory.

## Dependencies

- `.cache/mc/26.2/versions/26.2/server-26.2.jar` — the only acceptable origin
  (data source #1). Needed for extraction and for the `#[ignore]`d test; the
  always-on gate needs only the committed manifest.
- `python3` (stdlib `zipfile`, `hashlib`) for the extractor.
- `sha2` (dev-dependency of `lodestone-server`) for the gate's hashing.
- `crates/lodestone-server/build.rs` — already embeds `assets/worldgen/**.json`;
  unchanged by this phase.
