# Worldgen generation ledger

## What it is

The generation ledger is the world-owned retention lane for resumable world generation. It keeps typed stage frontiers and immutable products separate from packet-column caching, while source completions and sparse mutations remain available when an individual request is cancelled.

## How it works

`GenerationLedger` groups state by `PipelineIdentity`. Each pipeline has a bounded coordinate set, a `StageFrontier` for every admitted coordinate, immutable products and sidecars, shaped aggregate prefixes and terminal output products, globally deduplicated `(target, source, stage)` completions with a contiguous source-order cursor, sparse provenance-bearing overlays owned by their absolute destination, and a per-coordinate revision counter. A new pipeline identity evicts the least-recently-used identity; per-pipeline lane caps reject an operation before inserting the overflowing item.

Target-owned settlement may finish a padding coordinate before that coordinate
is requested directly. A completion is specific to the target's read view:
the requested session receives the canonical winning foreign writes with their
actual source and original ordinal, but does not claim that a padding source
completed in every later request context. Shaped packet neighbours remain
distinct from full requested outputs.

Before a requested output is captured, every owner in its reverse-writer domain
completes and the canonical provenance winner for each destination cell is
applied to the target column. The session records those winning foreign writes
and a compact proof of the completed domain; the packet snapshot, retained
Output product, and cache are then checked against one final column. Recomputing
a settled target-owned writer is a no-op even when its losing state differs from
the canonical cell. Writes outside the proven domain, writes whose source is
not their target, and dimensions without this proof still fail closed.
Settlement retires an overlay only after confirming that the persisted source
contains it or that the destination Output already contains the same state;
it never patches a published product.

Batch commits capture requested outputs and packet-neighbour columns, which need
not cover every mutation destination in the admitted generation halo. Writes
outside that materialized set remain sparse ledger overlays until their own
destination is captured. Source persistence receives only captured destinations;
an uncaptured write cannot be declared durable merely because its source target
finished. If a captured target came from persisted terrain, canonical winning
spills are applied to that copy before ledger validation, persistence, and packet
publication, so all three consumers see the same state.

`ChunkStore::lease_halo` canonicalizes coordinates, captures their write-gate revisions, and pins them in the cache. The lease holds the gate state records but not the gates themselves, so generation may run without blocking unrelated writes. `ChunkStore::execute_generation_session` admits the halo and snapshots a validated ledger checkpoint under the ledger lock, releases that lock while the borrowed driver runs, then publishes the session's immutable records, ordered source completions, and overlays through an atomic clone-and-swap. It rechecks cancellation immediately before `ChunkStore::commit_generation`; a cancelled request therefore leaves any committed ledger prefix and removes only newly admitted coordinates that are still empty. `ChunkStore::commit_generation` reacquires the canonical write gates, rejects any cache revision change, inserts the complete batch, and releases the gates before eviction work. Dropping the lease removes its pins and performs deferred LRU eviction.

Native cohort streaming commits each finished target before emitting it. If an external write changes a later target's captured revision, the store keeps the already emitted prefix, releases the old halo lease, and retries only unfinished targets with fresh leases. A revision conflict stays typed across the source boundary; other generation or consumer errors still stop the cohort.

## How to change it

Add new retained values through the typed `ImmutableProduct`/`ImmutableSidecar` APIs and keep their descriptor declarations in the worldgen stage schedule. A source completion is identified by target, source, stage, and its contiguous source order; do not use request identity for deduplication. Keep overlays sparse and bounded, accept only canonical provenance order for overwrites, and treat a revision conflict as a retry signal rather than overwriting newer state.

`GenerationSession::export_checkpoint` is the handoff shape between the request and ledger. It carries the validated frontiers, typed products, sidecars, aggregate prefixes, ordered source identities, and provenance-bearing overlays; pending worker values are never published. Aggregate prefixes are consumed directly on resume, and terminal output products can repopulate the cache after eviction without replaying the source driver. A source completion is keyed by `(target, source, stage)` and stores its canonical order separately, so two target requests sharing one source remain independent. Concurrent requests with one request key share an in-flight admission slot; waiters retry from the resulting resident or retained output. Ledger revisions used for optimistic overlay commits are per-target cache state and are deliberately distinct from `SessionRevision`, which only allocates mutation provenance inside a request. Overlay replacement compares provenance, so publication is deterministic even when workers finish in reverse order. A completed output frontier does not retain its target-scoped overlays because the output product is the reusable terminal state; partial mutable prefixes retain their ordered writes for resume. `ChunkStore::active_retained_bytes` reports logical bytes for both resident columns and retained ledger payloads.

Reverse-settlement proof belongs to `GenerationSession` rather than packet
encoding. The production region installs it only after every target-owned
writer in the domain completes. A cancelled request can publish that proof with
its committed FEATURES prefix; a later ledger checkpoint restores it. The proof
authorizes replay of a losing writer only after the Output product exists, and
the final column must still match the canonical winning writes. Keep this path
shared by scalar, batch, native, and browser execution. If feature ownership
becomes non-square or permits a distinct source coordinate, replace the compact
domain type with a representation that can authenticate that shape instead of
weakening the replay predicate.

Publication is monotonic across overlapping requests: a checkpoint whose records are already a prefix of the current frontier is accepted only when its aggregate metadata and covered records agree with the current state. The existing frontier, aggregate, sidecars, products, source completions, and overlays remain authoritative; a divergent stale checkpoint fails transactionally without replacing newer state.

## Configuration

`GenerationLedgerLimits` sets caps for pipelines, coordinates, source completions, overlays, retained products, and sidecars. Defaults are 4 pipelines, 4,096 coordinates, 16,384 source completions, 131,072 overlays, 32,768 products, and 16,384 sidecars per pipeline. The overlay cap matches the default request mutation budget so a valid partial source checkpoint is not rejected by a tighter publication limit. A new pipeline uses least-recently-used eviction except for pinned active pipelines; a full set of pinned pipelines rejects admission. Per-pipeline lane caps reject before inserting the overflowing item. Cache capacity continues to come from the existing view-radius policy; halo pins may temporarily exceed that soft bound and eviction resumes after the last lease releases.

## Dependencies

The ledger uses `lodestone_worldgen::stage_schedule::{PipelineIdentity, StageFrontier, StageRecord}` for typed schedule state and `crate::worldgen_session` for immutable products, sidecars, mutation provenance, and session-compatible revisions. `ChunkStore` uses its existing `ChunkWriteGates`, `ChunkLifecycleHandoff`, ticket residency, and wrapped `ChunkSource` for cache commits and deferred unloads.
