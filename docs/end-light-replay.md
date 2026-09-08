# End light replay

## What it is

End light replay keeps an initial chunk packet tied to the light snapshot captured after its admitted footprint is settled. A block change invalidates every retained snapshot that could have read the changed column, so a later packet cannot reuse light from an earlier admission state.

## How it works

`ChunkStore` owns the admission boundary. It serializes a block write over the complete cross-column light footprint, clears retained snapshots in its cache, and forwards the invalidation to the wrapped source. `RegionChunkSource` applies the same rule to saved column copies and marks those columns dirty, preventing a stale snapshot from being written back during the next save.

When a saved neighbour is no longer resident, the persistence layer keeps a
coordinate-keyed invalidation epoch until the owning region rewrite succeeds.
The save worker clears retained light from the stored column without restoring
live entities or scheduled ticks; a failed rewrite leaves the epoch queued for
retry, and a newer mutation cannot be erased by an older save completion.

Initial encoders may consume a retained snapshot verbatim. If no snapshot is available, the protocol computes a fallback from the supplied footprint; that fallback is not treated as evidence that a saved snapshot was settled. The raw-packet control in `crates/versions/26.2/tests/end_light_snapshot_admission.rs` compares two relative admissions with the same final footprint and requires identical packet bytes.

The End fallback also reproduces the initial light engine's storage admission: any admitted non-air section allocates a contiguous vertical corridor from the lowest admitted non-air section through the upper one-section apron. This preserves explicit empty block-light layers and full sky layers between the admitted terrain sections; an all-air footprint still allocates no layers. Nether keeps its per-section adjacency rule because it has no sky layer.

## How to change it

The dependency footprint is expressed by `RETAINED_LIGHT_NEIGHBOUR_OFFSETS` and the `ChunkSource::invalidate_retained_light_neighbourhood` forwarding seam. Add or change a retention layer only if it forwards that hook and clears its own snapshots. Keep controls relative to the edited column; do not add world coordinates or a dimension-wide light suppression rule.

The invalidation runs while coordinate gates are held. Preserve that ordering when changing the write path: an optimistic light computation must either observe the mutation or fail its revision check and recompute.

## Configuration

There are no runtime flags or environment variables. The protocol's `retains_initial_column_light` capability enables retained initial snapshots, while `uses_cross_column_light` describes the footprint that makes neighbouring invalidation necessary.

## Dependencies

The rule relies on `ChunkColumn` retained-light storage, `ChunkStore` revision gates, `RegionChunkSource` persistence, and the versioned `ServerProtocol` initial-light encoder. The authenticated P06 parity artifact and its full-digest sidecar remain external test inputs; they are not runtime data.
