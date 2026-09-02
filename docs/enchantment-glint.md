# Enchantment glint

## What it is

The shimmering foil overlay an enchanted item carries: a scrolling, rotated,
additively-blended pass over the item's **own geometry**, using vanilla's
`enchanted_glint_item.png`. This doc covers the render substrate —
`crates/lodestone-render/src/glint.rs` and
`crates/lodestone-render/src/shaders/glint.wgsl` — plus what still has to happen in
`lodestone-shell` for a player to see it.

## How it works

Four things, in order.

**1. The gate.** `glint::has_foil(&ItemComponents)` — vanilla's
`ItemStack.hasFoil()` is
`enchantment_glint_override ?? !ENCHANTMENTS.isEmpty()`.

**2. The clock and the offsets.** `glint::glint_clock` and `glint::glint_offsets`
implement `TextureTransform.setupGlintTexturing`:

```
m     = (long)(millis * glintSpeed * 8.0)      glintSpeed default 0.5
u_off = (m % 110000) / 110000                  scrolls NEGATIVE
v_off = (m %  30000) /  30000                  scrolls POSITIVE
```

**3. The matrix.** `glint::glint_texture_matrix` composes
`T(-u_off, +v_off, 0) · Rz(10°) · S(scale) · A(atlas)`, where `scale` is `8.0` for
items, `0.5` for the trident/shield special models and `0.16` for worn armour. The
shader applies it as `(M * vec4(uv, 0, 1)).xy`, exactly vanilla's `glint.vsh`.

`A(atlas)` is ours and vanilla has nothing corresponding to it. Read the next
section before changing it — it is the difference between a shimmer and a wash.

## `A(atlas)`: vanilla's scale is not a scale in any unit an item has

`setupGlintTexturing`'s `8.0` multiplies an **atlas** UV. It is therefore
`8.0` *of the whole sheet*, and what a player actually sees is how many glint
texels land across a sprite:

```
glint texels across a sprite = scale · glint_size · sprite_px / atlas_px
```

The atlas size is inside that number, so vanilla's constants were chosen against
vanilla's own packing. That size is written down nowhere in the jar — it is
whatever `Stitcher` produces — so it was recovered by porting `Stitcher`
(`smallestFittingMinTexel`'s slot rounding, the `-height, -width, name` sort, the
`expand`/`Region.add` shelf split) and running it over vanilla's own
`atlases/items.json` and `atlases/blocks.json` sprite lists at the default mip
level 4 with anisotropy off, i.e. `padding = 1 << 4 = 16`. **Both come out
2048×2048** — items from 860 sprites, blocks from 1278 — which is
`glint::VANILLA_ATLAS_PX`. So a 16-px item sprite receives
`8.0 · 128 · 16 / 2048 = 8` glint texels.

Our stitched model sheet is a single 4096×4096, so an uncorrected `Scale::Item`
puts **4** texels there instead — and four texels of a 128-px sheet stretched
over an item is a flat, uniform brightening rather than a moving pattern.
Measured on a dropped `diamond_sword`: a 4.634-texel window against vanilla's
9.262 (both are axis-aligned bounding boxes, widened from 4 and 8 by the matrix's
10° rotation).

Two consequences worth keeping:

- **The sheet is not a fixed size.** The stitcher's gutter is
  `1 << mipmapLevels`, so the packing — and every baked UV — follows a video
  setting. Before that gutter landed our sheet was 1024×2048 and the window was
  17.1 × 10.7 texels, near enough vanilla's that nobody looked. Without
  `A(atlas)` the shimmer's scale moves when the player drags the slider.
- **It is a parameter, never a constant.** There is more than one sheet here: the
  world and hand glints sample the stitched model atlas, the GUI icon glint
  samples the `ItemAtlas`, and they are different sizes. `glint_texture_matrix`
  and `GlintUniform::new` take the dimensions of the atlas whose UVs *that draw's*
  vertices carry, and all three call sites pass their own. The correction is
  applied innermost so vanilla's uniform scale and its rotation both act on
  vanilla-equivalent coordinates, and both axes are corrected separately because
  neither sheet is guaranteed square.

The gate is `crates/lodestone-render/tests/glint_pixels.rs`'s
`the_real_jar_glint_texture_produces_a_varying_pattern`, which asserts the derived
texel count and predicts the composited delta range for both hypotheses from
`enchanted_glint_item.png`'s own bytes. Its predecessor thresholded the spread at
a round `0.02`, which is a property of how much of the sheet the item covers
rather than of the glint, and so passed while the atlas was small, failed when the
gutter landed, and went on failing after the correction — vanilla's window is
smaller than the one that threshold had been calibrated against.

**4. The pass.** `glint::GlintPipeline` draws the **same vertex and index buffers**
the model pass drew, with depth `EQUAL` / no write / zero bias, culling off, and
`glint::glint_blend()`.

## Which pipeline it belongs to, and the bind-group count

**Its own, and it spends 2 of wgpu's 4 groups.**

The model pipeline (which draws every item form — GUI icon, dropped, held) is
already at wgpu's portable `max_bind_groups` floor of **4**: camera / atlas /
palette / anim. It cannot take a fifth group; a 5-group shader validates on an
adapter reporting 8 (this M5) and fails pipeline creation on any 4-group adapter,
i.e. a startup crash for other people and never for us.

The glint needs none of that budget, because **the floor is per-pipeline and the
glint is a separate pipeline**. `GlintPipeline` declares:

| group | contents |
|---|---|
| 0 | `GlintUniform` — `view_proj`, `tex_matrix`, and `origin_and_alpha` |
| 1 | the glint texture + its sampler |

Even inside that, `GlintAlpha` and the section origin are **folded into the group-0
uniform** rather than given bindings of their own — the same reasoning that folded
fog into the camera uniform. So nothing about this change moves the model pipeline
off 4, and the glint pipeline has two spare groups if it ever needs them.

For contrast: the entity pipeline spends only 2 of 4, so a glint drawn there would
also have had room. It is not drawn there — items go through the model pipeline
(`crates/lodestone-shell/src/gpu/world_items.rs`'s own module doc comment), and the glint has to
rasterise the same positions as whatever pass it overlays.

## What still needs doing for a player to see it

**This is a substrate, not pixels in the running game.** The pass is proven to
composite correctly on a real adapter (see *Gates*), but the production draw call
is recorded in `lodestone-shell`, which is where the remaining work is:

1. ~~**Set `ItemIcon.enchanted` for real.**~~ **Done.** The hotbar producer
   at `crates/lodestone-shell/src/app/redraw.rs` now fills it from
   `item_icon::stack_has_foil`, which delegates to
   **`glint::has_foil_enchantments`** — a sibling of `has_foil` added because the
   shell cannot call `has_foil` at all: shell stacks are
   `lodestone_game::item::ItemStack`, whose components are an opaque
   `BTreeMap<Identifier, ComponentValue>` and a *different type* from the
   `lodestone_model::item::ItemComponents` `has_foil` takes. The sibling keeps this
   crate the single owner of the predicate instead of the shell re-spelling
   `!list.is_empty()` far from the caveats above.

   Two producers are deliberately still `false`. `app/recipe_panel.rs`'s
   `toast_icon` has **no stack at all** — its only input is an `Identifier` — and a
   recipe-unlock toast depicts an item *type*, which vanilla also draws without
   glint; do not plumb a stack in for symmetry. `container/builder.rs` is the
   container-screen producer and *does* have a stack: it wants the same one-line
   change, and was left alone only because it sat outside the wiring task's file
   ownership.
   `container/builder.rs` is now wired too, from the same predicate.
2. ~~**Record the second pass.**~~ **Done for the two world/hand sites.**
   - First-person held item — `gpu/first_person.rs`, in the hand's own pass.
   - Dropped items and mobs' held items — `gpu/frame.rs`, in the main pass right
     after the base item draw. `prepare_item_geometry` returns a *second* mesh
     carrying only the enchanted items' quads, merged from the same
     `dropped_item_mesh` output so the two cannot diverge (depth-`EQUAL` rejects
     any divergence silently). The foil flag rides `EntityFacts::foil` ->
     `TrackedStack` -> `EntityDraw::foil`, the same path `count` takes; see
     `dropped-items.md`.

   Each site has its **own** group-0 uniform buffer
   (`GlintPass::uniform_buffer` for the hand, `world_uniform_buffer` for the
   world). That is not tidiness: `queue.write_buffer` is ordered against the
   *submit*, not against the encoder, and the two draws are in different passes of
   one submit — one buffer written twice hands both passes the last value and the
   shimmer lands nowhere.

   ~~**The 2-D GUI icon site is still open**~~ **Done.** Hotbar cells,
   inventory slots, container slots and the carried (cursor) stack all glint now,
   through a *second* glint pipeline in the shell — see "The 2-D GUI glint is its
   own pipeline" below.
3. ~~**Load the texture.**~~ **Done.** `crate::resources::load_glint_texture`
   reads `glint::textures::ITEM` out of the jar and `RenderState::install_glint`
   uploads it **non-sRGB** (see the gotcha below) with `glint::glint_sampler`.
   `glint::textures::ARMOUR` is still unused — armour glint needs the armour pass
   to grow a second rasterisation the same way the item pass just did.

## The 2-D GUI glint is its own pipeline

Flat sprites are the majority of what a hotbar holds (every sword, tool and ingot),
so the 3-D hook covers only block and chest icons. Those flat quads go through
`hud_sprite.wgsl`, not the model pipeline, and `GlintPipeline` **cannot be reused**
there: it mandates a `depth_format` and depth-`EQUAL`, and its vertex layout is
`ModelVertex`, whereas `draw_sprites_range` records into a caller-owned pass with
**no depth attachment** and an 8-float `[x, y, u, v, r, g, b, a]` stream.

So there is a second, shell-side pipeline: `item_icon::GuiGlint` plus
`crates/lodestone-shell/src/shaders/hud_glint.wgsl`. Everything *numeric* still
comes from `lodestone_render::glint` — `glint_texture_matrix`, `Scale::Item`,
`DEFAULT_SPEED`/`DEFAULT_STRENGTH`, `glint_blend`, `glint_sampler` — so there is
still exactly one owner of the maths and the blend.

Four things about it, each of which costs a design cycle to rediscover:

- **The mask comes from the item atlas, not from depth.** Without depth-`EQUAL` or a
  stencil, a glint quad over the item's rect would paint glint across the sprite's
  *transparent* pixels too — a sword's glint filling its whole 16×16 cell. The
  pass therefore binds **two** textures and discards where the item atlas's alpha
  is below vanilla's own `glint.fsh` threshold of `0.1`. Two textures in one group
  (bindings 0–3) plus the uniform group: 2 of 4 bind groups, same as the 3-D one.
- **The glint UV is the atlas UV, scaled by `A(atlas)`.** An earlier revision of this
  doc claimed it had to be the quad's local `0..1` coords; that is wrong, and
  `glint.wgsl`'s own comment (transcribed from `glint.vsh`) says so: vanilla's
  `texCoord0 = (TextureMat * vec4(UV0, 0, 1)).xy` takes the *baked model's* UVs,
  which for a `item/generated` item are atlas UVs. Using local coords would give
  every item the same phase; atlas UVs give each sprite its own, which is what the
  game looks like. The one thing that is *not* fed straight in is the sheet's own
  size: this pass samples the `ItemAtlas`, not the stitched model atlas, so it
  passes its own dimensions — see `A(atlas)` above.
- **The glint quads are a separate stream, not a flag.** `IconSink::glint` collects
  a copy of each enchanted stack's sprite quad. The blend differs (`SRC_COLOR/ONE`
  versus `ALPHA_BLENDING`) so it must be a separate draw, a separate draw needs a
  contiguous range, and the enchanted quads are *not* contiguous inside the sprite
  stream — one enchanted sword among nine hotbar cells would otherwise need nine
  ranges.
- **The carried stack needs its own split.** `ContainerGeometry` records
  `slot_glint_vertex_count` alongside `slot_item_vertex_count` for the same reason:
  the cursor's stack replays every stream in a later stratum, and a glint drawn in
  the *slot* pass would be painted over by the carried sprite in the pass after it,
  which looks exactly like "the glint doesn't work" rather than like an ordering
  bug.

It has **its own uniform buffer**, like each of the other two sites, for the reason
given above: `queue.write_buffer` is ordered against the submit, not the encoder.

Still worth wiring: `menu/options.rs` already carries live `glintSpeed` (0.5) and
`glintStrength` (0.75) sliders whose values match
`glint::DEFAULT_SPEED`/`DEFAULT_STRENGTH`, so feeding them into `GlintUniform::new`
and `gui_glint_uniform` is behaviour-preserving today and correct once a player
moves a slider.

## How to change it, and the gotchas

**The blend is `SRC_COLOR/ONE`, which is the source *squared*.**
`BlendFunction.GLINT` is
`(SRC_COLOR, ONE, ZERO, ONE)` — so colour is `dst += src²` and the destination
alpha is left completely untouched. It is **not** `BlendFunction.TRANSLUCENT`
and **not** `BlendFunction.ADDITIVE`; both are the obvious
guess and both are wrong. Measured: with the blend neutered to `ADDITIVE`, the gate
below flips from `mae=0.00082` to `mae=0.207` while the ADDITIVE prediction goes
from `0.199` to `0.00105`.

One useful consequence: **no alpha enters the colour equation**, so this repo's
measured warning about `ALPHA_BLENDING`'s effective alpha being an unpredictable
function of the fragment alpha on this Metal backend does not apply. The composited
byte here *is* exactly predictable.

**`CompareOp.EQUAL` is the one ported depth comparison that does not flip.** Our
depth is reversed-Z like vanilla's, so no ported
`GREATER_THAN_OR_EQUAL` becomes `LessEqual` and every positive depth bias becomes
negative. Equality is orientation-independent, so `EQUAL` ports across unchanged.

**Depth-`EQUAL` with zero bias means the glint pass must rasterise byte-identical
clip positions.** That is why `glint.wgsl` recomputes `clip` with the same
`view_proj`, the same `section_origin` add and the same order of operations as
`model.wgsl`, and why `GlintPipeline` consumes `ModelVertex::vertex_layout()`
unchanged so it can be handed the *same* vertex buffer. Re-meshing, or any
divergence in the position maths, z-fails the entire pass — which draws **nothing**
and is indistinguishable from "the glint is not implemented".

**Upload the glint texture as `Rgba8Unorm`, not `Rgba8UnormSrgb`.** Vanilla is not
colour-managed: its `texture(Sampler0, uv)` yields the raw byte over 255 with no
transfer function. An sRGB upload silently linearises the sheet and makes the
shimmer darker than the game's.

**`REPEAT`/`LINEAR` is derived, not chosen.** `withTexture("Sampler0", …)` passes a
`null` sampler (`RenderSetup.withTexture`), so the sampler comes from the
texture's own `.mcmeta`, which is `{"texture":{"blur":true}}` with no `clamp` —
`ReloadableTexture.apply` maps that to `REPEAT` + `LINEAR`, no mipmaps.
`CLAMP_TO_EDGE` would smear one edge texel across the whole item as soon as the
scroll offset carried a UV past 1.0.

**`glintStrength` defaults to `0.75`, not `1.0`** (`Options.glintStrength` field). Treating
it as `1.0` is 33% too bright — a *magnitude* error of exactly the kind that shipped
a hurt overlay here at 70% red where vanilla renders 30%.

**The composition order is `T · Rz · S`, and JOML makes it easy to invert.**
Vanilla's fluent chain is `new Matrix4f().translation(…).rotateZ(…).scale(…)`;
`translation` **sets** while `rotateZ`/`scale` **post-multiply**. Reading the chain
left-to-right as the application order gives `S · Rz · T`, which multiplies the
scroll offset by 8 and pushes the glint off the texture — and with `REPEAT` sampling
that still produces *a* moving pattern, so it looks plausible in a screenshot.

**The glint is drawn once, not twice.** The historical two-layer trick is gone; what
remains is a single translation with **two different periods on the two UV axes**
(110000 on `−U`, 30000 on `+V`). Drawing it twice doubles the brightness.

**Armour is not rotated differently.** The historical `-50°` is gone; every glint
pass rotates `+10°`. Armour differs only in scale (`0.16`), in using
`enchanted_glint_armor.png`, and in `ARMOUR_VIEW_OFFSET_SCALE` — a model-view scale
by `1 - 1/4096` toward the camera, the only glint type with a layering transform.

**There is no `enchanted_glint_entity.png`.** Two textures exist, not three; the
entity path reuses the item one.

## What the gate cannot cover, and the honest shortfall

`ItemComponents` models `enchantments` but **not**
`enchantment_glint_override`, so:

- an ordinary enchanted item glints correctly — the common case;
- a stack that explicitly suppresses its glint with
  `[minecraft:enchantment_glint_override=false]` glints anyway;
- the seven items whose glint comes *only* from a baked
  `ENCHANTMENT_GLINT_OVERRIDE=true` do **not** glint: `enchanted_golden_apple`,
  `experience_bottle`, `written_book`, `nether_star`, `enchanted_book`,
  `end_crystal`, `debug_stick` (`Items.ENCHANTED_GOLDEN_APPLE`, `Items.EXPERIENCE_BOTTLE`,
  `Items.WRITTEN_BOOK`, `Items.NETHER_STAR`, `Items.ENCHANTED_BOOK`,
  `Items.END_CRYSTAL`, `Items.DEBUG_STICK`).

That last group is **not** fixable by decoding harder. The override is a *prototype*
component baked into `Item.Properties`, so a clientbound stack carries no mention of
it at all — exactly like `max_stack_size`. It needs an item-prototype census behind
the version seam. Note also that `enchanted_book`'s enchantments live in
`STORED_ENCHANTMENTS`, which vanilla's `isEnchanted` deliberately does not read, so
it would not glint even with the override modelled by way of the enchantments list.
`minecraft:compass` has a code-level override (`LODESTONE_TRACKER` present means
foil, `CompassItem.isFoil`) and is likewise out of reach.

Zero glinting items with no vanilla pack is the honest degradation, matching how
armour, wool and flame counters behave; there is no synthetic-texture fallback.

## Configuration

Two vanilla options, both plumbed as parameters rather than read from anywhere:
`glintSpeed` (`glint::DEFAULT_SPEED`, `0.5`) and `glintStrength`
(`glint::DEFAULT_STRENGTH`, `0.75`). Neither is wired to our own options screen
yet.

## Dependencies

- `crates/lodestone-render/src/shaders/glint.wgsl` — the shader.
- `crate::models::ModelVertex` — the shared vertex layout that makes depth-`EQUAL`
  viable.
- `lodestone_model::item::ItemComponents` — the foil gate's input.
- `glam` for the matrix, `bytemuck` for the uniform.

## Gates

Unit gates in `crates/lodestone-render/src/glint.rs` (15, no GPU): every jar
constant, the truncating clock, the two distinct periods, the real-time wrap
periods derived from the option default, the `T · Rz · S` composition against its
reversed hypothesis, `U` negative / `V` positive, the blend factors against both
wrong guesses, depth-`EQUAL` unflipped, and the uniform's `std140` size.

GPU gates in `crates/lodestone-render/tests/glint_pixels.rs`, all `#[ignore]`d and
fail-closed:

```
cargo test -p lodestone-render --test glint_pixels -- --ignored --nocapture
```

Measured on a real Metal adapter:

| test | result |
|---|---|
| `the_glint_pass_composites_with_the_src_color_blend` | `GLINT` mae **0.00082**, `ADDITIVE` **0.199**, `TRANSLUCENT` **0.194** over 5734 channels |
| `the_glint_is_confined_to_the_items_own_silhouette` | 2038 pixels changed inside, **0** outside |
| `suppressing_the_glint_pass_leaves_the_frame_byte_identical` | two glint-less frames differ in 0 pixels; adding the pass changes 2038 |
| `the_real_jar_glint_texture_produces_a_varying_pattern` | 128x128 confirmed; mean delta 0.016, sd 0.0064, range [0.005, 0.033] |

The blend test uses a **uniform synthetic** glint texture on purpose: predicting a
byte from the real PNG would mean replicating the scroll matrix, wrap and bilinear
filter, i.e. predicting the code under test with a second copy of it. A known
constant source makes the prediction depend only on the jar's blend and strength
constants. The real asset is exercised separately, by the one property a uniform
texture structurally cannot show — that the added light **varies** across the
silhouette rather than being a flat wash.
