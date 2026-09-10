# Generated block-entity propagation

## What it is

Generated structures produce block states and block-entity state through separate products. This seam attaches typed spawner and container records to the receiving `ChunkColumn` so chunk packets and saved chunks carry the data behind generated blocks.

## How it works

`OverworldChunkSource` resolves structure references back to their origin starts, then asks `structure_loot` for payloads whose positions land in the requested chunk. Coded mineshaft corridors contribute cave-spider spawners and Nether fortress halls contribute blaze spawners. The sidecar matcher reads positions from the final receiving column's block field, after orientation and world-sensitive replay, so it cannot disagree with the block that actually won placement. The server converts each into `BlockEntity::Spawner(SpawnerState)`; the v26 encoder projects that state into update NBT containing the timing fields and nested `SpawnData.entity.id`.

At the protocol boundary, block-entity records use the external packet control's canonical order: packed local XZ, absolute Y, then registry id. Compound keys are recursively ordered by unsigned UTF-8 bytes; values and list element order are unchanged. This keeps semantically identical generated and persisted payloads byte-identical without depending on the source sidecar's insertion order.

Lifecycle replay has one additional boundary: a neighbouring feature may write a
container state into a target after that target's own structure sidecars were
attached. `LifecycleMaterializer::snapshot_for_packet` reconciles the detached
packet snapshot against its final states, adds an empty record for any newly
owned state, preserves a richer payload at an unchanged position, and removes a
stale built-in record when a later write replaced the block. The resident replay
map is not mutated by this finalization.

End structure templates have the same ordering hazard for face attachments:
template placement can report a banner entity before attachment survival turns
the unsupported banner into air. `ChunkColumn::from_end` validates those events
against the completed block field, and `ChunkColumn::reconcile_generated_block_entity_states`
performs the same strict state-owner check after source sidecars and later
feature writes. This keeps generated records for replaced blocks out of ordinary
End columns and leaves unclaimed plugin extension records on the general
reconciliation path. A surviving wall banner therefore retains its template
payload in a direct End source column; the End lifecycle packet boundary
intentionally filters those structure-owned records, so its external packet
can carry the banner block without a banner entity. An orphaned event does not
become an entity merely because placement reported it before the final state
pass.

## How to change it

Add a structure-piece family to `spawner_entity_type` when its generator emits a fixed spawner entity. Keep the mapping keyed by piece kind and let the receiving column's final block field provide positions; do not key payloads by world coordinates or replay a separate orientation transform. Extend the packet and source tests with independently expected field values and orientation controls when adding a new family.

## Configuration

There are no environment variables or flags. Structure references and piece ids come from the world generator; entity ids are literal resource keys validated by `ResourceKey` parsing.

## Dependencies

The path depends on `lodestone-worldgen` structure starts, `lodestone-server::structure_loot`, `BlockEntity`/`SpawnerState`, and the v26 block-entity registry and chunk encoder.
