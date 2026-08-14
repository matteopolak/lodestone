# The `SPAWN` generation stage: animals placed once, at chunk generation

Issue [#518](https://github.com/matteopolak/lodestone/issues/518), parts 2-4 (part 1, the
`spawners`/`spawn_costs` parse, landed earlier — see
[`docs/biome-spawners.md`](./biome-spawners.md)).

## What it is

Vanilla's `ChunkStatus.SPAWN` step places a handful of `MobCategory.CREATURE` animals once,
the moment a chunk is generated for the first time — which is why a freshly generated
vanilla world already has cows and sheep on it before any spawn tick runs. This is that
step, ported as three pieces that only make sense together:

* [`lodestone_worldgen::spawn_stage::spawn_candidates_for_chunk`] — the pure, version-free
  pick: one weighted species from the chunk's biome `spawners.creature` list, one pack, at
  one random position. Runs inside `OverworldGenerator::intern_from_dense`, right after the
  final palette/block field exists, and the result rides on
  [`GeneratedColumn::spawn_candidates`].
* `ChunkColumn::generation_spawns` (`crates/lodestone-server/src/chunk.rs`) — carries those
  candidates from `ChunkColumn::from_generated` into the server. **Populated only there**,
  which is what makes this one-shot: `from_generated` is only ever called on a genuine
  disk-miss (`RegionChunkSource::column` calls the generator only when a saved region has no
  chunk yet), so a column loaded off disk never carries any.
* `NaturalSpawner::validate_generation_spawns` (`crates/lodestone-server/src/natural_spawn.rs`)
  — re-validates each raw candidate against the real per-species [`SpawnRule`] and this
  world's own light, through the exact `permits` gate the tick-driven spawn cycle already
  uses. `MobHandle::reseed` drains the candidates, runs this, and calls `MobSim::spawn_species`
  for whatever survives.

## Why the candidates are unconditioned on light when they leave worldgen

`lodestone-worldgen` has no light engine — light needs neighbour-aware propagation, which is
a server-side concern (`lodestone_world::compute_column_light`, already built for the
tick-driven spawner). A placement port built on isolated-column data would be the *world*
species of vacuous test `CLAUDE.md` warns about: it would pass its own tests and place mobs
in the wrong places. So [`GenerationSpawn`] is a **candidate** — the position vanilla would
consider — and the light/Y-band/ground gate happens once, server-side, reusing infrastructure
that already has a real light cache instead of a second copy.

## The persistence decision, explicit

**Chosen: rely on chunk-generation being genuinely one-shot, plus the entity persistence that
already exists (issue #303's `EntityStorage`) — not a new persistence mechanism for this
feature.** Two already-true properties of this tree make that sound, not merely convenient:

1. **Terrain generation itself only runs once per chunk, ever.** `RegionChunkSource::column`
   checks the on-disk `edits` map and the Anvil region file first; it calls
   `OverworldGenerator::column` (and therefore `ChunkColumn::from_generated`) only on a
   genuine miss. So `ChunkColumn::generation_spawns` is non-empty at most once in a chunk's
   whole lifetime — a server restart re-loads that chunk from disk and gets an empty list,
   never a second placement.
2. **Any mob `MobSim::spawn_species` creates is already covered by `EntityStorage`.** The
   autosave path saves the *whole* live mob population generically (`server.mobs`, not a
   subset), and the world-open reseed path restores it before anything else can populate the
   sim. A generation-spawned cow is therefore persisted and reloaded exactly like a
   naturally-spawned or player-tamed one — nothing new needed there.

What stops a player seeing the herd respawn after a reload: the *terrain* for those chunks is
now on disk, so step 1 never re-runs `from_generated` for them, and the *mobs* are separately
persisted per step 2. Losing them would need losing the `entities/` region files, which is the
same failure mode as losing any other saved entity — not specific to this feature.

## Known scope cuts

* **Only the small `ChunkWorld` snapshot `MobHandle::reseed` builds** (`mob_area`, fixed at
  world-open) gets generation-time spawns — chunks generated later as a player walks (the
  normal streaming path) are not covered, because `ChunkWorld` is a static, once-built
  snapshot and nothing else in this tree currently reaches a live `MobSim` from that path. This
  is not a new limitation this feature introduces: the tick-driven `NaturalSpawner` cycle
  already only runs over the same fixed area — see
  [`docs/natural-mob-spawning.md`](./natural-mob-spawning.md).
* **One weighted pick and one pack per chunk**, not vanilla's bounded retry loop that can
  place more than one group.
* **The RNG stream is real per-chunk determinism, not vanilla's own draw order** — seeded via
  `WorldgenRandom::set_decoration_seed`, the same per-chunk derivation
  `UNDERGROUND_ORES` uses, so a given `(seed, cx, cz)` always proposes the same candidate.
* **A group's wander clamps back into its own 16×16 chunk** rather than reading a neighbour
  column, since this stage only ever sees the chunk it is finishing.

See [`lodestone_worldgen::spawn_stage`]'s own module doc for the full list, each one named
rather than hidden.

## The duplicated `MobCategory` (issue #518's third ask)

`crates/lodestone-server/src/mob_spawn.rs` used to define its own 8-variant `MobCategory`
next to [`lodestone_entity::spawn::MobCategory`] — the same eight categories, the same
constants, twice. `mob_spawn::MobCategory` is now `pub use lodestone_entity::spawn::MobCategory;`,
and `mob_spawn::check_despawn` delegates to `lodestone_entity::spawn::check_despawn` (feeding
it identity values for the peaceful/persistence fields this crate's callers handle elsewhere),
rather than re-deriving the same two distance gates. `lodestone_entity::spawn::MobCategory`
gained the `SPAWNING` constant `mob_spawn`'s callers needed, so no call site had to change
beyond the two that named the old inherent methods (`max_per_chunk` -> `max_instances_per_chunk`)
directly.

`lodestone_worldgen::spawners::MobCategory` (the biome-document JSON-key parser) is a
*third*, deliberately separate type — it exists purely to name `spawners` map keys in a
version-free crate that must not depend on `lodestone-entity`, and issue #518's ask named only
the `mob_spawn.rs`/`lodestone-entity` pair.

## How to change it

* A new species needs a [`SpawnRule`] row in `natural_spawn.rs`'s `SPAWN_RULES` table before
  it can ever pass `validate_generation_spawns` — a species absent from that table is dropped
  silently, by design (see that module's doc).
* Widening this beyond `MobCategory::Creature` means adding another category's list to
  `spawn_candidates_for_chunk`'s pick and deciding whether the one-attempt-per-chunk shape
  still holds for it (vanilla's chunk-generation call is `CREATURE`-only in 26.2, so there is
  currently no vanilla behaviour to port for the others).
* Reaching chunks outside the fixed `mob_area` needs `ChunkWorld` to gain a way to extend
  rather than replace its snapshot — see `MobHandle::reseed`'s own doc comment for why that is
  a documented, pre-existing scope cut and not something this feature could fix in passing.

## Configuration

None — driven entirely by the bundled biome documents
(`crates/lodestone-server/assets/worldgen/biome/*.json`) and the world seed.

## Dependencies

`lodestone_worldgen::spawners` (the parsed biome data), `lodestone_worldgen::spawn_stage`
(the pick), `lodestone_entity::spawn` (`SpawnConditions`/`MobCategory`/`check_despawn`), and
`crate::natural_spawn` (the light cache and per-species placement table).
