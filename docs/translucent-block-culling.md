# Translucent block culling

## What it is

Why an interior face between two identical translucent blocks (ice, glass,
stained glass, honey, slime) is not drawn, and why only the camera-facing side
of a solid translucent cube's face pair renders. Two independent rules, both
missing until #637: `Block.shouldRenderFace`'s `skipRendering` clause (the
"grid" bug) and vanilla's ordinary back-face culling on the translucent
render pipeline (the "no opacity from above" bug).

## How it works

Vanilla's `Block.shouldRenderFace(state, neighborState, direction)` has two
independent early-outs before it ever renders a face:

1. `neighborState.getFaceOcclusionShape(..) == Shapes.block()` — the
   neighbour's shape fully occludes. Ported as
   `ModelSectionView::occludes_at` (`crates/lodestone-render/src/models.rs`),
   backed by `BlockModels::occludes`. Correctly `false` for ice/glass/etc. —
   they are vanilla `noOcclusion()` blocks — so this clause alone can never
   cull the interior faces of a wall of the same translucent block.
2. `state.skipRendering(neighborState, direction)` —
   `HalfTransparentBlock.skipRendering` returns `neighborState.is(this)`: cull
   the face when the neighbour is the **exact same** `Block`. Ported as
   `ModelSectionView::skips_rendering_against` and
   `BlockModels::skips_rendering_against`, keyed on
   `StateModel::half_transparent_class` — the same real, decompile-sourced
   `FLUID_OVERLAY_HALF_TRANSPARENT_BLOCKS` list (`block_models.rs`) the
   fluid-overlay check already uses (glass, every stained-glass colour,
   tinted glass, ice, blue ice, frosted ice, honey, slime). Keyed on identity
   rather than "is this class", so `ice`/`blue_ice`/`frosted_ice` — three
   different vanilla `Block` instances — do not cull against each other.

Both clauses are consulted in `mesh_models_layers`'s cullface check
(`models.rs`), and `crates/lodestone-shell/src/mesher.rs`'s
`SnapshotModelView` is the only production implementor of clause 2 today.

Separately, `ModelPipeline::for_layer` (`model_pipeline.rs`) now keeps
back-face culling **on** for `RenderLayer::Translucent`, matching vanilla:
`RenderPipelines.TRANSLUCENT_TERRAIN`/`TRANSLUCENT_BLOCK` build on
`TERRAIN_SNIPPET`/`BLOCK_SNIPPET`, and neither those nor their translucent
variants call `.withCull(false)` — `RenderPipeline.Builder`'s own default is
`cull.orElse(true)`. With culling disabled (the pre-#637 state), a solid
cube's far face drew too, double-compositing the same partial alpha along any
view ray through the block — markedly more opaque than a single correct
blend.

## How to change it

- Same-block skip: add a class to `FLUID_OVERLAY_HALF_TRANSPARENT_BLOCKS` in
  `block_models.rs` if vanilla adds a new `HalfTransparentBlock` subclass —
  `skips_rendering_against` picks it up automatically, no mesher change
  needed.
- Back-face culling: `ModelPipeline::build`'s `cull_back_face` parameter is
  now independent of `translucent` (blend/depth-write). Do not re-couple
  them — that coupling was the bug.
- `LeavesBlock`'s own same-neighbour clause
  (`!cutoutLeaves && neighborState.getBlock() instanceof LeavesBlock`) is a
  **different** vanilla rule and is not implemented by
  `skips_rendering_against`.

## Configuration

None — both rules are unconditional (no `cfg`, no runtime option).

## Dependencies

- `crates/lodestone-render/src/block_models.rs` (`FLUID_OVERLAY_HALF_TRANSPARENT_BLOCKS`,
  `StateModel::half_transparent_class`, `BlockModels::skips_rendering_against`)
- `crates/lodestone-render/src/models.rs` (`ModelSectionView::skips_rendering_against`,
  `mesh_models_layers`)
- `crates/lodestone-render/src/model_pipeline.rs` (`ModelPipeline::build`'s
  `cull_back_face`)
- `crates/lodestone-shell/src/mesher.rs` (`SnapshotModelView`, the live
  production implementor)
- Gates: `crates/lodestone-render/tests/half_transparent_interior_cull_gate.rs`
  (hermetic), `crates/lodestone-render/tests/translucent_model_backface_cull_gate.rs`
  (GPU, `#[ignore]`d)

`ModelPipeline::for_fluid` (water) has the same pre-fix `cull_mode: None` and
was deliberately left unchanged — unaudited here, and out of scope for the
ice report this fix closes.
