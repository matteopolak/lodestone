# Player skin pipeline

## What it is

The player skin pipeline draws a 64×64 skin sheet over the baked player rig,
including the optional hat, jacket, sleeve and pants boxes. It keeps the
partially transparent outer layer while preventing an interior cube face from
painting a one-pixel seam through a nearer body part.

## How it works

Player skin geometry is emitted by `lodestone_assets::entity::player_model` and
wound by `lodestone_render::entity::push_part_quads`. The dedicated
`EntityPipeline::player_skin_pipeline` uses the player cutout threshold (`0.1`)
and straight alpha blending, while retaining the inclusive nearer-or-equal
depth test, depth writes, and the shared camera depth bias. Its only primitive
state difference from the general entity pipeline is back-face culling: an
outward-wound cube keeps its exterior face and drops the inner face that can
otherwise show at a silhouette or where an inflated outer layer meets the base
cube. Hat and jacket front faces remain ordinary child-part draws, so their
intended overlay is preserved.

Each sheet is uploaded as its own texture rather than a shared atlas. The
player skin sampler uses nearest minification/magnification and the descriptor's
edge-clamp addressing; there is therefore no atlas-neighbour seam to solve with
padding. The focused GPU controls in
`crates/lodestone-render/tests/entities/player_skin_artifact_controls.rs`
measure sampler/filter changes separately from centre-versus-silhouette depth
ordering.

## How to change it

Change the player render contract in `EntityPipeline::player_skin_pipeline` and
`player_skin_pipeline_contract` together. Keep `player_model`'s base and outer
boxes in the same part hierarchy; changing the inflation or draw order changes
which surfaces are expected to win depth. If UV work is needed, first run the
nearest-versus-linear control and inspect its changed-pixel bounding box. Do not
add a second legacy-sheet conversion here: 64×32 normalization belongs at the
skin ingest boundary in `lodestone-assets`.

The back-face choice relies on the shared outward winding in
`push_part_quads`. Any new player-only mesh must use that winding rule (or prove
an equivalent one) before enabling culling. Re-run the ignored controls after
changes to culling, alpha threshold, inflation, sampler state, or depth state.

## Configuration

There are no player-skin feature flags. The pipeline is selected for
`player_wide` and `player_slim` entity batches; the sheet URL and model variant
come from the resolved skin record. The texture format is
`Rgba8UnormSrgb`, with one mip level and no atlas padding.

## Dependencies

- `lodestone-assets` supplies the player model geometry and decoded image.
- `lodestone-render::entity` supplies part-local mesh baking and outward
  winding.
- `lodestone-render::entity_pipeline` supplies the shader, blend/depth state,
  and player-specific culling contract.
- `lodestone-shell` resolves/caches the sheet and selects the player pipeline
  for world draws.
