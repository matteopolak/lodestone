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
| invalidate on arrival | `Sim::on_column_arrived` → `TerrainMesh::mark_neighbours_dirty` → `dirty_columns` → `heal_dirty_columns` (budget `DIRTY_COLUMN_BUDGET = 4` columns/frame) | `ClientPacketListener.enableChunkLight` → `ClientLevel.setSectionRangeDirty(x-1, minSectionY, z-1, x+1, maxSectionY, z+1)` — the same 3×3 column footprint over the whole vertical range |
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
