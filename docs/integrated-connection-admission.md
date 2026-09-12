# Integrated connection admission

## What it is

Integrated connection admission keeps terrain generation away from the connection runtime while preserving packet order. Cold action targets, retained-light neighbours, spawn-clearance cells, portal searches, and gateway/platform footprints are admitted asynchronously before their existing synchronous consumers run.

## How it works

`SourceRef::admit_columns` brokers a coordinate batch through the source's generation path. Shared integrated sources use the world-generation dispatcher, so the connection task waits for admission without calling `ChunkSource::column` itself. `dispatch_play_packet` awaits `admit_action_footprint` before matching a terrain action; command effects admit their resolved write set before applying it. A missing resident cell in movement, vitals, beacon, gateway, or portal work defers that probe until a later packet or timer. Queued End-fight writes admit their target footprints before the timer applies them.

Initial and deferred chunk encoding admit the centre and any cross-column light neighbours before the synchronous source-aware encoder. The native deferred join stream parks that source-aware encode as a pending future in the connection `select!`; packet reads, keep-alive challenges, and timers therefore remain serviceable while a cold footprint is admitted. Stored spawn and restored-player clearance admit a small surrounding footprint first. Portal travel admits the bounded destination search, while gateway arrival and the End platform use their resident-only helpers after their required columns have been generated.

Dimension travel emits the already-built transition (respawn and position) before forgetting the old view; only then does it move the cache centre and stream destination columns. An empty transition remains a no-op, so a protocol that cannot encode the change does not partially unload the connection.

## How to change it

When adding a connection-side terrain read, classify it as either an ordered action or a periodic probe. Ordered actions must await their complete owned footprint before the existing handler and must never be dropped because terrain was cold. Periodic work must use resident-only reads and preserve its pending state when a column is absent or busy. A deferred join encode must own a shared source handle when it crosses a `select!` pass; borrowing the loop's active-dimension reference would prevent portal travel from replacing it. Extend the footprint helper rather than adding a direct `column()` call to the connection loop. Keep the cold-source controls: they distinguish worker-thread admission from a runtime-thread generation call and verify that an action read occurs after admission.

## Configuration

There are no runtime flags. Footprints are derived from block/chunk coordinates and protocol capabilities (`uses_cross_column_light` and `retains_initial_column_light`). World-generation dispatch uses the existing scheduler and its configured worker budget.

## Dependencies

The flow depends on `ChunkSource` resident accessors, `SourceRef`, the world-generation dispatcher, join-stream encoding, `world_spawn`, and the resident portal/gateway admission helpers. The integrated cache remains responsible for retaining admitted columns; this connection layer only establishes ordering and deferral.
