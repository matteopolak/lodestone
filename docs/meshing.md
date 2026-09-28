# Section meshing

## What it is

The shell terrain mesher turns immutable section neighbourhoods into packed face or baked block-model geometry. It keeps snapshot capture on the owning world thread and runs pure geometry work in the native worker pool or the bounded browser drain.

## How it works

`snapshot.rs` captures the 3×3×3 section neighbourhood, preserving the distinction between an unloaded streaming column and a complete world's true air edge. Newly arriving streaming columns enter `pending_arrivals`, which is separate from the ready `dirty_columns` queue. A first build is admitted against air when its halo is incomplete, so exposed faces at the current stream frontier are visible immediately; a later neighbour arrival invalidates that provisional boundary through the ordinary remesh path. Standalone light patches carry changed light-section indices. Their remesh queue includes a section with blocks only when those blocks can sample the patched section: the center always qualifies, while adjacent sections need blocks on the shared face, edge, or corner. Air-only and interior-only adjacent sections need no replacement upload. Light patches do not reopen column admission. `face.rs` emits the packed full-cube path, while `model.rs` adapts baked block models, biome tinting, and visibility; `fluid.rs` emits water and lava separately. The parent `mesher` module owns the scheduler, result generations, dirty-column policy, and ECS integration, and re-exports the established public entry points.

## How to change it

Keep worker inputs limited to `SectionSnapshot` and treat the public functions re-exported by `lodestone_shell::mesher` as compatibility seams. Add model-view behaviour in `model.rs`, fluid-specific lookups in `fluid.rs`, packed face behaviour in `face.rs`, and neighbourhood/light capture in `snapshot.rs`. A new snapshot field must be copied through every constructor and remain `Send`; update geometry consumers if a new `SectionGeometry` variant or pass is introduced. If the light-patch admission rule changes, preserve remeshing of loaded non-air neighbours, including diagonal and vertically adjacent sections whose boundary blocks sample the changed section. Keep the boundary predicate conservative for non-cube models and fluids.

## Configuration

`MeshScheduler` receives the worker count and classifier. `MeshPolicy` controls dirty-column admission, while `cutout_leaves` and `blend_radius` are stamped onto each submitted job. `ColumnSource` controls whether missing columns defer a mesh; deferred snapshots are still submitted for a first frontier build and are re-meshed when the missing column arrives. `SkyDefault` controls absent sky-light fallback. `MESH_SNAPSHOT_SECTION_BUDGET` bounds frame snapshot work by sections rather than columns. Native result handoff targets 2 ms of observed upload work, using an exponentially weighted per-section cost from redraw, with a 96-result ceiling and a 16 MiB geometry-payload ceiling. At startup it estimates 50 μs per result until redraw measurements arrive. Overflow stays queued in completion order; the first result is always allowed even if it alone exceeds the byte ceiling. The byte ceiling limits burst size, while the adaptive count lets inexpensive sections stream faster than a small fixed count without letting consistently expensive uploads monopolize redraw. The browser continues to use its 4 ms in-frame meshing budget. The renderer can acknowledge the GPU hand-off with `Sim::mark_mesh_uploaded`, which is the readiness boundary rather than CPU scheduler completion.

## Dependencies

The modules use `lodestone-world` snapshots and light data, `lodestone-render`'s packed/model/fluid mesh APIs, shell block classifiers and network state, and Bevy ECS for scheduler presentation systems. Native builds use `crossbeam-channel` and worker threads; `wasm32` uses the in-frame budgeted scheduler arm.
