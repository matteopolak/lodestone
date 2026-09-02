# Colour and tint

## What it is

The one rule that governs every colour operation in this renderer — vanilla is not
colour-managed, so tint, shade, fog and text all multiply and blend in **gamma**
(sRGB byte) space, never linear — and the concrete pipelines built on it: biome
tint, item tint, the enchantment glint, world-space text colour/lighting/blending,
and the menu background blur.

## How it works

### The gamma-space rule, and why a linear mistake is easy to ship

Every tint or shade multiply in this codebase is
`srgb_to_linear(linear_to_srgb(rgb) * tint * shade)` — the texel is converted to
sRGB, multiplied there against the tint/shade byte, then converted back. Doing the
multiply directly in the shader's native linear light instead pulls every factor
toward `1.0` and visibly washes the result out; the divergence is largest against a
dark background and vanishes near white, because black and white are the two fixed
points where gamma and linear agree. This shows up in three distinct places that
each cost a real bug once: the block/model/fluid shaders' tint and directional
shade; `fog::multiply_gamma`/`scale_gamma`, the CPU-side twin used for fog and void
fog; and world-space text's drop shadow (a flat quarter taken in gamma space, not a
linear blend).

A related but separate hazard is **which view a pass renders into**. A native
swapchain here is an sRGB view over the render target, so ordinary `ALPHA_BLENDING`
makes the hardware decode the destination byte, blend in linear light, and
re-encode — which is correct for most passes (they want colour-managed blending)
but wrong for the handful that composite vanilla's own flat gamma-byte colours
directly onto the framebuffer, as covered below.

### Biome tint (grass, foliage, dry foliage, water)

Grass/foliage/water quads resolve a real, **position-blended** colour rather than a
fixed plains default. The data (`lodestone-assets::tint`) is a 66-entry table of
each biome's temperature/downfall/water colour/optional colormap overrides,
transcribed from the real biome JSON, looked up by a compile-time first-byte
bucketing of the sorted table (measured faster than a `binary_search_by` here,
because a plain string-length-first comparison beats `memcmp`-based ordering for
short probe strings — the details are counter-intuitive enough that changing either
lookup strategy needs re-measuring). The actual colour is vanilla's own box average
(`ClientLevel.calculateBlockTint`, a `(2·radius+1)²` average of the *already
colormap-resolved* colour, radius 2 — this client has no biome-blend-radius
setting, so `2` is the only value ever reached), computed with a sliding
row-cursor that reuses 20 of 25 samples between adjacent cells and must stay
bit-exact (integer, floored once at the end) with vanilla's own division placement.

Because the frame-shared tint palette can only hold one colour per slot for the
whole frame, it cannot represent "grass in a desert" and "grass in a swamp"
simultaneously — the real per-position colour instead rides an **additive**
per-vertex field (`ModelVertex::tint_rgb_override`, an rgb triple plus an override
flag), computed once per quad at mesh time and left at its default (unset) for every
caller that supplies no biome view, which keeps every non-block-mesher consumer
(GUI items, headless tests) unaffected. The live biome-name lookup comes off the
server's own registry sync (not a hardcoded jar-derived id table), so a data pack's
renamed or reassigned biome resolves correctly; an unresolvable id or an empty
registry falls back to the plains default rather than rendering untinted — those two
failure modes look identical on a plains world and need an instrumented probe
(`LODESTONE_TINT_PROBE`) to tell apart from a screenshot alone. The
mangrove-swamp/swamp noise-based colour variation is the one known unported detail;
64 of 66 biomes are unaffected.

### Item tint

Three stages: **parse** an item model's `tints` array into a source description
(`TintSource`, one of vanilla's eight registered kinds — constant, dye, grass,
firework, potion, map colour, team, custom model data); **evaluate** one source
against a live stack's context into a resolved ARGB; **bake** each sprite layer's
resolved colour into the shared tint palette, the same one the block mesher uses,
for the item definition's own default appearance. Because that palette is one
colour per slot for the whole frame, a **fourth** step re-resolves per instance
wherever a live stack actually varies the colour — the flat 2-D GUI icon
re-resolves and writes straight into the sprite's vertex colour, and every 3-D draw
(dropped item, thrown potion, held item, an item in a mob's hand) stamps the live
colour onto the same `tint_rgb_override` field the biome tint uses, keyed off the
`(palette slot, TintSource)` pairs baked alongside the item's geometry.

Only `dye` and `potion` currently have a typed, decoded component to read (a stack
here is a closed struct of known fields, not an open component map); `map_color`,
`firework_explosion` and `custom_model_data` still resolve to the item definition's
own JSON default, which is the *correct* fallback for an uncustomised stack and
wrong only for a customised one (a coloured map, a dyed firework star).
Spawn-egg tints need no work at all in 26.2 — the two-tone background/foreground
colours are gone from the game entirely; every spawn egg is a pre-coloured PNG with
no `tints` array.

The 2-D GUI icon path needed one further, texel-independent correction: its target
is an sRGB view sampling an sRGB atlas, so an ordinary `texel * tint` there is a
**linear** multiply and visibly washes out a coloured icon. Because the correction
is a pure function of the tint byte alone (not of the texel, which isn't available
until the fragment stage), it is folded into the vertex-side tint value itself —
`srgb_to_linear(tint_channel)` — rather than into the shader, which needs no change.

### Enchantment glint

The shimmering foil overlay draws over an item's own geometry, scrolling and
rotating vanilla's `enchanted_glint_item.png` and blending additively. Its own
pipeline (not the model pipeline's) spends only 2 of wgpu's 4 bind groups, leaving
the model pipeline's own 4-group floor untouched. The blend is `SRC_COLOR/ONE` —
colour is `dst += src²`, alpha untouched — which is neither the obvious
`ADDITIVE` nor `TRANSLUCENT` guess and is fully predictable (no alpha enters the
colour equation, so this pipeline is not subject to this backend's usual
unpredictable `ALPHA_BLENDING` behaviour). Depth uses a bare `EQUAL` test with zero
bias and no write, so the glint pass must recompute byte-identical clip positions
to the pass it overlays — any divergence z-fails the whole pass silently, which
looks exactly like "the glint isn't implemented".

Vanilla's own scroll-speed/scale constants are chosen against **vanilla's own
atlas size**, so porting them verbatim onto a differently-sized stitched atlas
changes how many glint texels land across a sprite — too few and the shimmer
becomes a flat, uniform brightening instead of a moving pattern. The correction
(`A(atlas)`) rescales the texture matrix by the ratio of vanilla's atlas dimensions
to this renderer's own, applied per draw site (world/hand items sample the model
atlas; GUI icons sample a differently-sized item atlas), because there is more than
one sheet and neither is guaranteed square. A second, shell-side glint pipeline
exists purely for flat 2-D GUI icons, since those quads go through a completely
different pipeline with no depth attachment to test equality against — it masks the
glint against the item atlas's own alpha instead of depth.

Not modelled: `enchantment_glint_override`, a *prototype* flag baked into an item's
server-side definition rather than carried on the clientbound stack, so seven items
whose glint comes only from that flag (enchanted golden apple, experience bottle,
written book, nether star, enchanted book, end crystal, debug stick) never glint
here regardless of their actual enchantments; this needs an item-prototype census
behind the version seam, not a decode fix.

### World-space text: colour, gamma blending, and lighting

**Colour.** Server-authored text arrives in one of three shapes depending on the
surface: a legacy `§`-coded string (chat, the action bar), a component tree already
expanded into spans (the scoreboard, tab list, kick screen), or — historically, for
about ten of seventeen text-drawing surfaces — a plain string handed to a
"plain-draw" path that had no way to apply `§` codes at all, because there is no
non-decomposing string draw in vanilla to be faithful to (`Font.drawInBatch` always
applies legacy codes at draw time). Every one of those ten now decomposes. The
sixteen legacy colours and the `TextColor`-carrying path share one Rust-side
lookup table so they cannot disagree about what a named colour means, but a legacy
`§`-coded string structurally cannot carry a hex colour at all — flattening a
component tree to `§` codes before drawing is where a server's hex colour dies,
regardless of how correct the renderer is downstream. Nametags and `text_display`
carry hex colours through per-run today; sign text still renders every line in one
uniform dye colour, because the world-storage layer for sign text discards a
message's formatting at parse time, one layer above the renderer.

**Gamma-space blend.** Entity nametags, sign text and `text_display` share one flat-
colour shader with no texture at all, and every colour any of them submits is a raw
vanilla gamma byte (a nametag's translucent black plate, a sign's dye scaled by a
fixed factor for an unlit side, a display's background colour). Compositing those
through this renderer's ordinary sRGB swapchain view — which decodes, blends
linearly and re-encodes — disagrees with vanilla's direct byte multiply everywhere
except pure black, which is the one fixed point. Because a `wgpu` render pass fixes
one attachment format for every pipeline drawn inside it, these three passes need
their **own** render pass over a raw (non-sRGB) view of the same target, encoded
right alongside the ordinary block/entity passes — not a renderer-wide format
change, which was tried once and broke every other flat-colour stream sharing that
pass.

**Lighting.** The same three passes multiply vanilla's lightmap texel into every
vertex colour they emit, so a sign in a dark room reads dark and a glowing one
does not — until this was wired, none of the three sampled any lightmap at all, so
`has_glowing_text` had no visible effect in the one situation it exists for.
Vanilla samples the lightmap in the *vertex* stage (`Color * sample_lightmap(...)`),
so folding the light byte into the vertex colour on the CPU before upload is the
same arithmetic at the same rate as vanilla's, with no shader change. The
**see-through** render variant of each pass (an occluded nametag, a `FLAG_SEE_THROUGH`
display) samples no lightmap at all in vanilla and is full-bright by construction,
regardless of what light value its submission carries — reading the submission's
light argument rather than which shader it selects is the trap here, because the
argument really is passed and really is discarded downstream.

### Menu background blur

Vanilla blurs whatever is already on screen behind most menu screens (a six-pass
separable box blur) before drawing that screen's own widgets on top; this client
had the accompanying dim wash but not the blur. The pass captures the frame's
texture once right after acquire, then runs the box filter (bilinear-expanded so
one tap covers two texels) at the **live** blur-radius option, skipping the whole
pass entirely at radius `0` — vanilla's own gate, not an optimisation, since a
zero-radius box filter is exactly the identity convolution. Whether a screen blurs
is a separate axis from whether it dims (`MenuFrame::blur` vs. `MenuBackdrop::Dim`)
— vanilla's real fork is whether the screen is "in-game UI" (a container screen
dims but does not blur) versus an overlay like Pause or in-world Options (both), so
each screen builder sets the two flags independently rather than one implying the
other.

## How to change it, and the gotchas

- **Never substitute the block-tint table for an item's own tint list**, and vice
  versa — vanilla's item renderer never calls the block colour resolver. The two
  agree for leaves and disagree for e.g. a lily pad, so a substitution looks correct
  on the common cases and is wrong on a real one.
- **An item's `minecraft:grass` tint is a fixed climate sample from the item
  definition's own JSON, not a biome-position lookup** — an item in your hand does
  not turn green when you walk into a swamp.
- **A multi-layer item cannot be verified by a whole-frame pixel-colour ratio** — two
  stacked layers (e.g. a potion's tinted liquid under its untinted glass) compete for
  the same pixels by depth order, so verify per-layer tint assignment at the bake
  level and reserve a pixel gate for single-layer subjects.
- **An unknown tint or glint source applies nothing, never white/a loud fallback
  colour** — white is the multiplicative identity and indistinguishable from
  "handled", so use the type's own `is_known`/similar predicate to tell "we've never
  heard of this type" from "we know it and there was nothing to apply".
- **The glint's depth-`EQUAL` pass and the pass it overlays must recompute clip
  position with the identical vertex layout, origin add and operation order** — a
  re-mesh or any divergence there fails the whole overlay silently.
- **Prefer `Vec<TextSpan>`/`Text::to_spans` over a legacy `§` string for any new
  text-carrying surface** — flattening to `§` first is lossy at the call site in a
  way nothing warns you about, and never add a "plain, cannot carry a code" string
  path back to the vanilla font: every `String` that came off the wire can carry
  one.
- **A world-text pixel gate that builds an `Rgba8Unorm` target cannot see the
  gamma-blend fix at all** — the raw and corrected views are the same format there,
  so the divergence only reproduces against the real `Bgra8UnormSrgb` production
  surface format.
- **Adding a fourth flat-colour world-text pass**: decide the see-through question
  from which render type vanilla selects, not from the light argument passed to it
  — the two disagree, by design.
- **Install both arms of any per-scene render-source switch** (e.g. a first-person
  hand suppressor) — a source installed only for one scene leaks into whichever
  scene runs after it, with nothing red anywhere to catch it.

## Configuration

- No env vars or feature flags anywhere in this cluster.
- `Options::menu_background_blurriness` (`0..=10`, default `5`, `0` = off) —
  persisted, driven from both the Video and Accessibility settings screens.
- `glint::DEFAULT_SPEED` (`0.5`)/`DEFAULT_STRENGTH` (`0.75`) are plumbed as
  parameters but not yet read from the live `glintSpeed`/`glintStrength` options on
  the settings screen.
- `options.chat.color` (`false` strips colour from chat only, matching vanilla —
  it has no effect on the sidebar or MOTD).

## Dependencies

- `lodestone_assets::tint` — biome effects table, the vanilla box-average kernel.
- `lodestone_assets::item_model`/`item_tint` — the `tints` parser and per-source
  evaluator.
- `lodestone_render::block_models`/`models` — the shared tint palette, per-vertex
  override field, and glint texture-matrix/blend constants.
- `lodestone_model::{Text, TextSpan, TextStyle, TextColor}` — the component model
  and the single legacy-colour table every draw path shares.
- `lodestone-shell`'s `gpu/{nametag,sign_text,display_text}.rs`, `hud/item_icon.rs`,
  and `menu/render/blur.rs` — the live draw sites for world text, item icons, and
  the menu blur respectively.
- The real 26.2 client jar/decompile for every constant this doc cites.
