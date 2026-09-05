# WASM world snapshots

## What it is

The `world:read` WASM capability gives a runtime-loaded plugin a bounded, copied
view of block-state ids in the current client chunk store. It is observation
only; a missing value means no loaded cell exists at that position, while state
`0` remains a loaded air cell.

## How it works

`lodestone:plugin@0.24.0` imports `world-snapshot.read-blocks(positions)` (the
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
after the call. On singleplayer it still names the client replica. Block mutation
continues to use the existing local-player break/place lifecycles, whose request
is validated and adjudicated by the server; there is no direct WASM world-write
path in this slice.

## How to change it

To change the read vocabulary, update `wit/lodestone-plugin.wit`,
`lodestone_wasm_host::host::GuestState`, the capability table, and the separately
built guest fixture together. Bump the WIT package version and `ABI_WORLD` for
any compatibility-affecting change. Keep the request limit explicit and test a
value that distinguishes an absent cell from state `0`.

Do not turn this into a retained world handle or call guest code while a chunk
guard is held. Larger scans must be split over host ticks. A future mutation API
must enqueue a server-owned proposal and report a finite server result; writing
the client `ChunkWorld` would only alter a replica and violate multiplayer
authority.

## Configuration

Plugins declare `world:read` in `plugin.toml`; the embedding host must also add
`lodestone_wasm_host::Capability::ReadWorld` to its grant policy. The maximum
positions per call is `lodestone_wasm_host::MAX_BLOCK_SNAPSHOT_POSITIONS` (128).

## Dependencies

This feature depends on `lodestone-wasm-host`'s WIT component bindings and
`lodestone_ecs::ChunkWorld`. It intentionally has no server or protocol-family
dependency: the query reads the version-local state ids already installed in the
client's chunk store.
