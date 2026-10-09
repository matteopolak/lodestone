# GUI item rendering

## What it is

How an item reaches a hotbar or inventory slot as pixels: the pose math for a 3-D block item's isometric icon, the shell draw path for flat sprites and 3-D icons on the hotbar and container screens, and the font-glyph fallback chain (bitmap sheets, then Unifont HEX, then an embedded TrueType face) behind the text drawn over those slots.

## How it works

### Item GUI geometry

A block item's icon is a real baked 3-D model posed into the slot. Resolving and baking (item definition, resolved model, baked quads) happens once at asset load against the pack's `GuiItemContext` (the "gui" display branch, which some items such as spyglass, trident and bundles deliberately differ on). The quads reuse the **same** stitched block atlas and tint palette as world terrain, so a hotbar icon and the world block cannot differ in colour or texture, and they draw through the existing model pipeline.

The pose is `T(clamp(translation/16, ±5)) · R · S(clamp(scale, ±4)) · T(-0.5,-0.5,-0.5)`: centre, scale, rotate, translate, so the centre always lands on the translation term. The `/16` conversion and both clamps belong at pose-application time; the JSON parser stores raw numbers. A composite whose second part has its own per-part offset (every coloured bed: head plus foot) is a disclosed gap: the offset is not parsed, so only the first part bakes (concatenating without the offset would z-fight).

### The draw half

One shell module serves both the hotbar and the container screen, so there is one atlas upload, one tint palette and one pipeline pair. Three icon part kinds: a flat sprite (one quad on a 2-D sprite pipeline), a 3-D block mesh (posed on the CPU into one shared vertex buffer, so the whole hotbar is one draw), and a "special" item (chests, shulkers, skulls and other block-entity items with no baked model: its own small `EntityPipeline` pass, sharing the rig/sheet lookup with every surface drawing that kind; see [Held items](held-items.md)).

Only the model pass needs depth and attachments are fixed per render pass, so each screen splits into passes: sprites, 3-D models, then overlay text and durability bars, with the model pass first and depth **cleared** rather than loaded (the world's depth buffer is still resident and would swallow a shallow GUI item). The container's carried (cursor) stack needs a **second** full stratum of the same passes with another depth clear; otherwise a 3-D block in a slot wins the depth test against the carried one regardless of draw order.

The pose needs a winding invariant derived from the real world camera's projection (negative for this engine's convention), not a remembered determinant sign. Getting it backwards yields an inside-out block that still looks like a plausible cube and has identical coverage area; only a per-face-brightness assertion tells them apart.

### GUI text and the font-glyph chain

A codepoint resolves through ordered glyph providers; an earlier provider wins where ranges overlap. Three kinds are implemented, in the reference priority order:

1. **Bitmap sheets** (`ascii`, `accented`, `nonlatin_european` PNGs): a few thousand Latin/Cyrillic/accented codepoints.
2. **Unihex** (GNU Unifont HEX from a zip): the bulk of the Basic Multilingual Plane. A glyph line is a codepoint plus a hex bitmap whose digit count sets the pixel width (two widths in the stock file, two more legal for packs). A blank glyph is one column wider than its declared width, not zero-width.
3. **TTF** (a face named in a pack's font definition): astral-plane glyphs. Rasterised by a pure-Rust `no_std`-capable library chosen because it compiles for wasm with no filesystem or clock.

All three converge on one glyph representation; the HUD text draw walks each glyph's ink as a coverage grid and emits merged run-length quads on the existing colour stream, so no provider needs its own draw path, atlas or pipeline. There is deliberately **no GPU glyph atlas**: the consumer emits quad coverage, and the model shader is already at wgpu's guaranteed four-bind-group floor, so a fifth group would fail on some adapters. Bold offset and shadow offset differ per provider and must be read per glyph (Unihex and TTF offsets are smaller than a bitmap sheet's; one font-wide constant over-widens CJK). A genuinely absent font asset (no unifont archive, a missing TTF) is a **soft skip**: the provider contributes zero glyphs rather than failing the font load.

## How to change it

- Never bake the display transform into block-local geometry: the blockstate placement transform and an item's display pose apply at different times, and conflating them breaks world/GUI atlas sharing.
- Derive expected winding from a real camera's projection matrix in the test and pair any coverage-count assertion with a per-face-brightness assertion.
- New GUI icon geometry not reachable from an item definition should seed its textures into the shared atlas at build time and bake through the same path.
- A "special" item kind's rig/sheet lookup lives in one shared function used by GUI slot, first-person hand, dropped, worn and framed; add a kind there once.
- A new glyph-provider kind means extending the shared glyph-raster enum, its per-kind advance/bold/shadow accessors and the priority chain; enum exhaustiveness finds the call sites.
- Known gaps (check source before assuming fixed): tinted flat sprites, the enchantment glint on icons, incompletely baked composite items (beds).

## Configuration

No runtime flags. Degradation follows what is attached: no item atlas means no icons; no baked 3-D models means block items draw as an empty well; a missing font archive/file contributes nothing. `fetch-assets` is the one explicit entry point for fetching font archives.

## Dependencies

- `lodestone-assets`: `ItemIconBuilder`, `GuiItemContext`, model baking, tint palette, font loading and rasterisation (`FontLoader`, `RasterFont`).
- `lodestone-render`: `BlockModels`, `gui_item_pose`, `gui_ortho`, the shared model pipeline.
- `lodestone-shell`: the shared icon-draw module and the atlas view/sampler, tint palette, animation buffer and depth view it borrows from the world renderer.
- A pure-Rust TTF rasteriser and `zip` for the Unifont archive.
