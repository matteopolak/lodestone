# Join column ordering

## What it is

The join scheduler chooses a deterministic order for chunk columns that are
still owed to a connection. The same distance-first and frustum-aware key is
used when a moving player reveals a new set of columns.

## How it works

`join_scheduler::ColumnQueue` owns pending coordinates, queue mutation, and
re-prioritisation. Its pure ordering arithmetic lives in the private
`join_scheduler::join_order` module: Chebyshev distance is the primary key, a
frustum penalty reorders columns within one distance ring, and the original
input index (or coordinate for a set) provides a deterministic tie-break.
Keeping this calculation separate prevents the movement path and join path
from drifting while leaving worker admission independent of ordering.

`ColumnPipeline` fixes the order of admitted requests and cancels their tokens
when the pipeline is dropped. Running native work may finish, but it cannot
publish a result for a connection that no longer owns the request.
On native Overworld sources, it admits a bounded coordinate-local cohort and
receives committed stable outputs through a bounded channel. A small reorder
buffer emits only the contiguous prefix of the original queue order; worker
completion order never changes the wire order. The initial center remains a
singleton. Other sources keep the existing batch boundary.

## How to change it

Change `crates/lodestone-server/src/join_order.rs` when the distance or facing
policy changes. Change `join_scheduler.rs` for queue state, cancellation,
window admission, or ordered emission. Preserve the distance-first rule so a
far in-frustum column cannot starve a nearer column behind the player, and keep
the ordering gates in `join_scheduler.rs` green when changing tie-breaks.

## Configuration

The frustum half-angle and yaw-sector quantisation are constants in
`join_scheduler.rs`. `ColumnQueue::reprioritise` only sorts after a centre
change or sector change; it does not sort for every small rotation.
Native cohort width is reported by the source, capped at 64 targets and an
8-by-8 target extent. It is independent of worker count; a single worker can
amortize one region without delaying the initial singleton center.

## Dependencies

The ordering helpers use only standard floating-point and tuple operations.
They are consumed by `join_scheduler::ColumnQueue` and the server view tracker;
protocol encoding remains outside this module. Cohort admission depends on
`ChunkSource`, `ChunkStore`, and the native generation dispatcher.
