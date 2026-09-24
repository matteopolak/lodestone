# Region-owned world-generation engine

## What it is

This is the performance design gate for production Full-column generation. It proposes replacing repeated column representations and mutable replay with one bounded request region, while preserving the existing stage, ownership, and packet contracts. The 200-column/s single-worker goal and a roughly threefold instruction reduction are targets, not measured outcomes.

## How it works

The current production path expands a square of 64 requested outputs into 100 ordered feature owners and 144 immutable terrain contexts. The outer 44 contexts can supply real feature reads. A region engine therefore cannot remove them merely because they produce no packet.

The proposed region owns four distinct forms of state:

1. Immutable, seed/config-keyed terrain and sidecars. A compact canonical `StateId` column is the source of feature reads; a read-only context need not acquire an output palette or mutable column.
2. One bounded overlay of latest writes. Feature placement reads this immediately, including earlier writes in the same source stream. Owner completion follows canonical `(z, x)` order even when immutable workers finish out of order.
3. Sparse winner provenance and an ordered palette transcript. The winner determines the settled value of a destination cell; the transcript preserves first introduction of palette states, including values later overwritten. They cannot be collapsed into one final-state map.
4. A committed output snapshot. Only requested targets are materialized and section-packed, after all owners that may affect their block field or packet-neighbor ring have passed. The store publishes each stable output transactionally before its callback is exposed.

An active owner cursor retires terrain and overlays once no future source can read them and no pending output needs them. The request's admitted halo remains leased until its final dependent output commits. Cancellation discards the current uncommitted owner but retains published prefixes and products.

This is a replacement boundary, not another cache beside `DenseBlockGrid`, `RegionFeatureEpoch`, and `LifecycleMaterializer`. A prototype is useful only if it deletes corresponding conversion, replay, or resident paths. The existing packed fill, direct-address overlay, shared prefix preparation, and bounded staged store already avoid several obvious copies; preserve those gains.

At `6e7731960`, a one-worker, seed-42, square-64 production cohort retired 268.95 million instructions and 54.85 million cycles per requested output, reached 74.8 outputs/s, and peaked near 377 MB. Its block, biome, and heightmap checksums were `7259e555bbbb09dd`, `77e054e21804273e`, and `cb8ddce98e46838d`. A CPU-time sample restricted to `request_generation_cohort` attributed about 69% of leaf work to the two generation crates and 22% to server/session code. These are sampled CPU shares, not instruction shares. A session-only rewrite cannot explain a threefold instruction reduction.

The larger 256-output cohort previously reached about 211 million instructions/output and 93 outputs/s, but used roughly 964 MB. Cohort widening alone is therefore neither a bounded-memory design nor a solution to the single-worker target. At the 64-output baseline, a threefold instruction reduction would require about 179 million fewer instructions/output. No current component-level measurement supports claiming that saving from the region representation alone.

Before the cutover, measure three discriminators in one bounded diagnostic cohort:

- For read-only outer contexts, total and unique feature-read lanes/cells, and whether they require mutable state. A lazy terrain projection is unjustified if reads cover most cells or if it needs another full backing column.
- Per-role complete-column passes and retained bytes from packed fill through section encoding. Count eliminated passes rather than allocating an additional slab and comparing only wall time.
- Read, write, winner, and palette-event volumes per feature owner. A final-state-only experiment must fail a deliberate overwritten-palette-state control.

Then implement one vertical slice: canonical terrain read access for the read-only context role, with the existing owners and requested outputs unchanged. Promote a context to the mutable/output role only at its actual boundary. If the slice does not reduce measured work or cannot preserve exact reads, remove it before widening the redesign. Subsequent slices replace owner replay and final materialization together, then make the owner cursor sliding and memory-bounded.

## How to change it

The terrain handoff is in `RegionPrefixBatch`, `PackedStateCarrier`, and `PreOreResult`. Feature reads and ordered writes pass through `MixedReplayWindow`, `VegGrid`, and `RegionFeatureEpoch`. `LifecycleMaterializer`, `GenerationSession`, `ProductionGenerationRegion`, and `ChunkStore` own settlement, checkpoints, and publication. Change their shared data contract before replacing an individual implementation layer; do not retain two authoritative mutable block stores.

Require same-input output checksums and raw packet-byte comparisons, plus independent reference evidence for changed behavior. Controls must cover cross-boundary competing writers, read-after-write within a feature step, overwritten palette entries, heightmap timing, block entities, negative coordinates, radius-one packet neighbors, cancellation before and after commit, and revision conflicts. Compare matched one-worker instructions, cycles, first-output latency, throughput, and peak RSS. Track the memory cost of active products rather than only final store length.

## Configuration

Use the ignored `strict_single_thread_production_worldgen` benchmark with `LODESTONE_WORLDGEN_WORKERS=1`, `LODESTONE_WORLDGEN_BENCH_COHORT=1`, `LODESTONE_WORLDGEN_BENCH_LAYOUT=square`, and a fixed seed/count/batch. `worldgen-stage-pmu` provides stage and region counters; `gen-counters` is diagnostic and changes hot-path cost. `scripts/profile-cost-table.py --under request_generation_cohort --require-cpu-time` excludes construction samples from a Samply capture. Keep captures under `.cache/worldgen-perf/`.

## Dependencies

The design depends on the stage-radius descriptors, generator sidecars, canonical state IDs, the request-scoped halo lease, ordered mutation provenance, output heightmaps, and packet/light settlement. It adds no runtime dependency. The current reference map and captured packet fixtures remain the behavioral authority for parity decisions.
