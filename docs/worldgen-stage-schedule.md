# Worldgen Stage Schedule

## What it is

`lodestone_worldgen::stage_schedule` names the ordered passes that turn a
dimension's density field into a packet-ready chunk. The table is shared by
the three generators and is intended to be the vocabulary used by parity
replay and production dispatch code.

## How it works

There are four intentionally separate typed schedules:

* `StageSchedule` describes one column's pass order. The Overworld and Nether
  resolve structure starts, references, and terrain influence before fill;
  structure placement then happens after carving. The End has no terrain
  influence stage and resolves starts while placing structures after
  materialisation. Biomes, surface rules, decoration, and the final output are
  named separately. The Overworld has the final top-layer pass, while the
  Nether and End do not. Its `shaped_boundary` is explicit because `Shaped`
  does not mean the same final pass in every dimension: Overworld and End
  include structure placement, while Nether resumes with placement at source
  completion.
* `LifecycleSchedule` describes the common externally observable status graph:
  empty → structure starts → structure references → biomes → noise → surface
  → carvers → features → initialize light → light → spawn → full → packet
  finalization. Every `PhaseContract` gives its prerequisite phase and radius,
  plus its block-write radius. A dimension with no actual carver still advances
  through that no-op lifecycle status; this is intentionally distinct from its
  internal `StageSchedule`. Packet finalization is a Lodestone serving boundary
  that depends on both a full centre and the radius-one settled-light footprint.
* `SourceSchedule` describes the candidate source window for a mutable
  three-by-three feature dispatcher. Nether completion order is derived from a
  `ChunkRequest`: its admitted rectangle is tiled from the request's halo
  minimum, then ordered by tile-z, tile-x, local-z, and local-x. The resulting
  `AdmissionWavefront` sorts each target's source window; it is not a global
  centre-first/centre-last permutation. This is not a column-stage order: it
  exists because one source can write into a neighbouring resident chunk.
* `FeatureSchedule` describes the configured decoration ordinals inside one
  FEATURES pass. `DecorationStep` gives those ordinals names while preserving
  the numeric value needed by the seed derivation. External numeric values are
  decoded once at the data boundary.

`DimensionPipeline` is the executable contract for one dimension. It derives
its descriptors, prefixes, and production `StageExecutor` from the schedule's
single `stages` slice, so a second pass-order list cannot drift. An executor can
resume at a cached-prefix boundary, asserts the next named stage in every
build, and can optionally append typed stage keys to a borrowed diagnostic
trace without logging source code. `StageCursor` remains available to
unmigrated helpers. `StageGate` and `StageOption` record which data-dependent
passes may be no-ops, while `PipelineOptions` changes only whether a gated
operation runs; the stage still advances in the same order. The source guard
`cargo xtask check-worldgen-schedule` checks that production entrypoints obtain
their executor from the matching central schedule and that all three dimension
schedules retain their typed gate metadata.

Every dimension now has one `StageDescriptor` for every entry in its schedule.
A descriptor names typed products and sidecars, a conservative read/write
radius, seed scope, resident-write barrier, and cancellation boundary.
`StageSchedule::descriptors` derives descriptor order from the same `stages`
slice; there is no second pass list to drift. An absent pass is rejected by
dimension-qualified lookup (for example, Nether cannot obtain the
Overworld-only top layer, and End cannot obtain carvers). End's independently
measured contracts retain the detailed structure, gateway, spill, and heightmap
declarations; the Overworld and Nether descriptors use the same typed hand-off
vocabulary while their dimension-specific footprints are refined.

`StageFrontier` is the chunk-level resumable record. It admits only the next
stage in the central order, checks the descriptor's required products and
sidecars, and rejects a foreign dimension or schedule version before mutating
the retained prefix. Its `PipelineIdentity` combines dimension, schedule
version, and option fingerprint, so a cached far-column prefix cannot be
resumed under another dimension or option set. It records fingerprints and
retention declarations but does not own the generator's product store; a
caller persists those products alongside the frontier record.
`GenerationTarget::{Shaped,Full}` derives the public prefix or complete stage
slice from the same schedule, so a server or test entrypoint cannot copy a
shaped list by hand.

`RetainedStageFrontier` is the first production ownership seam for that
eventual cache. It keeps typed product and sidecar handles beside the ordered
frontier, advances only the missing prefix, and atomically validates every
descriptor-declared payload before committing a stage. A changed pipeline
fingerprint or an eviction clears both records and payloads, while a repeated
full request is a no-op. The payload map erases concrete types only inside the
retention boundary; callers use generic typed insertion and downcast accessors.
The current executor hook is deliberately small so each dimension can wire its
existing generator without introducing a second worldgen implementation.

The schedules and descriptors describe pass order and the named request-order
vocabulary. They do not perform generation or replace dimension-specific
dependency logic. For distant chunks, construct the executor at the first
unfinished index and advance the same stage cursor as the player approaches;
do not add a separate far-generation pipeline.
The Nether production dispatcher resolves `NETHER_SOURCES::order_for` from the
same `ChunkRequest` used by the replay plan, so a schedule change cannot
silently leave one comparator with a copied tile loop. The scalar one-column
fallback retains its historical source order because it has no authenticated
admission stream; only target-scoped completions consume the admission-derived
order.

The tables are based on the external runtime's generation pyramid and task
dispatch, read as two separate sources: the pyramid defines statuses,
dependency radii, and write radii; the task dispatch defines what work each
status performs. Internal names deliberately describe behavior instead of
copying external implementation identifiers. This distinction matters when a
single external status contains several Lodestone helper passes, or when a
status is a no-op for one dimension.

## How to change it

Add a `ColumnStage` variant only when a pass has an observable worldgen
boundary. Add
it to each affected dimension's static schedule in the exact order consumed by
that generator, update its `shaped_boundary`, then extend the schedule tests.
When wiring a generator whose
pipeline is split across cached helpers, use `executor_at` at the prefix boundary
through `shaped_boundary_index`; do not repeat a stage name at each resumption
site. Call `enter` or `run` at each helper boundary. Keep derived values such as
heightmaps inside their consuming pass rather than promoting them to stages. Add
a `DecorationStep` variant only when the external ordinal has a stable named
meaning; do not use a column stage to represent a source admission or a feature
ordinal.

Change `LifecycleSchedule` only after checking both the external pyramid and
its task dispatcher. A new phase needs a typed `LifecyclePhase`, one
`PhaseContract`, and independently derived dependency/write radii. Never infer
those radii from Lodestone's current neighborhood loops: that would make the
contract repeat the implementation it is meant to check. Keep strings out of
all of these tables; diagnostic text is derived from enum variants only at the
display boundary.

Do not use this table to paper over an unknown parity mismatch. First capture
the external order, then add a focused production and replay assertion that
proves both paths consume the same request/admission/completion model. A
request whose source window crosses a tile boundary is a useful control: it
must produce a different local source permutation than a request wholly inside
one tile.

When extending a dimension pipeline, update the descriptor for the existing
stage in that dimension's schedule; do not create a parallel descriptor order.
If a new retained product or sidecar is needed, add its enum variant, include
it in the descriptor, and add a frontier test that proves a record missing it
is rejected atomically. `StageFrontier` remains the identity and validation
record until the executor can persist and restore the corresponding product at
the same boundary. In particular, a descriptor does not authorize rerunning
feature sources against a mutable resident region without the source-order and
transaction guarantees named by its barrier.

## Configuration

The canonical schedules and descriptors are compile-time constants. Runtime
configuration is represented by `PipelineOptions::{structures,decorations}`;
the default enables both. `StageExecutor` validates entered stages in every
build and has no trace allocation unless explicitly requested.
`StageSchedule::validate` checks that each gate names one scheduled stage,
appears once, and has a descriptor whose typed key matches the dimension.
`LifecycleSchedule::validate` also checks that dependencies point strictly
backward and that packet finalization remains terminal.

## Dependencies

The module depends only on the Rust standard library and is re-exported as
part of `lodestone-worldgen`. Dimension generators, the server scheduler, and
parity tools may depend on it without introducing a dependency cycle.
