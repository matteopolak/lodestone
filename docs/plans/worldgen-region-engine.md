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

At `6e7731960`, a one-worker, seed-42, square-64 production cohort retired 268.95 million instructions and 54.85 million cycles per requested output, reached 74.8 outputs/s, and peaked near 377 MB. Its block, biome, and heightmap checksums were `7259e555bbbb09dd`, `77e054e21804273e`, and `cb8ddce98e46838d`. A CPU-time sample restricted to `request_generation_cohort` attributed about 69% of leaf work to the two generation crates and 22% to server/session code. These are sampled CPU shares, not instruction shares.

The stage-PMU square-64 run at `65cde7395` retired 265.49 million instructions/output with unchanged checksums. Exclusive scopes attributed 122.93 million to terrain prerequisites, 21.84 million to feature bodies, 11.81 million to finalization, and 108.91 million outside those scopes. Terrain includes 55.47 million in shape, 20.43 million in surface, and 18.09 million in ordered materialization. A session-only rewrite cannot explain a threefold instruction reduction: even eliminating its entire measured remainder would leave about 156.6 million instructions/output. Region-phase scopes such as admission (143.42 million) and mutable completion contain stage work and must not be added to these exclusive groups.

A fresh baseline stage-by-active-region PMU control on the same square-64 production path retired 264.25 million instructions/output. All seven terrain stages ran under Admission, while Full-output interning ran under MutableTarget. The 143.49-million Admission envelope contained about 20.52 million non-stage instructions. MutableTarget plus MutablePadding covered 73.95 million; subtracting their feature, top-layer, and interning stages and the nested direct-transition mirror leaves roughly 31.2 million. Snapshot finalization consumed a separate 15.78-million envelope, mostly covered by its nested feature-commit scope; ledger publication consumed 6.19 million elsewhere. These are measured opportunities for a shared state plane, not predicted savings from replacing it.

The larger 256-output cohort previously reached about 211 million instructions/output and 93 outputs/s, but used roughly 964 MB. Cohort widening alone is therefore neither a bounded-memory design nor a solution to the single-worker target. At the 64-output baseline, a threefold instruction reduction would require about 179 million fewer instructions/output. No current component-level measurement supports claiming that saving from the region representation alone.

The first bounded diagnostic cohort measured the outer-context read contract. Of 44 read-only chunks, 42 supplied state reads, but only 3,006 of 11,264 possible XZ lanes and 1,359 of 33,792 coarse vertical cells were touched. All 44 supplied height reads, covering 5,604 XZ lanes. This confirms that full state materialization is often wasted in the rim, but a state-only lazy reader would still leave substantial height work and cannot plausibly supply the whole threefold reduction. Do not add a second terrain representation solely for this role.

Before the first carrier cut, each of the 144 immutable prefixes wrote a 98,304-cell packed shape, walked it again to form canonical states and surface classes, then walked it in z,x,y order to apply vein states, collect ocean-floor facts, and establish palette order. The 64 requested outputs undergo one additional section-packing walk that also computes packet heightmaps and counts. Feature settlement can force an `Arc` copy of a target's whole index field, but the sampled profile assigns all `memmove` calls only 2.81% of CPU time. Removing that copy is worthwhile only as part of a larger representation replacement, not as the main campaign.

The first terrain-handoff cut removed the separate code-to-state/class walk and byte class array. Surface writes remain tagged in the original `u16` field, and the required ordered palette walk decodes them. The square-64 production run kept all three checksums and fell from 268.95 million to 264.19 million instructions/output, with cycles moving from 54.85 million to 54.12 million and peak footprint from 376.7 MB to 377.2 MB. This is a measured simplification, not the larger redesign: it saves about 1.8% of instructions and leaves the terrain algorithm and session replay costs intact.

Before the cutover, measure the remaining discriminators in one bounded diagnostic cohort:

- Per-role complete-column passes and retained bytes from packed fill through section encoding. Count eliminated passes rather than allocating an additional slab and comparing only wall time.
- Read, write, winner, and palette-event volumes per feature owner. A final-state-only experiment must fail a deliberate overwritten-palette-state control.

A compact 4×8×4 carrier was tested in the current fill/surface/materialization handoff. With 85.4% uniform cells, its logical intermediate field fell from 28.3 MB to about 5.6 MB across 144 prefixes and preserved the output checksums. Yet its direct-coordinate revision raised instructions per output from 264.53 million to 273.77 million, cycles from 55.05 million to 58.40 million, and peak footprint from 377.8 MB to 378.9 MB. It was removed. A compressed carrier is viable only as part of a full path that avoids per-block decompression into the existing dense product and then a second mutable representation.

The replacement path should make immutable terrain states readable without first building a per-context palette. Carver, structure, feature, and top-layer writes then enter one request-bounded latest-state plane. Keep two independent histories: canonical winner provenance and state introduction in observable order, including entries later overwritten. Requested outputs materialize once from the settled plane in the established z,x,y base-palette order followed by write-order introductions. Read-only contexts never become packet-ready columns. This must replace the current epoch projection, lifecycle override/revision replay, and target copy path together; retaining any as a parallel authority defeats the design.

The exact upper terrain plateau may permit a compile-time-gated sign proof for eight Y cells per horizontal tile. It is a bounded shortcut, not the region redesign: cache sharing means skipping 128 cell decisions per column saves far fewer than 1,024 fresh corner evaluations. Keep it behind a structural graph check and reject it on a modified gradient control.

The first implementation slice should replace one complete ownership path: a canonical terrain reader, latest-write plane, and requested-output materialization over a bounded region. It must remove the corresponding replay or conversion path, not coexist as another cache. The prior compact-carrier experiment establishes a specific rejection criterion: lowering logical intermediate bytes while leaving `DenseBlockGrid` construction, feature replay, and output interning intact is not a successful slice. Run the replacement against the same square-64 controls before adding sliding eviction or more dimensions. If it cannot preserve exact reads and palette history or save measured work, remove the slice rather than widening it.

## How to change it

The terrain handoff is in `RegionPrefixBatch`, `PackedStateCarrier`, and `PreOreResult`. Feature reads and ordered writes pass through `MixedReplayWindow`, `VegGrid`, and `RegionFeatureEpoch`. `LifecycleMaterializer`, `GenerationSession`, `ProductionGenerationRegion`, and `ChunkStore` own settlement, checkpoints, and publication. Change their shared data contract before replacing an individual implementation layer; do not retain two authoritative mutable block stores.

Require same-input output checksums and raw packet-byte comparisons, plus independent reference evidence for changed behavior. Controls must cover cross-boundary competing writers, read-after-write within a feature step, overwritten palette entries, heightmap timing, block entities, negative coordinates, radius-one packet neighbors, cancellation before and after commit, and revision conflicts. Compare matched one-worker instructions, cycles, first-output latency, throughput, and peak RSS. Track the memory cost of active products rather than only final store length.

## Configuration

Use the ignored `strict_single_thread_production_worldgen` benchmark with `LODESTONE_WORLDGEN_WORKERS=1`, `LODESTONE_WORLDGEN_BENCH_COHORT=1`, `LODESTONE_WORLDGEN_BENCH_LAYOUT=square`, and a fixed seed/count/batch. `worldgen-stage-pmu` provides stage and region counters; `gen-counters` is diagnostic and changes hot-path cost. `scripts/profile-cost-table.py --under request_generation_cohort --require-cpu-time` excludes construction samples from a Samply capture. Keep captures under `.cache/worldgen-perf/`.

## Dependencies

The design depends on the stage-radius descriptors, generator sidecars, canonical state IDs, the request-scoped halo lease, ordered mutation provenance, output heightmaps, and packet/light settlement. It adds no runtime dependency. The current reference map and captured packet fixtures remain the behavioral authority for parity decisions.
