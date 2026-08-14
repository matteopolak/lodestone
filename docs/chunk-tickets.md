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
- **Issue #619/#297: production now grants real tickets, through a threaded
  handle rather than the trait-method route.** `crate::server`'s
  `serve_connection_inner` (and every `serve_connection*` wrapper above it)
  takes a `tickets: &TicketStoreHandle` parameter — the signature-change
  option this doc used to describe as lower-risk, since it needs no new
  `ChunkSource` trait method and no `DimensionalSource` change. Every
  `IntegratedServer` join path (`open_in_memory`'s wasm32 arm,
  `open_in_memory_with_mobs_using` — singleplayer and `open_persistent_with_mobs`
  alike, `open_to_lan`, `publish`) passes the real handle
  `ChunkStore::tickets()` returns; every pre-existing entry point (the
  `_shared` compatibility wrappers, `serve_connection_with_plugin_channels`,
  test harnesses) passes a fresh `TicketStoreHandle::default()`, exactly the
  compatibility shape `server.rs` already uses for `BlockTickFeed`/
  `ExplosionFeed`/`WorldStateHandle` and friends — a disconnected handle
  nobody else reads, so no pre-existing caller's behaviour changed.
  `IntegratedServer::tickets()` hands a host the same handle back for
  inspection or a `/forceload`-shaped command.
- **The world-spawn ticket is granted (and refreshed) from the connection
  side, not from `run_tick_loop`.** `crate::server`'s `ConfigurationFinished`
  arm grants `ticket_type::PLAYER_SPAWN` at the resolved world-spawn column,
  radius `ticket::PLAYER_SPAWN_RADIUS` (3) — re-granting under the same
  `(TicketOwner::Spawn, TicketKind::PlayerSpawn)` key on every join is a
  refresh, not a second ticket. `PlayerTicketGuard::refresh_world_spawn`
  (called from `serve_play`'s own `keep_alive_tick` timer, native only) is
  vanilla's `Ready.keepAlive()`, moved to the one per-tick hook this crate
  already has for a connected player rather than a new `tick.rs` insertion —
  see `tick.rs`'s own off-limits status in `CLAUDE.md`'s hazard notes for why
  that route was never on the table. On `wasm32`, which has no timer arm at
  all, the spawn ticket is granted once at join and left to expire under
  vanilla's own 20-"tick" (here: `TicketStore::tick`-unit) countdown; the
  player's own `PLAYER_LOADING`/`PLAYER_SIMULATION` pair is unaffected
  (`timeout: 0`).
- **Per-player loading/simulation tickets replace `ViewTracker`'s implicit
  residency role.** `TicketStoreHandle::grant_player` grants both at join
  (`serve_connection_inner`, radius = the connection's own `view_radius`),
  returning a `PlayerTicketGuard` `serve_play` owns for its whole lifetime.
  `dispatch_play_packet`'s `PlayerMoved`/`ClientInformationChanged` arms call
  `PlayerTicketGuard::move_to` whenever `ViewTracker::recenter`/
  `set_view_radius` actually changed the tracked centre or radius (compared
  before/after the call, not merely "a movement packet arrived") — this is
  what makes "a chunk near two players stays loaded independent of either
  one's view" real: two independent tickets under two independent
  `TicketOwner::Player` ids both cover the shared column, and
  `TicketStore::propagate`'s own min-over-all-active-tickets rule is what
  keeps it resident until *both* have moved away.
  `crates/lodestone-server/tests/ticket_residency_live.rs` is the live gate,
  against a saved-then-reopened world, for a single connection's grant/move/
  disconnect and for the two-connection union property.

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

- **A `FORCED`/`/forceload` command.** The ticket type and `ChunkStore::set_forced_ticket`
  already exist and are tested against a real saved world; only the command
  handler is missing.
- **Ticket persistence** (vanilla's `TicketStorage` `SavedData`). Nothing here
  writes a ticket to disk — every `TicketStore` is rebuilt fresh at world
  open, so a `FORCED` ticket does not survive a restart yet.
- **`portal.rs`'s ad-hoc parallel column pre-warm is not subsumable by this
  module, and should not be routed through it.** `create_portal`'s 33×33
  destination search calls `crate::chunk::generate_columns_parallel` directly
  to fix a *throughput* problem (a fresh column costs ~909 ms, and the search
  used to pay that serially, once per column, inside one unserviced window).
  The ticket graph answers a different question — *should this column be
  resident at all* — and has, by this module's own design (see "What this
  deliberately does not port" in `ticket.rs`'s module doc), no scheduler, no
  worker pool and no priority queue behind it: granting a ticket does not
  generate anything, it only changes what `ChunkStore`'s eviction later
  leaves alone. Wiring the portal search through a ticket grant would still
  leave every column in the search generated **serially, on read**, the
  moment the scan calls `block_state` — the exact defect the parallel
  pre-warm exists to avoid. If the two are ever unified, the ticket graph's
  *level* is the right input to a priority-aware version of
  `crate::join_scheduler::ColumnPipeline`, not a replacement for the pre-warm
  itself.
- **`wasm32`'s world-spawn ticket has no refresh path.** See the note above:
  a real gap on that one target, not a bug this work introduced, since
  `serve_play`'s `wasm32` build has no timer arm to hang a refresh off.
