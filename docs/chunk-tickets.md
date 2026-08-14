# Chunk tickets and the status pipeline

## What it is

`crate::ticket` (`crates/lodestone-server/src/ticket.rs`) is a vanilla-shaped
ticket/level graph that answers one question independent of any single
connection's view radius: *why should this chunk exist at all, and how
urgently*. `crate::chunk_store::ChunkStore` (`crates/lodestone-server/src/
chunk_store.rs`) is the one production consumer — it grants tickets, checks in
with the graph on its own read traffic, and evicts a chunk through the real
persistence-aware [`crate::chunk::ChunkSource::unload`] path once nothing
wants it any more.

Before this, nothing in the crate had a residency concept independent of
"has some connection asked for this recently" — `ChunkStore`'s own LRU
capacity bound was, and remains, the only thing deciding what stays cached.
Tickets add a second, orthogonal axis: *wanted regardless of recency*, which
is what a `/forceload`-shaped region or a world's spawn area need and an LRU
cache structurally cannot express.

## How it works

### The type, ported from vanilla in shape, not transliterated

`.cache/mc/26.2/src/net/minecraft/server/level/{TicketType,Ticket,
DistanceManager,ChunkTracker}.java` and `world/level/TicketStorage.java` are
the citations; `ticket.rs`'s own module doc carries the full per-symbol
account. The short version:

- A [`TicketType`] is `(timeout_ticks, flags)`. Nine registered constants live
  in `ticket::ticket_type` (`PLAYER_SPAWN`, `FORCED`, `PORTAL`, …), transcribed
  from `TicketType.java`.
- A ticket carries a **level**, not a radius. `TicketStore::set_ticket_with_radius`
  stores exactly one ticket at the centre chunk, at level
  `FULL_CHUNK_LEVEL - radius` — vanilla's `TicketStorage::addTicketWithRadius`.
  There is no per-chunk fan-out at grant time.
- `TicketStore::propagate` is a from-scratch recompute (not vanilla's
  incremental `DynamicGraphMinFixedPoint` — this crate does not have the chunk
  count that optimisation exists for): every position within a ticket's reach
  gets `ticket.level + chebyshev(ticket.pos, pos)`, and the minimum over all
  active tickets wins.
- **Two independent trackers**, never collapsed: a *loading* level (tickets
  with `does_load()`) and a *simulation* level (`does_simulate()`). This is
  what lets a loading-only ticket (`PLAYER_SPAWN`) make a chunk resident
  without making it tick — `ticket.rs`'s own
  `collapsing_the_two_trackers_breaks_the_s3_property` test demonstrates the
  bug a one-tracker build would reintroduce.
- [`ChunkStatus`] is two states, `Empty`/`Full` — a deliberate simplification
  of vanilla's twelve. `OverworldGenerator::column` is one monolithic call
  with no seam to stop at `NOISE` or `SURFACE`, so there is exactly one
  transition this crate can express.

### The consumer: `ChunkStore`

`ChunkStore<S>` owns a `TicketStoreHandle` (a plain `Arc<Mutex<TicketStore>>`
newtype — the same shape as `crate::tick::BlockTickFeed`/`ExplosionFeed`) and
exposes:

- `tickets()` — a cloneable handle for granting/removing tickets from outside.
- `set_spawn_ticket`/`refresh_spawn_ticket` — vanilla's `PLAYER_SPAWN`.
- `set_forced_ticket`/`remove_forced_ticket` — vanilla's `FORCED`, keyed by a
  caller-assigned `u64` id so more than one forced region can coexist.
- `ticket_status(cx, cz)` — the ticket graph's `ChunkStatus` answer,
  independent of whether the column happens to be cached right now.

Every real op through the store (`ensure`, called from `column`/`block_state`/
`block_entity`) calls `maybe_tick_tickets`, which purges expired tickets,
re-propagates, and — for every position that just became unresident — drops
it from the cache and calls `self.source.unload(cx, cz)`, exactly the call
`ChunkStore`'s pre-existing capacity eviction already makes. That call is
what reaches `crate::region_source::RegionChunkSource::unload` in a
persistent world: it marks the column pending-unload, and
`WorldSaveHandle::save`'s sweep releases it from the edit map once — and only
once — it is safely on disk. An edited column is therefore never lost to a
ticket-driven eviction; it costs a disk reload, never a wrong block, the same
guarantee the store's existing LRU eviction already carries.

### Why this is not threaded through `run_tick_loop`

The "obvious" design ticks the graph once per game tick from
`crate::tick::run_tick_loop`, exactly like `BlockTickFeed`. That is the more
*correct* design (ticket expiry would then mean exactly N game ticks, matching
vanilla's `purgeStaleTickets`), and it was rejected for this session because
`run_tick_loop`'s signature has eleven direct-or-wrapped call sites across
`tick.rs`, `redstone_placement_gate.rs` and `integrated.rs`, and `tick.rs`
carries concurrent in-flight redstone work — exactly the file this repo's own
hazard notes say to touch with named-anchor insertions, never a signature
change, when avoidable.

Instead, `ChunkStore::maybe_tick_tickets` piggybacks on the store's own read
traffic, rate-limited by `Cache::stamp` (the store's existing monotonic op
counter) rather than a new atomic. The cost, named rather than hidden: ticket
expiry is "approximately N ticks, decided by read cadence" rather than exactly
N game ticks, and a dimension nobody reads from never checks in — which is
also exactly when nothing needs evicting. `ticket.rs`'s own tests drive
`TicketStoreHandle::tick` directly for exact-tick semantics; `chunk_store.rs`'s
and `region_source.rs`'s tests drive it through real `column()` calls, so the
wiring itself is under test, not just the arithmetic.

### Why there is no new `ChunkSource` trait method

`crate::dimension::DimensionalSource<S>` wraps every real production world
(the overworld gets wrapped to reach the Nether/End) and implements
`ChunkSource` method-by-method, with no catch-all forward. It already carries
a scar from exactly this trap: `is_column_resident`'s default silently
answered `true` through this wrapper for a whole production path until issue
#504 added the explicit forward. Since `dimension.rs` is out of scope for this
change, adding a new trait method here would reproduce that bug on day one and
nobody would be able to fix the forwarding — so ticket mutation and status are
plain inherent methods on the concrete `ChunkStore`, reached by a caller that
holds (or can obtain) that concrete type, never through generic `ChunkSource`
dispatch.

## How to change it, and the gotchas

- **`propagate` is bounded per ticket, not per world.** Each ticket's BFS is
  clamped to `MAX_LEVEL - ticket.level` in every direction — do not replace it
  with a whole-store scan.
- **A ticket is keyed by `(TicketOwner, TicketKind)`, not by position.**
  Moving a ticket (a player walking, if/when that gets wired — see below) is
  granting it again at the new position under the same key.
- **`MAX_LEVEL` is `33`, not vanilla's `33 + RADIUS_AROUND_FULL_CHUNK`.** This
  crate's generator has no per-status neighbour requirement to justify the
  extra radius. **Do not "fix" this to match a vanilla source citation** — it
  would silently widen residency by however many rings were added, for no
  corresponding generation step.
- **`ChunkStore::TICKET_CHECK_PERIOD`** (20 cache ops) is the only knob on the
  piggyback cadence. Lowering it makes ticket bookkeeping more tick-accurate
  at the cost of more frequent graph recomputes on hot read paths; raising it
  does the opposite.
- **The mechanism is production-safe, but nothing currently grants a real
  ticket in production.** `set_spawn_ticket`/`set_forced_ticket` are called
  today only from tests. The natural spawn-ticket call site is
  `crate::server`'s `ConfigurationFinished` join arm (where the real spawn
  point is resolved via `crate::world_spawn::find_initial_spawn`), and that
  arm operates over a generic `S: ChunkSource`-parameterised connection with
  no concrete path to `ChunkStore`'s ticket handle without either the
  unforwarded-trait-method trap described above or a signature change to a
  public, cross-crate entry point (`serve_connection` and friends, consumed by
  `crates/protocol/v770/tests` and `lodestone-shell`). This is the honest
  remaining gap — see "Open work" below.

## Configuration

No env vars or flags. `ticket.rs`'s level constants (`FULL_CHUNK_LEVEL`,
`ENTITY_TICKING_LEVEL`, `MAX_LEVEL`, the `ticket_type` table) are
transcriptions of vanilla's own constants, not tunables — changing one means
re-deriving it from `ChunkLevel.java`/`TicketType.java`, not picking a new
number.

## Dependencies

`ticket.rs` depends on nothing beyond `std` — it is pure policy. `chunk_store.rs`
depends on it for the ticket graph and reuses the existing
`ChunkSource::unload` path (defined in `crate::chunk`, overridden by
`crate::region_source::RegionChunkSource`) to reach persistence; no new
dependency edge was added there. `crate::join_scheduler` depends on
`ticket::FULL_CHUNK_LEVEL`-shaped arithmetic only (`ticket_level_for_ring`), to
keep its own, independently-built per-connection wire-order priority queue
describing the same physical quantity as a real ticket's level without
sharing a store with it — see that module's own doc comment for why the two
stores stay separate.

## Open work

- **Grant a real spawn ticket at join.** Wire `crate::server`'s
  `ConfigurationFinished` arm (or a new, narrow accessor) to
  `ChunkStore::set_spawn_ticket` once the connection knows the concrete store
  type, or thread a `TicketStoreHandle` into the connection's existing
  resource bundle (`BlockEntityHandle`, `MobHandle`, and friends are already
  threaded that way) — the second is the lower-risk shape since it needs no
  new trait method and no `DimensionalSource` change.
- **Player-following loading/simulation tickets** (`docs/plans/
  chunk-lifecycle.md`'s U5) — replacing `crate::server`'s `ViewTracker`'s
  residency role with a per-player ticket pair. Same blocker as above:
  `serve_connection`'s generic connection code has no concrete path to a
  `ChunkStore`'s ticket handle today.
- **A `FORCED`/`/forceload` command.** The ticket type and `ChunkStore::set_forced_ticket`
  already exist and are tested against a real saved world; only the command
  handler is missing.
- **Ticket persistence** (vanilla's `TicketStorage` `SavedData`). Nothing here
  writes a ticket to disk — every `TicketStore` is rebuilt fresh at world
  open, so a `FORCED` ticket does not survive a restart yet.
