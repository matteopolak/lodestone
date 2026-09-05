# WASM world snapshots and authoritative mutations

## What it is

The `world:read` WASM capability gives a runtime-loaded plugin a bounded, copied
view of block-state ids in the current client chunk store. `world:write` adds a
separate, finite singleplayer mutation request that reaches the integrated
server; it never writes the client replica.

## How it works

`lodestone:plugin@0.26.0` imports `world-snapshot.read-blocks(positions)` (the
Rust guest binding is `world_snapshot::read_blocks`). A call accepts
at most 128 positions and returns one `option<u32>` for each input in the same
order. `WasmHostPlugin` attaches the composed session's `ChunkWorld` handle to
each loaded guest at `TickSet::Intent`. The import takes that handle's chunk lock,
copies the requested block ids into a `Vec`, and drops the lock before returning
to guest code. No ECS guard, chunk guard, world handle, or callback crosses the
component boundary.

The capability is a WIT import rather than an action/event filter. A guest that
references it cannot instantiate unless both its manifest and the host policy
grant `world:read`; `CapabilitySet::default_policy()` withholds it. The native
comparison path is a normal `GameTick` system reading the same `ChunkWorld` and
keeping only its copied result. `crates/lodestone-wasm-host/tests/reaches_the_real_action_queue.rs`
proves both paths see a loaded state, an unloaded column, and an out-of-range
height identically through the composed client application.

This is a client-view snapshot, not an authority grant. On multiplayer, it is
only what the server has already sent to the client and may be stale immediately
after the call. On singleplayer it still names the client replica.

`action.set-resident-block` instead appends a copied request to
`PendingWasmWorldMutations`. `Sim::drain_action_queue` releases its ECS guard,
then passes it through the bounded `NetClient` relay. The net task is the only
task that owns the optional `IntegratedServer`: a hosted session validates the
state id and awaits `IntegratedServer::set_resident_block_state_proposed`; a
remote session returns `unavailable`. The server's own bounded proposal pass
finishes before its authoritative source write. The result returns to the same
plugin as a later `resident-block-mutation-outcome` event with one of the finite
WIT status values. The normal server block feed still updates the client replica.

## How to change it

To change either vocabulary, update `wit/lodestone-plugin.wit`, the ABI lowerer,
the conductor resource, the shell net bridge, the capability table, and the
separately built guest fixture together. Bump the WIT package version and
`ABI_WORLD` for any compatibility-affecting change. Keep the request limit
explicit and test a value that distinguishes an absent cell from state `0`.

Do not turn this into a retained world handle or call guest code while a chunk
guard is held. Larger scans must be split over host ticks. The `world:write`
relay must continue to target `IntegratedServer::set_resident_block_state_proposed`,
including when the client is hosting singleplayer; a client-replica write would
violate multiplayer authority.

## Configuration

Plugins declare `world:read` and/or `world:write` in `plugin.toml`; the embedding
host must also add `lodestone_wasm_host::Capability::ReadWorld` and/or
`WriteWorld` to its grant policy. Both are withheld by default. The maximum
positions per read call is `lodestone_wasm_host::MAX_BLOCK_SNAPSHOT_POSITIONS`
(128); mutation ingress is capped at 64 outstanding handoffs.

## Dependencies

This feature depends on `lodestone-wasm-host`'s WIT component bindings,
`lodestone_ecs::ChunkWorld`, `lodestone-shell`'s net task, and the integrated
server. The read half intentionally has no server dependency; the write half is
available only to the in-process singleplayer server and refuses remote sessions.
