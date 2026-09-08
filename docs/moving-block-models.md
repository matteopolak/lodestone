# Moving block models

## What it is

The moving-block model pass renders baked block geometry at an entity or block-entity transform. It currently serves falling blocks, block displays, moving pistons, primed TNT, selected minecart contents, and item-frame blocks.

## How it works

`gpu/moving_blocks.rs` gathers each producer into a `MovingBlock` containing a validated `StateId`, a world transform, and packed light. `RenderState::merge_moving_block` passes that value to `CrackResolver::state_quads`, whose typed boundary indexes the resident model snapshot, then appends the transformed vertices to one frame mesh. The mining-crack overlay resolves the current world target at frame time, validates the raw read once at `Sim::crack_target`/`Sim::crack_targets`, and carries the resulting `StateId` through `CrackTarget` into `CrackResolver::mesh_for`.

Falling-block and block-display states originate as `BlockStateRef` values. Their ECS components (`FallingBlockState` and `DisplayBlockState`) and extracted draw records preserve whether an id came from the built-in 26.2 registry or a protocol-local/dynamic registry. `built_in_state_id` is the boundary to the built-in model table: it accepts only `Canonical` values that pass `lodestone_data::block_states::StateId::new`. A protocol-local value is skipped even when its raw number overlaps a built-in id, preventing an older or custom registry from drawing the wrong model.

Moving-piston records are a different ingress: their gather resolves state strings and currently stores the resulting raw values in `MovingPistonSpawn`. `merge_piston_heads` validates those values with `StateId::new` immediately before constructing `MovingBlock`; no raw piston value reaches the baked-quad snapshot, and an unresolved or future source can still be declined at that boundary.

Primed TNT keeps its block state fixed to the default TNT model, but its fuse is not fixed. The protocol adapter type-gates the ambiguous index-8 integer as `tnt_fuse`, ECS folds it into `TntFuse`, and `extract_entity_draws` adjusts it by the frame partial tick. `primed_tnt_pose` applies the final-ten-tick fourth-power swell (`0` at fuse `10`, `0.3` at `0`), while the model vertex's dedicated white-flash marker applies the fixed white-overlay blend in alternating five-tick windows. Until the first fuse metadata packet arrives, extraction preserves `None`; the merge renders that short startup window at scale one with no flash instead of treating the accessor default (`80`) as a lit cadence. The blend retains `63/255` of the textured material before world shading; it is not opaque white. This path intentionally shares the ordinary model pipeline, so a flash does not allocate a second mesh or draw call.

## How to change it

Add a producer by constructing `MovingBlock` only after obtaining a `StateId`, or add a source-specific resolver that can turn a protocol-local reference into a canonical state with demonstrated equivalence. Do not unwrap or range-check `BlockStateRef::ProtocolLocal` as a built-in id. For a raw canonical-side source, validate with `StateId::new` at the producer boundary; the `MovingBlock` field and `CrackResolver::state_quads` do not accept raw integers. Keep the transform-specific tests next to the producer and extend the source-tag control if adding another network path.

When changing TNT rendering, preserve the type gate at metadata decode: index 8 is also used by several unrelated entity families. Keep the end-to-end draw witnesses at early (`80`/`75`), middle (`5`), and final (`0`) fuse timing, plus the missing-metadata control. The production moving-block merge must consume one fuse-derived visual state for both scale and the shader marker; a generic integer test cannot detect a class mix-up, a cadence reversal, or the startup frame that accidentally stays white.

## Configuration

There are no runtime flags. The generated block-state census bounds canonical ids; re-generating the data changes the accepted range.

## Dependencies

The pass depends on `lodestone-render` for baked quads and mesh construction, `lodestone-data` for canonical state validation, ECS extraction for entity/display input, and the active version adapter to tag network state ids.
