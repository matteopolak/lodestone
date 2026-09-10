# Entity ownership transfer

## What it is

`lodestone_server::entity_handoff::EntityOwnershipHandoff` is the bounded
source-stop/destination-start barrier for a moving entity that crosses from one
tick-region owner to another. The first production consumer is dropped-item
motion; this slice establishes the hand-off contract without changing the
typed network-id surface.

## How it works

`EntityHandoffToken` names the stable `i32` entity id, its tick-start serial
slot, the nonzero owner-plan epoch, and typed source and destination
`TickOwner`s. The source owner stops first; only then does the central writer
replace live state and admit the destination. A second source stop, a
destination start without a source stop, a stale epoch, a mismatched route, or
a replay is rejected. Global work cannot enter this chunk-region barrier.

While a source-stop token is pending, `EntityOwnershipHandoff::source_unload_ready`
is false for that source owner. A chunk lifecycle/save coordinator must keep the
source resident until destination admission, then perform its normal durable
unload acknowledgement. The item path is synchronous at this boundary, so its
central state replacement and destination admission occur in one tick task;
future asynchronous region workers must retain the token across their save
acknowledgement rather than treating worker completion as durability.

`MobSim::tick_item_owner_batches` reads the owner admitted with each item at
tick start. Its immutable bounded job advances a copy and derives the
destination from the post-motion position using floor plus Euclidean chunk
division, including negative coordinates. `MobSim::apply_item_tick_owner_batches`
validates the complete unique plan and serial slots, stops every crossing
source, replaces item state without changing its id, then admits every
destination. The next tick therefore submits the item only to its newly
admitted owner.

Item snapshots are sorted by entity id, so completion order cannot change
client publication order. Items have no passenger or leash relationship; those
relationships remain outside this first consumer. `saved_entities` and
`native_entities` read the same centrally applied state, so a save sees either
the pre-transfer or post-transfer item record. An asynchronous region worker
must retain the token until its durable-save acknowledgement before releasing
the source; the current item path remains synchronous at this boundary.

## How to change it

For another moving entity, store its tick-start owner alongside its live state,
put both owners and the serial slot in its completion, and use
`EntityHandoffToken` at the central writer. Workers must receive immutable
snapshots only; they must not publish packets, mutate a shared registry,
allocate ids, or update passenger/leash relationships. Apply all source stops
before any destination starts, then publish through the existing central
snapshot path.

Keep controls for a negative-coordinate boundary, both sides of a positive
boundary, reversed owner completion, missing or duplicate completion,
stale/replayed tokens, and serial-versus-four-lane parity. If a transfer
removes an entity, call `EntityOwnershipHandoff::forget_entity`; do not retain
a process-wide acknowledgement history.

## Configuration

There is no new server setting or region-size choice. The existing dropped-item
worker threshold and four-lane cap are unchanged; this slice establishes
correctness and ordering only.

## Dependencies

- `lodestone_server::tick_region::TickOwner` supplies the typed chunk-owner
  identity.
- `MobSim` owns dropped-item lifecycle and motion state, while the live
  connection stream consumes its ordered `EntitySnapshot` output.
- Native persistence reads the same central simulation state through
  `MobSim::native_entities` and the existing world-storage hand-off.
