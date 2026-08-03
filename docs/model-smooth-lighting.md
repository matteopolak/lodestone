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

### Known divergences from vanilla

Re-read against the jar after the port landed. Each of these is a **real** difference,
not a simplification that happens to agree; none is currently covered by a gate.

**1. ~~The occluder predicate is the wrong question (most visible).~~ Fixed** —
see [The occluder predicate](#the-occluder-predicate) below for what shipped, the
census it reads, and three things this section used to get wrong (ice, honey and
copper grates). The description of the *bug* is kept verbatim because it is the
clearest statement of why the two predicates are not interchangeable.

`quad_corner_sample` decided "does this neighbour darken the corner?" with
`ModelSectionView::occludes_at`, i.e. `BlockModels::occludes` — a *rendering*
predicate (all six faces fully cover, opaque, non-cutout). Vanilla never asks that.
It calls `cache.getShadeBrightness(state, level, pos)`, which is
`BlockBehaviour.getShadeBrightness` (`BlockBehaviour.java:315`):

```java
return state.isCollisionShapeFullBlock(level, pos) ? 0.2F : 1.0F;
```

— a *collision* predicate, with exactly seven overrides in the whole 26.2 tree
(`grep -rln "protected float getShadeBrightness" net/`): `TransparentBlock` (glass,
ice, tinted glass) and `StructureVoidBlock`/`BarrierBlock`/`LightBlock` return a flat
`1.0`, `SnowLayerBlock` returns `0.2` only at 8 layers, and `MudBlock`/`SoulSandBlock`
return their own values. **`LeavesBlock` is not among them.**

The consequences of the mismatch:

| block | our `occludes` | vanilla shade | agree? |
| --- | --- | --- | --- |
| stone, dirt | `true` → `0.2` | `0.2` (full collision cube) | yes |
| glass, ice | `false` → `1.0` | `1.0` (`TransparentBlock` override) | yes, **by coincidence** |
| slab, stairs | `false` → `1.0` | `1.0` (not a full collision cube) | yes |
| water | `false` → `1.0` | `1.0` (empty collision) | yes |
| **leaves** | `false` → `1.0` | **`0.2`** (full collision cube, no override) | **no** |
| **slime, honey** | `false` → `1.0` | **`0.2`** | **no** |
| **spawner, grates** | `false` → `1.0` | **`0.2`** | **no** |

Glass and ice agreeing is the trap here: it makes the predicate look correct on the
blocks a test scene is most likely to contain, while the divergence class is "full
collision cube that does not occlude for culling". The player-visible symptom is that
**the underside of a tree canopy does not darken**, where in vanilla it is markedly
dimmer than open sky.

That seam is what shipped, one layer better sourced: `ao_occludes_at` defaults to
`occludes_at`, is overridden in `crates/lodestone-shell/src/mesher.rs`, and reads
`lodestone_data::shade_brightness` — vanilla's **own answer**, dumped per state —
rather than `collision_shapes` plus a by-hand exception table.

### The occluder predicate

`quad_corner_sample` now uses **two** predicates, and mixing them up is the whole
bug:

| term | trait method | vanilla | why |
| --- | --- | --- | --- |
| **AO shade** | `ModelSectionView::ao_occludes_at` | `getShadeBrightness == 0.2F` | `BlockModelLighter.java:45-110` averages `cache.getShadeBrightness` per corner |
| **smooth light** | `ModelSectionView::occludes_at` | `translucentN` / `smoothBlend` | `BlockModelLighter.java:59-66` keys the light substitution on `!isViewBlocking \|\| getLightDampening() == 0`, which is a *rendering* question, not a collision one |

So only the AO half moved. `occludes_at` and `corner_light_at` were not touched —
swapping the light half too would have a leaf cell hand its own darkness to its
neighbours' *light*, which vanilla does not do. `models.rs`'s
`ao_reads_the_shade_predicate_and_light_reads_the_culling_one` drives the two
predicates to **opposite** answers in one call and pins both outputs (`ao 0.8`
with `light 0xB0`, then `ao 1.0` with `light 0xF0`), so neither can be mistaken
for the other.

`ao_occludes_at`'s default **is** `occludes_at`. That is deliberate — a view with
no block census (the GUI item path, unit-test doubles) keeps its old behaviour —
and it is also the island hazard: the mechanism is inert until
`SnapshotModelView` overrides it, exactly like `ambient_occlusion_at` before
`2b96bbb`.

#### Where the census comes from

`lodestone_data::shade_brightness` — one 4,046-byte bitset, O(1) by global
block-state id, generated into `src/generated/shade_brightness.rs` from
`crates/lodestone-data/oracle-java/ShadeBrightnessOracle.java`: a headless 26.2
server walking `Block.BLOCK_STATE_REGISTRY` and calling
`state.getShadeBrightness(EmptyBlockGetter.INSTANCE, BlockPos.ZERO)`. Dump
committed at `crates/lodestone-data/tests/support/shade_brightness_jvm.txt`
(byte-reproducible across two runs, md5 `a03cd79dfd71f4753960c129eba88f49`).

**It dumps vanilla's answer, not the recipe for it**, which is the point: the
seven overrides are on *classes*, and `TransparentBlock` alone is **26 registered
blocks** (glass, all 16 stained glasses, tinted glass, and the eight copper
grates via `WaterloggedTransparentBlock`). Any hand-written block list has to
expand that family by hand — the mistake that shipped two off-by-one entity
metadata indices.

Measured facts the dump establishes, each asserted in
`crates/lodestone-data/tests/shade_brightness.rs`:

- `getShadeBrightness` returns **exactly two** distinct values across all 32,366
  states — 1.0 for 29,112 and 0.2 for 3,254. That is what makes the one-bit
  encoding lossless rather than merely convenient, and a third value would fail
  loudly instead of being silently rounded.
- The overrides move **39 states across 30 blocks** relative to
  `isCollisionShapeFullBlock` alone, and they move them in **both** directions:
  3 states become occluding (`mud`, `soul_sand`, `snow[layers=8]`) and 36 stop
  (the glass family, `barrier`, the copper grates). So no monotone "also treat X
  as solid" shortcut over `collision_shapes` works.
- `snow` is the one per-**state** override, which is why the census is keyed by
  state id and not by block.

#### Three things the old version of this doc had wrong

Each was true-looking and evidenced by a `grep`; the JVM dump disagreed.

| claim | reality |
| --- | --- |
| "glass and ice agree with `occludes` by coincidence" | glass does; **ice does not**. `IceBlock extends HalfTransparentBlock`, and only `TransparentBlock` overrides `getShadeBrightness`. Ice, packed ice and blue ice all darken at `0.2`, so they were part of the bug, not part of the agreement. |
| "slime, **honey**, spawner, grates diverge" | slime and spawner do (and `beacon`, `trial_spawner`, `vault`, `mangrove_roots`, every leaf). **honey_block does not** — its collision box is inset, so the base formula already answers `1.0`. **Copper grates do not** either — they are `WaterloggedTransparentBlock`, so vanilla exempts them. |
| "`TransparentBlock`, `Barrier`, `Light`, `Mud`, `SnowLayer`, `SoulSand`, `StructureVoid` all override to a flat `1.0`" | four of them do. `MudBlock` and `SoulSandBlock` override to a flat **`0.2`** (both sink an entity, so neither collision box is a full cube — the override exists to keep them dark), and `SnowLayerBlock` is `LAYERS == 8 ? 0.2 : 1.0`. |

The pattern is the same each time: the class list came from `grep -l`, which
tells you *which files* override the method and nothing about what they return or
how wide each family is.

**2. The AO neighbourhood is centred on the wrong cell for partial quads.**
`prepareQuadAmbientOcclusion` (`BlockModelLighter.java:39`):

```java
BlockPos basePosition = this.faceCubic ? centerPosition.relative(direction) : centerPosition;
```

`faceCubic` (`:265`) is true when the quad is flat against the block boundary *or* the
state is a full collision cube. `quad_corner_sample` always uses `np` — the block plus
the face normal — which is the `faceCubic == true` branch. So for a genuinely partial
quad (a stair's or slab's interior face) vanilla samples the ring around the block's
**own** cell and we sample the ring one cell further out. `lightCenter` and
`shadeCenter` follow the same fork (`:114`–`:123`).

**3. `smoothBlend`'s sky-inherit branch is missing.** `LightCoordsUtil.smoothBlend`
(`:66`) has three cases per neighbour, not one:

```java
if (sky(center) > 2 || block(center) > 2) {
   if (neighbor1 == 0)            neighbor1 = center;              // ported
   else if (sky(neighbor1) == 0)  neighbor1 |= center & 0xFF0000;  // NOT ported
}
```

Two things follow. First, vanilla's substitution triggers on the neighbour's **packed
light being zero**, not on it occluding — those coincide for opaque blocks but not for a
pitch-dark air cell. Second, a neighbour with block light but *no* sky light inherits the
centre's sky. So a cell shadowed under an overhang but lit by a nearby torch averages in
the centre's sky in vanilla and a sky of `0` here — we render it darker. Note also that
the threshold gate is `sky > 2 || block > 2` over the whole blend, whereas
`quad_corner_sample` applies `SMOOTH_LIGHT_MIN_CENTRE` per channel independently.

**4. Smooth light is quantised to a whole light level.** `smoothBlend` returns
`(n1 + n2 + n3 + center) >> 2` in `pack` format and masks to `smoothPack`, whose scale is
`0..240` — 16 units per light level, so the blended value carries **quarter-level**
precision (`MAX_SMOOTH_LIGHT_LEVEL = 240`). `quad_corner_sample` ends in
`round_level(sum / 4.0)`, a `0..=15` nibble, because `ModelVertex::light` is a `u8`
holding `sky << 4 | block`. The error is bounded by half a light level per vertex (the
GPU still interpolates smoothly *between* vertices, so this is a corner-value offset, not
banding across a face). Widening it means widening the vertex format, so it is a real but
low-priority cost, recorded here so nobody re-derives it.

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

**The live shell now calls `ambient_occlusion_at`** — `2b96bbb` added the override
to `crates/lodestone-shell/src/mesher.rs`, closing what had been an island: the
mechanism was built and tested inside this crate while `ModelSectionView`'s default
(`true`) meant no block in the live world ever rendered flat through it. The
default is still the correct fallback, and still reproduces pre-#22 behaviour for
any view that does not override it. For reference, the override is one method,
mirroring the `occludes_at`/`face_light_at` overrides beside it:

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

### The directional face shade, and why it is *not* a dot product

`face_shade` in `models.rs` multiplies into the same `ao` slot as the corner
blend. It is a **fixed constant per face direction**, not a diffuse term:
`CardinalLighting.DEFAULT` in the 26.2 jar
(`net/minecraft/world/level/CardinalLighting.java`) is the record
`(down 0.5, up 1.0, north 0.8, south 0.8, west 0.6, east 0.6)`, and
`face_shade` is exactly that. The nether variant
(`(0.9, 0.9, 0.8, 0.8, 0.6, 0.6)`, selected by `CardinalLighting.Type.NETHER`) is
**not** ported — a dimension-dependent shade table is the open half here.

Do not confuse this with the *entity* diffuse. Entities and the first-person arm
have no per-face direction to look up, so they run vanilla's two-light
`minecraft_mix_light` instead (see
[entity-rendering.md](./entity-rendering.md)); blocks never do. Issue #383 asked
whether block faces had drifted onto a dot product, and they had not.

Re-verified against the **live** mesher (`mesh_models`, which is what
`lodestone-shell/src/mesher.rs` calls for real terrain — never `mesh_simple`,
which cannot exercise `face_shade` at all), on a mid-grey byte-128 texel at full
sky light, `entity_light_pixels::lighting_census_by_location`:

| face | `face_shade` | measured byte | `128 x shade` | if multiplied in *linear* |
| --- | --- | --- | --- | --- |
| up | 1.0 | 128 | 128 | 128 |
| north/south | 0.8 | 102 | 102.4 | 115 |
| east/west | 0.6 | 77 | 76.8 | 100 |
| down | 0.5 | 64 | 64.0 | 92 |

Every face lands on `128 x shade` to within a byte, and every one is far from the
linear-space column — so the constants and the colour space are both vanilla, and
neither needed changing for #383.

Grass was singled out in that report because it carries a biome tint *and* a face
shade, so a colour-space error would show up on it first. It does not:
`grass_light_response_gate` measures the `grass_block` top at `(89, 116, 54)`,
`G/R = 1.298`, against the plains tint `#91BD59`'s own `G/R = 1.303`. A
linear-space tint multiply collapses that ratio to ~1.13 (the defect `4e8f058`
removed). What is still missing on grass is **per-biome** tint — the palette holds
each tinted source's *plains* colour — which changes hue, not brightness.

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
- **The predicate has three layers of gate, and they cover different failures:**

  | gate | proves | control that must fail |
  | --- | --- | --- |
  | `models.rs`'s `ao_reads_the_shade_predicate_and_light_reads_the_culling_one` (hermetic, in the default suite) | `quad_corner_sample` routes each term to the right method | the mirror view, `occludes_at` true / `ao_occludes_at` false |
  | `lodestone-render/tests/model_ao_corner_gate.rs`'s `ao_occluder_predicate_is_shade_brightness_not_face_culling` (GPU) | the **pixel** consequence, against a predicted byte of `round(255 x 0.8) = 204` | `barrier` (vanilla-exempt but a full collision cube) and a culling-only occluder, both of which must stay ≥ 250 |
  | `lodestone-shell/tests/canopy_ao.rs` (real `client.jar` geometry) | the **production** path — `mesh_snapshot_models` over a real `SectionSnapshot`, i.e. that the shell override is not an island | a solid glass section, whose distinct vertex `ao` set must be exactly the four `face_shade` constants |

  `canopy_ao.rs` measures a solid oak-leaves section's darkest vertex against
  `face_shade(Down) x (1 - 0.2 x 3) = 0.20`, with the bug's value (`0.50`) computed
  alongside it — both from constants outside this codebase, and far enough apart
  that no predicate satisfies both. It deliberately does **not** locate a single
  quad by centroid: a block's `Down` quad and the block-below's `Up` quad are
  geometrically identical, so telling them apart would mean asserting a winding
  polarity, which `CLAUDE.md` forbids.
- **To close the light-emission gap**: add a per-block-state light-emission
  source. The natural approach, matching how `collision_shapes`/`hardness` are
  sourced (`crates/protocol/v770/tests/{collision_shapes,hardness}.rs` +
  `oracle-java/`), is booting the real server headlessly and walking
  `Block.BLOCK_STATE_REGISTRY[i].getLightEmission()`. Thread the result into
  `BlockStateRegistry` (or a sibling lookup) so `block_models.rs`'s baking loop
  can fold `light_emission == 0` into `StateModel::ambient_occlusion` alongside
  the model flag already there.
- **The leaves/slime/spawner gap is closed** (divergence 1). To change the
  predicate, the only two places are `quad_corner_sample`'s `shade_occ` closure
  and `SnapshotModelView::ao_occludes_at`. To refresh the census after a version
  bump, follow the module docs on
  `crates/lodestone-data/tests/shade_brightness.rs` — re-dump with the Docker
  command there, then `LODESTONE_REGEN=1 cargo test -p lodestone-data --test
  shade_brightness committed_table_matches_dump -- --ignored`.
  **Do not replace the census with a `collision_shapes` derivation.** It is wrong
  on 39 states in both directions, and `canopy_ao.rs`'s glass control plus
  `model_ao_corner_gate`'s barrier scene both exist to fail if someone tries.
- The flag is already wired into live rendering — `2b96bbb` added the
  `ambient_occlusion_at` override to the shell (see the data-path section above).
  This bullet used to say it still needed doing.

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
- `lodestone_data::shade_brightness` — the AO occluder census. A dependency of
  `lodestone-shell` (which supplies the `ao_occludes_at` override) and a
  **dev**-dependency of `lodestone-render` (so `model_ao_corner_gate`'s scenes use
  real state ids). `lodestone-render` itself still needs no block census, which is
  the whole reason `ao_occludes_at` has a default.
- The real 26.2 client jar decompile
  (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/block/`) for the
  vanilla behaviour this ports.
