# Moving block models

## What it is

The moving-block model pass renders baked block geometry at an entity or block-entity transform. It serves falling blocks, block displays, moving pistons, primed TNT, selected minecart contents and item-frame blocks.

## How it works

- `gpu/moving_blocks.rs` gathers each producer into a `MovingBlock` (validated `StateId`, world transform, packed light). `RenderState::merge_moving_block` passes it to `CrackResolver::state_quads`, whose typed boundary indexes the resident model snapshot, and appends transformed vertices to one frame mesh. The mining-crack overlay resolves the world target at frame time, validates the raw read once at `Sim::crack_target`/`crack_targets` and carries the `StateId` through `CrackTarget` into `CrackResolver::mesh_for`.
- Falling-block and block-display states start as `BlockStateRef` values; their ECS components (`FallingBlockState`, `DisplayBlockState`) and draw records keep whether an id is from the built-in 26.2 registry or a protocol-local/dynamic one. `built_in_state_id` is the boundary to the built-in model table and accepts only `Canonical` values passing `lodestone_data::block_states::StateId::new`; a protocol-local value is skipped even if its raw number overlaps a built-in id.
- Moving pistons enter through NBT: the compound resolves to a `StateId`, and typed property replacement builds the short head and retracting base states without state text. `MovingPistonSpawn` becomes a raw numeric field only at the render-crate boundary, where `merge_piston_heads` validates it. Minecart contents and fixed TNT models start from generated `Block` defaults and typed properties, so no per-frame path parses names.
- **Primed TNT** keeps a fixed default TNT state but a live fuse: the adapter type-gates the ambiguous index-8 integer as `tnt_fuse`, ECS folds it into `TntFuse`, and `extract_entity_draws` adds the frame partial tick. `primed_tnt_pose` applies the final-ten-tick fourth-power swell (`0` at fuse 10, `0.3` at 0) while the vertex's white-flash marker blends white in alternating five-tick windows, retaining `63/255` of the textured material before world shading (not opaque white). Until the first fuse packet arrives extraction keeps `None` and the merge draws scale one with no flash, not the accessor default (`80`). The flash shares the ordinary model pipeline, adding no mesh or draw call.

## How to change it

- A new producer builds `MovingBlock` only from a `StateId`, or through a resolver that turns a protocol-local reference into a canonical state with demonstrated equivalence. Never unwrap or range-check `BlockStateRef::ProtocolLocal` as built-in. Validate raw canonical sources with `StateId::new` at the producer; `MovingBlock` and `CrackResolver::state_quads` take no raw integers. Keep transform tests beside the producer and extend the source-tag control for a new network path.
- For TNT, keep the type gate at metadata decode (index 8 is shared by unrelated entity families) and the end-to-end draw witnesses at early (`80`/`75`), middle (`5`) and final (`0`) fuse plus the missing-metadata control. The merge must take one fuse-derived visual state for both scale and the shader marker.

## Configuration

None. The generated block-state census bounds canonical ids; regenerating data changes the accepted range.

## Dependencies

`lodestone-render` (baked quads, mesh construction), `lodestone-data` (canonical state validation), ECS extraction for entity and display input, and the active version adapter to tag network state ids.
