# Smooth lighting / ambient occlusion on the model path

## What it is

Per-corner ambient occlusion and light smoothing for baked block-model geometry —
vanilla's `ModelBlockRenderer.AmbientOcclusionFace`, ported to
`quad_corner_sample`/`mesh_models` in `crates/lodestone-render/src/models.rs`. This
is the "not Minecraft" tell issue [#22](https://github.com/matteopolak/lodestone/issues/22)
named: flat per-block light plus directional shade, with no darkening in corners
and crevices.

Closes out Tier 1 epic #1. The bulk of the per-corner math shipped earlier in
`1b8e46b`; this doc and the work around it add the missing `ambientocclusion`
model-flag gate and the pixel-level proof that both halves (the corner math and
the flag) actually reach the screen.

## Which mesher this is (read this first)

**There are two meshers, and this doc is about only one of them.**

- `crate::mesh`'s `mesh_simple`/`mesh_greedy` is the **packed full-cube path**:
  untinted, geometrically-a-cube blocks (stone, dirt, deepslate — see
  `is_packed_cube` in `models.rs`). It has its own, separate AO implementation
  (`face_corner_lighting` in `mesh.rs`, with its own `ao_darkens_against_an_occluder`
  / `ao_flat_where_unoccluded` unit tests) and is **not** what this doc covers.
  `mesh_simple` is also what `cargo run --bin lodestone -- --headless` drives for
  its single-orientation demo scene — a scene that structurally cannot exercise
  `mesh_models` or `face_shade` at all. A fix "verified" only against `--headless`
  proves nothing about the model path; this is the exact trap `CLAUDE.md`'s
  "world species of vacuous test" entry documents.
- `mesh_models` (this doc) is the **baked non-cube model path**: stairs, slabs,
  fences, cross-plants, and every tinted or partial-geometry block (grass,
  leaves, water-adjacent glass). `crates/lodestone-shell/src/mesher.rs` calls
  `mesh_models` directly for live terrain — it is not a demo or a fallback.

Both paths are real and both run in the live client; they simply cover disjoint
block populations. If you are chasing an AO bug, check `is_packed_cube` on the
block in question before assuming which file owns it.

## How it works

### The four-corner blend

`quad_corner_sample` (in `models.rs`) computes, per vertex of a `BakedQuad`:

- which of the quad's four block-grid corners the vertex is nearest, by
  projecting its position onto the face's in-plane axes (`face_uv_axes`);
- the two edge-adjacent neighbour cells and the diagonal cell around that
  corner, plus the cell the face opens into (`face_light_at`'s target, the
  "centre");
- **AO**: the average of four per-cell shade samples — `1.0` for an open cell,
  `AO_OCCLUDED = 0.2` for an occluding one (vanilla's darkest AO sample is
  `0.2`, never `0.0`, so a fully-occluded corner still averages to `0.4` once
  the always-open front cell is folded in);
- **light**: the average of the same four cells' sky/block levels, except an
  *occluding* neighbour's value is replaced by the centre's own light once the
  centre is lit above `SMOOTH_LIGHT_MIN_CENTRE` (vanilla's `smoothBlend`) — so a
  corner pressed against a wall reads as dim, not pitch black.

Verified against the real 26.2 jar
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/block/{ModelBlockRenderer,BlockModelLighter}.java`):
`BlockModelLighter.prepareQuadAmbientOcclusion` samples exactly these three
neighbours per corner (`AdjacencyInfo.corners`), replaces a same-sign-occluded
diagonal with the edge sample (`!translucent2 && !translucent0 → shadeCorner02 =
shade0`), and blends light via `LightCoordsUtil.smoothBlend`. Not ported: vanilla's
`faceShape`-weighted interpolation for partial (non-full-face) quads and the
`translucentN` hidden-diagonal substitution — both matter only for non-cube
models' interior faces, which is a documented, narrower gap than the corner math
itself.

### The `ambientocclusion` gate

Vanilla does not always take the AO path. `ModelBlockRenderer.tesselateBlock`
(26.2, line 65):

```java
if (this.ambientOcclusion && blockState.getLightEmission() == 0 && this.parts.getFirst().useAmbientOcclusion()) {
    this.tesselateAmbientOcclusion(...);
} else {
    this.tesselateFlat(...);
}
```

Three conditions, and this codebase can only honour one of them:

1. `this.ambientOcclusion` — the client's "Smooth Lighting" video option. No
   equivalent setting exists here (smooth lighting is always on), so this
   condition is vacuously true and needs no code.
2. `parts.getFirst().useAmbientOcclusion()` — the model JSON's
   `"ambientocclusion"` property (default `true`), read from the **first**
   resolved model of a (possibly multipart) block state. **Implemented** — see
   below.
3. `blockState.getLightEmission() == 0` — the block state's registered light
   emission. **Not implemented**: no source of per-block-state light emission
   exists anywhere in this codebase (not in `blocks.json` — see `CLAUDE.md`'s
   data-sources note — and not read by any oracle dump under `crates/protocol`).
   A light-emitting full-cube model (`sea_lantern`, a lit `redstone_lamp`,
   `glowstone`) will still take the smooth-AO path here, where vanilla would
   flatten it. See "How to change it" below for what closing this needs.

`crates/lodestone-render/src/block_models.rs:924`'s `ambient_occlusion: false` is
**not** this gate and is not a bug to "fix" by flipping it — it configures a
synthesised `ResolvedModel` used only by `extruded_sprite_geometry`, the
`builtin/generated` GUI-item sprite extrusion (a flat 2-D icon baked into a thin
3-D slab for the inventory). That path never reaches `mesh_models`; it feeds
`mesh_item_quads`, which has no AO computation at all (only the constant
per-face `shade`, or `1.0` under `GuiLight::Front`). The `false` there matches
vanilla's real `ItemModelGenerator`-produced models, which don't declare
`ambientocclusion` either.

### Data path for the implemented half

```
model JSON "ambientocclusion"
  -> ResolvedModel.ambient_occlusion   (lodestone-assets/src/model.rs, already existed)
  -> BakedModel.ambient_occlusion      (lodestone-assets/src/bake.rs, new — first-model-wins,
                                         mirroring particle_uv's own "first resolved model" rule)
  -> StateModel.ambient_occlusion      (lodestone-render/src/block_models.rs, new)
  -> BlockModels::ambient_occlusion(state_id) -> bool   (new accessor)
  -> ModelSectionView::ambient_occlusion_at(x, y, z) -> bool   (new trait method, default `true`)
  -> mesh_models: branches once per block between quad_corner_sample and a flat
     (1.0, light) fallback, matching tesselateFlat exactly (models.rs)
```

The branch is **per block**, not per quad — `parts.getFirst()` in vanilla applies
to the whole block, and `mesh_models` mirrors that: `ambient_occlusion_at` is
read once per cell, before the quad loop.

**The live shell does not call `ambient_occlusion_at` yet.** `ModelSectionView`'s
default (`true`) reproduces the exact pre-existing behaviour, so nothing regresses,
but no block actually renders flat via this mechanism until
`crates/lodestone-shell/src/mesher.rs`'s `SnapshotModelView` (or whatever type
implements `ModelSectionView` there) overrides it with
`block_models.ambient_occlusion(state_id)`. That file is outside this crate's
ownership for this change; the patch is one method, mirroring the existing
`occludes_at`/`face_light_at` overrides already on that type:

```rust
fn ambient_occlusion_at(&self, x: usize, y: usize, z: usize) -> bool {
    self.block_models.ambient_occlusion(self.state_id_at(x, y, z))
}
```

### Gamma space, not linear

Verified two ways this multiply is **not** colour-managed, matching
`CLAUDE.md`'s standing rule:

1. **Jar**: `QuadInstance.scaleColor`/`multiplyColor` call `ARGB.scaleRGB`/
   `ARGB.multiply`, which operate on the packed 8-bit-per-channel `int` directly
   — no `sRGB <-> linear` round trip anywhere in `ModelBlockRenderer` or
   `BlockModelLighter`.
2. **Shader**: `crates/lodestone-render/src/model_pipeline.rs`'s fragment shader
   computes `srgb_to_linear(linear_to_srgb(tex.rgb) * tint_col * in.shade)` —
   texel converted *to* sRGB, multiplied there, converted back — exactly the
   gamma-space round trip the jar's byte-space multiply implies. `in.shade` is
   `ao * light_term` from the vertex shader, so AO rides the same round trip as
   tint and directional shade.

Both confirmed unchanged by this issue's work (no shader touched):
`model_shade_gamma_gate` still measures an `East`/`Up` shade ratio of `0.602`
against a `0.600` gamma-space prediction (vs. `0.794` for the linear-space bug),
and the new `model_ao_corner_gate` (below) reads a single-occluder corner byte of
`210` against a `204` gamma-space prediction (`round(255 * 0.8)`) — a plain
linear byte multiply would have landed further off given the same non-linear
sRGB re-encode the shade gate already characterises.

### Non-full-cube models and fluids

- **Non-cube models** (stairs, slabs, fences): AO still applies, using the
  nearest-corner approximation described above — vanilla's per-quad
  `faceShape`-weighted interpolation (for a partial quad that doesn't span a
  full block face) is not ported. Documented as a narrower, acceptable gap in
  `quad_corner_sample`'s own doc comment.
- **Fluids** (`mesh_fluids`): no AO at all, by design — matches vanilla, which
  renders fluid surfaces through a completely separate path
  (`FluidRenderer`/`bake_fluid`) with no ambient-occlusion term. See
  [fluid-rendering.md](./fluid-rendering.md).

## How to change it

- The corner math lives entirely in `quad_corner_sample` (`models.rs`). Its unit
  tests (`ao_matches_vanillas_one_occluder_ratio_and_leaves_the_far_corner_bright`,
  `smooth_blend_substitutes_the_centre_only_above_the_threshold`) probe it
  directly, without going through a full `mesh_models` pass.
- The flag gate is `ModelSectionView::ambient_occlusion_at` plus the branch in
  `mesh_models`. `ambient_occlusion_at_false_flattens_ao_through_mesh_models`
  (`models.rs`) exercises it end to end, with an executed control proving the
  same occluder darkens the mesh when the flag is left at its default.
- **To close the light-emission gap**: add a per-block-state light-emission
  source. The natural approach, matching how `collision_shapes`/`hardness` are
  sourced (`crates/protocol/v770/tests/{collision_shapes,hardness}.rs` +
  `oracle-java/`), is booting the real server headlessly and walking
  `Block.BLOCK_STATE_REGISTRY[i].getLightEmission()`. Thread the result into
  `BlockStateRegistry` (or a sibling lookup) so `block_models.rs`'s baking loop
  can fold `light_emission == 0` into `StateModel::ambient_occlusion` alongside
  the model flag already there.
- **To wire the flag into live rendering**: add the one-method
  `ModelSectionView` override to the shell's implementing type, shown above.

## Configuration

No env vars or flags. `AO_OCCLUDED` (`0.2`) and `SMOOTH_LIGHT_MIN_CENTRE` (`2`)
in `models.rs` are vanilla-derived constants, not meant to be tuned.

## Dependencies

- `lodestone_assets::bake::{BakedQuad, BakedModel}` — geometry and (as of this
  change) the model-level AO flag.
- `lodestone_assets::model::ResolvedModel::ambient_occlusion` — the parsed JSON
  flag, resolved down the parent chain (nearest-defined wins, default `true`).
- `crates/lodestone-render/src/block_models.rs` — per-state baking and the
  `BlockModels::ambient_occlusion` accessor.
- The real 26.2 client jar decompile
  (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/block/`) for the
  vanilla behaviour this ports.
