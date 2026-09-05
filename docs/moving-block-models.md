# Moving block models

## What it is

The moving-block model pass renders baked block geometry at an entity or block-entity transform. It currently serves falling blocks, block displays, moving pistons, primed TNT, selected minecart contents, and item-frame blocks.

## How it works

`gpu/moving_blocks.rs` gathers each producer into a `MovingBlock` containing a validated `StateId`, a world transform, and packed light. `RenderState::merge_moving_block` passes that value to `CrackResolver::state_quads`, whose typed boundary indexes the resident model snapshot, then appends the transformed vertices to one frame mesh. The mining-crack overlay keeps its separate raw-id ingress because it resolves the current world target at frame time; it must validate or source-resolve before joining this typed moving-block seam.

Falling-block and block-display states originate as `BlockStateRef` values. Their ECS components (`FallingBlockState` and `DisplayBlockState`) and extracted draw records preserve whether an id came from the built-in 26.2 registry or a protocol-local/dynamic registry. `built_in_state_id` is the boundary to the built-in model table: it accepts only `Canonical` values that pass `lodestone_data::block_states::StateId::new`. A protocol-local value is skipped even when its raw number overlaps a built-in id, preventing an older or custom registry from drawing the wrong model.

Moving-piston records are a different ingress: their gather resolves state strings and currently stores the resulting raw values in `MovingPistonSpawn`. `merge_piston_heads` validates those values with `StateId::new` immediately before constructing `MovingBlock`; no raw piston value reaches the baked-quad snapshot, and an unresolved or future source can still be declined at that boundary.

## How to change it

Add a producer by constructing `MovingBlock` only after obtaining a `StateId`, or add a source-specific resolver that can turn a protocol-local reference into a canonical state with demonstrated equivalence. Do not unwrap or range-check `BlockStateRef::ProtocolLocal` as a built-in id. For a raw canonical-side source, validate with `StateId::new` at the producer boundary; the `MovingBlock` field and `CrackResolver::state_quads` do not accept raw integers. Keep the transform-specific tests next to the producer and extend the source-tag control if adding another network path.

## Configuration

There are no runtime flags. The generated block-state census bounds canonical ids; re-generating the data changes the accepted range.

## Dependencies

The pass depends on `lodestone-render` for baked quads and mesh construction, `lodestone-data` for canonical state validation, ECS extraction for entity/display input, and the active version adapter to tag network state ids.
