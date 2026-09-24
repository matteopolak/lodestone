# GUI item rendering

## What it is

How an item reaches a hotbar/inventory slot as pixels: the geometry and pose math for a 3-D block item's
isometric mini-block icon, the shell-side draw path that puts both flat sprites and 3-D icons on screen
for the hotbar and container screens, and the font-glyph fallback chain (vanilla bitmap sheets, then
Unifont HEX, then an embedded TrueType face) that renders the text drawn over those slots.

## How it works

### Item GUI geometry

A block item's inventory icon is a real baked 3-D model, not a picture of one — vanilla poses vanilla's
own block geometry into the slot with an isometric transform, and this renderer does the same. All
resolving and baking (item definition → resolved model → baked quads) happens once at asset-load time,
against the pack's `GuiItemContext` (the "gui" branch of an item's display-context selector, not the
in-hand branch some items — spyglass, trident, bundles — deliberately differ from). The baked quads reuse
the **same** stitched block atlas and tint palette the world terrain uses, so a hotbar icon and the block
in the world can never resolve to a different colour or texture, and the pass draws through the
*existing* model pipeline rather than a dedicated one.

The pose composes as `T(clamp(translation/16, ±5)) · R · S(clamp(scale, ±4)) · T(-0.5,-0.5,-0.5)`, where
the model is centred on the origin first, then scaled, then rotated, then translated — vanilla's own
composition order, and the reason the model's centre always lands exactly on the translation term
regardless of rotation. The `/16` unit conversion and both clamps belong at pose-application time, not in
the JSON parser, which stores the raw numbers verbatim.

A composite item whose second part carries its own per-part transform (every coloured bed, which is a
head model plus a foot model offset from it) is a known, disclosed gap: the offset field isn't parsed at
all, so only the first part bakes — concatenating both without the offset would stack them and z-fight,
which is worse than a partial bake.

### GUI item icons (the draw half)

The shell-side draw path is shared by both screens that have item slots (the hotbar and the
container/inventory screen) through one module, so there is exactly one atlas upload, one tint palette,
and one pipeline pair for both — not a copy per screen. Three kinds of icon part exist: a flat sprite (the
majority of items, one textured quad on a 2-D sprite pipeline), a 3-D block-item mesh (the baked geometry
above, posed and pre-transformed on the CPU into one shared vertex buffer so the whole hotbar is one draw
call), and a "special" item (chests, shulkers, skulls and similar block-entity-driven items with no baked
model at all — its own small `EntityPipeline` pass, sharing rig/sheet resolution with every other surface
that draws the same kind of item; see the held-items doc for that shared lookup).

Because only the 3-D model pass needs a depth attachment, and a render pass's attachments are fixed for
its lifetime, the icon draw is split into multiple passes even within one screen: sprites, 3-D models, and
overlay text/durability-bar content each get their own pass, in an order where the model pass runs first
(so nothing paints over it) and depth is **cleared**, not loaded — the world's own depth buffer is still
resident from the terrain pass and would otherwise swallow a GUI item sitting at a much shallower clip
depth. The container screen's carried (cursor-held) stack needs a **second** full stratum of the same
three passes, clearing depth again, matching vanilla's own explicit "next stratum" call before it draws
the carried item — without the second depth clear, a 3-D block already in a slot wins the depth test
against a 3-D block being carried on the cursor, regardless of draw order.

The GUI item pose needs a horizontal-axis-independent winding invariant, not a "positive determinant"
rule: the sign that matters is whichever sign the real world camera's own projection produces (which is
negative for this engine's projection convention), and getting this backwards produces an inside-out
block that still looks like a plausible isometric cube in a screenshot — a coverage-area pixel count alone
cannot catch it either, since a flipped icon's back faces project to the same silhouette; only a separate
per-face-brightness assertion (using the different per-face light constants) tells the two apart.

### GUI text and the font-glyph fallback chain

Vanilla's default font resolves a codepoint through an ordered chain of glyph providers, each one
claiming a codepoint range and yielding to whichever provider was declared earlier for a codepoint they
both cover. Three provider kinds are implemented, in vanilla's own priority order:

1. **Bitmap sheets** (`ascii`, `accented`, `nonlatin_european` PNGs) — the base Latin/Cyrillic/accented
   coverage, a few thousand codepoints.
2. **Unihex** (GNU Unifont's HEX format, loaded from a zip archive) — the broad fallback, taking total
   coverage from a few thousand codepoints to the bulk of the Basic Multilingual Plane (CJK, Hangul, Thai,
   Arabic, Cyrillic extensions, box drawing). Each glyph line is a codepoint plus a hex-digit bitmap whose
   *digit count* determines the glyph's pixel width (only two widths appear in vanilla's own file, but two
   more are legal and must be supported for a resource pack that uses them). A blank glyph is one column
   wider than its declared width, not zero-width — the empty-ink case trims to "one past the last real
   column" by construction, not to nothing.
3. **TTF** (an embedded TrueType/OpenType face named in a pack's font definition) — closes the remaining
   gap, chiefly astral-plane glyphs a resource pack supplies its own font for. Rasterisation goes through a
   pure-Rust, `no_std`-capable font library chosen specifically because it (and its own dependency) compile
   cleanly for the wasm target with no filesystem or clock access, unlike the alternatives considered.

All three kinds converge on one glyph representation with the same downstream consumer: the HUD's text
draw walks each glyph's ink as a coverage grid and emits merged run-length quads on the existing colour
stream, so none of the three provider kinds needs its own draw path, atlas, or pipeline. There is
deliberately **no GPU glyph atlas at all** for any of the three: the consumer emits quad coverage rather
than sampling a texture, so a lazily-populated atlas (vanilla's own approach) would be solving a problem
this renderer doesn't have, and the model shader is already at wgpu's guaranteed four-bind-group floor —
a fifth group for a font atlas would fail to validate on some real adapters.

Two per-glyph quantities (bold offset, shadow offset) differ between provider kinds and must be read per
*glyph*, never as one font-wide constant — a Unihex or TTF glyph's correct offset is smaller than a
bitmap-sheet glyph's, and applying the sheet's constant everywhere over-widens CJK/astral text.

A required font asset that is genuinely absent (no unifont archive fetched, a pack's TTF file missing) is
a deliberate **soft skip**: that provider silently contributes zero glyphs rather than failing the whole
font load, since these are additive coverage layers over a font that still works without them.

## How to change it

* **Never bake the display transform into block-local geometry.** The blockstate placement transform and
  an item's own display pose are two different things applied at two different times; conflating them
  breaks the world/GUI atlas sharing this whole design exists for.
* **Do not assert winding by a remembered determinant sign.** Derive the expected sign from a real
  camera's own projection matrix in the test, and pair any coverage-count assertion with a
  per-face-brightness assertion — coverage alone is blind to an inside-out mesh.
* **A new source of GUI icon geometry** (anything not reachable from an ordinary item definition) should
  seed its textures into the shared atlas at build time and bake through the same path, so it shares the
  atlas and palette rather than allocating its own.
* **A "special" item kind's rig/sheet lookup lives in one shared function**, used by every surface that
  draws that kind (GUI slot, first-person hand, dropped-in-world, worn, framed) — add a kind there once,
  not once per surface, or the GUI slot and the hand will disagree about what the item looks like.
* **Adding a new glyph-provider kind** means teaching the shared glyph-raster enum, its per-kind
  advance/bold-offset/shadow-offset accessors, and the provider-priority chain about it; the compiler's
  exhaustiveness check on the enum is what finds every call site that needs updating.
* **Tinted flat sprites, the enchantment glint on icons, and the incompletely-baked composite items (the
  beds) remain known, disclosed gaps** — check current source before assuming any of the three is fixed.

## Configuration

No runtime flags for the geometry or draw paths. Behaviour degrades based on what's attached: no item
atlas means no icons at all; no baked 3-D models means block items draw as an empty well rather than a
sprite; no font's glyph archive/file fetched means that provider silently contributes nothing. Font
archive fetching (`fetch-assets`) is the one explicit CLI entry point; everything else resolves through
the ordinary asset-discovery search path.

## Dependencies

* `lodestone-assets` — item definition resolution (`ItemIconBuilder`, `GuiItemContext`), model baking,
  the tint palette, and the font-loading/rasterisation stack (`FontLoader`, `RasterFont`, the glyph enum
  and its per-kind readers).
* `lodestone-render` — the baked item geometry (`BlockModels`), the GUI pose/projection math
  (`gui_item_pose`, `gui_ortho`), and the shared model pipeline both the world and the GUI icon pass use.
* `lodestone-shell` — the hotbar and container screens' shared icon-draw module, and the borrowed GPU
  resources (atlas view/sampler, tint palette buffer, animation buffer, depth view) it reuses from the
  world renderer rather than duplicating.
* External rasterisation crate for the TTF path (pure Rust, wasm-compatible, no filesystem/clock access);
  `zip` for reading the Unifont archive.
