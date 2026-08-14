# Item tint pipeline

## What it is

Resolving an item model's `tints` list — potion liquid colour, leather dye, map
colour, foliage constants — to a concrete ARGB that the item's tinted sprite
layer multiplies, and carrying that colour through the bake into the tint palette
the model shader reads. This is the *item* tint mechanism, which is unrelated to
and never consults the *block* tint mechanism in [`biome-tint.md`](./biome-tint.md).

## How it works

Three stages, in two crates.

**1. Parse** — `lodestone_assets::item_model::parse_tint` reads each entry of an
item definition's `tints` array into a `TintSource { kind, default, grass, index }`.
Parsing never evaluates anything: the values a tint needs (a stack's components, a
pack's colormap) are runtime state the parser does not own.

**2. Evaluate** — `lodestone_assets::item_tint::resolve` maps one `TintSource`
plus an `ItemTintContext` to a `ResolvedTint { argb, provenance }`. It covers all
eight of vanilla's registrations (`ItemTintSources.bootstrap`):

| JSON `type` | reads | jar (`net.minecraft.client.color.item`) |
|---|---|---|
| `minecraft:constant` | nothing | `Constant` |
| `minecraft:dye` | `minecraft:dyed_color` | `Dye` |
| `minecraft:grass` | the pack's `colormap/grass.png` | `GrassColorSource` |
| `minecraft:firework` | `minecraft:firework_explosion` | `Firework` |
| `minecraft:potion` | `minecraft:potion_contents` | `Potion` |
| `minecraft:map_color` | `minecraft:map_color` | `MapColor` (not `world.level.material.MapColor`, an unrelated homonym) |
| `minecraft:team` | the holder's team | `TeamColor` (not `world.scores.TeamColor`, an unrelated homonym) |
| `minecraft:custom_model_data` | `minecraft:custom_model_data` | `CustomModelDataSource` |

**3. Bake** — `lodestone_render::block_models::item_layer_tint_slots` resolves each
sprite layer's tint and interns the colour into `BlockModels::tint_palette`;
`extruded_sprite_geometry` stamps the resulting slot onto that layer's quads'
`tint_index`. `model.wgsl` then multiplies `palette.colors[tint_idx]` into the
sampled texel. No shader, pipeline, bind group or vertex format changed.

## What is *not* implemented, and why

**Per-stack tints do not reach pixels, because the components they need are
dropped at decode.** `ItemStack` here is a closed struct of known fields
(`crates/lodestone-model/src/item.rs`, alongside `ItemComponents`) rather than an
open component map, and a component this build does not model is not represented
at all. Of the six component-reading tint sources, `ItemComponents` carries
exactly one — `dyed_color`. So `potion_contents`, `map_color`,
`firework_explosion` and `custom_model_data` cannot be read, and those four
sources resolve to the item definition's own JSON `default`.

That is the correct colour for an uncustomised stack — it is vanilla's own
fallback for an absent component — and therefore right for the overwhelming
majority of what an inventory holds. It is wrong for a *customised* stack: a
custom-colour potion, a filled map with a `map_color`, a dyed firework star.
`TintProvenance::Unmodeled` records exactly this, so a caller can report on it
rather than silently presenting a guess as a measurement.

**Adding a per-stack tint is therefore not a render change.** It needs (a) the
component modelled in `ItemComponents` and decoded in `crates/protocol/v770`, then
(b) a per-*draw* channel, because a frame-shared palette slot cannot hold two
different potions' colours at once. `ModelVertex::tint_rgb_override` (vertex
location 4, `.a` as the override flag, read in `model.wgsl`'s `fs_main`) already
exists for exactly this shape and is currently hardcoded inert for items in
`crates/lodestone-render/src/models.rs`'s `mesh_item_quads`.

**Spawn-egg tints do not exist in 26.2 and there is nothing to implement.**
`SpawnEggItem` has no colour fields — its whole record declaration is the entire class
body — and `assets/minecraft/items/creeper_spawn_egg.json` carries no `tints` array.
The two historical background/highlight integers are gone from Java, from
`assets/**.json` and from `data/`; the colours are now pixels in per-mob textures
(`textures/item/creeper_spawn_egg.png`). Spawn eggs need no special handling
beyond ordinary untinted sprite rendering.

**The 2-D GUI slot path is wired now** — it was not, for long enough
that a white lily pad and white leaves were the shipped appearance. Both call
sites (`crates/lodestone-shell/src/hud/item_icon.rs`, in `draw_item_icon_counted`
and `draw_item_icon_popped`) now take their multiplier from `sprite_layer_tint`
instead of a literal opaque white.

**The fix was neither of the two options this doc previously suggested, and the
reason generalises.** The diagnosis was right — `hud_sprite.wgsl` has no transfer
functions, its atlas is `Rgba8UnormSrgb`, so `textureSample` returns linear light
and `tex * tint` is a **linear** multiply, and a raw ARGB there washes out. But
"add the gamma round-trip to that shader" costs a shader edit and two conversions
per fragment, and "pre-multiply with `fog::multiply_gamma`" **cannot be done at
all** from the CPU side: that function needs the *texel*, and the call site has
only the tint — the texel is not read until the fragment stage.

The correction is exact, needs no shader change, and is **texel-independent**,
which is why it belongs on the tint. Under sRGB's pure-power model:

```text
want:  linear_to_srgb(L_out) = texel * t   =>  L_out = (texel * t)^2.2
have:  L_out = srgb_to_linear(texel) * m   =        texel^2.2 * m
  =>   m = t^2.2 = srgb_to_linear(t)          (texel cancels)
```

So the vertex multiplier is `fog::srgb_to_linear_f32(channel_byte / 255)` per
channel, and a gamma-space multiply falls out of a linear-space shader for free.
Generally: when a per-fragment correction is a pure function of a per-vertex
value, push it to the vertex data rather than into the shader.

**That the round trip is real is settled by an observable, not an assumption.**
With an sRGB atlas and a *non*-sRGB target, an untinted `[1.0; 4]` sprite would
store `srgb_to_linear(texel)` — a mid-grey 128 landing at 55 — so every HUD sprite
in the game would be visibly dark. They are not, so the target is sRGB and the
multiply is in linear light. Note the consequence for gates: the headless gates in
`gpu/pixel_gates.rs` use `Rgba8Unorm` targets, which do **not** reproduce this
arithmetic. A pixel gate for a 2-D tint must use `Rgba8UnormSrgb` or it measures a
different composite than the player sees.

**What is still not live: a dyed stack.** `sprite_layer_tint` passes a default
`ItemTintContext` (no components, no colormap). That is vanilla's own answer for an
uncustomised stack — an undyed leather helmet's brown *is* the definition's `dye`
default — and `constant`, the majority source, reads nothing at all. The gap is a
wire-side one rather than a wiring one: `lodestone_game::item::ItemComponents`
defines no `minecraft:dyed_color` member (no `DYED_COLOR_COMPONENT` exists in that
crate), so the shell holds no live dye to pass. `grass` resolves to `None`
(untinted) without a colormap, which `item_tint::resolve` documents as the honest
degradation.

## How to change it, and the gotchas

**The colour multiply is in gamma space.** `srgb_to_linear(linear_to_srgb(rgb) *
tint * shade)`. Doing it in linear pulls every factor toward `1.0` and washes the
item out. `model.wgsl`'s `fs_main` already does this correctly and needed no change;
`fog::multiply_gamma` (`crates/lodestone-render/src/fog.rs`) is the CPU-side
equivalent. Measured on a real adapter over six channels of two items: the gamma
prediction sits **0.15–0.3/255** from the rendered byte and the linear prediction
**16–34/255** away.

**`minecraft:constant` spells its colour `value`, not `default`.** Seven of the
eight sources use `default`; `constant` alone uses `value` (`Constant`'s `MAP_CODEC`).
`parse_tint` read only `default`, so every constant item tint in the game parsed to
`None` and was discarded — the six leaves items, `vine`, `lily_pad`,
`filled_map`'s layer 0, `firework_star`'s layer 0, `wolf_armor`. Nothing failed:
a greyscale sprite rendered with the multiplicative identity is indistinguishable
from a sprite with no tint authored, which is why a white lily pad survived.

**Do not substitute the block tint table for the item tint list.** Vanilla's item
renderer never calls `BlockColors`; `CuboidItemModelWrapper.update` evaluates the
item definition's own list. The two agree for leaves (`0x48B518` either way) and
for `grass_block`, and **disagree** for `lily_pad` — item `0x71C35C` vs block
`LILY_PAD_IN_WORLD` `0x208030`. The agreement in the common cases is exactly why
the substitution looked correct. The item-model bake loop in `block_models.rs`
still derives tints from block identity via `vanilla_tint_kind` for `IconPart::Model`
items; that is a known remaining approximation, correct for the leaves and grass
cases that use it today.

**Item `minecraft:grass` is a fixed climate sample, not a biome-dependent tint.**
It interns through `TintPalette::intern` like any constant rather than going to
`GRASS_TINT_SLOT`, because the item definition names its own `temperature`/`downfall`
and an item in your hotbar does not change colour when you walk into a swamp. All
six vanilla files say plains, which is why this and the block path agree today —
but sample the climate the JSON asked for, not the plains constant.

**A multi-layer item cannot be verified by a per-pixel colour ratio.** `potion`'s
untinted layer1 (the glass bottle) extrudes at the same depth as its tinted layer0,
so depth ordering decides which layer owns a pixel. Measured: the pixel gate run
against `potion` reports *both* hypotheses wrong (`gamma_mae=0.263`,
`linear_mae=0.247`), which is the signature of measuring the wrong geometry rather
than a colour-space bug. Verify per-layer assignment at the **bake** level, where
the layers are still distinguishable, and reserve the pixel gate for single-layer
subjects.

**An unknown tint type applies nothing rather than white.** White is the
multiplicative identity and so indistinguishable from "handled";
`item_tint::is_known` is what separates "a pack used a type we have never heard of"
(worth a `BlockModels::item_bake_misses` note) from "we know this type and there was
nothing to apply". A pack with no `colormap/grass.png` likewise gets no tint rather
than vanilla's loud magenta out-of-range fallback — zero with no vanilla pack is the
honest degradation.

## Configuration

None. No env vars, features or flags. The tint data comes entirely from the pack
stack's `assets/<ns>/items/*.json` and `textures/colormap/grass.png`.

## Dependencies

- `lodestone_assets::item_model` — the `tints` parse.
- `lodestone_assets::tint::Colormap` — the grass colormap sampler, shared with the
  block tint path.
- `lodestone_model::item::ItemComponents` — the component seam; the reason most
  sources resolve to a default.
- `lodestone_render::block_models` — the palette intern and the sprite bake.
- `crates/lodestone-render/src/shaders/model.wgsl` — the gamma-space multiply.

## Gates

`crates/lodestone-render/tests/item_tint_pixels.rs`, all `#[ignore]`d and
fail-closed (a missing adapter or jar is a failure, never a skip):

```
cargo test -p lodestone-render --test item_tint_pixels -- --ignored --nocapture
```

- `the_item_definitions_own_tints_reach_the_baked_palette` — jar, no GPU. Palette
  slots hold the jar constants exactly; `potion`'s two layers land in two distinct
  slots, one of them untinted.
- `an_all_white_palette_is_the_untinted_frame` — the negative control, rendered
  and counted rather than described.
- `an_item_tint_multiplies_in_gamma_space_at_the_jars_colour` — renders the same
  frame twice (real palette, all-white palette) and requires the ratio to land on
  the gamma prediction and sit far from the linear one, per channel.

Plus eleven unit gates in `crates/lodestone-assets/src/item_tint.rs`, including one
asserting every jar-derived default against the decompiled integer.
