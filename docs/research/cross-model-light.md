# Diagnosis: cross-plant faces go dark against a solid neighbour

## What it is

Root cause of "cross-plant faces go dark next to a solid neighbour": `mesh_models` samples
light from `pos + quad.direction` unconditionally, but vanilla only samples the neighbour when
a quad's plane is flush with the block boundary (`faceCubic`) or the quad carries a
`cullface`. A cross blade's plane is diagonal and carries no `cullface`, so vanilla lights it
from the plant's own cell; this client instead reads the interior of the adjacent solid
block, which the light engine stores as 0. A per-quad fix and its mesh-level proof gate are
specified in §5–6 below; nothing in the repo has been edited by this read-only investigation.

**Status:** root cause established by reading both sources plus one f32 simulation of our own
baker. Read-only investigation — nothing in the repo was edited.

**One-line cause:** `mesh_models` samples light from `pos + quad.direction` for *every* quad.
Vanilla only does that when the quad's plane is flush with the block boundary (`faceCubic`) or
the quad carries a `cullface`. A cross blade's plane is diagonal and it carries no `cullface`,
so vanilla lights it from the plant's **own** cell; we read the interior of the adjacent solid,
which the light engine stores as `0`.

**Not the `SkyDefault` gap.** See §3.

---

## 1. Vanilla's actual rule

### 1a. Which path a cross plant takes

`block/cross.json` in the real 26.2 `client.jar` (`assets/minecraft/models/block/cross.json`,
read with `unzip -p`):

```json
{
    "ambientocclusion": false,
    "elements": [
        {   "from": [ 0.8, 0, 8 ], "to": [ 15.2, 16, 8 ],
            "rotation": { "origin": [ 8, 8, 8 ], "axis": "y", "angle": 45, "rescale": true },
            "shade": false,
            "faces": { "north": {...}, "south": {...} } },
        {   "from": [ 8, 0, 0.8 ], "to": [ 8, 16, 15.2 ],
            "rotation": { "origin": [ 8, 8, 8 ], "axis": "y", "angle": 45, "rescale": true },
            "shade": false,
            "faces": { "west": {...}, "east": {...} } }
    ]
}
```

Three facts from that file: `ambientocclusion: false`, `shade: false`, and **no `cullface` on any
face**. `block/tinted_cross.json` and `block/sunflower_top.json` are identical in all three.
`short_grass`'s blockstate is a bare `{"variants": {"": {"model": "minecraft:block/short_grass"}}}`,
which inherits `block/cross`.

So in `ModelBlockRenderer.tesselateBlock`
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/block/ModelBlockRenderer.java:65-69`):

```java
if (this.ambientOcclusion && blockState.getLightEmission() == 0 && this.parts.getFirst().useAmbientOcclusion()) {
   this.tesselateAmbientOcclusion(...);
} else {
   this.tesselateFlat(...);
}
```

`useAmbientOcclusion()` is false → **cross plants take `tesselateFlat`.** Any analysis that only
reads `prepareQuadAmbientOcclusion` is looking at the wrong branch for this bug.

### 1b. `tesselateFlat` has two sub-cases, keyed on the quad's bucket

`ModelBlockRenderer.java:157-190`:

```java
for (Direction direction : DIRECTIONS) {
   List<BakedQuad> culledQuads = part.getQuads(direction);
   if (!culledQuads.isEmpty()) {
      BlockPos relativePos = this.scratchPos.setWithOffset(pos, direction);   // :165
      ...
      int lightCoords = this.lighter.getLightCoords(state, level, relativePos);  // :175
      for (BakedQuad quad : culledQuads) {
         this.lighter.prepareQuadFlat(level, state, pos, lightCoords, quad, this.quadInstance);
      }
   }
}
for (BakedQuad quad : part.getQuads(null)) {                                  // :186
   this.lighter.prepareQuadFlat(level, state, pos, -1, quad, this.quadInstance);  // :187
}
```

`-1` is `BlockModelLighter.CHECK_LIGHT` (`BlockModelLighter.java:19`). The buckets are keyed by
**`cullface`**, not by geometric facing — `QuadCollection.getQuads(null)` returns `this.unculled`
(`QuadCollection.java:54-63`), and the only producer of a keyed bucket is
`UnbakedCuboidGeometry.java:64-66`:

```java
builder.addUnculledFace(quad);                                                    // no cullface
builder.addCulledFace(Direction.rotate(modelState.transformation().getMatrix(), face.cullForDirection()), quad);
```

### 1c. The `faceCubic` rule

`BlockModelLighter.prepareQuadFlat` (`BlockModelLighter.java:197-216`):

```java
if (lightCoords == -1) {
   this.prepareQuadShape(level, state, pos, quad, false);
   BlockPos lightPos = this.faceCubic ? this.scratchPos.setWithOffset(pos, quad.direction()) : pos;   // :207
   outputInstance.setLightCoords(this.cache.getLightCoords(state, level, lightPos));                  // :208
} else {
   outputInstance.setLightCoords(lightCoords);
}
```

and `faceCubic` itself (`BlockModelLighter.java:265-272`, from `prepareQuadShape`, whose min/max
are over the quad's four vertex positions at `:228-239`):

```java
this.faceCubic = switch (quad.direction()) {
   case DOWN  -> minY == maxY && (minY < 1.0E-4F || state.isCollisionShapeFullBlock(level, pos));
   case UP    -> minY == maxY && (maxY > 0.9999F || state.isCollisionShapeFullBlock(level, pos));
   case NORTH -> minZ == maxZ && (minZ < 1.0E-4F || state.isCollisionShapeFullBlock(level, pos));
   case SOUTH -> minZ == maxZ && (maxZ > 0.9999F || state.isCollisionShapeFullBlock(level, pos));
   case WEST  -> minX == maxX && (minX < 1.0E-4F || state.isCollisionShapeFullBlock(level, pos));
   case EAST  -> minX == maxX && (maxX > 0.9999F || state.isCollisionShapeFullBlock(level, pos));
};
```

**The rule, stated once:**

| quad | light sample cell |
|---|---|
| has a `cullface` C | `pos + C` — the cell C opens into, unconditionally |
| no `cullface`, plane flush with the block boundary on its own facing axis | `pos + quad.direction()` |
| no `cullface`, plane *not* on the boundary | **`pos` — the block's own cell** |
| no `cullface`, state is a full collision cube | `pos + quad.direction()` (the `||` clause) |

Note the first row: for a culled quad the step is the **bucket/`cullface`** direction, which is
not always `quad.direction()` — `powder_snow`'s east shell is a west-facing quad with
`cullface: east` (our own `block_models.rs:2031-2032` documents that pair).

### 1d. The AO path expresses the same rule twice

For completeness, since a fix should keep both branches consistent
(`BlockModelLighter.prepareQuadAmbientOcclusion`):

```java
BlockPos basePosition = this.faceCubic ? centerPosition.relative(direction) : centerPosition;   // :39
...
int lightCenter = this.cache.getLightCoords(state, level, centerPosition);                       // :114
pos.setWithOffset(centerPosition, direction);
BlockState nextState = level.getBlockState(pos);
if (this.faceCubic || !nextState.isSolidRender()) {                                              // :117
   lightCenter = this.cache.getLightCoords(nextState, level, pos);
}
```

`:39` moves the whole 4-corner AO/light ring back onto the block's own cell for a non-cubic face.
`:117` is a second, independent guard with the same intent stated plainly: **take the neighbour's
light only if the face is cubic, or the neighbour is not solid-render.**

---

## 2. Where our code diverges

### 2a. The divergence

`crates/lodestone-render/src/models.rs:635`:

```rust
// Per *quad*, not per block: each face carries the light of
// the cell it opens into (see `face_light_at`).
let light = view.face_light_at(x, y, z, quad.direction);
```

Unconditional. There is no `faceCubic` test anywhere on this path, and the step direction is
`quad.direction` rather than the `cullface`. Every quad in the model path reads the neighbouring
cell.

`crates/lodestone-render/src/models.rs:636-645` repeats it for the AO ring:

```rust
let face_n = face.normal();
let np = [x as i32 + face_n[0], y as i32 + face_n[1], z as i32 + face_n[2]];
```

`np` is always `pos + normal` — vanilla's `faceCubic == true` branch of `:39`.

The consumer chain: `crates/lodestone-shell/src/mesher.rs:867` implements `face_light_at` as
`SnapshotLight::face_light` (`mesher.rs:692-699`), which is `levels_at(pos + normal)`. Inside a
stone cell `levels_at` returns real stored `(0, 0)` — not a missing-nibble default. The
`SnapshotLight` doc at `mesher.rs:624-638` states the measurement: **99.5 % of solid cells store
sky light 0**.

### 2b. The island: the cross-plant handling already exists and is unreachable

`crates/lodestone-shell/src/mesher.rs:861-865`:

```rust
fn light_at(&self, x: usize, y: usize, z: usize) -> u8 {
    // No facing (cross plants, and any view that ignores `face_light_at`):
    // the brightest cell in the immediate neighbourhood, self included.
    self.light.max_light(x, y, z)
}
```

and `SnapshotLight::max_light`'s own doc (`mesher.rs:701-703`) says "geometry with no single
facing (fluid surfaces, **cross-shaped models**)". `mesh_models` never calls `light_at` — the only
light hooks it calls are `face_light_at` (`:635`) and `corner_light_at` (`:573`). `light_at` on
`SnapshotModelView` is dead code; the live consumer of `max_light` is `SnapshotFluidView::light_at`
(`mesher.rs:928`) via `mesh_fluids`. So the intent was written down and the wiring never happened —
`CLAUDE.md` rule 1's island shape.

### 2c. The prediction, and why the report says "sometimes ... on one side"

`crates/lodestone-assets/src/bake.rs:521-538` (`calculate_facing`, vanilla `FaceBakery::
calculateFacing`) snaps a rotated quad's normal to the nearest axis, tie-broken by first-wins over
`DIRECTIONS` at `bake.rs:47-54`, whose order is `Down, Up, North, South, East, West` — **North and
South precede East and West.**

A 45°-about-Y rotation puts each cross quad's normal exactly on a diagonal, so every quad is a
tie between one N/S and one E/W direction, and N/S always wins. I simulated our exact arithmetic
in float32 (`setup_shape` → `FACE_INFO` winding → `mat3_rot_y(45°)` with `RESCALE_45` →
`cross(b,a)` → `calculate_facing`); script kept at
`.../scratchpad/cross-facing-sim-diagcrossmodel.py`:

```
element1 north -> facing north  normal [-0.89999986  0.  -0.89999986]
element1 south -> facing south  normal [ 0.89999986 -0.   0.89999986]
element2 west  -> facing south  normal [-0.89999986  0.   0.89999986]
element2 east  -> facing north  normal [ 0.89999986  0.  -0.89999986]
```

**All four cross quads bake to `North` or `South`, two each.** So the falsifiable prediction is:

- solid block to the **north** (z−1) → the two north-facing blades go dark;
- solid block to the **south** (z+1) → the two south-facing blades go dark;
- solid block to the **east or west** → **no visual effect at all** in our client.

That asymmetry is the "sometimes", and "two of the four blade halves" is the "one side". If the
player reports darkening from an east/west neighbour, this diagnosis is wrong and the tie-break
needs re-deriving.

### 2d. Wider population than cross plants

Any unculled quad whose plane is not on the block boundary has the same bug. Confirmed from the
jar:

- `block/fence_post.json` — post from `[6,0,6]` to `[10,16,10]`; `down`/`up` carry `cullface`,
  the four **side faces carry none** and sit at 0.375/0.625. A fence post against a wall darkens.
  This one is on the *AO* path (`ambientocclusion` defaults to true), so it also hits the `np`
  divergence at `models.rs:639-643`.
- `block/template_torch.json` — same shape, `ambientocclusion: false`.

Cross plants are the loud case because the sprite is large and the quad faces horizontally.

### 2e. The doc that made it invisible

`docs/model-smooth-lighting.md:284` claims our flat branch is "matching `tesselateFlat` **exactly**".
It matches the uniform-per-vertex, no-AO half and not the light *position* half. The doc's known
divergence #2 (`docs/model-smooth-lighting.md:192-204`) correctly names the AO-path
`basePosition` fork — but explicitly frames it as affecting "a stair's or slab's interior face",
and never notices that cross plants take the **flat** path, where the same fork exists at `:207`
and is undocumented. Both need updating with the fix.

---

## 3. Relation to the entity/particle `SkyDefault` gap

**Independent.** Different mechanism, different fix site, no shared code.

- The `SkyDefault` bug is about *resolving an absent nibble*: `LightData::Missing` → `0` where
  vanilla's above-the-world default is `15`. It happens in open air, where no light data exists.
- This bug is about *choosing the wrong cell*: the sampled cell exists, is present in the
  snapshot, and genuinely stores sky `0` — because it is inside a solid block and vanilla's light
  engine puts `0` there on purpose. `WorldSectionLight`'s `sky_default` is never consulted
  (`world.rs:184-185` only fires on `LightData::Missing`), so setting it to `Full` would change
  nothing here.

They are the same *family* only in the loose sense that both are "light sampled without vanilla's
rule". Fixing one has no effect on the other, and neither fix touches the other's file.

---

## 4. Is the face black, or at the ambient floor?

**At the ambient floor, ≈ 0.0935 of the daylight value — not black.** Believed on this chain:

1. `block/cross.json` has `ambientocclusion: false`, so `SnapshotModelView::ambient_occlusion_at`
   (`crates/lodestone-shell/src/mesher.rs:816-819`) returns false and `mesh_models:646-650` takes
   the flat branch `[(1.0, light); 4]` — the whole quad carries one light byte, `0x00`, with
   `ao = 1.0`.
2. `shade: false` → `face_shade` (`models.rs:666`) returns `1.0`, so nothing else scales it.
3. `crates/lodestone-render/src/shaders/model.wgsl:182`: `out.shade = ao * lightmap_term(sky, block)`,
   applied in gamma space against an sRGB target (`model.wgsl:213-214`).
4. `light_term_from_levels(0, 0, 1.0)` = `apply_brightness_option(10/255 + 0)`
   (`crates/lodestone-render/src/light.rs:192-199`)
   = `c + (1-(1-c)^4 - c) * 0.5` with `c = 0.0392157` = **0.0935452**.

So a blade texel that would display as 255 displays as ~24. Perceptually black next to a lit
blade, which is consistent with the report. **If the observed face is genuinely 0/0/0, this
diagnosis is wrong** and the light term is being bypassed rather than fed a zero.

---

## 5. The minimal fix

**Mesher: `mesh_models`, in `crates/lodestone-render/src/models.rs`. Only that one.**
`mesh_simple`/`mesh_greedy` (`crates/lodestone-render/src/mesher.rs`) emit only full-cube faces on
the block boundary, where `faceCubic` is unconditionally true — they are already correct and a
change there would be wrong. `--headless` drives `mesh_simple` and **cannot** reproduce or verify
this; the live path is `mesher.rs:1093-1105` → `mesh_snapshot_models` → `mesh_models`.

### 5a. New helper, next to `quad_is_full_face` (`models.rs:256`)

```rust
/// Vanilla `BlockModelLighter.prepareQuadShape`'s `faceCubic`
/// (`BlockModelLighter.java:265-272`): whether the quad's plane is flush with the
/// block boundary on its own facing axis.
///
/// **Not** [`quad_is_full_face`], which additionally demands a full 1x1 span and
/// `cullface == direction`. Vanilla's test is planarity plus position only, so a
/// stair's top step qualifies and a cross blade or a fence post's side does not.
#[must_use]
fn quad_is_on_face_boundary(q: &BakedQuad) -> bool {
    const EPS: f32 = 1e-4;
    let (fixed, plane) = face_plane(q.direction);
    q.positions.iter().all(|p| (p[fixed] - plane).abs() <= EPS)
}
```

`face_plane` already exists at `models.rs:235-244` and returns exactly the `(axis, 0.0|1.0)` pair
vanilla's switch encodes. The `all(...)` form folds vanilla's `minC == maxC` planarity test and
its `minC < 1e-4` / `maxC > 0.9999` position test into one pass.

### 5b. In `mesh_models`, replace lines 633-650

Hoist once per block, inside the `for y/z/x` body next to `ao_enabled` (`models.rs:624`):

```rust
// Vanilla's `state.isCollisionShapeFullBlock(level, pos)` clause of `faceCubic`
// (`BlockModelLighter.java:265-272`). We have no collision-shape table on this
// trait; `occludes_at` on the block's *own* cell covers the population the clause
// exists for — opaque full cubes, whose interior quads must still be lit from the
// neighbour. A non-opaque full collision cube (slime, spawner, ice) falls to the
// own cell instead, which for a non-opaque cell carries real light, so the
// approximation errs bright rather than black.
let own_is_full_cube = view.occludes_at(x as i32, y as i32, z as i32);
```

then, per quad:

```rust
// Vanilla `ModelBlockRenderer.tesselateFlat` (:165, :175, :186-187) plus
// `BlockModelLighter.prepareQuadFlat` (:205-208) and
// `.prepareQuadAmbientOcclusion` (:39, :117):
//   * a quad in a *culled* bucket is lit from the cell its `cullface` opens into
//     — the bucket direction, which is not always `quad.direction`
//     (powder_snow, see `block_models.rs:2031`);
//   * an *unculled* quad is lit from the neighbour only when its plane is flush
//     with the block boundary (`faceCubic`), otherwise from the block's OWN cell.
// A cross blade is unculled and its plane is diagonal, so it is lit from its own
// cell. Sampling the neighbour reads the interior of an adjacent solid, which the
// light engine stores as 0 — the "grass is black on one side" report.
let sample_dir = quad.cullface.or_else(|| {
    (quad_is_on_face_boundary(quad) || own_is_full_cube).then_some(quad.direction)
});
let (np, light) = match sample_dir {
    Some(d) => {
        let n = face_of_direction(d).normal();
        let np = [x as i32 + n[0], y as i32 + n[1], z as i32 + n[2]];
        (np, view.face_light_at(x, y, z, d))
    }
    // Vanilla's `faceCubic == false` branch: the ring and the centre light both
    // move back onto the block's own cell. `corner_light_at` at the own coordinate
    // IS the own cell's exact packed light, and using it keeps the centre value
    // consistent with the ring — a `max`-over-neighbourhood centre against exact
    // corners is the self-inconsistency `grass_light_response_gate.rs:255-270`
    // documents.
    None => (
        [x as i32, y as i32, z as i32],
        view.corner_light_at(x as i32, y as i32, z as i32),
    ),
};
let corners = if ao_enabled {
    let face = face_of_direction(quad.direction);
    [0, 1, 2, 3].map(|i| quad_corner_sample(view, np, face, quad.positions[i], light))
} else {
    [(1.0, light); 4]
};
```

Note this deliberately reuses `corner_light_at` rather than adding a trait method — it already
returns the exact packed light at a signed coordinate and `SnapshotModelView` already implements
it correctly (`crates/lodestone-shell/src/mesher.rs:872-875`). Nothing new is needed in
`lodestone-shell`. `face` for the AO ring stays `quad.direction` (vanilla's
`prepareQuadAmbientOcclusion` only ever sees `quad.direction()`); only the base position moves.

This single change fixes three things at once: the flat-path light position (the reported bug), the
AO-path `basePosition` divergence already recorded as #2 in `docs/model-smooth-lighting.md:192-204`,
and the culled-bucket step direction for quads whose facing differs from their `cullface`.

### 5c. Existing test views — checked individually, and one to tidy

`corner_light_at`'s trait default is `0xF0` (`models.rs:390-393`), so any view that overrides
`face_light_at` but not `corner_light_at` now sees a full-bright own cell. I checked every
`impl ModelSectionView` that asserts a light value:

- `crates/lodestone-render/tests/grass_light_response_gate.rs`'s `OneQuad` (~line 240) —
  **passes.** It re-seats the blade onto a clip-space rect at `z = 0.5` (`full_frame`), so the quad
  is not on the boundary and falls to the own cell — but the view already overrides
  `corner_light_at` to `self.light`, so its `mesh.vertices.iter().all(|v| v.light == light)`
  assertion at line 336 still holds.
- `crates/lodestone-render/src/models.rs:1070`'s `PerFace`
  (`mesh_models_asks_for_light_per_quad_facing`) — **passes, but accidentally.** `cube_face`
  (`models.rs:1015-1028`) leaves `positions: [[0.0; 3]; 4]` with the comment "exact corner
  positions are irrelevant to the culling logic under test", which was true and is no longer:
  `Up`/`South`/`East` would now fail `quad_is_on_face_boundary`. The test survives only because
  `PerFace::occludes_at` returns `true` unconditionally, including for the block's own cell, so the
  new `own_is_full_cube` clause forces the neighbour branch. **Give `cube_face` real boundary
  positions** so the test proves what its name says rather than riding that clause.
- `crates/lodestone-shell/src/mesher.rs:2236`'s `cube_quads` (the `LightRule` probe backing
  `face_light_distinguishes_…` and `a_placed_block_meshes_with_its_neighbours_light_not_full_bright`)
  — **unaffected.** It builds real boundary positions *and* `cullface: Some(d)`, so every quad takes
  the culled-bucket branch, which is unchanged.

Docs to update: `docs/model-smooth-lighting.md` — retract the "matching `tesselateFlat` exactly"
claim at `:284`, and close out divergence #2 at `:192-204`. Add the flat-path half of the rule,
which the doc currently does not mention at all.

---

## 6. How to prove it

A **mesh-level** gate is sufficient and needs no GPU and no adapter — the defect is entirely in the
light byte `mesh_models` writes, and `model.wgsl`'s consumption of that byte is already gated by
`grass_light_response_gate`. Proposed new file:
`crates/lodestone-render/tests/cross_plant_light_position_gate.rs`, `#[ignore]`d on
`require_client_jar` (real baked geometry — a hand-authored cross would be the *world* species of
vacuous test, since the bug lives in the baked `direction`/`cullface`/vertex positions).

### The view

One `ModelSectionView` over a 16³ cell space with a light table:

| cell | contents | `occludes_at` | `corner_light_at` / `levels` |
|---|---|---|---|
| `(1,1,1)` | real baked `minecraft:short_grass` quads | false | `0xF0` (sky 15) |
| `(1,1,0)` | stone (north neighbour of the plant) | **true** | `0x00` |
| `(5,1,1)` | real baked `minecraft:stone` quads | true | `0x00` |
| `(5,1,0)` | stone | true | `0x00` |
| every other cell | air | false | `0xF0` |

`face_light_at(x,y,z,dir)` = `corner_light_at` at `(x,y,z) + normal(dir)`, i.e. the honest
neighbour rule, so the view itself encodes no opinion about which cell is right.

### Assertion 1 — the subject

Every vertex belonging to a `short_grass` quad carries `light == 0xF0`.

**Expected value's origin, outside our code:** `BlockModelLighter.java:207` says the sample cell is
`pos` when `faceCubic` is false; `BlockModelLighter.java:268` says `faceCubic` for `NORTH` needs
`minZ == maxZ`, which `block/cross.json`'s `"angle": 45` rotation makes false; and
`ModelBlockRenderer.java:65` + `"ambientocclusion": false` selects `tesselateFlat`, whose unculled
bucket passes `-1`. The plant's own cell is `0xF0` by the view's construction, so vanilla's answer
is `0xF0`.

**Predict both hypotheses, per `CLAUDE.md`.** The *current* build produces an exact, different
histogram, and the gate should assert that too as its anti-vacuity check on the fixture:
`short_grass` bakes 4 quads → 16 vertices, of which the two `Direction::North` quads (8 vertices)
read `0x00` and the two `Direction::South` quads (8 vertices) read `0xF0`. Correct build:
`{0xF0: 16}`. Broken build: `{0xF0: 8, 0x00: 8}`. A "did it get brighter" assertion passes on both.

**Failure output says *where*:** print one line per quad — index, `direction`, `cullface`,
`quad_is_on_face_boundary`, the quad's block-local bbox `(minX..maxX, minY..maxY, minZ..maxZ)`,
and the light byte. The bbox is the load-bearing column: it shows *why* the quad was classified as
it was, which a count cannot.

### Assertion 2 — negative control against an over-broad fix (the important one)

The `(5,1,1)` **stone** block's `North` quad must still read `0x00`.

This is the control that matters, because the naive fix ("always sample the own cell") passes
assertion 1 and re-introduces `fda948f` — a uniformly dark world, the exact regression
`SnapshotLight`'s doc (`crates/lodestone-shell/src/mesher.rs:624-638`) was written to prevent.
Stone's own cell is `0x00` here too, so to make the control discriminating give stone's own cell a
*distinguishable* value: set `(5,1,1)`'s light to `0x0A` and `(5,1,0)`'s to `0x00`, then assert the
stone north quad reads **`0x00`** (neighbour) and not `0x0A` (own cell). Stone's faces carry
`cullface`, so this exercises the first row of the rule table.

### Assertion 3 — control proving the detector is sensitive to the sample *position*

Take the two baked `short_grass` `North` quads, clone them, and snap every vertex's `z` to `0.0`
(making `minZ == maxZ == 0`, so `quad_is_on_face_boundary` is true). Placed at `(1,1,1)` with the
same stone north neighbour, those must read **`0x00`**.

This is executed, not described, and it fails assertion 1's predicate. Without it, assertion 1 is
satisfied by any build that returns `0xF0` unconditionally — including one that ignores the view's
light table entirely. It proves the predicate discriminates *where* the light was sampled rather
than merely *that* it was bright.

### Optional pixel gate

If someone wants screen-level proof, the `grass_light_response_gate` harness already has every
piece. The magnitude prediction is `0.0935452` (light `0x00` vs `0xF0`, derived in §4 from
vanilla's `AMBIENT_LIGHT_COLOR = 0xFF0A0A0A` at `DimensionTypes.java:36` and `Options.gamma = 0.5`),
against `0.000` if `AmbientColor` were dropped and `1.000` once fixed. Three well-separated values.
Given the mesh-level gate pins the byte and `model.wgsl`'s consumption of the byte is already
gated, this is redundant; I would not write it.

---

## 7. Looked at and ruled out

- **`SkyDefault` / `LightData::Missing`.** Not involved — see §3. `world.rs:184-185` shows the
  default only fires on a genuinely absent nibble; the cell here is present and stores `0`.
- **`face_shade` / directional shading.** `block/cross.json` sets `"shade": false` on every
  element, so `models.rs:666` returns `1.0`. Already refuted in
  `grass_light_response_gate.rs:11-16`; shading a blade would be the defect.
- **Tint, palette slot, cutout layer.** All three already refuted by measurement in
  `grass_light_response_gate.rs:14-24`. Not re-investigated.
- **`ao_occludes_at` / `shade_brightness`.** Irrelevant to cross plants: `ambientocclusion: false`
  means the AO branch never runs for them. It *is* relevant to the fence/torch instances in §2d.
- **`ambient_occlusion_at` wiring.** Correct and live (`crates/lodestone-shell/src/mesher.rs:816-819`).
- **`mesh_simple` / `mesh_greedy` / `--headless`.** Structurally cannot exercise this. Do not
  attempt to reproduce there.
- **`is_packed_cube` as a live dispatch.** It is *not* one: `crates/lodestone-shell/src/mesher.rs:1093-1106`
  sends the entire section through `mesh_snapshot_models` whenever `classifier.models()` is `Some`.
  The `models.rs:17-24` module doc describes a packed/model split that the live worker does not
  actually perform, and `is_packed_cube` has callers only in `model_census.rs` and `live_gate.rs`.
  Consequence worth keeping: stone is meshed by `mesh_models` in the live client, so §6's stone
  control is on the same code path as the subject.
- **Tie-break in `calculate_facing`.** Simulated in float32 rather than assumed; the two normal
  components come out bit-identical (`-0.89999986`), so the tie is exact and
  `DIRECTIONS`' `Down, Up, North, South, East, West` order decides it. That is why only ±Z
  neighbours matter (§2c).
- **A live RCON probe of light levels.** Attempted to plan one and dropped it: 26.2 exposes no
  server command that reports a cell's sky/block light (`/data` reads block entities and entities,
  not the light engine). The "solid cells store sky 0" figure is already a repo measurement
  (`crates/lodestone-shell/src/mesher.rs:627-628`, 99.5 % against the live oracle), and the
  remaining chain is deterministic from source on both sides.
