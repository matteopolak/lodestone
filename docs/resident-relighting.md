# Resident relighting

## What it is

`ResidentLightJob` computes fresh light for up to nine nearby resident columns from one shared input field. The integrated server uses it for grouped native relights and cooperative browser relights while still servicing packets and ticks.

## How it works

**Solver** (`lodestone_world::lighting::resident`)
- `ResidentLightFootprint` validates distinct output coordinates and unions their radius-one halos, which must fit a 5x5 column field. `ResidentLightInputs` binds exactly those borrowed `BlockVolume`s with a common vertical shape. A missing input is an error, never a partial result; unused tiles are opaque barriers.
- It samples each cell once, builds opacity and emission buffers, seeds open sky columns, floods both layers in descending levels and packs only the selected outputs, each keeping its own highest non-air section, the ordinary one-section full-sky framing, explicit zero block layers and both apron sections.
- Light loses at least a level per step (max 15), so a source beyond an output's 16-block halo cannot matter; the outer ring of a nine-output solve is accurate for centres but not a set of complete snapshots.
- `step(NonZeroUsize)` charges field scans, sky seeding, frontier work, queue pops and packing, reported per category by `ResidentLightWork`. The budget bounds operations, not wall time. `finish` runs the same machine synchronously. The job clones nothing and owns no locks, revision checks, generation, encoding or delivery ledger; the caller captures a stable revision set, rejects conflicts and commits outputs as one transaction.

**Source contract.** `ChunkSource::try_begin_resident_light` captures complete cache-owned terrain once per input with pinned revisions. Capture and commit use nonblocking locks and never generate terrain, read disk or touch wrapped persistence. Commit validates every input and installs light for requested outputs only; input-only halos keep their previous status. The light is transient cache state, not a terrain edit.

**Initial packets** use `ChunkSource::try_begin_initial_packet`: an `InitialPacketTransaction` captures the centre and exactly the requested radius-one neighbours in sorted order, holding no gates while lighting and encoding. `try_commit(None)` validates reused or no light read-only; a computed `ColumnLightSettlement` marks the centre `CentreSettled` and new dependencies `DependencyInitialized` without overwriting settled ones. Busy commits retry, conflicts need recapture, a dropped transaction publishes nothing. Initial admission also retains the exact serving snapshots through `ChunkSource::try_store_resident_lights`, a nonblocking all-or-none hook persistent wrappers must implement (the default retains nothing); `RegionChunkSource` try-locks its invalidation, edit and dirty records first, so light survives save and reload, and nothing enters the cache before the wrapped layer accepts.

**Connection scheduling.** One relight is in flight per connection, grouping delivered destinations within the oldest queued target's immediate ring. Native uses the persistent dispatcher and the shared solver for multiple outputs (single outputs keep the faster scalar path). Browser connections hold a compute future as a separate select branch, with the version adapter advancing 16,384 charged operations between yields and no source locks held. Busy, missing or changed inputs leave destinations queued: a deferred batch rotates its oldest destination behind its neighbours, backs off 50 ms and retries one output, so a permanently missing halo cannot starve nearby work. Before sending, the server rechecks the destination is still delivered and its light current. Unsupported sources and families keep the scalar route.

## How to change it

- Keep all-input revision validation and all-or-none commit; never give an input-only halo a completed-centre status; resident-only work must not generate columns; no coordinate gate across a browser yield.
- A source adding complete-column retention must implement and forward the batch hook, or persistence parity silently breaks. Mutate terrain only through the owning `ChunkStore`.
- Controls compare every output (section tags and aprons included) against independent radius-one centres over uneven roofs, water, emitters and transparent blocks, plus hand-arithmetic levels (14 from an outer-halo emitter; 13/12/11 across a torch seam; 13/9 under a roof beside open sky; 15 down a shaft); the outer-halo control runs positive and negative. Transaction and retention tests live in `lodestone_server::chunk_store` and `region_source`.
- Perf: native release with `LODESTONE_RESIDENT_LIGHT_PERF_ITERATIONS` (1..=128) and `every_output_matches_independent_halo_with_distinct_sky_trimming --nocapture` prints paired one- and nine-target timings (`RESIDENT_LIGHT_PERF`, no assertion). Measure before replacing scalar single-target work.

## Configuration

Only the perf variable above. At most 9 outputs in a 5x5 field. The version adapter owns the browser slice budget; the server queue owns retry delay and grouping. `BlockVolume` gives vertical extent and air ceiling; `LightProperties` gives emission, opacity and the dimension's skylight rule.

## Dependencies

World lighting field layout, descending bucket queues, `ColumnLight`, `NibbleArray`; in production the server's coordinate-gated cache, delivered-column ledger and dispatcher, the version adapter's light tables and packet encoder, and `lodestone-time`.
