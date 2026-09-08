# Entity ownership transfer

## What it is

`lodestone_server::entity_handoff::EntityOwnershipHandoff` is the typed barrier for a moving entity that crosses from one tick-region owner to another. The first production consumer is dropped-item motion, which now carries its admitted chunk owner through each tick and changes owners only through this barrier.

## How it works

An `EntityHandoffToken` names the stable entity id, its tick-start serial slot, the nonzero owner-plan epoch, and typed source and destination `TickOwner`s. The source owner stops first; only then does the central writer replace the live state and admit the destination. A second source stop, a destination start without a source stop, a stale epoch, a mismatched route, or a replay is rejected.

`MobSim::tick_item_owner_batches` reads the owner stored with each item at tick start. Its immutable owner job advances a copy and derives the destination from the post-motion position using floor plus Euclidean chunk division, including negative coordinates. `MobSim::apply_item_tick_owner_batches` validates the complete unique plan and serial slots before it performs every source stop, replaces item state without changing the entity id, and performs every destination start. The next tick can therefore submit the item only to its newly admitted owner.

Client publication remains deterministic: item snapshots are emitted in entity-id order, while the existing mob leash and passenger fields remain attached to their original entity ids and are resolved by the central snapshot pass. The same central state replacement is what native entity persistence observes, so an unload/save boundary sees either the pre-transfer or post-transfer record, never a worker-owned half-state. A future asynchronous region worker must retain the token through its durable acknowledgement rather than treating source acceptance as persistence completion.

## How to change it

For another moving entity, store its tick-start owner alongside its live state, put both source and destination owners plus the serial slot in its completion, and use `EntityHandoffToken` at the central writer. Workers must receive immutable snapshots only; they must not publish packets, mutate a shared registry, allocate ids, or update passenger/leash relationships. Apply all source stops before any destination starts, then publish through the existing central snapshot path.

Keep deterministic controls for a negative-coordinate boundary, both sides of a positive boundary, reversed owner completion, missing or duplicate completion, stale and replayed tokens, and serial-versus-four-lane parity. If a transfer removes an entity, forget its barrier state with the entity; do not retain a process-wide acknowledgement history.

## Configuration

There is no new server setting or region-size choice. The existing dropped-item worker threshold and four-lane cap are unchanged; this slice establishes correctness and ordering only.

## Dependencies

The barrier uses `lodestone_server::tick_region::TickOwner`. Dropped-item motion and lifecycle state live in `MobSim`, and the live connection stream consumes its ordered `EntitySnapshot` output. Native saves consume the same central simulation state through `MobSim::native_entities` and the existing chunk/world persistence hand-off.
