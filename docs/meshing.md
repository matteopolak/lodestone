# Section meshing

## What it is

The shell terrain mesher turns immutable section neighbourhoods into packed face or baked block-model geometry. It keeps snapshot capture on the owning world thread and runs pure geometry work in the native worker pool or the bounded browser drain.

## How it works

`snapshot.rs` captures the 3×3×3 section neighbourhood, preserving the distinction between an unloaded streaming column and a complete world's true air edge. `face.rs` emits the packed full-cube path, while `model.rs` adapts baked block models, biome tinting, and visibility; `fluid.rs` emits water and lava separately. The parent `mesher` module owns the scheduler, result generations, dirty-column policy, and ECS integration, and re-exports the established public entry points.

## How to change it

Keep worker inputs limited to `SectionSnapshot` and treat the public functions re-exported by `lodestone_shell::mesher` as compatibility seams. Add model-view behaviour in `model.rs`, fluid-specific lookups in `fluid.rs`, packed face behaviour in `face.rs`, and neighbourhood/light capture in `snapshot.rs`. A new snapshot field must be copied through every constructor and remain `Send`; update geometry consumers if a new `SectionGeometry` variant or pass is introduced.

## Configuration

`MeshScheduler` receives the worker count and classifier. `MeshPolicy` controls dirty-column admission, while `cutout_leaves` and `blend_radius` are stamped onto each submitted job. `ColumnSource` controls whether missing columns defer a mesh, and `SkyDefault` controls absent sky-light fallback.

## Dependencies

The modules use `lodestone-world` snapshots and light data, `lodestone-render`'s packed/model/fluid mesh APIs, shell block classifiers and network state, and Bevy ECS for scheduler presentation systems. Native builds use `crossbeam-channel` and worker threads; `wasm32` uses the in-frame budgeted scheduler arm.
