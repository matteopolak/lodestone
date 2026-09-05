# Moving block models

## What it is

The moving-block model pass renders baked block geometry at an entity or block-entity transform. It currently serves falling blocks, block displays, moving pistons, primed TNT, selected minecart contents, and item-frame blocks.

## How it works

`gpu/moving_blocks.rs` gathers each producer into a `MovingBlock` containing a canonical state id, a world transform, and packed light. `RenderState::merge_moving_block` reads baked quads from the resident model snapshot and appends their transformed vertices to one frame mesh.

Falling-block and block-display states originate as `BlockStateRef` values. Their ECS components (`FallingBlockState` and `DisplayBlockState`) and extracted draw records preserve whether an id came from the built-in 26.2 registry or a protocol-local/dynamic registry. `built_in_state_id` is the boundary to the built-in model table: it accepts only `Canonical` values that pass `lodestone_data::block_states::StateId::new`. A protocol-local value is skipped even when its raw number overlaps a built-in id, preventing an older or custom registry from drawing the wrong model.

## How to change it

Add a producer by constructing `MovingBlock` only after obtaining a validated built-in state, or add a source-specific resolver that can turn a protocol-local reference into a canonical state with demonstrated equivalence. Do not unwrap or range-check `BlockStateRef::ProtocolLocal` as a built-in id. Keep the transform-specific tests next to the producer and extend the source-tag control if adding another network path.

## Configuration

There are no runtime flags. The generated block-state census bounds canonical ids; re-generating the data changes the accepted range.

## Dependencies

The pass depends on `lodestone-render` for baked quads and mesh construction, `lodestone-data` for canonical state validation, ECS extraction for entity/display input, and the active version adapter to tag network state ids.
