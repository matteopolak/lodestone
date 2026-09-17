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

FEATURES has an explicit `LifecycleFeatureDispatch` ownership contract.
`TargetOwned` dimensions run one complete target dispatcher and retain the
returned mutation sequence as the stage's source ordinal; `SourceOrdered`
dimensions keep the scalar source wavefront as the compatibility fallback.
This distinction is part of the behavior contract, not an executor detail:
an overlapping target may still execute source bodies again when its read view
and ownership boundary differ. Capture replay remains source-ordered so its
authenticated event stream is not changed by the production fast path.

Before an Overworld output becomes terminal, production completes every
target-owned writer within the FEATURES mutable-write radius. Those writers
run in global `(z, x)` order. Their immutable read context expands the requested
set by the separate FEATURES read radius, while only requested targets receive
OUTPUT products or packet snapshots. `ChunkStore` gates, pins, and admits that
complete context. Adjacent targets share one request-owned replay product, so
overlapping terrain prefixes and source-selection plans are prepared once.

Settlement padding uses the explicit `LifecycleCompletionMode::SparsePadding`
boundary. A padding writer still runs the mixed FEATURES stream followed by
TOP_LAYER in order, but retains only final local writes, outward spills, and
block entities destined for requested columns. It does not create an output
product, attach padding structures, settle light, or repair padding-local
entities. Local writes are retained in a coordinate-keyed sparse overlay and
are applied to the shaped prefix only if that padding column later becomes a
packet neighbour or another mutable consumer; unrelated transaction overrides
are not folded into it. A generated `PacketNeighbour` owns that overlay and
applies it once, lazily at its first `column()` or `into_column()` call, so
repeated packet or light reads reuse the same materialized column and its
incremental heightmaps.
Deferred future-target entries in the CARVERS read overlay are rolled back by
the same transaction boundary, while sparse padding-local entries remain.

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
FEATURES ownership, shaped sidecars, and top-layer behavior. A shaped
admission is imported as one
authenticated `MaterializedWorld` aggregate with its required sidecars; for the
Overworld it may retain an immutable `GeneratedColumn` rather than immediately
constructing a mutable `ChunkColumn`. The latter conversion is deferred until
the first mutation, heightmap, light, or packet consumer, and is deduplicated
per admitted coordinate. Earlier stage records are coverage metadata, not
copied density, biome, or surface products. `ImmutableComputeExecutor` is the
replaceable seam for immutable admission work. The default uses the persistent server
dispatcher on native targets, the initialized Rayon pool in threaded browser
workers, and a serial fallback in browser workers without shared memory.
Mutable feature and top-layer commits remain in session order. Target-owned
FEATURES results retain their internal write order and provenance while the
session commits one ordered source transaction. Dimensions whose Features
stream places structures retain its typed structure-write product from that
same execution; the session never reruns the structure body to manufacture an
earlier stage product.

Pristine Overworld admission batches use one request-scoped staged-store lease
for the union of their radius-ten closures. If any requested coordinate is
edited or hydrated, the source rejects that batch and the existing per-column
precedence path is used. Target-owned transactional writes into untouched
generated neighbours remain ordered sparse overrides until a later consumer
actually needs a mutable `ChunkColumn`; persistent, already-materialized, and
source-ordered destinations still cross that boundary immediately.

The Overworld target-owned path can return its finished dense target column
directly after FEATURES and top-layer work. The materializer adopts that
column while its authenticated generated shaped prefix remains compact, and
forwards only cross-column mutations. It also retains a sparse
target-local final-FEATURES/top-layer read-state product: later target-owned
operations seed their CARVERS view from it, without reapplying those writes to
the already finished resident column or recording them as cross-target
provenance. The session's reusable provenance map therefore retains only
writes whose destination crosses into another column; this keeps mutable-source
validation proportional to reusable overlays.

Batch sessions share the materializer's immutable shaped-prefix handles while
they import their frontier coverage. The resident column remains mutable and
authoritative; its short-lived shared-prefix cache is invalidated at each
mutable target boundary, so later targets cannot observe stale blocks while
avoiding one deep column copy per session.

The generated resident map is request/region scoped and bounded by the
admission halo. It preserves block access, stage identity, structures,
heightmaps, sidecars, fingerprints, mutation transactions, and packet output
through the existing materialization boundary; it is not a completed-column
cache. This boundary also leaves room for a future producer to hand off
section-aligned compact storage without forcing an intermediate flat-grid
repack.

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

If a cache miss finds a retained output product, the store reapplies any
checkpointed destination overlays to that target before returning the existing
column. This keeps a later spill into an already-completed destination visible
after cache eviction; the same product-backed settlement also retires the
pending bucket once its destination is complete.

Batch requests share one prepare/finalize transaction:
halo and generation-region leases, ledger hydration, ordered mutation commit,
and rollback are identical on native and browser targets. Ledger-reused output
columns join generated columns in that final commit rather than publishing an
intermediate cache write against the same halo revision. Only the execution step
differs; browser batches await the yielding source boundary so a multi-column
batch cannot starve the server worker's tick and packet tasks.

Ledger publication uses a bounded journal over only touched pipeline entries.
Validation or capacity failure restores those entries and the ledger stamp
exactly, without cloning unrelated pipelines or the growing product store.
When a session has already committed its target `OUTPUT` frontier, a
direct target-local mutation (the target is also the source and destination)
is still checked against the pipeline, destination frontier, and coordinate
revision, but publication skips reinserting it because the committed output
is authoritative. Cross-column and foreign-source mutations remain ledger
overlays until their destination reaches `OUTPUT`; after that boundary, the
destination product is authoritative for the cell.
When a later request hydrates such an overlay, the destination must be in its
halo and the destination must remain inside the recorded source's write radius;
the foreign source itself need not also be in the later request's halo.
The world-owned ledger indexes pending overlays by destination chunk, so a
checkpoint visits only the buckets in the request halo rather than scanning all
historical writes. After a destination `OUTPUT` product is available, its
bucket is folded into that product in one ordered batch and the pending values
are retired. Nonpersistent sources use the same product-backed path; sources
that retain complete columns may retire a write as soon as the source accepts
the snapshot. A destination output product is authoritative: an identical
later write is a no-op and any conflicting write is rejected. Leading-edge
writes remain overlays only until the destination output is published, then
they are folded in one ordered batch. Admission evicts the least recently used
closed coordinate bundles when the coordinate ceiling is reached; active
pipeline leases block eviction, and a later request regenerates an evicted
bundle through the normal source/cache boundary.
Any new retained ledger map must add a journal slot, record its prior value
before mutation, and restore it on rollback; include a later-failure control
that reaches the new entry before returning an error.

Integrated mob seeding uses this boundary too. It cannot run the scalar column
generator beside an active join. The low-priority seed task waits until a player
has joined and its whole seed region is resident, then reuses those session-owned
columns.

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
revisions remain separate. Checkpoints are intentionally in-memory. Set
`LODESTONE_WORLDGEN_LEDGER_TRACE=1` on native builds to print a strict frontier
or late-output conflict when diagnosing a rejected publication; browser builds
keep this disabled. Aggregate-prefix
imports carry a dimension-specific executor version and two fingerprints so a
checkpoint cannot silently cross an executor or shaped-boundary change.
The ledger's `coordinates_per_pipeline` ceiling is an LRU bound for closed
coordinate bundles; pinned pipelines reject an admission that cannot fit, while
unpinned closed bundles are regenerated on a later request.
Fingerprint v2 hashes the ordered palette-index stream directly. Cell ordinals
are implicit in that fixed stream order, so hashing each ordinal again added
CPU work without adding identity information. Indices are fed to the digest a
section at a time instead of through one digest call per cell.
Mutable stage and packet-output identities wrap that content digest with the
stage boundary and coordinate. When a later stage only carries the same block
field, production reuses the prior content digest instead of rescanning the
column; stages with writes still compute a fresh content digest. This keeps
content hashing separate from the smaller lifecycle identity chain.
For pristine Overworld generated prefixes, an authenticated resolver identity
also covers the seed, settings, fallback biome, and asset bundle, so the
request records use a coordinate/stage provenance digest without scanning the
compact block field. Dynamic resolvers and edited or persisted columns retain
the exact content-digest path. The lifecycle marks that provenance only after
the typed prefix has been admitted; direct target output and the
FEATURES-to-TOP_LAYER-to-OUTPUT digest chain require the marker. A dynamic
typed column, an edit, a hydrated column, or a cross-target override therefore
cannot accidentally take the provenance shortcut. The one-block mutation
control remains on the exact path and must change the resulting fingerprint.
When an authenticated target receives a persistent cross-target write, the
lifecycle folds that ordered `(position, state)` event into the resident
identity in constant work; temporary speculative writes are never folded.

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
