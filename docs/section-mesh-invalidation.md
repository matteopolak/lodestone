# Section mesh invalidation: absent vs. not-yet-loaded neighbours

## What it is

A section's mesh is a function of its whole **3×3×3 = 27-section neighbourhood**,
not just the section itself: face culling reads the six orthogonal neighbours,
ambient occlusion and the smooth-light corner blend read the edges *and* corners,
and fluid corner heights and flow vectors read a 3×3 ring. So the mesher has to
answer, for every slot of that neighbourhood, *what is there?* — and when the
answer is "nothing", it has to answer a second question that used to be
conflated with the first: **is nothing the truth, or have I just not been told
yet?**

This doc covers that distinction, the two mechanisms that keep meshes honest as
chunks stream in, and how they map onto vanilla. It is the "how a chunk-load
invalidates geometry" companion to
[chunk world resource](./chunk-world-resource.md) (which owns the ECS shape of
terrain meshing) and [fluid rendering](./fluid-rendering.md) (which owns *which
face* a fluid emits, given a neighbourhood).

Everything here lives in `crates/lodestone-shell/src/mesher.rs`.

## The defect this closed (issue #389)

**Reported from play:** distant water is visibly blocky along chunk boundaries —
"you can see where the chunks are" — and it corrects itself as the player
approaches.

The chain:

1. `snapshot_section_in` filled every absent neighbourhood slot with a shared
   **all-air** section. Correct at the edge of the world and at an elided all-air
   section; wrong for a column that simply had not arrived.
2. Air does not occlude and is not water, so a section meshed while its east
   neighbour was in flight emitted its whole east seam: 16×16 full-height
   translucent side faces, each **double-sided** (`FluidRenderer.addFace`'s
   reversed-winding copy — see `docs/fluid-rendering.md`), and with the surface's
   corner heights collapsed at the seam because a non-fluid neighbour drags the
   weighted average down.
3. The neighbour then did the same thing from its side. Two coincident
   translucent walls per seam: doubled alpha, a visible grid, and **no
   z-fighting** to make it read as a depth bug.
4. "It fixes itself when I get close" is the tell. Approaching loads the ring
   beyond, which re-meshes the seam correctly.

The mechanism is not water-specific. Because the neighbourhood is 27 and not 6,
the same stale snapshot also produced **wrong cross-chunk ambient occlusion and
wrong smooth-light corners** along every seam it touched. Water was merely the
loudest symptom, because a translucent wall composites and a slightly-wrong AO
corner does not.

**What was *already* right, and is worth knowing before touching this.** The
dirty-propagation half was in place and wired: `Sim::on_column_arrived` meshes
the arriving column and calls `TerrainMesh::mark_neighbours_dirty`, which
coalesces its eight loaded horizontal neighbours into `dirty_columns` for
`heal_dirty_columns` to drain on a budget. So an *interior* seam did heal, within
a frame or two of the neighbour landing. What had no mechanism at all was the
**frontier**: the outermost ring of loaded columns has a permanently absent
neighbour, nothing will ever arrive to invalidate it, and it is by definition
distant. That is exactly the reported symptom.

## How it works

### Two types, because there are two facts

```rust
pub enum ColumnSource { Complete, Streaming }
```

Whether an absent *column* is the edge of the world or a chunk in flight. Nothing
downstream can derive this — an empty slot looks identical either way — so it is
a parameter. `Complete` is the offline demo world (`worldgen::generate` emits its
whole radius up front) and hermetic fixtures; `Streaming` is a live session.

```rust
pub enum Neighbour { Present(Arc<ChunkSection>), Air, Unloaded }
```

Per slot, and this is the part the issue asked for in the *type* rather than by
convention. `Air` means air is the **truth**: above the build ceiling, below the
bedrock floor, an all-air section elided inside a column that *has* arrived, or
any absent column in a `Complete` world. `Unloaded` means air is a **guess**.
Both resolve to the same shared all-air section when the mesher reads them
(`Neighbour::section`), so nothing downstream changed; what changed is that the
snapshot now knows the difference.

```rust
pub enum SnapshotOutcome { Ready(SectionSnapshot), Empty, Deferred(SectionSnapshot) }
```

`snapshot_section_in` returns `Deferred` when any slot is `Unloaded`. Note the
vertical boundary never defers: `si == -1` and `si == section_count` are `Air`,
which is why a column at the bottom of the world is not held back forever.

### Two mechanisms, and vanilla does both

| | ours | vanilla 26.2 |
|---|---|---|
| invalidate on arrival | `Sim::on_column_arrived` → `TerrainMesh::mark_neighbours_dirty` → `dirty_columns` → `heal_dirty_columns` (budget `DIRTY_COLUMN_BUDGET` columns/frame — 64 today; read the constant, several places in this doc's history quoted the old 4) | `ClientPacketListener.enableChunkLight` → `ClientLevel.setSectionRangeDirty(x-1, minSectionY, z-1, x+1, maxSectionY, z+1)` — the same 3×3 column footprint over the whole vertical range |
| defer the **first** build | `SnapshotOutcome::Deferred` + `TerrainMesh::route`'s `uploaded_sections` check | `LevelExtractor.extract`: compile when `section.sectionMesh.get() != CompiledSectionMesh.UNCOMPILED \|\| sectionUpdateTracker.hasAllNeighbors(level, node)`; `SectionUpdateTracker.hasAllNeighbors` checks the four orthogonal **and** four diagonal neighbour columns |

`TerrainMesh::route` is where the second row lands:

* `Ready` → submit.
* `Empty` → queue a GPU removal (nothing to draw).
* `Deferred` **and already in `uploaded_sections`** → submit anyway. This is
  vanilla's `!= UNCOMPILED` clause and it is not optional: without it, a chunk
  unloading at the far edge of the view would blink out the ring beside it, and a
  block edit next to the frontier would not show.
* `Deferred` and never uploaded → hold back, bump `TerrainMesh::deferred`, and
  **do not** queue a removal (there is nothing on the GPU, and a removal would
  make the wait look like an unload).

The deferral cannot become permanent for an interior section, because
`mark_neighbours_dirty` re-drives it the moment the missing column lands. That is
why the two mechanisms are one design and not two alternatives.

### Three facts, not two — the departed column (issue #479)

**The paragraph directly above was true for an interior section and false for a
trailing-edge one, and that gap was issue #479.** "The moment the missing column
lands" assumes the missing column is going to land. For a column on the trailing
edge of a moving view the column it waits on *already came and went*, so the
deferral is permanent and nothing re-queues it. The player walks away from spawn
and new chunks stop being drawn while their collision — which re-reads the store
every tick and needs only the player's own column — is perfectly present.

So `ColumnSource`'s two facts were not enough. An absent column is one of **three**
things, and only the first two were representable:

| absent because | air is | mechanism |
|---|---|---|
| the world ends there (`Complete`) | the truth | `Neighbour::Air`, `Ready` |
| it has not arrived yet (`Streaming`) | a guess | `Neighbour::Unloaded`, `Deferred`, re-driven on arrival |
| **it left the view** | the truth, and nothing will ever say otherwise | `TerrainMesh::departed` + `forced_columns`, forced re-mesh |

The third is learned from the unload signal rather than from the store, because
the store cannot tell it from the second — both are simply absent. Vanilla does
not need this: its client tracks the view rectangle and can ask directly whether a
column is outside it.

Two pieces:

* `TerrainMesh::forget_column` drops the departing column's own GPU sections and
  records it in `departed`. `force_neighbours_of_departed` queues its **loaded**
  neighbours into `forced_columns`.
* `heal_dirty_columns` drains `forced_columns` first, on its own
  `DIRTY_COLUMN_BUDGET`, through `mesh_column_forced` — which forces `route` only
  when `all_absent_neighbours_departed` agrees.

**That predicate is the load-bearing part, and the harness caught its absence.**
Forcing every loaded neighbour of a departing column also drags in the *outermost*
ring of the view — the buffer ring singleplayer streams `render_distance + 1` to
keep off screen — and a section meshed without its outer neighbour bakes its seam
against air, which is the "blocky water far away" report `app/session.rs` already
documents. Measured: 32 columns resident against 30 expected, before the predicate
went in.

`departed` is bounded rather than a second leak: an entry is dropped once it has no
loaded neighbour, at which point it cannot influence any decision.

### Why this was latency-coupled, and why that made it look like starvation

Whether a given column was lost was a race between the 4-columns-per-frame heal
budget and the player's speed. A column the budget reached *while the column behind
it was still loaded* uploaded and was thereafter exempt via the `uploaded_sections`
clause; one it reached later was dropped forever. That is why the symptom worsened
with distance and with falling frame rate, and why it was initially indistinguishable
from mesh workers merely being starved by a `Can't keep up! 99 ticks behind`
integrated server.

`standing_still_drains_the_heal_backlog_and_no_column_is_lost` separates them, and
does it with **counts of frames and columns rather than any duration** — a
millisecond figure on a shared machine gets attributed to the wrong cause:

| measurement | value |
|---|---|
| peak heal backlog over a 12-step walk | 45 columns |
| frames to drain it standing still | 12 |
| resident columns before the fix | 25 of 30 |
| resident columns after | 30 of 30 |

The backlog draining is what rules out a stuck queue. The test models mesh workers
as **infinitely fast** (they are drained to completion every frame), so worker
throughput cannot influence the outcome and what remains under test is purely
scheduling — which makes 25 of 30 a scheduling defect and not starvation. The
integrated server's tick lag is a real, separate throughput problem; it is the
*trigger* that made this reachable in ordinary play, not the cause.

### The order the budget is spent in — near the player, then in front of them

`DIRTY_COLUMN_BUDGET` is finite, so `dirty_columns` is not just a set: whatever
does not fit waits a frame, and the queue therefore *chooses* which part of the
world appears first. It was a `BTreeSet<(i32, i32)>` drained with `pop_first()`,
which is **lexicographic** — smallest `cx`, then smallest `cz`. That is a corner
of the world, not a place the player is, so a backlog was worked from `−x/−z`
outward no matter where the camera pointed.

That mattered the moment the server started streaming **view-first**
(`crates/lodestone-server/src/join_scheduler.rs`, and
[join scheduler](./join-scheduler.md)): the server's ordering reached no pixels,
because the client re-sorted it into corner order on arrival. Reported from play
as chunks appearing behind you while you stare at a hole.

`DirtyColumns` (in `mesher.rs`) keys the drain on the **same shape** as the
server's `view_order_key`:

```text
(Chebyshev distance from the player's column, in-frustum penalty, cx, cz)
```

* **Distance is primary, and that is the anti-starvation property.** A column at
  distance `d` *behind* the player still precedes every column at distance
  `d + 1`, in view or not. Pure frustum-first lets a slow spin starve what is
  behind you, and then you turn round into a hole.
* **The penalty is `0` inside a 120° horizontal cone, `1` outside**, so it only
  reorders *within* one distance band. 60° half-angle, generous against vanilla's
  ~106° horizontal at 16:9, and the player's own column plus its eight neighbours
  always count as in view.
* Re-keying happens in `heal_dirty_columns`, once per frame, from the local
  player's live `PhysicsState` (`position`, `yaw` — the values mouse-look writes
  each frame). `DirtyColumns::reprioritise` is a **no-op unless the player's
  column or one of 16 quantised yaw sectors changed**, so a typical frame costs a
  comparison, and a rebuild is ≤ 4,225 integer keys at the largest view.

The constants are a deliberate second copy of the server's, not an import: the
shell must not depend on `lodestone-server` (the version seam — see
`just check-seam`), and singleplayer is the only build where both crates exist.
What is shared is the semantics, stated in both places.

**Structure, and the property that had to survive the change.** A `BTreeSet`
gave "a column dirtied twice is meshed once" for free. The replacement is a
`BinaryHeap` of keys plus a `HashSet` of membership, where the *set* is the truth:
`insert` only pushes a key when the set did not already hold the coordinate, and
`pop_next` skips any popped key whose coordinate is no longer live. `remove` (a
column left the view) drops from the set and leaves the heap entry as a
**tombstone**, compacted when the heap exceeds twice the live count — a heap has
no cheap arbitrary erase and this is a queue, so paying for one on every unload
would be the wrong trade. A sorted `Vec` was the alternative and loses on insert
cost: every chunk arrival dirties up to eight columns.

While keying this, the pre-existing backlog warning turned out to be
**unreachable**: it sat inside the `let … else` arm of the pop, which only runs
when the queue is *empty*, and then tested `!is_empty()`. It is now after the
loop, where "budget depleted with work still queued" is actually true.

### Why deferring the frontier costs nothing

Vanilla's server sends **`viewDistance + 1`** columns in each direction
(`ChunkTrackingView.maxX/maxZ` = `center + viewDistance + 1`), precisely so that
every column inside the render distance has all eight neighbours and passes
`hasAllNeighbors`. The extra ring exists to be a neighbour, not to be drawn. So
our deferral lands on the same ring vanilla also does not draw.

### Where `ColumnSource` comes from

`MeshScheduler::new` derives it from the `ShellClassifier` it is handed:
`is_vanilla()` → `Streaming`, otherwise `Complete`. `TerrainMesh::new` copies it
off the scheduler.

That is a derivation, not a coincidence, and the invariant is stated in two
places already: `ShellClassifier::is_vanilla`'s own docs ("the session meshes the
live world only under this variant and the demo world only under `Demo`") and
`Sim::build`'s `debug_assert!(!(demo_world && render_live))`. Deriving it keeps
the id space and the column provenance from ever being set inconsistently, which
is the failure this would otherwise invite in both directions: a `Streaming` demo
world blanks its outer ring forever, a `Complete` live world is #389 unfixed.

The one non-obvious case is the fallback session — vanilla assets failed to load,
so a live connection meshes under `Demo`. `Complete` is still right there,
because `MeshPolicy::id_spaces_agree` is `false` and `mesh_column` meshes nothing
at all.

## How to change it, and the gotchas

- **Adding a reason a slot can be empty** → add a `Neighbour` variant, not a
  convention. The whole point of the type is that `Option` could not carry the
  distinction and so nobody noticed it was missing.
- **A section renders nothing where terrain should be** → check
  `TerrainMesh::deferred`. A count that keeps climbing while nothing is loading
  means `mark_neighbours_dirty` has stopped re-driving deferred sections, and the
  symptom is terrain *missing* rather than terrain *wrong* — the opposite failure
  direction from #389, and the price of the fix if the invalidation half breaks.
- **Gotcha: a fully-deferred column is not a drop.** `mesh_column` counts
  `TerrainMesh::drops` and warns only when a loaded column produced neither
  geometry nor a deferral. Counting the frontier would drown the "invisible
  blocks" alarm in noise on every join.
- **Gotcha: there are two meshers, and only one of them has fluids.**
  `--headless` and the demo world render through `mesh_simple`, which has no
  fluid path at all; live terrain goes through `mesh_models` + `mesh_fluids`.
  Anything asserted about water has to run through `mesh_snapshot_fluids`. This
  is why the #389 gate is in two halves (see Tests).
- **Gotcha: the demo palette makes every non-air block occlude, water included.**
  So a demo-world fixture measures the *snapshot and culling* seam faithfully and
  says nothing at all about water as water.
- **Gotcha: `snapshot_section` is deliberately `Complete`.** `crate::gpu`'s
  hermetic mesh gates and the offline world call it, and for both the world
  really is complete, so it keeps returning `Option<SectionSnapshot>`. If you
  point it at a streaming world you will silently re-open #389.

## Configuration

- `DIRTY_COLUMN_BUDGET` (`mesher.rs`) — columns re-meshed per frame by
  `heal_dirty_columns`. Bounds a chunk-load burst; a spiral load coalesces
  several arrivals naming the same column into one re-mesh.
- No env vars. The jar-backed gate needs `client.jar` +
  `generated/reports/blocks.json` under `.cache/mc/<version>/` and fails closed
  rather than skipping.

## Dependencies

- `lodestone-world` — `World::get` / `ChunkColumn::section_arc` (the `Arc`
  copy-on-write seam the snapshot reads through), `ColumnLight`.
- `lodestone-ecs` — `ChunkWorld` (the one store), `WorldExtent`.
- `lodestone-render` — `mesh_simple` / `mesh_models` / `mesh_fluids`,
  `BlockModels`, `SkyDefault`.

## Tests

Hermetic (`cargo test -p lodestone-shell --lib mesher`):

- `the_seam_fixture_really_has_water_on_both_sides` — the *world*-species
  guard, asserted against the fixture's own data: 128 water-against-water cells
  and 128 water-against-air across the seam. A fixture without that cannot
  exercise this bug at all.
- `a_seam_meshed_without_its_neighbour_converges_on_the_neighbour_present_answer`
  — both halves. The stale count **drops** (256 → 128) *and* equals the
  from-the-start count exactly. The second half is the load-bearing one: "it
  changed" would also be satisfied by converging on something else wrong. The
  east-boundary **bounding box** is asserted too (`z 0..8, y 0..16`), not just the
  count — the first draft of that assertion had the two halves the wrong way
  round and only the printed box said so.
- `an_absent_neighbour_column_defers_the_build_and_is_typed_as_unloaded` — the
  outcome is `Deferred`, `ready()` refuses it, and `unloaded_neighbours()` is
  exactly `3`: the three `dy` slots of the one absent column, and **not** the
  vertical/elided ones, which are genuinely air.
- `control_a_complete_world_never_defers_an_absent_neighbour` — the demo
  world's rim must keep rendering.
- `control_a_seamless_fixture_shows_no_convergence` — the same three
  measurements with a *present but empty* neighbour: 256 → 256. The world species
  made to fire.
- `the_mesh_drain_prefers_near_and_in_front_but_never_starves_what_is_behind` —
  the drain order, as two claims rather than a fixed sequence: distance is
  monotone over a whole 11×11 window (so `(0, −3)` behind the player precedes
  `(0, 4)` in front of them — the assertion a pure frustum-first drain fails), and
  within ring 5 the in-frustum columns form the **prefix** (so the facing bonus is
  not inert, which is what stops the first claim being satisfied by an ordering
  that ignores rotation entirely). Also that a column dirtied *after* the keying
  joins at its priority rather than at the end.
- `a_column_dirtied_twice_is_meshed_once_and_an_unloaded_one_not_at_all` — the two
  properties the `BTreeSet` gave for free: dedup, and a removed column's tombstone
  not resurfacing as a phantom pop.
- `rekeying_only_fires_when_the_column_or_the_yaw_sector_moves` — the cheap-path
  gate: identical pose and a 10°-of-22.5° nudge do not rebuild; a quarter turn and
  a chunk-boundary crossing do.

Jar-backed, `#[ignore]`d
(`cargo test -p lodestone-shell --test water_seam_convergence -- --ignored --nocapture`):

- `crates/lodestone-shell/tests/water_seam_convergence.rs` — the same
  convergence through the **real** fluid path: real `BlockModels` from
  `client.jar`, real `minecraft:water[level=0]` state ids, real
  `mesh_snapshot_fluids`. Measured 512 → 256 water quads on the seam (each side
  face double-sided), converging on the from-the-start answer, with
  `control_an_absent_neighbour_defers_rather_than_meshing` and
  `control_a_seamless_fixture_shows_no_convergence` beside it.

### The controls were watched failing — twice, at two break depths

Two independent neuters were applied and run, because a control that only catches
the *coarse* break is not evidence it catches a subtle regression.

| break | what it removes | result |
|---|---|---|
| `snapshot_section_in`'s `awaiting` forced to `false` | the whole detection — pre-#389 behaviour, verbatim | 16/17 lib, 4/5 gate: `an_absent_neighbour_column_defers_the_build_and_is_typed_as_unloaded` and `control_an_absent_neighbour_defers_rather_than_meshing` FAIL |
| `SnapshotOutcome::ready()` also returns `Deferred`'s snapshot | detection kept, **enforcement** removed — the subtler regression, since the outcome still *says* `Deferred` | same two tests FAIL (on their `ready().is_none()` assertions) |

The *convergence* gates keep passing under both breaks, and that is by design and
worth stating plainly: convergence is a property of the mesher's culling, not of
the deferral. A suite containing only the convergence half would have passed with
the fix fully reverted, which is exactly why the deferral controls exist and why
`ready()`/`any()` are two separate accessors rather than one.

### What was *not* run when this landed

The session that wrote this was told to stop using cargo mid-verification (a
build hung compiling `aws-lc-sys` and `target/` was being deleted), so the
following are **owed** rather than green:

- `cargo test -p lodestone-shell --doc` — started, killed before it reported. No
  doctest in the tree names any symbol this change touched (grepped:
  `snapshot_section*`, `mesh_snapshot*`, `SectionSnapshot`, `SnapshotOutcome`,
  `ColumnSource` — every hit is ordinary code or `docs/*.md` prose, and the shell
  crate has no `rust` doc fences), so the expectation is that it is clean, but
  per `CLAUDE.md` **no `cargo check` sees a doctest** and that expectation is
  reasoning, not a measurement.
- `cargo test -p lodestone-shell --no-fail-fast` to completion. It got through
  the lib target (**556 passed, 0 failed**) and 20 further `ok` results before
  being cut off partway through the `#[ignore]`d pixel-gate binaries. Those
  binaries all *compile* under `cargo check --workspace --all-targets`, which was
  green, and they are `#[ignore]`d, so nothing was skipped that would have run.
- No live oracle and no GPU gate. `scripts/live-oracles/terrain.sh` (:25580)
  against a real ocean is the obvious next confirmation and has not been done —
  the seam is argued from the two hermetic/jar-backed gates and from vanilla's
  source, not from a screenshot.

What *was* green before the stop: `cargo check --workspace --all-targets`; the
same with `--all-features --exclude lodestone-allocbench`; `cargo check -p
lodestone-shell --no-default-features`; `cargo test -p lodestone-shell --lib`
(556/556); and the jar-backed gate 5/5 under `--ignored`.
