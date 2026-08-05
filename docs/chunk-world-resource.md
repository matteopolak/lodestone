# The chunk world as an ECS resource

> **Naming note.** This doc is about `lodestone_ecs::ChunkWorld` — the
> **client**-side bevy resource described below, used for rendering/collision
> in `lodestone-shell`. There is a second, unrelated `ChunkWorld` —
> `lodestone_server::mobs::ChunkWorld` — the **server**-side `PathWorld`
> adapter mob pathfinding runs over (see `docs/roadmap/server-entities.md`'s
> #204 entry and `crates/lodestone-server/src/mobs.rs`). The two share a name
> and a general shape ("terrain storage as a value the reader borrows") but
> are different types in different crates with no dependency between them;
> nothing on this page describes the server one.

## What it is

One `lodestone_world::World` for the whole process, held behind
`Arc<RwLock<_>>` as the `bevy_ecs` resource `lodestone_ecs::ChunkWorld`, plus
all terrain-mesh scheduling state as a second resource
(`lodestone_shell::mesher::TerrainMesh`) driven by one `Update` system. This is
Stage 4 of [`bevy-migration.md`](./bevy-migration.md) and the §4.1(d) half of its
`World` unification.

`lodestone_shell::sim::Sim` holds **no** `world`, **no** `demo_collision`, **no**
`scheduler`, **no** `dirty_columns`, **no** `pending_removals`, **no**
`uploaded_sections` and **no** `mesh_drops`. That is the stage's authority test:
with a second copy on `Sim`, a plugin's write to the chunk store would be a write
to nothing.

Chunks are still **not** entities and the mesher is still a worker pool — see
[`bevy-migration.md`](./bevy-migration.md) §2.3 and §8 for why both of those stay.

## The defect this closed

There were two `lodestone_world::World`s in the process, and *exactly one of them
was ever populated*:

| store | populated when | read by |
|---|---|---|
| `Sim.world` | `Sim::with_demo_world` / `--headless` | `snapshot_section`, `WorldCollision`, `block_at_world` |
| `SharedState.world` | any live session | `snapshot_section_live` via `NetClient::sections_and_light_at` |

So the duplication was not "two stores holding the same data" — it was **two
stores, one of them dead, and a three-term branch at every read site** to decide
which. That branch — `vanilla_atlas.is_some() && net.is_some() &&
net.world_dimensions().is_some()` — appeared five times (`mesh_dimensions`,
`remesh_section`, `column_is_loaded`, `mark_column_dirty`, `tick_collision`) and
had drifted:

- **Light at the vertical boundaries.** The live path gated light on the same
  in-range test as blocks, so the `si == -1` and `si == section_count` slots kept
  the full-bright bridge; the offline path read the real boundary light section
  that `World::section_light` exists to serve. One of the two is wrong, and it is
  the live one (see "Behaviour changes" below).
- **Drop accounting.** An absent column was a silent `return` offline and a
  counted drop live.
- **Column height.** Offline read `worldgen::MIN_Y` with *no* section count;
  live read `WorldDimensions`. Both are derivable from any loaded column, which
  is what `ChunkWorld::extent()` now does for both.

`snapshot_section_live` also took **24 lock acquisitions per column** (one
`sections_and_light_at` batch per section). It is now one read lock per column.

## How it works

```
net thread (tokio)                         driver thread (winit / headless)
┌────────────────────────────┐             ┌──────────────────────────────────┐
│ VersionAdapter::handle_pkt │             │ Sim::step(dt)                    │
│   └ WorldSink ─────────────┼──┐          │   poll_net                       │
│                            │  │          │     adopt_live_world  ← once     │
│ SharedState.world ─────────┼──┤          │     refresh_mesh_policy          │
│ SharedState.ecs:           │  │          │     on_column_arrived → mesh_col │
│   ChunkWorld ──────────────┼──┤          │   run_schedule(Update)           │
└────────────────────────────┘  │          │     FrameSet::Terrain:           │
                                │          │       heal_dirty_columns         │
        one Arc<RwLock<World>> ─┤          │   Sim.ecs:                       │
                                └──────────┼─→    ChunkWorld (same Arc)       │
                                           │      TerrainMesh                 │
                                           └──────────────────────────────────┘
                                                    │ submit(SectionSnapshot)
                                                    ▼
                                            MeshScheduler worker threads
```

### Adoption: where "one store" actually happens

`Sim` starts with a store of its own — the generated demo world for
`with_demo_world`, an empty `World` for a real client. `Sim::adopt_live_world`
(called from `poll_net`, so once per frame) replaces the resource with
`ClientHandle::chunk_world()` the first frame a handle exists.

It is **deferred rather than done in `attach_net`** because `NetClient::connect`
publishes its `ClientHandle` asynchronously from the net thread: at attach time
there is no store to adopt. It is idempotent — after the first success it is an
`Arc::ptr_eq`.

**A session that has offline terrain of its own keeps it.** The emptiness test in
`adopt_live_world` is the discriminant, not a proxy for one: a real client
session's store is empty at login, and the `with_demo_world` fixture's is not.
That fixture attaching a loopback feed is a real configuration —
`sim::tests::the_demo_world_fixture_is_the_control_that_fails_the_gate_above`
requires its uploaded sections to still be resident afterwards.

`Sim.adopted_live_world` records which of the two the resource currently names.
It is not a second copy of anything: it is the one fact nothing else can answer
once `net` has been dropped, and `end_session` needs it to release a server's
terrain while leaving the fixture's alone.

### The read/write split (issue #423)

`ChunkWorld` is the **read** handle; `lodestone_ecs::ChunkWorldWrite` is the
**write** handle, installed beside it at every site that owns the store. The two
always name the same `Arc` — an installer builds the write handle from the raw
`World` (or the client's `Arc<RwLock<World>>`) and derives the read handle from
*it* (`ChunkWorldWrite::read_handle`), so the "one store" invariant is one
`Arc`, not two agreeing stores.

The split exists so a plugin system that takes `Res<ChunkWorld>` physically
cannot mutate the store: the read handle exposes no `write()` and no `Arc`.
Writing goes through `ChunkWorldWrite` only, which is held by the store's
legitimate writers:

| writer | holds | route |
|---|---|---|
| `drive_placement`'s predicted write | `Res<ChunkWorldWrite>` (system param) | `write_predicted_block` + re-mesh |
| `Sim::predict_block` / `Sim::set_block_world` | `Sim::chunk_world_write()` accessor | the write resource in `self.ecs` |
| the net-ingest path (`lodestone-client`) | `SharedState.world` itself | the adapter's `WorldSink` through `world_write`, untouched by the split |
| test harnesses | the write handle they build (`ChunkWorldWrite::new` + `read_handle`) | `client.chunk_world_write()` for live-shaped tests |

`Sim::adopt_live_world` and `Sim::end_session` replace the **pair** together —
a write handle left naming the released server store while the read resource
names a fresh empty one would be the two-worlds defect this design exists to
delete. The split reaches `lodestone-client`: `ClientHandle` now hands out both
`chunk_world()` and `chunk_world_write()` on the same `Arc`, and the client's
own ECS resource install pairs them.

### `MeshPolicy`: the two facts the store cannot answer

```rust
pub struct MeshPolicy {
    pub sky_default: SkyDefault,   // the connected dimension's has_skylight
    pub id_spaces_agree: bool,     // classifier's block-id space == the store's
}
```

`Sim::refresh_mesh_policy` recomputes both every `poll_net`, because the
dimension changes on a portal trip and the id-space agreement changes the moment
a session attaches.

`id_spaces_agree` is what survives of the old three-term guard. The worker pool's
classifier is chosen once at construction from `!demo_world`, so "the atlas we
have" *is* "the id space the pool meshes":

```rust
let id_spaces_agree = if net.is_some() { atlas.is_some() } else { atlas.is_none() };
```

`false` is the jar-less-but-connected session: the demo palette and the vanilla
registry are disjoint id spaces, so meshing one with the other's classifier does
not fail, it draws garbage or nothing. That case used to `return` silently on a
`world.get` miss and render an empty world with a clean log; it now counts into
`TerrainMesh::drops` and warns with `branch = "id-space-mismatch"`.

There is a **third** such fact — `ColumnSource`, whether an absent neighbour
column is the edge of the world or a chunk still in flight — and it deliberately
does *not* live here: unlike these two it never changes over a session's life, so
it is fixed once on `TerrainMesh` from the worker pool's classifier rather than
recomputed every `poll_net`. See
[section mesh invalidation](./section-mesh-invalidation.md).

### Meshing stayed off the frame thread, and here is the argument

The plan asks for `Update` systems that "enqueue and drain". That is exactly what
landed, and nothing about the work's *location* changed:

| step | where it runs | before Stage 4 | after |
|---|---|---|---|
| snapshot 27 sections | driver thread | ✓ | ✓ (one read lock, was 24) |
| `mesh_snapshot_models` / `mesh_fluids` | `MeshScheduler` worker OS threads | ✓ | ✓ |
| collect finished meshes | driver thread, `try_recv` | ✓ | ✓ |

`heal_dirty_columns` is the only new system. It pops at most
`DIRTY_COLUMN_BUDGET` (4) columns, snapshots them, `submit`s, and returns. A slow
frame therefore delays the *upload* of finished geometry, never the meshing and
never `Sim::step`'s tick loop — which is the invariant
[`frame-pacing.md`](./frame-pacing.md) exists to protect, because a client the
server considers stalled is sent no chunks at all.

`MeshScheduler::drain_blocking` is the one method that blocks the caller. It has
exactly two callers, both outside the frame loop (the headless one-shot render
and `TerrainMesh::end_session`'s flush), and **it must stay that way.**

`MeshScheduler` became a `Resource`, which required making it `Sync`: an `mpsc`
`Receiver` is `Send` but not `Sync`, so `result_rx` is wrapped in a `Mutex`. The
lock is uncontended (only the driver drains) and both drain methods take it with
`get_mut`, i.e. no runtime lock at all when the resource is held mutably.

## What was deleted

| deleted | replaced by |
|---|---|
| `Sim.world: World` | `ChunkWorld` resource |
| `Sim.demo_collision: Option<Arc<DemoCollision>>` | nothing — see below |
| `struct DemoCollision(World)` | `struct ChunkWorldCollision(ChunkWorld)` |
| `Sim.scheduler` / `dirty_columns` / `pending_removals` / `uploaded_sections` / `mesh_drops` | `TerrainMesh` resource |
| `Sim::mesh_dimensions()` | `ChunkWorld::extent() -> Option<WorldExtent>` |
| `Sim::column_is_loaded()` | `ChunkWorld::contains_column()` |
| `Sim::drain_dirty_columns(budget)` | the `heal_dirty_columns` system |
| `Sim::demo_source()` | `Sim::chunk_collision()` |
| `sim::world_sections()` | `ChunkWorld::read().iter()` |
| `snapshot_section_live`'s 27-slot request batch (~60 lines) | `snapshot_section_in` |
| the `vanilla_atlas && net && world_dimensions` branch, ×5 | one store + `MeshPolicy` |

**`demo_collision` deserves its own note**, because deleting it retired a
documented hazard rather than merely relocating it.
[`local-player-components.md`](./local-player-components.md) records: *"Anything
that mutates `Sim.world` must clear `Sim.demo_collision`. A missed invalidation is
a player colliding against pre-edit geometry — 'I mined the block but still cannot
walk through it'."* That cache existed only because `WorldCollision` borrows and a
`Resource` must be `'static`, so the offline path cloned the **whole world**,
`O(loaded columns)`, once per block edit. `ChunkWorldCollision` holds the `Arc` and
takes the read lock inside `CollisionSource::with_view`, so there is nothing to
invalidate and nothing to clone. The rule is gone, not obeyed.

## Behaviour changes, all three of them

1. **Vertical boundary light on the live path.** `snapshot_section_in` follows the
   offline path's rule: light is fetched for `si >= -1` and left to
   `World::section_light`'s own range check, rather than gated on the block
   in-range test. The two boundary slots therefore read the real below-world and
   above-world light sections instead of the full-bright bridge. Overworld and End
   are unaffected (absent sky defaults to 15 either way, and the overworld
   measures 0 of 192 sky sections `Missing`); the Nether's build ceiling now reads
   its real sky `0` rather than the bridge's `15`. That direction is a fix, and it
   is **unverified against a live Nether** — no overworld gate can see it.
2. **`collide_against_live_world = false` now names an explicitly empty store.**
   This is a negative control that Stage 4 would otherwise have silently broken:
   the pre-fix behaviour it reproduces is "collide a live session against the
   offline world it does not have", i.e. nothing to stand on. With one store,
   falling through to `chunk_collision()` would have collided the control against
   the server's real terrain through the *demo* classifier — where every non-air
   vanilla id happens to read as solid — and `live_stands_on_server_ground.rs`'s
   control would have stopped failing while looking fine. `tick_collision` returns
   `ChunkWorld::default()` for that branch.
3. **An unloaded column is a silent no-op on both paths.** It used to be silent
   offline and a counted drop live. `mesh_column` guards on
   `contains_column` first, which strictly reduces false drops; the
   "invisible blocks" diagnostic still fires for a *loaded* column that yields no
   geometry (`branch = "all-air-loaded-column"`).

## How to change it, and the gotchas

- **`ChunkWorld`'s lock is `std::sync::RwLock`, not `parking_lot`**, deviating
  from [`bevy-migration.md`](./bevy-migration.md) §11. That prescription is about
  the **bevy** `World` lock (`EcsHandle`), which does use `parking_lot`. This one
  is the lock `SharedState` already owned and that the adapter writes decoded
  columns through as a `lodestone_world::WorldSink`; converting it would churn
  every world access in `lodestone-client` for no behavioural gain. Poisoning is
  recovered with `into_inner`, matching what `SharedState` already did at every
  call site. The two locks are never taken as a nested pair.
- **Never hold the chunk read lock across a mesh.** The whole point of the
  copy-on-write `Arc<ChunkSection>` snapshots is snapshot-then-release.
  `TerrainMesh::mesh_column` takes the lock for the snapshot loop and drops it in
  an explicit block *before* submitting, and the `Vec<(SectionKey,
  SnapshotOutcome)>` between the two exists for exactly that reason — not for
  borrow-checker convenience. (That was a `Vec<Result<SectionSnapshot,
  SectionKey>>` until issue #389 gave the snapshot a *third* outcome — see
  [section mesh invalidation](./section-mesh-invalidation.md).)
- **`TerrainMesh` is one resource on purpose.** Five separate resources would be
  five `ResMut`s of one invariant: a column that snapshots to nothing pushes a
  removal *and* may count a drop, and a drained mesh records an upload. Splitting
  them would make `heal_dirty_columns`'s signature grow and buy nothing.
- **`TerrainPlugin` deliberately does not insert `TerrainMesh`.** The worker pool
  has to be built with the classifier for whichever id space this session meshes,
  and that is the session owner's decision — the same rule `CorePlugin` follows
  for `WorldTime` and `LocalPlayerPlugin` for the local-player entity. It *does*
  insert a default `ChunkWorld`, so a harness installing only this plugin has a
  store to read.
- **`snapshot_section` and `snapshot_section_live` survive as thin wrappers**
  around `snapshot_section_in`, and only because their callers are outside this
  stage's editable set: `gpu.rs`'s hermetic mesh gates call the former,
  `tests/live_world_mesh.rs` calls the latter. Collapsing them to the one function
  is a follow-up, not a design decision.
- **Adding a read of the store? Take the guard into a `let` binding.**
  `WorldCollision::new(&store.read())` compiles nowhere useful — the guard is a
  temporary dropped at the end of the statement while the view still borrows it.
  Five call sites across `crates/lodestone-shell/src/sim/` bind
  `let world = store.read();` first — `actions.rs`, `camera.rs`, `meshing.rs`,
  `render_sources.rs` and `step.rs`. (This said "three call sites in `sim.rs`",
  which the seam extractions falsified twice over: the count grew and the file
  is now a directory. Grep the binding, not a remembered location.)
- **`heal_dirty_columns` runs only because `Sim::step` calls
  `run_schedule(Update)`.** That call is new in Stage 4 — `Sim`'s `World` had no
  `Update` systems before — and it is the one thing between this stage and being
  an island. `sim::tests::the_update_schedule_drains_the_dirty_column_set` pins
  it, asserting both that the set empties *and* that draining it submits real mesh
  jobs.

## What did **not** move, and why

- **The three bevy `World`s are still three** (the net thread's, the entity
  interpolator's, `Sim`'s). §4.1's `World` unification has two independent
  clauses and this stage did the chunk one, (d). The *bevy* one, (c), needs
  `Sim`'s `EcsHandle` threaded into the client at construction — i.e. through
  `NetClient::connect` in `crates/lodestone-shell/src/net.rs`, which is outside
  this stage's editable set. The reverse direction (adopting the client's handle
  the way `adopt_live_world` adopts its store) is **not** an alternative: `Sim`
  must work with no session at all, and `Sim.local`'s stability across
  `end_session` is documented as load-bearing, so a `World` that changes identity
  mid-session would invalidate the `Entity` the driver and plugins hold.
- **`CorePlugin` still refuses to insert `WorldTime`**, and that guard must stay
  for exactly as long as the bevy `World`s are more than one. Its purpose is to
  stop two `World`s becoming two diverging clocks; the chunk-store unification did
  not reduce the `World` count, so the guard is not obsolete. Retire it in the
  change that lands §4.1(c).
- **`PlayerSnapshot`'s vitals** are therefore still a duplicate of the driver's
  `Vitals` / `Xp` / `ServerEntityId` / `Dead`. That residue is bounded by §4.1(c),
  not by this stage — see [`session-components.md`](./session-components.md) for
  the measurement.
- **A world-space debug-geometry `Extract` channel** is not here.
  [`plugin-api.md`](./plugin-api.md) recommends folding it into Stage 4 or 5, and
  Stage 4 is indeed the natural home for the *resource* half now that a spatial
  store exists. The pipeline half does not: `gpu.rs` has a single-box outline
  pipeline and nothing general, and `gpu.rs` is outside this stage's editable set.
  Adding an `ExtractSet::Debug` label and a `Vec` resource with no pipeline behind
  them would be the tenth confirmed island, so it was deliberately not done.

## Configuration

None. No feature flags, no env vars. `DIRTY_COLUMN_BUDGET` (4) moved from
`sim.rs` to `mesher.rs` beside the system that consumes it.

## Dependencies

- `lodestone-ecs` → **`lodestone-world`** is new. wasm-safe: `lodestone-client`
  already depended on it and is in `scripts/wasm-check.sh`'s crate list. Still
  never a version crate.
- `lodestone-client` → unchanged set; `state.rs` inserts `ChunkWorld` **and**
  `ChunkWorldWrite` as resources sharing `SharedState.world`'s `Arc`, and
  `handle.rs` hands the pair out through `ClientHandle::chunk_world()` /
  `ClientHandle::chunk_world_write()`.
- `lodestone-shell` → unchanged set; `mesher.rs` gained `bevy_ecs` /
  `lodestone_ecs` imports and lost nothing.
