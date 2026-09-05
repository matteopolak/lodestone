# Tick region ownership

## What it is

`lodestone_server::tick_region::TickRegionPlan` makes the ownership of every
chunk selected for a server tick explicit. The current plan assigns every
selected chunk to the smallest region possible, its own `TickOwner::Chunk`, but
the server executes those owners serially while parity and populated-world
profiling remain prerequisites for concurrent workers.

## How it works

`tick_area::FollowArea` first builds its stable, duplicate-free chunk set from
player anchors (or its playerless fallback). It then stores that set in a
`TickRegionPlan`, which attaches a `TickOwner::Chunk { cx, cz }` to every entry.
The production tick loop consumes `FollowArea::owned_chunks()` for the
per-chunk random-tick and thunder-decision passes. This is an execution
boundary, not a concurrency claim: the iterator retains the old canonical
order, so its random-number draws and visible results cannot move merely by
introducing ownership.

The plan rejects a duplicated list before assigning ownership while preserving
the producer's visit order. Stable order matters because random-tick draws
consume chunks in that order; a duplicate would advance one chunk twice in a
tick. This check makes the one-owner-per-chunk invariant testable now and
provides the input contract a future partitioner must preserve.

`TickStats::owner_work` records `random_tick_owned_chunks` and
`thunder_owned_chunks` from those same live passes. They count completed
ownership visits rather than block changes or bolts, so an empty chunk and a
non-striking thunder roll remain visible in a named workload. This makes the
spatial plan a production-observed boundary without treating either counter as
a timing result or a concurrency claim.

`tests/tick_region_owner_parity.rs` is the finite single-thread parity gate for
that random-tick owner sequence. It runs the live serial `TickRegionPlan`
iteration over four seeded chunk columns whose selected grass cells produce
client-visible dirt updates, then compares both update order and persisted
watched states with an independent physical-region scheduler. The reference
groups columns into two-cell Euclidean regions without reading `TickRegionPlan`
and restores their original publication sequence at its central edge. Its
swapped-visit and duplicated-visit controls must fail the same comparison, so
the gate is not a counter check or a self-round-trip that accepts arbitrary
owner scheduling.

`TickRegionPlan::owner_workloads` reports the current real ownership (one
single-chunk workload per selected column). `FollowArea::spawnable_chunks`
consumes that report on the live tick path, so the count cannot become an
unobserved parallel data structure. Scheduled queues and block entities now
have their own chunk-local ownership seams: `ChunkScheduledTickQueue` keeps
pending ticks at their target chunk and merges due heads into one serial order,
while `BlockEntityRegistry::tick_plan` assigns its tick-start snapshot to
chunk owners. It executes one owner batch at a time and returns a
`BlockEntityTickEffectBatch` for each owner, including an empty batch when no
visible write occurred. `tick::apply_block_entity_effect_batches` is the sole
central consumer: it validates the complete tick-start batch set, restores its
serial slots after independent completion, then applies and publishes furnace
changes in the established order. This makes the message boundary real without
claiming workers can run concurrently. The ambient entity-effect phase uses the
same shape: `MobSim::take_ambient_sound_effect_batches` groups each emitted
effect under `EntityTickOwner::Chunk`, and
`tick::apply_entity_effect_batches` is the only publisher to the connection
feed. Entity simulation itself remains serial in its existing vector order.
Effects carry that original sequence because entities from two chunks may be
interleaved; the central publisher restores it rather than changing behavior to
owner-major order. Negative positions use `floor` followed by Euclidean chunk
division, so an entity at `x = -0.5` belongs to chunk `-1`, not chunk `0`.
Chunk lifecycle has the same explicit smallest owner before the cache crosses
its source boundary. `ChunkLifecyclePlan` assigns each on-demand load and each
selected cache release to `ChunkLifecycleOwner::Chunk { cx, cz }`; `ChunkStore`
consumes it through `ChunkLifecycleHandoff` for the real `column_at` and
`unload` calls used by every `IntegratedServer`. Ticket transitions remain
demand-driven: becoming resident does not pre-generate a column, and becoming
unresident only releases a column that is actually cached. An eviction batch is
bounded by current cache entries, deduplicated, and ordered `(cx, cz)` before
`ChunkSource::unload`, so the hash-map iteration behind a ticket delta cannot
make negative-column unload order vary.

The hand-off gives every bounded batch and slot a typed acknowledgement token.
Each slot must move through `SourceReady`, `SourceInFlight`,
`PersistenceReady`, `PersistenceInFlight`, and `Complete`; a duplicate command,
out-of-order reply, or reply with an old batch, slot, action, phase, or
coordinate is rejected. This is the persistence/source ordering seam a future
region worker must keep: cache residency is removed before a release is
selected, and a same-coordinate load or release owns a small source gate until
the persistence hand-off acknowledges. Gates use weak references and disappear
after work completes, so they are not a second unbounded chunk-residency map.
Independent coordinates remain free to generate in parallel. The current
`ChunkStore` path closes its source hand-off synchronously when the source has
accepted its queued request; the wrapped `RegionChunkSource` then creates a
second, coordinate-scoped durable-save token. `WorldSaveHandle::begin_save`
creates a single-use `WorldSaveJob` before the owner dispatches it to the
blocking writer. The job carries only that bounded token snapshot,
acknowledges each token only after its owned region writes succeed, and
releases the source's authoritative edit only from the durable phase. A failed
save leaves the token queued for retry; a newer unload of the same coordinate
supersedes the old token, so a delayed worker reply is stale rather than able
to release the wrong ownership. The blocking writer may run at most two
independent physical region-file owners at once. It joins each bounded batch
and consumes results in canonical owner order; any failed owner is re-dirtied,
and no durable token is acknowledged until every selected owner succeeds. This
does not make unload a worker, change storage format, or permit cross-owner
mutation.
Entities outside this ambient-effect hand-off, natural-spawn planning, world
border, game rules, time, weather and other cross-column work remain global.
For a named populated scene,
`FollowArea::candidate_region_workload` groups the same selected chunks into an
observer-supplied spatial edge using Euclidean division. Its sorted cell counts,
total, and largest-cell count establish whether that scene is spatially spread
out or concentrated before any worker or lock is introduced. Candidate cells
are measurements only; they do not alter the chunk-owner assignment or
simulation order.

`chunk_owner_profile::SCENE_NAME` is the separate deterministic populated
workload for this ownership seam. Its eight resident chunk owners each receive
a furnace, one due block tick and one due fluid tick; 64 cows are distributed
across those owners so periodic ambient effects return through entity-owner
batches. The `profile-harness` feature gives the standard run a paused runtime
that advances exactly 128 tick periods. `TickStats::owner_work` accumulates due
block/fluid entries and the block-entity/entity batch and effect counts at the
live central hand-offs, so the profile proves its work crossed those boundaries
rather than merely being seeded in a fixture.

## How to change it

Do not add region workers or coalesce these chunk owners into a larger region
size in this module alone. First complete the parity and named-scene profiling
prerequisites in `docs/plans/regionised-server-ticking.md`. A concurrent
partitioning change must retain deterministic ordering within each owner and
add an explicit cross-owner hand-off path before any mutation may cross a
boundary. Reuse the block-entity shape: an owner returns a typed batch carrying
its tick-start slot and the central writer validates the complete slot set
before applying batches in that declared order. Do not give a worker a
second owner's `ChunkSource` just to avoid defining that message.

Keep `FollowArea` as the producer until the live tick loop obtains its work
through another production-consumed boundary. Any new producer must remove
duplicates before constructing a plan and preserve a deliberate visit order;
bypassing that check makes duplicate random ticks possible.

Run `cargo test -p lodestone-server --test tick_region_owner_parity -j2` after
changing random-tick owner assignment or its central publication boundary. Keep
the fixture finite and seeded: it is a deterministic scheduling discriminator,
not a populated-world throughput profile. If a later worker changes how owners
are grouped, retain an independently built reference schedule and controls that
make swapped and duplicated ownership visibly diverge before accepting it.

When extending entity ownership, do not publish from a chunk owner. Add a typed
batch to `MobSim`, retain an explicit source position and old serial sequence,
and make `tick` centrally consume it. Grouping alone is not parity: if owners
are interleaved in the serial simulation list, applying all of one owner's
effects before another's changes observable packet order. Add both a
negative-coordinate control and an interleaved-owner order control before a
future executor is allowed to run owners separately.

Keep lifecycle planning at the existing cache boundaries. A ticket becoming
resident is permission for a later demand load, not authority to generate in a
background task. A ticket or LRU eviction may call `ChunkSource::unload` only
after it has removed that retained cache entry, and must pass the selected
coordinates through `ChunkLifecyclePlan`; calling the source under the cache
lock would reintroduce the lock/I/O and re-entry hazards this boundary avoids.
If a future owner releases a column concurrently, open a batch through
`ChunkLifecycleHandoff::open`, retain it between its source and persistence
commands, and acknowledge persistence exactly once after the durable writer
completes. `execute_with_persistence` is the current synchronous callback seam
for source-side ordering; `RegionChunkSource`'s durable-save ledger is the
production disk boundary beneath it. Do not replace either bounded ledger with
a global acknowledgement history: an old reply must be stale, not a permanent
resident record. Preserve `(cx, cz)` selection order, including negative
coordinates, before dispatching any workers. The remaining gap is deeper
regionization: the `ChunkSource` contract still exposes unload as a synchronous
queue-acceptance hint. `WorldSaveJob` carries the durable token across the
current save worker boundary, but a future region owner must propagate that
same job/result contract through its own message boundary instead of treating
the source-side acknowledgement as disk completion.

The save job also turns its global dirty-column snapshot into a bounded
`WorldSaveRegionPlan`: each column belongs to its physical region-file owner,
with canonical region and within-region chunk order. `WorldSaveHandle` splits
that plan into batches of at most two owners and writes each batch concurrently;
the owners have separate region and temporary-file paths. This is deliberately
not a general scheduler: a later executor must retain the same per-owner
assignment, canonical result selection, failure requeue, and durable-token
result before raising the bound or introducing cross-owner messages.

When recording a candidate report, name the populated scene and the explicit
edge passed to `candidate_region_workload`. Compare total chunks, the number of
non-empty cells, and the largest-cell count across clustered and spread-out
scenes. This is spatial workload evidence, not an MSPT measurement: use it to
decide whether a later profile deserves a regionisation experiment, never as a
claim that a particular edge is the right size for coalescing chunk owners.

For the owner-boundary workload, run `just bench-chunk-owner-tick` first. It
constructs the live scene and rejects a missing phase sample, scheduled drain,
block-entity owner batch, or ambient entity owner batch before Criterion takes
its short sample. For a local call tree, run `just samply-chunk-owner-tick`.
The wrapper first saves a direct witness line, then captures the same finite
128-tick command under Samply. It caps input at 512 ticks, gives the workload
and profiler separate deadlines, refuses to overwrite artifacts, and requires
the capture plus its presymbolication sidecar. Its witness requires all eight
owners and 64 ambient mobs, then rejects missing scheduled-block,
scheduled-fluid, block-entity, or ambient-entity work from those owners, so a
profile of an empty or one-owner loop cannot look valid. Pass `--ticks <1..512>`,
`--wall-deadline-secs <1..60>`, `--output-dir <path>`, or `--run-id <name>`
after the recipe to select a finite alternative. Inspect the scheduled-and-physics
phase beside `owner_work.random_tick_owned_chunks`,
`owner_work.thunder_owned_chunks`, `owner_work.scheduled_block_ticks`, and
`owner_work.scheduled_fluid_ticks`; inspect mobs-and-items beside
`owner_work.block_entity_batches`, `entity_effect_batches`, and
`entity_effects`. A high phase cost with a missing or unexpectedly small count
is a fixture/wiring failure, not evidence for parallelization.

## Configuration

None. Chunk ownership has no tunable size. Lifecycle batches are bounded by the
existing cache selection rather than a new queue capacity. A candidate edge
remains an explicit measurement argument, while any useful multi-chunk worker
size depends on profiling a populated, named workload and has not been selected.
The profile entry point accepts an optional tick count; its normal 128-tick run
is fixed so counter comparisons name the same workload. The `profile-harness`
feature is required for the fast paused-clock path and is selected by both
`just` recipes; without it the finite example uses normal server sleeps.

## Dependencies

`tick_area::FollowArea` supplies the live chunk set. `tick::run_tick_loop`
consumes its ownership sequence for random ticks and thunder decisions. The
`chunk_store::ChunkStore` consumes lifecycle assignments around its real source
load/unload boundary, including the stores owned by `IntegratedServer`.
regionised ticking design document records the prerequisites for changing this
serial ownership seam into concurrent execution.

The profile scene uses `IntegratedServer`, `BlockEntityRegistry`, scheduled
tick queues and `MobSim`; it intentionally drives their ordinary production
tick path instead of a parallel test-only executor.
