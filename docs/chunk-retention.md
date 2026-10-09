# Chunk retention and residency

## What it is

Ownership and eviction boundaries for decoded client columns, server-generated columns, terrain-meshing snapshots and persistence overlays: bounded steady-state residency without unloading data that an active ticket or visible render path still needs.

## How it works

- The integrated server keeps generated columns in `ChunkStore` behind an LRU cache whose capacity derives from the connection view radius plus bounded tick-scan headroom (hosted stores add a configured ceiling). The wrapped source stays the persistence authority, so eviction only releases the cached copy and the column later regenerates or reloads.
- Ticket residency beats cache capacity: a column covered by a loading or simulation ticket is excluded from LRU victim selection, so a cache miss between ticket-graph sweeps cannot unload an active column. If everything cached is pinned, the cache temporarily exceeds its soft capacity.
- The client owns one decoded `World` for the active dimension; dimension transitions and a new login epoch unload every column before new packets are admitted. Meshing keeps copy-on-write section handles only for submitted jobs, and the scheduler drops generation records and queued results when a column leaves the view. Persistence keeps edit and block-entity overlays longer than the cache where correctness needs it.
- A player edit to a resident column goes through `ChunkStore::try_set_block`: it commits the post-edit snapshot to the source's edit ledger before changing the cache, then invalidates retained light in the neighbouring footprint. A busy or absent coordinate, or a source without the typed resident-edit hook, falls back to the blocking writer. This avoids regenerating a full column on its first edit while preserving edits across eviction.

Measured (roaming origin, a distant centre, origin again; the cache returned to its bound in every case):

| slider render distance | view radius | capacity | retained packed block bytes | direct test-binary RSS* |
|---:|---:|---:|---:|---:|
| 8 | 9 | 512 | 12.38 MiB | 32.72 MiB |
| 16 | 17 | 1,275 | 30.82 MiB | 58.14 MiB |
| 32 | 33 | 4,539 | 109.71 MiB | 159.77 MiB |

*`/usr/bin/time -l` around the built debug test binary, excluding compilation. Packed bytes exclude palettes, metadata, map buckets and allocator overhead; the release RSS probes remain authoritative.

## How to change it

- Change cache policy in `lodestone_server::chunk_store`, updating capacity derivation and measurements together. Keep ticket protection in the eviction predicate (a periodic sweep is no substitute). A policy that shrinks a high-water capacity must pass the current view window and keep visible and ticket-resident coordinates.
- Keep the resident-edit hook mutation-only: using the full-column persistence hook for light settlement would make unedited terrain a permanent edit. A successful resident edit updates ledger and cache before reporting completion.
- Keep client unload's dimension/login clear before packet admission and the copy-on-write contract used by `lodestone_shell::mesher`. Never do persistence unload work under the cache mutex or add compression to the tick path.
- `lru_pressure_protects_ticket_resident_columns` (capacity-one store) checks that pressure neither evicts a ticketed column nor sends it an unload callback.

## Configuration

`DEFAULT_CAPACITY`, `CONCURRENT_SCAN_COLUMNS`, `MAX_CAPACITY`, `FULLY_RESIDENT_VIEW_RADIUS` in `crates/lodestone-server/src/chunk_store.rs`; the ticket graph decides protection; client render distance sets the streamed view and meshing worker count the in-flight snapshots. `RUST_LOG=lodestone_edit_trace=debug` times successful resident edits; fallback edits of 50 ms or more log `lodestone_server::stall` phase timings.

## Dependencies

`ChunkStore`, `ChunkSource`, `TicketStoreHandle`, `ChunkLifecycleHandoff`; `lodestone-world::World`; `lodestone-shell::mesher`; the region source and its edit ledger.
