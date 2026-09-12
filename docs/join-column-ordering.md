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

## Dependencies

The ordering helpers use only standard floating-point and tuple operations.
They are consumed by `join_scheduler::ColumnQueue` and the server view tracker;
generation, protocol encoding, and the world-generation dispatcher are outside
this module.
