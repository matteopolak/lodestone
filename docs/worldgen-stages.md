# Global worldgen stage architecture

## What it is

This document is the cross-dimension contract for turning a world seed and a
chunk coordinate into a column, its retained intermediate products, and its
serving sidecars. It makes the order, dependency radius, mutable state,
randomness scope, and resumable completion status explicit for the Overworld,
Nether, and End without merging their dimension-specific generators.

The document describes the current call graph and the frontier model. The End
now exposes compile-checked descriptors and a metadata-only chunk frontier in
`lodestone_worldgen::stage_schedule`; descriptor-driven executors and durable
product storage remain design work, not silently landed behavior.

## How it works

### Two contracts, one vocabulary

There are two different orders that must not be conflated:

* A **column schedule** is the ordered set of passes inside one dimension's
  generator. It is represented by `ColumnStage`, `StageSchedule`, and
  `StageCursor` in `lodestone_worldgen::stage_schedule`.
* A **lifecycle schedule** is the externally observable admission, dependency,
  mutation, light, spawn, and packet order. It is represented by
  `LifecyclePhase`, `PhaseContract`, and `LifecycleSchedule`.

`SourceSchedule` describes which neighbouring source columns may feed a
  mutable feature pass. `FeatureSchedule` and `DecorationStep` name the
  configured feature ordinals inside that pass. They are not extra column
  stages: a feature ordinal, a source completion, and a packet status have
  different dependency and persistence rules.

The compile-checked constants `OVERWORLD`, `NETHER`, `END`, and `LIFECYCLE`
are the single vocabulary shared by production dispatch, server scheduling,
and parity replay. `StageCursor` only checks that a caller entered the named
passes in order; it is not a durable completion record and it cannot replace
the frontier described below. End's `StageDescriptor` table is derived from
`END.stages()` and records the measured structure scan and feature window
contracts. `GenerationLevel` maps Terrain, Structures, Decorated, and Output
to dimension-specific prefixes; the compact mask on `StageFrontier` is only an
admission summary, while its fingerprinted records remain the validation
authority.

Canonical semantics are the reference execution. They define, for every
source and feature entry, the input snapshot, seed scope, source completion
order, writes, sidecars, and observable trace. An optimized execution plan may
fuse passes, run immutable products on worker threads, or share a dependency
rectangle only when it produces the same canonical trace and final products.
The optimized plan is not allowed to redefine order merely because its loop
nest is convenient.

### Current dimension schedules and entrypoints

The table is the implementation inventory. “Shaped” and “Full” are the only
public server generation labels today; their cutoffs are intentionally
dimension-specific.

| Dimension | Ordered column passes | Shaped cutoff | Full suffix and entrypoint | Mutable decoration |
| --- | --- | --- | --- | --- |
| Overworld | `StructureStarts → StructureReferences → StructureInfluence → Fill → Biomes → Surface → Materialize → Carvers → StructurePlacement → Features → TopLayer → Output` | Through `StructurePlacement` | `OverworldGenerator::column` resumes at `Features`, then `TopLayer` and `Output`; `column_shaped` stops at the prefix | A 3×3 source window, a 5×5 immutable context, unified feature/ore stream, and source-order-sensitive writes; top-layer modification follows all feature writes |
| Nether | `StructureStarts → StructureReferences → StructureInfluence → Fill → Biomes → Surface → Materialize → Carvers → StructurePlacement → Features → Output` | Through `Carvers` | `NetherGenerator::column` resumes with target-local `StructurePlacement`, then the mixed feature pass and `Output`; `column_shaped` returns the pre-placement prefix | Admission-dependent source completion; the mixed feature step interleaves ore and decoration entries and synchronizes the resident read view after each entry |
| End | `Fill → Biomes → Surface → Materialize → StructureStarts → StructurePlacement → Features → Output` | Through `StructurePlacement` | `EndGenerator::column` builds a private 3×3 base region, decorates it, and emits `Output`; `column_shaped` returns one undecorated base world | The nine immutable bases are copied into a private 48×256×48 region; sources decorate in fixed order, with gateway and block-entity sidecars retained |

The cutoffs are schedule boundaries, not claims that all dimensions have the
same work at a named status. In particular, Nether target-local structure
placement belongs to source completion after the shaped prefix, while
Overworld and End shaped values already contain structure placement.

#### Overworld call graph

`OverworldGenerator::column` opens the staged store view, obtains the cached
`pre_ore_stage`, runs the feature dispatcher, applies the top layer, and
converts the mutable field to the output column. `pre_ore_stage` performs
starts, references, structure influence, fill, height sampling, biomes,
surface rules, materialisation, carving, and target structure placement. It
also captures the post-carve/post-structure ore height view needed by later
feature selection.

`OverworldGenerator::column_shaped` consumes that same prefix and deliberately
does not run features, top-layer modification, or generation-time spawn
selection. The production feature path is source-major in the stable
`(-1,-1), (-1,0), (-1,1), (0,-1), (0,0), (0,1), (1,-1), (1,0), (1,1)` order.
The feature catalog and the mixed ore/decoration dispatcher preserve the
global step/index identity even when an entry is not selected.

The retained `PreOreResult` contains the prefix world, 256 height values,
surface biome values, and the immutable biome-cell product. The final output
also carries block entities, a motion-blocking heightmap, and spawn
candidates. Structure starts/references and structure-owned loot/spawner
metadata are attached by the server source rather than being inferred from a
finished block palette.

#### Nether call graph

`NetherGenerator::pre_decoration_stage` caches the immutable prefix through
carving. It intentionally excludes target-local structure placement so a
source completion can observe the resident target and previously committed
overlays. `NetherGenerator::column` resumes with structure placement, then
calls the mixed feature pass and folds the result into `NetherColumn`.

The direct scalar path uses the fixed x-major/z-fastest source order, while
the lifecycle path derives completion from the admitted request wavefront.
Those are equivalent only when the request has the same admission order; the
source schedule therefore records `AdmissionDependent` rather than pretending
that one 3×3 permutation is universal.

The prefix retains the working world, 256 height values, and 16 quart biome
values. The full resident window is 256 rows even though the terrain carrier
is 128 rows. Upper-row decoration writes are retained as decoration spill and
must not be dropped when the carrier is converted. Placement loot and server
structure/container metadata are separate sidecars.

#### End call graph

`EndGenerator::base_world_for_batch` memoizes the immutable fill, biome,
surface, materialisation, and structure-placement product. `column` copies the
target's 3×3 base worlds into a private region, captures the three client
heightmap snapshots at the feature boundary, runs source decoration, and
emits the target palette. `columns_spatial_batch` may compute unique base
worlds in parallel and then decorates/emits in requested-coordinate order.

The End base product retains a full 256-row world and structure block-entity
events because structure pieces can write above the 128-row terrain field.
Feature output retains gateways and the final client heightmaps. The server
source separately attaches structure references and container payloads.

### Current lifecycle consumers and distant terrain

The server's `ChunkGenerationStage` currently has only `Shaped` and `Full`.
`ChunkSource::column_at` selects the dimension-specific shaped or full
entrypoint, and `ChunkStore` retains the highest available value. A full
column therefore dominates a shaped one, but the store cannot represent “fill
and surface complete, structure placement absent” or “features complete but
packet light unsettled.” A later request for a lower value must reuse a higher
value; it must not downgrade or rerun a completed prefix.

The shell has two distinct far-terrain consumers:

* `HorizonSurfaceQuery` constructs a local Overworld generator and asks for a
  preliminary surface level. It is a coarse query, not a stored completed
  stage, and it does not retain a column, sidecar, seed fingerprint, or
  resumable cursor.
* `horizon_profile` exercises far coordinates through
  `OverworldChunkSource::column_at(..., Shaped)`, then reports shaped/full
  counts and staged-store entries. This is a real shaped-column path, but it
  still computes the whole shaped prefix and is not a mip-level product.

The distant terrain renderer consumes the first query, not a generalized
worldgen frontier. Thus “far chunks use only the first stages” is not yet a
runtime guarantee. The frontier design below makes that policy expressible:
far geometry can stop at a low-fidelity stage, while a nearer request resumes
from the retained product without recomputing or silently skipping stages.

### Typed descriptors and executors

The layer above `StageSchedule` has one descriptor per End pass. The following
is an interface sketch for the eventual executor, not a second hand-maintained
schedule:

```rust
struct StageDescriptor {
    key: StageKey,
    prerequisites: &'static [StageKey],
    read_radius: Radius2d,
    write_radius: Radius2d,
    mutable_inputs: &'static [ResourceKey],
    outputs: &'static [ResourceKey],
    retained_sidecars: &'static [SidecarKey],
    seed_scope: SeedScope,
    barrier: BarrierPolicy,
    cancellation: CancellationPolicy,
}

trait StageExecutor<Context> {
    type Input;
    type Output;

    fn execute(
        &self,
        context: Context,
        input: Self::Input,
    ) -> Result<Self::Output, StageError>;
}
```

The End descriptor fields are now available in the worldgen crate and are
covered by focused schedule tests. A descriptor must not be a stringly typed
copy of the current loops:

* `StageKey` identifies a dimension and a `ColumnStage` (and, for the mutable
  feature subgraph, a typed decoration step/source scope).
* `Radius2d` is the worst-case dependency/write footprint, not merely the
  current loop bounds. A 3-D or block-space radius is needed when a stage's
  vertical footprint matters.
* `ResourceKey` names immutable fields, resident mutable overlays, or a
  retained height/biome product. `SidecarKey` names metadata such as gateways,
  block entities, loot, spills, heightmaps, or spawn candidates.
* `SeedScope` states whether the stream is world-wide, target-column,
  source-column, or `(source, decoration step, global index)`. It also records
  the algorithm family and the seed/config fingerprint used to derive it.
* `BarrierPolicy` says whether the stage is pure, source-ordered, stage-wide,
  or packet-domain-wide. `CancellationPolicy` says where a job may be
  canceled without exposing a partial mutation.

The schedule table can remain a small `const` table or be generated from one
checked manifest. Either approach must produce one artifact consumed by
production and replay. Do not add a second per-dimension list in a dispatcher.

### Completed-stage frontier, persistence, and resume

`StageCursor` is a debug assertion for a single call. Massive render distance
needs a durable, typed `StageFrontier` instead:

```text
StageFrontier {
    schedule_version,
    dimension,
    coordinate,
    completed: set<StageKey>,
    records: map<StageKey, StageRecord>,
}

StageRecord {
    input_fingerprint,
    output_fingerprint,
    retained_products,
    retained_sidecars,
    seed_scope_fingerprint,
    executor_version,
}
```

The set is deliberately not just “highest ordinal.” The lifecycle graph has
branches such as lighting and spawn, and a future fidelity plan may retain a
terrain product while deferring mutable structures. A stage is ready only when
all typed prerequisites and their radius footprints are available. The
frontier planner then computes the smallest missing prerequisite closure for a
requested target tier.

The rules are strict:

* A stage is committed atomically with every output and sidecar named by its
  descriptor. A product is not “complete” if a required heightmap, spill,
  gateway, block entity, or structure reference was omitted.
* A higher completed stage dominates lower requests, but a lower request never
  downgrades the stored value. A missing intermediate product cannot be
  skipped merely because a later palette happens to exist; it must be
  reconstituted from a durable canonical artifact or the frontier is invalid.
* Resume is allowed only at a valid committed boundary. The record includes
  coordinate, seed/config, schedule, and executor fingerprints so a changed
  registry, radius contract, or RNG algorithm invalidates the record rather
  than producing a plausible mixed-version column.
* Mutable stages commit source completions through a transaction. A canceled
  job before commit is simply not complete. A cancellation after a write has
  begun is honored only at the declared transaction boundary, with temporary
  cross-target writes rolled back or discarded from the private working grid.
* Persistence must include the dependency halo or a content-addressed handle
  to it. Saving only the centre palette is insufficient when the next stage
  reads a 5×5 context or a retained upper-row spill.

This gives the renderer a real fidelity policy. A far request can ask for a
terrain frontier and produce a low-resolution mesh; when that coordinate
enters the near band, a structures/decorated/packet request resumes from the
frontier. The policy is an admission decision, not a special-case “skip
features” branch hidden in the renderer.

### Fidelity tiers

These are proposed logical tiers. Only `Shaped` and `Full` are public server
tiers today.

| Logical tier | Canonical cutoff | Overworld | Nether | End | Current status |
| --- | --- | --- | --- | --- | --- |
| Horizon sample | No column stage | `preliminary_surface_level` query | Not exposed | Not exposed | Existing shell query only; no frontier or persistence |
| Terrain | `Surface` | Fill, biomes, surface product | Fill, biomes, surface product | Fill, biomes, surface product | Proposed low-cost far/mip product |
| Structures | `StructurePlacement` | Current `Shaped` boundary | Requires target-local completion after current `Shaped` prefix | Current `Shaped` boundary | Proposed typed tier; not a new server label |
| Decorated | `Features` plus any dimension-required top layer | Features and `TopLayer` | Features | Features | Proposed resumable near-worldgen tier |
| Output | `Output` | Packet-ready generator column | Packet-ready generator column | Packet-ready generator column | Generator `Full` result, before all server lifecycle work |
| Packet | Lifecycle `PacketFinalization` | Full + sidecars + settled light | Full + sidecars + settled light | Full + sidecars + settled light | Server lifecycle target, not a worldgen pass |

`Terrain`, `Structures`, and `Decorated` are aliases over dimension-specific
closures, not permission to pretend their work is identical across
dimensions. A tier request must expand to the schedule keys for that
dimension, validate the frontier, and admit the required halo.

### Inputs, outputs, radii, and barriers

The following is the conservative audit to encode in descriptors. “Contract
gap” means the current schedule names the pass but has not yet exposed this
metadata as a typed runtime value; it is not a license to infer a smaller
radius from one implementation loop.

| Pass/product | Required reads | Writes and retained products | Execution/barrier |
| --- | --- | --- | --- |
| Structure starts | Seed, coordinate, structure configuration | Start records; no block writes | Pure and independently parallel; commit before dependent references |
| Structure references | Starts in the lifecycle radius-8 dependency domain | Reference records; no block writes | Parallel over immutable starts; barrier before influence/fill |
| Fill, biomes, surface | Own density/noise inputs, structure influence, dimension settings | Field/world, solid-height product, biome quarts/cells | Coordinate/source-major workers; commit the immutable prefix before readers |
| Materialize and carvers | Materialized field plus declared carver/structure inputs; exact carver radius remains a descriptor contract gap | Dense resident world and carved state | Pure per prefix product once dependencies are present; no mutable feature overlays |
| Structure placement | Overworld/End retained starts and prefix world; Nether also reads the admitted resident target and prior overlays | Structure blocks, structure events/references, placement sidecars | Overworld/End can be parallel over immutable inputs; Nether requires admission/source completion and a mutation boundary |
| Feature context | 5×5 immutable context (`WIDE_RADIUS = 2`), 3×3 source selection, post-carve/structure heights | Read-only context; source seed/index plan | Build in parallel, then freeze the context for the mutable pass |
| Mutable features | Source-specific 3×3 window, resident overlays, and per-entry height/biome views | Block writes, block entities, spills, placement loot | Current Overworld/Nether/End production is source-major/fused. A stage-outer plan needs a stage-wide barrier unless writes commute and traces match |
| Overworld top layer | Final post-feature motion-blocking view of the target | Snow/ice/top-layer writes and final motion-blocking map | Target-local after all relevant feature writes; cannot run before those writes |
| Output | Complete world field and required sidecars | Palette/blocks, biome quarts, heightmaps, spawn candidates, gateways/events | Barrier after all mutable writes; output is an immutable snapshot |
| Light and packet finalization | Full centre plus settled-light dependency radius 1 | Packet-side light/status metadata | Stage-wide packet barrier; the packet is not complete at `Output` alone |

Known concrete footprints are important controls: the feature context is
5×5, mutable feature write coverage is currently modeled conservatively as
radius 2, source selection is 3×3, and packet light settlement is radius 1.
The End structure-start scan can inspect a 33×33 candidate square (radius 16)
for each source. The lifecycle schedule independently records starts radius 8
for reference-style dependencies. These radii are different contracts; one
must not be substituted for another.

### RNG seed scope

The seed is part of the stage input contract, not an implementation detail of
the loop:

* Terrain and fill streams derive from the generator seed, dimension settings,
  coordinate, and the dimension's configured random algorithm. Nether terrain
  uses its legacy algorithm; that does not select the feature algorithm.
* Each feature source receives a fresh feature random stream. The decoration
  seed, global step, and global feature index determine the stream; they must
  not be replaced with a flattened local index.
* Gaussian caches and other mutable random-wrapper state are local to one
  source stream. A worker must never share a random cursor with another source
  or reuse a continuation after a stage resume.
* A stage-outer replay must derive the same `(source, step, index)` seed that a
  source-major fused plan would have used. Persisting only the post-draw random
  state is not enough to prove this after a cancellation or reordered worker
  completion.

The trace therefore records the seed scope and input fingerprint at every
mutable boundary. A change in source order, feature index assignment, or RNG
family is an intentional compatibility change, not an optimization.

### Fused execution, barriers, admission, and cancellation

| Work | Can be fused/parallelized | Required boundary |
| --- | --- | --- |
| Starts, references, influence, fill, biomes, surface, materialisation, and prefix carving | Yes, over immutable coordinate/source products with bounded worker admission | Commit each prerequisite product before a consumer reads it |
| Overworld mixed features | The current canonical implementation is source-major and fuses the selected feature/ore streams; immutable 5×5 contexts can be prepared in parallel | Preserve source order and per-entry synchronization. Stage-major replay requires an equivalence trace before it may replace the fused plan |
| Nether mixed feature step | Source-major fusion is allowed only with the existing per-entry resident synchronization and admission-derived completion | Target-local structure placement and source commit are transaction boundaries; do not assume fixed order for every request |
| End base worlds | The nine immutable bases and overlapping spatial-batch dependencies can run in parallel | Copy into a private request region before any source decoration |
| End decoration | Source bodies may share a private 3×3 region, but the committed source order is serial within that region | Do not expose a partially decorated region as a completed column |
| Overworld top layer | The scan itself is target-local | Barrier after every feature that may alter the target's motion-blocking view |
| Output, light, packet sidecars | Output extraction can be parallel after all inputs are immutable | Output barrier, then packet/light radius barrier |

Admission supplies the missing ordering input. A `ChunkRequest` expands a
target rectangle by its dependency halo, admits the resident wavefront, and
records which source completions are legal. Backpressure must stop admission
before the retained halo exceeds the configured budget. The dispatcher may
cancel pure jobs before commit; it may cancel mutable work only at a declared
transaction boundary. No canceled or rejected job advances the completed
frontier.

### Canonical trace and equivalence tests

The reference interpreter should emit one typed event for every observable
boundary:

```text
(dimension, target, source, column_stage, decoration_step, global_index,
 seed_scope, input_fingerprint, ordered_writes, sidecars)
```

The optimized plan is equivalent only if its normalized trace and all retained
products match the canonical plan. Final block equality alone is insufficient:
two orderings can overwrite to the same final state while producing different
sidecars, RNG consumption, or intermediate height views.

Required tests are:

* Schedule tests validate unique order, dimension-specific cutoffs, lifecycle
  dependency direction, and terminal packet finalization. The existing
  `worldgen_stage_lifecycle_contract` test is the first guard.
* Cold-full versus shaped-then-full generation must be byte-identical for
  palette, biomes, heightmaps, block entities, spills, gateways, and spawn
  candidates. Full-then-lower requests must perform no work and must not
  downgrade the frontier.
* Persist/resume must be exercised at every valid boundary for every
  dimension. A missing product or sidecar must reject the record, not trigger
  a hidden recomputation or skip.
* Serial, ordered-worker, source-major-fused, and stage-outer plans must
  compare canonical traces. Include a control where reversing source order
  changes an order-sensitive placement; a no-op fixture is not evidence of
  equivalence.
* Radius controls must fail when a write escapes its declared write radius or
  a read requires an unadmitted source. End's wider structure scan and the
  packet light halo are useful controls because their footprints differ from
  feature context.
* Heightmap/sidecar controls must prove their timing: the Overworld ore height
  view is post-carve/post-structure; End client maps are captured before
  feature decoration while final block output includes it; Nether upper-row
  decoration remains in its spill sidecar.
* Cancellation tests must show no partial resident mutation, no frontier
  advancement, and deterministic retry. Admission tests must show that two
  requests with different halos can have different legal source orders.
* Far-tier tests must prove that a terrain frontier uses only the declared
  prefix and that a later decorated/packet request resumes from its retained
  products. The test must count stage executions, not only compare final
  pixels, so accidental recomputation is visible.

These tests should be small coordinate fixtures and trace dumps. They do not
require a large render-distance run.

## How to change it

When adding or moving a pass:

1. Update the one dimension entry in `stage_schedule.rs` and its shaped
   boundary, then update the schedule/lifecycle contract tests.
2. Give the pass one descriptor with typed prerequisites, read/write radii,
   mutable resources, outputs, retained sidecars, seed scope, and barrier.
3. Make the dimension executor consume the descriptor's typed input and return
   its declared products. Keep the canonical production path and reference
   replay on the same feature dispatcher; do not add a second feature loop for
   a new fidelity tier.
4. Extend the frontier serializer and fingerprints when a new retained
   product is introduced. A stage is not resumable until every later reader
   can obtain its declared inputs from persistence.
5. Add one order-sensitive trace fixture, one persistence/resume fixture, and
   one cancellation or radius control before enabling a new optimized plan.

Do not infer a dependency radius from a convenient current loop, use a higher
stage to paper over a missing lower product, or call a query-only horizon
sample a completed column stage. Keep canonical semantics stable while an
optimized execution plan is being tuned.

## Configuration

The current schedule and lifecycle tables are compile-time constants and have
no runtime flags. The public server tier remains `Shaped`/`Full`; End's
`GenerationLevel` and metadata-only `StageFrontier` are available to an
incremental scheduler, but durable product storage and executor selection are
not yet configuration options. Worker count, admission budget, cancellation,
and persistence format belong to the eventual scheduler and must be included
in the frontier's compatibility fingerprint when they affect observable order.

The shell's horizon controls select the query-only `HorizonSurfaceQuery` path.
They do not change the worldgen stage schedule or make its preliminary result
resumable.

## Dependencies

The schedule vocabulary lives in `lodestone-worldgen` and is consumed by the
three dimension generators. The server owns `ChunkGenerationStage`,
`ChunkSource`, `ChunkStore`, light settlement, packet sidecars, and source
admission. `lodestone-worldgen-parity` owns lifecycle replay, source/target
transactions, and the trace comparator. The shell owns horizon sampling and
distant terrain presentation. The feature region view, dense block grid,
heightmap helpers, structure registry, and dimension-specific decoration
modules provide the products named by the descriptors.

No generator behavior, stage boundary, or distant-rendering policy is changed
by this document. The existing const schedule is the non-invasive,
compile-checkable skeleton; the descriptor, executor, frontier, and trace
implementations should land only with the tests above and with the active
Overworld lifecycle-wave work coordinated against them.
