# Overworld source-once FEATURES experiment

## What it is

This diagnostic seam executes the absolute source bodies for a bounded Overworld settlement once against one request-scoped mutable `VegGrid` region, then projects the final overlay into requested full columns and compact padding mutations. Production settlement uses the separate target-owned `RegionFeatureEpoch` path.

## How it works

Requested targets are deduplicated and sorted by `(z, x)`. Their source union defines a rectangular write region and read halo; immutable pre-ore products are borrowed through a dynamic source layout while all vegetation writes land in one overlay. Ore receives a borrowed source-centred window that translates its centre-relative coordinates into that same absolute grid. Each source is admitted from the fixed Overworld source schedule in first-occurrence order, and its mixed step/index stream retains the source's RNG sequence. The overlay is folded into each requested pre-ore prefix once at the end, followed by the top-layer stage; writes outside requested columns remain in the compact padding stream.

Source provenance is opt-in through `source_once_features_with_capture`. Capture records one dirty-log range per source, then walks the combined log backwards once. A single coordinate set keeps only the last source for each destination; the final local state is converted once to the canonical registry `StateId` after all sources finish. Mutation records retain that canonical id directly and never create state strings. The default source-once call and the scalar generator path do not create those capture ranges or coordinate set.

## How to change it

Keep the source schedule and absolute-coordinate routing deterministic. Extend the source result only when provenance or mutation order remains observable; do not silently sort away the intentional shared-region divergence. The differential control records the known scalar mismatch, while the input-order control checks that canonical source order and final projections are invariant. The capture control covers disabled capture, a last-writer collision, and negative chunk coordinates.

## Configuration

The experiment uses the generator's seed, resolver data, and existing replay contexts. Capture is disabled by default; pass `true` to `source_once_features_with_capture` for the diagnostic stream. It has no environment variables or runtime flags.

## Dependencies

It depends on `OverworldGenerator`, `MixedReplayBatch`, `VegGrid`, `OreWorldAccess`, the Overworld stage schedule, and the pre-ore/top-layer/output boundaries. Its result retains canonical block-state ids without requiring state-string conversion in worldgen.
