# World-generation session

## What it is

`lodestone_server::worldgen_session` is the request-scoped state boundary for
parallel world generation. It keeps a target's dependency halo admission plan,
retains typed stage products, commits mutable source work deterministically,
and detaches a packet snapshot only after the requested generation prefix and
light domain are ready; the protocol installs the numeric light product at the
detached encoding boundary.

## How it works

`GenerationSession::new` derives a deterministic admission order and creates a
`StageFrontier` for every admitted coordinate. Pure stage results may arrive in
any worker order through `complete_immutable`; `advance_ready_immutable` commits
only the next stage of each coordinate, so completion timing cannot change the
frontier or retained products.

Mutable work is represented by a `MutableTransaction`. Each write carries the
request target, source coordinate, stage, source ordinal, absolute destination,
and session revision. `complete_mutable_source` queues source results and drains
only the contiguous source-order prefix. Unsubmitted transactions can be
rolled back, while cancellation clears pending work and retains committed
products and mutations.

Target-scoped feature replay keeps source-body deduplication within the active
target transaction. An overlapping target may execute the same source again
because its resident read view and ownership boundary are different; the
resulting absolute writes still apply in canonical completion order.

`ResidentRead` captures the current committed revision and owns its product
handles. `RequestCancellation` can be cloned into worker jobs; cancellation
stops later session work without interrupting a running job. `complete_light_domain`
records the revision covered by the light fence; `finalize_packet_snapshot`
then returns an owning `PacketSnapshot` containing a concrete target
`ChunkColumn` and packet-neighbour columns, so encoding can run without
borrowing the session or its halo. The snapshot starts unprepared: its
one-shot `ColumnLightSettlement` product is installed by the protocol-specific
detached encoder, and `is_light_settled` is true only afterward. The dependency
form accepts shaped neighbours because they are light inputs, not
packet-generation targets. The packet neighbour set is declared before
finalization and every supplied neighbour must match it. Full packet snapshots
also require the output product and all output sidecars, including client
heightmaps.

The initial-light broker passes the owned radius-one columns to the version
encoder; it does not reread a mutable source after detachment. Sources without
this request boundary use the scalar generation fallback.

Production dimension drivers live at the `ChunkSource` boundary in
`production_worldgen_session`. Their typed policies select source order,
shaped sidecars, and top-layer behavior. A shaped admission is imported as one
authenticated `MaterializedWorld` aggregate with its real column and required
sidecars; earlier stage records are coverage metadata, not copied density,
biome, or surface products. `ImmutableComputeExecutor` is the replaceable seam
for immutable admission work. The default uses the persistent server
dispatcher on native targets and the serial fallback in browser builds.
Mutable feature and top-layer commits remain in session order. Dimensions whose
Features stream places structures retain its typed structure-write product from
that same execution; the session never reruns the structure body to manufacture
an earlier stage product.

`SessionBudget` bounds product, sidecar, mutation, and explicitly accounted
retained-byte usage. `new` accounts inline value size; heap-backed values use
`new_with_retained_bytes` (and mutable writes use
`push_with_retained_bytes`) so callers do not silently under-report retained
memory. Pending values are charged when accepted and released on cancellation.

Mutable stages require `declare_mutable_sources` with a complete zero-based
canonical source set. A stage cannot commit after only a contiguous prefix;
source coordinate and order are both checked at submission. Ordered source
transactions are preflighted before any member of a ready prefix is applied.

`export_checkpoint` and `from_checkpoint` provide a validated in-memory
handoff for committed frontiers, products, sidecars, ordered source identities,
and mutation history. Pending worker values are omitted, while already
committed source completions and overlays from an active mutable stage remain
available for a resumed request. The pipeline identity plus every declared
retention value is checked during restore.

`ChunkSource::request_generation` is the object-safe production boundary. It
returns either an existing cache/edit/disk column or an owning `PacketSnapshot`;
on a cache miss, `ChunkStore` hydrates a world-owned ledger and passes the live
session through persistence wrappers to the dimension driver. Region persistence
resolves edits before disk before delegating, and therefore does not expose the
low-level driver through its wrapper. A source that has no request capability
continues to use `column`/`column_at` unchanged, which is the explicit legacy
fallback used by tests and older protocols.

## How to change it

Add new immutable products or sidecars to the central worldgen stage
descriptors first; the session validates those declarations before accepting a
completion. A mutable stage should declare its exact source set, submit every
source, and call `commit_mutable_stage` only after all source completions have
drained. Keep worker submission and `ChunkSource` adaptation outside this
module: the session is a state machine, not another executor. Production
callers that need a different immutable executor use the driver helper's
executor parameter while retaining the same policy and commit boundaries.
Changes to the
commit order, budgets, checkpoint validation, or revision rules need controls
for out-of-order completion, duplicate writes, rollback, incomplete sources,
oversized values, and cancellation.

## Configuration

The request selects a dimension, target coordinate, generation target, and
dependency radius. `SessionBudget::DEFAULT` allows 4096 products, 2048
sidecars, 131072 mutations, and 64 MiB of explicitly accounted retained
values; `with_budget` selects tighter limits. A world-owned `GenerationLedger`
can supply a checkpoint to the store execution boundary; session and ledger
revisions remain separate. No environment variables or persistent storage are
used by this module; checkpoints are intentionally in-memory. Aggregate-prefix
imports carry a dimension-specific executor version and two fingerprints so a
checkpoint cannot silently cross an executor or shaped-boundary change.

## Dependencies

The module uses `lodestone_worldgen::stage_schedule` for dimension pipelines,
typed descriptors, and `StageFrontier`. The existing server dispatcher is the
default immutable worker-admission boundary. `ChunkStore` imports and exports
the checkpoint at its production wiring
boundary; packet codecs and the world-owned ledger remain separate from packet
encoding. Generated streaming writes the cache only, so it does not turn every
viewed column into a dirty persistent edit. Initial-light protocols consume
the detached radius-one snapshot at the encoding seam; sources without the
request-scoped capability retain the scalar fallback.
