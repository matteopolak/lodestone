# The ticked area follows the player

## What it is

The set of chunk columns the integrated server's world tick loop actually simulates —
random ticks, scheduled block and fluid ticks, the natural-spawn cycle and its census — and
the mechanism that keeps that set **centred on the players** instead of on world spawn. It
lives in `lodestone_server::tick_area` (`FollowArea`, `TickAnchors`, `TickAnchor`,
`TickFollow`) and is consumed by `lodestone_server::tick::run_tick_loop_with_weather`.

## The bug it replaces

`chunk_store.rs` recorded it about itself: *"`mob_area` is centred on world spawn and never
moves"*. The shell passed `mob_radius = view_radius.clamp(1, 3)`, so the world tick was a
49-column square nailed to chunk `(0, 0)` for the life of the world. Three owner-visible
consequences, all confirmed rather than suspected:

- **Natural spawning stopped** once the player walked out of the box. Two separate causes,
  and fixing only one would have looked like a fix: the chunk list handed to
  `MobSim::run_spawn_cycle` was fixed, *and* the terrain the `NaturalSpawner` proposed into
  was the sim's leaked `ChunkWorld` snapshot, whose `column()` returns `None` outside those
  49 columns — so `random_pos_within` found no surface and `cluster` returned nothing.
- **Random ticks stopped** with it: crops, grass, fire, leaf decay and the fluid queue all
  drain over the same list.
- The 49 columns were still touched at 20 Hz *outside* the streamed view, so the chunk
  store's working set was the **union** of two disjoint squares. Following the player
  collapses that union into the view, which makes the store's job easier rather than harder.

## How it works

Two things move, at two different cadences, and conflating them is the whole trap:

| thing | rebuilt | cost |
|---|---|---|
| the chunk coordinate list (`FollowArea::recompute`) | every tick | integer arithmetic over ≤49 pairs, no I/O |
| the terrain view the spawner reads (`FollowArea::snapshot_terrain`) | only when the list changes, or every `LIGHT_TTL_TICKS` | one `ChunkSource::column` per column |

The per-tick half is free. The expensive half is gated on `recompute` returning `true`,
which is what keeps a whole area's worth of column fetches out of one unserviced window.

**Why the expensive half is affordable is a property of the geometry, not of a cache.** The
follow radius is `chunk_store::CONCURRENT_TICK_RADIUS` (3) and a connection streams
`view_radius` ≥ 9, so every column the area covers has already been generated *for
streaming* and `ChunkStore::column` finds it resident — the measured ~3.1 µs clone, so a
full 49-column rebuild is ~152 µs against a 50 ms budget. Crossing one chunk boundary is
cheaper still: a 7-wide square shares six of its seven `cx` values with its one-step
neighbour, so 42 columns are retained and exactly **7** are new.

Where the anchors come from:

```
dispatch_play_packet (ServerBound::PlayerMoved)
  -> world.tick_anchors().publish([TickAnchor { dimension: source.dimension(), cx, cz }])
        (WorldStateHandle, shared)
  -> FollowArea::recompute()  in run_tick_loop_with_weather, once per tick
```

`TickAnchors` rides `WorldStateHandle` rather than being a new parameter on the
`serve_connection*` chain, because that handle is *already* threaded to both ends — the
packet dispatch (which is where a player's chunk position and its dimension are both in
hand) and the tick loop. A new parameter would have changed six wrapper signatures and every
`crates/protocol/v770/tests/*` call site, which is the cost `SourceRef`'s own doc records as
the reason those wrappers exist. It is a *sibling* of `WorldState` rather than a field inside
it, because `WorldState` is the persisted scalar set and a live player's chunk must not enter
a save schema.

The dimension is read off the connection's `SourceRef`, which switches to
`SourceRef::Dimension` on portal travel. Without that check a player in the Nether would drag
the overworld's tick area to the matching overworld coordinates and spawn overworld mobs into
a place nobody is.

## How to change it, and the gotchas

- **The fallback square is load-bearing.** An empty anchor set yields the
  `(cx_range, cz_range)` the caller passed — the fixed origin box this replaces.
  `chunk_store`'s memory gates, `redstone_placement_gate` and `tick`'s own tests drive the
  loop with **no players at all** and their expectations are written against a specific
  square; removing the fallback would silently void them rather than fail them. It also
  covers the real window between a join and the player's first movement packet.
- **`TickAnchors::publish` replaces the whole set**, exactly like `MobSim::set_players`. With
  two connections each clobbers the other's anchor, so the LAN path follows whichever player
  moved most recently. `FollowArea` already computes the *union* over anchors and dedupes it,
  so what is missing is per-connection bookkeeping (a keyed map plus a deregistration on
  disconnect), not geometry.
- **Do not put the terrain rebuild on a per-tick cadence.** 49 resident columns per tick is
  ~30 MiB/s of allocation churn for no benefit, and the moment a column is *cold* it is
  ~909 ms on the tick thread.
- **Do not raise the radius past `CONCURRENT_TICK_RADIUS`** without also revisiting
  `chunk_store::capacity_for_view_radius`, which sizes the LRU's headroom for exactly that
  reserve. The symptom of getting it wrong is cold columns on the tick thread, not a failure.
- **The column order is stable on purpose.** The random-tick stream's draw positions depend
  on visit order, so the set is sorted and deduped rather than iterated out of a `HashSet`;
  an order that varied per tick would make growth unreproducible.
- **The spawn cap moves with the area.** `FollowArea::spawnable_chunks` feeds vanilla's
  `spawnableChunkCount`, whose caps are `per-chunk maximum × count / MAGIC_NUMBER` (289), so
  widening the radius raises the mob cap as a side effect.

## What this deliberately does not fix

**Mob AI pathing still reads the world-open snapshot.** `MobSim` borrows
`world: &'w ChunkWorld` where `'w` is `'static` via `MobHandle`'s deliberate `Box::leak`, and
that snapshot is still the 49 columns around spawn. So a mob spawned far from the origin
navigates against an all-air `PathWorld`: it stands still rather than pathing, and it does
**not** fall through the world (mobs have no gravity in the sim; their Y comes from the path
and from the spawn position, which now *is* correct because the spawner reads the follow
area's terrain).

Closing that needs `MobSim` to **own** its world — `Arc<ChunkWorld>` instead of
`&'w ChunkWorld`, which also removes the leak — and then a `MobHandle::retarget` that swaps
the view as the area moves while keeping the population. That is a wide mechanical refactor
across `mobs.rs` and every `MobSim::new` call site, and it is the reason `NaturalSpawner`'s
own `world` field was converted to `Arc` here: the spawner is the half that could move
without it.

Two smaller cuts, stated so they are not rediscovered:

- Only the overworld has a tick loop at all. `IntegratedServer` opens one loop over one
  `ChunkSource`, so a player in the Nether correctly moves *no* area — the overworld stops
  ticking (vanilla's "no player tickets, no ticking") and the Nether never started. A
  per-dimension loop is a separate piece of work; the anchor already carries the dimension it
  will need.
- Item settling was already fixed independently and does not depend on this:
  `run_tick_loop` passes `MobSim::tick_with_terrain` a closure reading the live
  `ChunkSource`, so a dropped item lands on real terrain anywhere in the world.

## Configuration

| knob | where | default |
|---|---|---|
| `TickFollow::radius` | `IntegratedServer::open_in_memory_with_mobs` | `chunk_store::CONCURRENT_TICK_RADIUS` (3 ⇒ 49 columns) |
| `TickFollow::radius` (LAN) | `IntegratedServer::bind` | `LAN_TICK_RADIUS` (2 ⇒ 25 columns) |
| `TickFollow::dimension` | both | the source's own dimension |
| terrain staleness cadence | `tick.rs` | `natural_spawn::LIGHT_TTL_TICKS` (200) |
| fallback square | the loop's `tick_area` parameter | unchanged from before |

## Dependencies

`lodestone_server::dimension::Dimension` for the tag, `lodestone_server::chunk::ChunkSource`
for the terrain rebuild, `lodestone_server::mobs::ChunkWorld` as the view type
`NaturalSpawner` consumes, and `lodestone_server::world_state::WorldStateHandle` as the
transport for the anchor set.

## Gates

- `tick_area::tests` — the geometry, all with the player at chunk **(100, -37)** rather than
  `(0, 0)`. That matters: `(0, 0)` is the single position where "fixed at the origin" and
  "centred on the player" are the *same set*, so every pre-existing gate in this area was
  incapable of failing under the old behaviour. The assertions are written as the old
  behaviour's failure — the origin must **not** be in the area — and the axes are distinct so
  a transposition cannot survive.
- `tick::tests::the_random_tick_pass_follows_the_player_and_abandons_the_fixed_area` — the
  production-level gate: drives the real `run_tick_loop` with a `ColumnProbe` source and
  asserts the random-tick pass visits the 3×3 around the player and **zero** times visits the
  fallback column. Verified live by neutering `FollowArea::recompute`'s anchor filter, which
  fails it.
- `a_one_chunk_step_moves_only_a_seven_column_strip` predicts `(42, 7)`. Its first draft
  predicted `(35, 7)` — the plausible round number — and failed. That arithmetic is what
  decides how many columns `snapshot_terrain` fetches per boundary crossing, so guessing it
  low would under-estimate the cost this whole design is built around.
