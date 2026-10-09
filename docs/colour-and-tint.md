# Colour and tint

## What it is

The rule behind every colour operation in the renderer, and the pipelines built on it. Vanilla is not colour-managed, so tint, shade, fog and text multiply and blend in gamma (sRGB byte) space, never linear. Covered here: biome tint, item tint, the enchantment glint, world-space text, and the menu background blur.

## How it works

### The gamma-space rule

Every tint or shade multiply is `srgb_to_linear(linear_to_srgb(rgb) * tint * shade)`. Multiplying in linear light pulls every factor toward `1.0` and washes the result out; the error is largest on dark backgrounds and vanishes near white.

It applies in three places: block/model/fluid shader tint and directional shade; `fog::multiply_gamma` and `scale_gamma` (the CPU twin for fog and void fog); and world-text drop shadow (a flat quarter in gamma space).

The native swapchain is an sRGB view, so ordinary `ALPHA_BLENDING` decodes the destination, blends linearly and re-encodes. That is right for most passes and wrong for those compositing vanilla's flat gamma-byte colours (world text, below).

### Biome tint

Grass, foliage, dry foliage and water quads get a position-blended colour. `lodestone_assets::tint` holds a 66-entry biome table (temperature, downfall, water colour, colormap overrides). Lookup uses compile-time first-byte bucketing, which measured faster than `binary_search_by`; re-measure before changing it.

- The colour is a `(2r+1)^2` box average of already colormap-resolved colour with `r = 2` (there is no blend-radius option). A sliding row cursor reuses 20 of 25 samples and must stay bit-exact with vanilla's integer division placement (floored once at the end).
- The frame-shared tint palette holds one colour per slot, so the per-position colour rides an additive per-vertex field, `ModelVertex::tint_rgb_override` (rgb plus override flag), set at mesh time. It is unset for callers with no biome view (GUI items, headless tests).
- Biome names come from the server's registry sync, so renamed data-pack biomes resolve. An unresolvable id or empty registry falls back to plains, which looks identical to success on a plains world; set `LODESTONE_TINT_PROBE` to print per-section resolved and skipped counts to stderr.
- Not ported: the noise-based colour variation of swamp and mangrove swamp (2 of 66 biomes).

### Item tint

1. **Parse** an item model's `tints` array into a `TintSource` (constant, dye, grass, firework, potion, map colour, team, custom model data).
2. **Evaluate** a source against a live stack into ARGB.
3. **Bake** each sprite layer's colour into the shared tint palette for the item's default look.
4. **Re-resolve per instance** where a stack varies the colour. The 2-D GUI icon writes straight into the sprite's vertex colour. Every 3-D draw (dropped, thrown, held, in a mob's hand) stamps `tint_rgb_override`, keyed by the `(palette slot, TintSource)` pairs baked with the geometry.

Only `dye` and `potion` have a typed component to read. `map_color`, `firework_explosion` and `custom_model_data` resolve to the definition's JSON default, which is wrong only for a customised stack. Spawn eggs need nothing: they are pre-coloured PNGs.

The 2-D icon target is an sRGB view sampling an sRGB atlas, so `texel * tint` there is a linear multiply. Because the fix depends only on the tint byte, it is applied to the vertex tint (`srgb_to_linear(channel)`), not the shader.

### Enchantment glint

The foil overlay scrolls and rotates `enchanted_glint_item.png` and blends additively on its own pipeline (2 of 4 bind groups).

- Blend is `SRC_COLOR/ONE`: `dst += src^2`, alpha untouched. This is neither `ADDITIVE` nor `TRANSLUCENT`.
- Depth is a bare `EQUAL` test with no bias or write, so the pass must recompute byte-identical clip positions to the pass it overlays. Any divergence z-fails the whole overlay silently, which reads as "glint not implemented".
- Vanilla's scroll and scale constants assume vanilla's atlas size. The texture matrix is rescaled by the ratio of vanilla's atlas dimensions to ours (`A(atlas)`), per draw site, because the model atlas and GUI item atlas differ and are not square.
- Flat 2-D GUI icons use a shell-side glint pipeline that masks by the item atlas's alpha, since they have no depth attachment.
- Not modelled: `enchantment_glint_override`, a prototype flag not carried on the stack. Seven items glint only through it (enchanted golden apple, experience bottle, written book, nether star, enchanted book, end crystal, debug stick). Fixing it needs an item-prototype census behind the version seam.

### World-space text

**Colour.** Text arrives as a legacy `§` string (chat, action bar), a component tree expanded to spans (scoreboard, tab list, kick screen), or a plain string. Every surface now decomposes `§` codes. The sixteen legacy colours and the `TextColor` path share one lookup table.

A `§` string cannot carry hex, so flattening a component tree to `§` before drawing is where a server's hex colour is lost. Nametags and `text_display` carry hex per run; sign text still draws one dye colour per line because sign storage drops formatting at parse time.

**Gamma blend.** Nametags, sign text and `text_display` share one untextured flat-colour shader whose inputs are raw gamma bytes. Blending through the sRGB view disagrees with vanilla everywhere except pure black. A `wgpu` pass fixes one attachment format, so these three have their own render pass over a raw (non-sRGB) view of the same target. A renderer-wide format change breaks other flat-colour streams.

**Lighting.** The three passes multiply the lightmap texel into each vertex colour on the CPU, the same arithmetic as vanilla's vertex-stage sample. The see-through variant (occluded nametag, `FLAG_SEE_THROUGH` display) is full-bright regardless of the light value submitted; decide from the render variant, not the light argument.

### Menu background blur

Most menu screens blur the frame behind them (six-pass separable box blur, bilinear-expanded). The frame is captured once after acquire and filtered at the live blur radius; radius `0` skips the pass entirely, which is identity anyway.

- Blur and dim are independent axes (`MenuFrame::blur`, `MenuBackdrop::Dim`). A container screen dims without blurring; Pause and in-world Options do both. Each screen builder sets both.
- The menu renderer borrows the surface texture for the frame. Keep the order: acquire, blur submission, `MenuRenderer::end_frame` after overlays, then present.
- Blur copies from the swapchain, which is a copy source only on request (on Metal a copyable swapchain cannot use display-only storage). `MenuRenderer::wants_frame_copy` reports whether the last overlay blurred, and `WindowApp::redraw` calls `SurfaceTarget::set_copy_source` before acquiring. The first frame of a new blurred overlay draws unblurred once.

## How to change it

- Never substitute the block-tint table for an item's tint list, or the reverse. They agree for leaves and differ for a lily pad.
- An item's `minecraft:grass` tint is a fixed sample from the definition JSON, not a biome lookup.
- Verify multi-layer items (a potion's tinted liquid under glass) per layer at bake level. A whole-frame pixel ratio cannot separate layers competing by depth.
- An unknown tint or glint source applies nothing. White is the multiplicative identity and looks handled; use the type's `is_known` predicate to tell unknown from nothing-to-apply.
- Prefer `Vec<TextSpan>` and `Text::to_spans` over a `§` string for new text surfaces, and never add a plain string path to the vanilla font that cannot carry a code.
- A world-text pixel gate built on `Rgba8Unorm` cannot see the gamma-blend fix; only the production `Bgra8UnormSrgb` format reproduces it.
- For a new flat-colour world-text pass, decide see-through from vanilla's render variant.
- Install both arms of any per-scene render-source switch (such as the first-person hand suppressor), or the source leaks into the next scene.

## Configuration

- `Options::menu_background_blurriness`: `0..=10`, default `5`, `0` disables. Persisted; set from the Video and Accessibility screens.
- `glint::DEFAULT_SPEED` (`0.5`) and `DEFAULT_STRENGTH` (`0.75`) are parameters, not yet read from the settings screen's glint options.
- `options.chat.color`: `false` strips colour from chat only.
- `LODESTONE_TINT_PROBE`: biome-tint diagnostics.

## Dependencies

- `lodestone_assets::tint`, `item_model`, `item_tint`: biome table, box-average kernel, `tints` parser and evaluator.
- `lodestone_render::block_models` and `models`: shared tint palette, override field, glint constants.
- `lodestone_model::{Text, TextSpan, TextStyle, TextColor}`: component model and the single legacy-colour table.
- `lodestone-shell`: `gpu/{nametag,sign_text,display_text}.rs`, `hud/item_icon.rs`, `menu/render/blur.rs`.
