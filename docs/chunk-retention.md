# Chunk retention and residency

## What it is

This document describes the ownership and eviction boundaries for decoded client columns, server-side generated columns, terrain-meshing snapshots, and persistence overlays. The goal is bounded steady-state residency without unloading data that an active ticket or a visible render path still requires.

## How it works

The integrated server keeps generated columns in `ChunkStore` behind an LRU cache. The cache capacity is derived from the connection view radius plus the bounded tick scan headroom; hosted stores also apply their configured ceiling. The wrapped source remains the authoritative persistence layer, so an ordinary cache eviction can release its cached copy and later regenerate or reload the same column.

Ticket residency is stronger than the cache capacity. A column covered by a loading or simulation ticket is now excluded from ordinary LRU victim selection. This prevents a cache miss between ticket-graph sweeps from unloading an active column; if every cached column is pinned, the cache temporarily exceeds its soft capacity until a ticket is removed.

The client owns one decoded `World` for the active dimension. Dimension transitions and a new login epoch unload every decoded column before new packets are admitted. Terrain meshing retains copy-on-write section handles only for submitted jobs; the scheduler removes generation records and queued results when a column leaves the view. Persistence keeps edit and block-entity overlays longer than the cache when required for correctness.

## How to change it

Change cache policy in `lodestone_server::chunk_store`: update the capacity derivation and its measurements together. Keep ticket protection in the eviction predicate; a periodic ticket sweep is not an adequate substitute. If a future policy needs to shrink a high-water capacity, pass the current view window and preserve both visible coordinates and ticket-resident coordinates before unloading anything.

When changing client unload behavior, keep the dimension/login clear before packet admission and preserve the copy-on-write contract used by `lodestone_shell::mesher`. Do not move persistence unload work under the cache mutex or add compression to the tick path.

The regression `lru_pressure_protects_ticket_resident_columns` exercises the critical boundary with a capacity-one store. It verifies that capacity pressure does not remove the ticketed column or send an unload callback for it.

The roaming regression and the ignored RSS probe use the same three view
regimes. After visiting the origin, a distant centre, and the origin again,
the cache returned to its configured bound in every case:

| slider render distance | view radius | capacity | retained packed block bytes | direct test-binary RSS* |
|---:|---:|---:|---:|---:|
| 8 | 9 | 512 | 12.38 MiB | 32.72 MiB |
| 16 | 17 | 1,275 | 30.82 MiB | 58.14 MiB |
| 32 | 33 | 4,539 | 109.71 MiB | 159.77 MiB |

*RSS was measured with `/usr/bin/time -l` around the already-built debug test
binary, excluding Cargo compilation. Packed block bytes exclude palettes,
metadata, map buckets, and allocator overhead; the existing release RSS probes
remain the authoritative production-profile measurements.

## Configuration

`DEFAULT_CAPACITY`, `CONCURRENT_SCAN_COLUMNS`, `MAX_CAPACITY`, and `FULLY_RESIDENT_VIEW_RADIUS` in `crates/lodestone-server/src/chunk_store.rs` control the server cache policy. The ticket graph controls which coordinates are protected. Client render distance controls the streamed view, while meshing worker count controls the number of in-flight snapshots.

## Dependencies

Server residency uses `ChunkStore`, `ChunkSource`, `TicketStoreHandle`, and `ChunkLifecycleHandoff`. Client decoded residency uses `lodestone-world::World`; terrain snapshots use `lodestone-shell::mesher`; persistence is provided by the region source and its edit ledger.
