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
`ItemStack.hasFoil()` (`ItemStack.java:968-971`) is
`enchantment_glint_override ?? !ENCHANTMENTS.isEmpty()`.

**2. The clock and the offsets.** `glint::glint_clock` and `glint::glint_offsets`
implement `TextureTransform.setupGlintTexturing` (`TextureTransform.java:31-38`):

```
m     = (long)(millis * glintSpeed * 8.0)      glintSpeed default 0.5
u_off = (m % 110000) / 110000                  scrolls NEGATIVE
v_off = (m %  30000) /  30000                  scrolls POSITIVE
```

**3. The matrix.** `glint::glint_texture_matrix` composes
`T(-u_off, +v_off, 0) · Rz(10°) · S(scale)`, where `scale` is `8.0` for items,
`0.5` for the trident/shield special models and `0.16` for worn armour. The shader
applies it as `(M * vec4(uv, 0, 1)).xy`, exactly vanilla's `glint.vsh`.

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
(`crates/lodestone-shell/src/gpu/world_items.rs:1-16`), and the glint has to
rasterise the same positions as whatever pass it overlays.

## What still needs doing for a player to see it

**This is a substrate, not pixels in the running game.** The pass is proven to
composite correctly on a real adapter (see *Gates*), but the production draw call
is recorded in `lodestone-shell`, which is where the remaining work is:

1. **Set `ItemIcon.enchanted` for real.** The field exists
   (`crates/lodestone-shell/src/hud/item_icon.rs:124`) and every constructor
   hardcodes `false` — `crates/lodestone-shell/src/app/redraw.rs:160` and
   `crates/lodestone-shell/src/app/recipe_panel.rs:224`. `glint::has_foil` is the
   predicate to call.
2. **Record the second pass.** Wherever an item is drawn, re-bind
   `GlintPipeline` and re-draw the same buffers: dropped items at
   `crates/lodestone-shell/src/gpu/frame.rs:667-688`, first-person held at
   `crates/lodestone-shell/src/gpu/first_person.rs:711-735`, GUI icons at
   `crates/lodestone-shell/src/hud/item_icon.rs:1735-1743`.
3. **Load the texture.** `glint::textures::ITEM` /
   `glint::textures::ARMOUR`, uploaded **non-sRGB** (see the gotcha below) with
   `glint::glint_sampler`.

The 2-D GUI *sprite* path is a separate problem: it draws flat quads through
`hud_sprite.wgsl`, not the model pipeline, so it needs either its own glint
pipeline over the same quads or a switch to the 3-D path for foiled stacks.

## How to change it, and the gotchas

**The blend is `SRC_COLOR/ONE`, which is the source *squared*.**
`BlendFunction.GLINT` (`BlendFunction.java:8`) is
`(SRC_COLOR, ONE, ZERO, ONE)` — so colour is `dst += src²` and the destination
alpha is left completely untouched. It is **not** `TRANSLUCENT`
(`BlendFunction.java:10-12`) and **not** `ADDITIVE` (`:17`); both are the obvious
guess and both are wrong. Measured: with the blend neutered to `ADDITIVE`, the gate
below flips from `mae=0.00082` to `mae=0.207` while the ADDITIVE prediction goes
from `0.199` to `0.00105`.

One useful consequence: **no alpha enters the colour equation**, so this repo's
measured warning about `ALPHA_BLENDING`'s effective alpha being an unpredictable
function of the fragment alpha on this Metal backend does not apply. The composited
byte here *is* exactly predictable.

**`CompareOp.EQUAL` is the one ported depth comparison that does not flip.** Our
depth is `[0,1]` DirectX-style rather than vanilla's reversed-Z, so every ported
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
`null` sampler (`RenderSetup.java:138-141`), so the sampler comes from the
texture's own `.mcmeta`, which is `{"texture":{"blur":true}}` with no `clamp` —
`ReloadableTexture.java:24-29` maps that to `REPEAT` + `LINEAR`, no mipmaps.
`CLAMP_TO_EDGE` would smear one edge texel across the whole item as soon as the
scroll offset carried a UV past 1.0.

**`glintStrength` defaults to `0.75`, not `1.0`** (`Options.java:867-874`). Treating
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
  `end_crystal`, `debug_stick` (`Items.java:1122`, `:1471`, `:1481`, `:1557`,
  `:1571`, `:1609`, `:1697`).

That last group is **not** fixable by decoding harder. The override is a *prototype*
component baked into `Item.Properties`, so a clientbound stack carries no mention of
it at all — exactly like `max_stack_size`. It needs an item-prototype census behind
the version seam. Note also that `enchanted_book`'s enchantments live in
`STORED_ENCHANTMENTS`, which vanilla's `isEnchanted` deliberately does not read, so
it would not glint even with the override modelled by way of the enchantments list.
`minecraft:compass` has a code-level override (`LODESTONE_TRACKER` present means
foil, `CompassItem.java:29-31`) and is likewise out of reach.

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
