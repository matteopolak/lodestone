# Generation animal population

## What it is

Initial animal population proposes creature packs when a column reaches full generation and
materializes valid candidates through one shared per-world consumer. Native startup, browser
worlds and newly explored columns use the same claim and completion state.

## How it works

`lodestone_worldgen::spawners::BiomeSpawners` compiles biome attributes into the existing shared
spawn settings. `minecraft:gameplay/natural_mob_spawns` supplies an `overlay` argument containing
ordered `spawns_by_category` lists and `spawn_costs`.
`minecraft:gameplay/creature_world_gen_spawn_probability` defaults to `0.1`.
The bundled snowy plains and ice spikes use `0.07`; badlands and eroded badlands use `0.03` and wooded badlands use
`0.04`. Generation performs a fresh probability draw before every pack and stops at the first
failure. The default expected pack count is `0.1 / (1 - 0.1) = 1/9`. Each successful gate draws
a creature species by weight, its inclusive pack size, and candidate positions within the column.
An empty creature list yields no candidates regardless of other category lists.

Entry counts accept integer constants and `minecraft:uniform` inclusive ranges. Constants become
equal bounds; distinct uniform endpoints retain one bounded count draw in generation population.
All 811 entries in the current 67-biome resource census fit this representation: 605 constants
and 206 distinct-endpoint uniforms. Equal-endpoint uniforms are explicitly unsupported, because
they still consume a draw and cannot safely become constants in the bounds-only representation.
Malformed attributes, unknown count providers and ranges exceeding the bounded integer draw fail
instead of silently producing empty spawn lists. This census does not establish full natural-spawn
or generation placement parity.

Candidate Y uses the finished column's fused motion-blocking height summaries. Ordinary creatures
use the first free row above motion-blocking or fluid cells, excluding leaves; parrots and ocelots
include leaves. Stored heights are relative to `min_y`, so candidate Y is `min_y + stored_height`.
Short grass does not raise this height, allowing ordinary animals to stand in vegetation above the
supporting grass block. The typed species reaches the height callback after the weighted pick.
This reads existing summaries and performs no additional column scan or generation request.

Only `Full` columns carry candidates. `GenerationSpawnBatch` supplies a clone-shared claim and
completion state for each nonempty candidate list. Copying a terrain column preserves this
state. Before publishing a claimable batch, the resident source retains its exact column through
the existing authoritative terrain-edit boundary. Cache eviction and a later visit therefore
recover that column and completion state instead of reconstructing a new generation identity.
Empty columns allocate no batch. `GenerationPopulation` retains only unresolved candidates;
there is no separate historical coordinate set.

The shared world tick owns one `GenerationPopulation`. It admits the source's bounded
`pending_generation_spawn_batches` publication, including resident columns beyond the natural
tick area, and snapshots only the sparse coordinates its unresolved batches need. Placement runs
outside the mob lock; accepted candidates enter `MobSim::spawn_species` under the lock, with the
creature category and persistence flag. The ordinary end-of-tick entity publication carries them
to connections. Generation population does not replace the live simulation. Native startup
installs terrain, restores the saved roster, and releases its tick hold before this same consumer
runs; browser worlds use the shared tick body too.

Generation workers retain population-bearing Full terrain while holding its coordinate gate,
then append the shared batch to a transient unresolved publication queue. Tick discovery uses a
try-lock and a bounded result count, not a scan of historical terrain edits. Claimed batches stay
published until completion so a dropped consumer can return unresolved work; completed entries
are pruned. Retained source columns take precedence over reusable pure generation outputs.
Republishing a reconstructed column reuses its retained population identity while retaining the
new terrain snapshot; it cannot create a second claim for the same generation.

`Terrain263ChunkSource` proposes the packs itself, Overworld only, the first time a chunk is
generated (its `populated` set): the biome is the one at the chunk's minimum corner at the top of the
world, and `spawn_candidates_for_chunk` draws with `worldgen_data::bundled_spawners_by_builtin()`.
It publishes the batch straight to its pending queue, so its `retain_generation_population` retains
nothing. A column generated again after leaving every cache unedited gets no packs. The Nether's
strider packs are not proposed, since their standing position needs a ceiling-aware downward scan,
and the End generates none.

Placement classification distinguishes unavailable terrain or light from a definitive rejection.
A missing column, incomplete column or exhausted light admission budget retains the candidate.
Successful materialization removes it once; a definitive invalid placement also removes it.
The batch becomes complete only after its final candidate is removed, allowing persistence to
discard the transient candidate handoff safely. Dropping a consumer restores only its unresolved
candidates to the shared batch. It never restores animals already materialized.

Generation population bypasses the natural spawn game rule, population caps and player-distance
exclusions, matching its generation-time role. Real placement rules still apply.
Candidate selection uses deterministic chunk seeds but does not
claim exact reference RNG ordering or placement retry parity: each member has one candidate,
and its wander clamps to the generated column. The one-block land-pathfindability adjustment is
also not modeled; species height selection does not replace live terrain/light placement validation.

The validator starts one light cycle per world tick. Switching from the sparse generation view
to the natural view preserves that cycle's four-column admission budget and cached light.
Natural spawning can use a partial resident follow-area snapshot; a missing neighbor is skipped
and the incomplete snapshot is refreshed next tick without requesting generation.

## How to change it

Update the biome parser and `spawn_stage::spawn_candidates_for_chunk` when changing probability
or pack selection. The probability must remain below one so the repeat gate can terminate.
The compact official-resource projection in `biome_spawners_current` checks every ordered list,
count, cost and probability; deleting attributes or overrides supplies its negative controls.
Supporting degenerate uniforms requires retaining the provider kind through both count consumers,
not merely widening the parser's endpoint check.
Use scripted independent draws to distinguish guaranteed, single-pack and repeated admission:
with probability `0.1`, `0.27` admits zero packs; `0.07, 0.37` admits one; and
`0.03, 0.08, 0.42` admits two.

Change the surface-height closure in `Terrain263ChunkSource`'s generation-spawn proposal
(`lodestone_server::chunk::terrain263`) when adding a species-specific heightmap. Reuse the
client motion summaries, whose predicates include fluids and the leaves tag; the separate
generation motion summary has snow-support provenance and is not this placement heightmap.
Authored grass, short-grass and leaf stacks distinguish ground animals from canopy species;
the bundled generator's bounded placement test independently scans states and checks actual
grass-supported candidates through the shared validator.
The bounded placement test samples biomes at a fixed cohort of independently probability-positive coordinates,
then generates at most two creature-bearing land contexts. Its fixed Y probes are a habitat
prefilter, not a substitute for the required full-column grass-support and acceptance assertions.

Change `GenerationPopulation` when modifying deferred placement or completion. A false
materialization result must mean no entity was created, because it retains the candidate for a
later attempt. Keep terrain and light classification outside the mob lock, then materialize the
accepted candidate under that lock. Do not replace the simulation to add animals.

Preserve the batch object across every resident column clone. Rebuilding it from the same candidate
list creates a second generation identity and can duplicate population. Publish it only after
authoritative terrain retention succeeds. Keep retention on generation workers, not tick discovery.
Preserve the retained provenance when changing source admission: a population claim changes entity
state, so it must not make an otherwise unmodified generation halo look like edited terrain.
Synchronous legacy drains are for explicit storage fixtures; terrain snapshots must leave the
shared batch intact. Native chunk records refuse pending batches and omit completed ones. Saving
chunks and the live entity roster remains separate storage work, so this handoff does not make
those writes crash-atomic.

## Configuration

Biome attributes control generation probability, category weights and inclusive pack counts.
The default probability is `0.1`, with accepted range `[0, 0.9999999]`. During the bundled-resource
cutover, the same parser also accepts the old top-level `spawners`, `spawn_costs` and
`creature_spawn_probability` fields with `minCount`/`maxCount` entries. Current spawn attributes
take precedence and do not borrow missing settings from those old fields. Remove that fallback,
its old count branch and the corresponding old-input unit fixtures after the live resolver bundle
and synthetic biome fixtures use attributes; this is input compatibility, not another generator.
There are no environment flags. `PENDING_BATCH_LIMIT` bounds unresolved
column batches to 256; `CANDIDATE_BUDGET` limits each consumer cycle to 256 placement decisions.
The placement validator shares the natural spawner's four-column light admission budget.
Native `RegionSource` terrain retention and `Terrain263ChunkSource`'s `populated` set own the
completion carrier for the world lifetime; their existing save/edit policies control its storage.

The `spawn_mobs` game rule gates this path: with it off every pending candidate is rejected and
drained (not held for a later re-enable), so no generation-time animal appears. Spawned animals
take the species' own category and are counted by the natural-spawn census.

## Dependencies

`lodestone-worldgen` supplies parsed biome settings, deterministic RNG and candidate positions.
`lodestone-server::generation_population` owns claims and deferred work. Server terrain snapshots,
the natural spawner's species placement rules and light cache, and `MobHandle` provide the live
placement path. Entity persistence saves successfully materialized animals through the ordinary
population roster; native chunk persistence refuses batches that remain pending.
