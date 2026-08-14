# Vanilla HUD text

## What it is

Every string the HUD draws — chat, the F3 overlay, titles, the action bar, the
scoreboard, the tab list, stack counts — rendered as **vanilla's proportional
`default` font** with its 1 px drop shadow, instead of the shell's fixed-advance
5×7 debug bitmap.

Two halves:

- **Metrics + pixels** — `lodestone-assets::font`. `Font`/`FontLoader` (advances,
  provider chain) existed since the first commit; `RasterFont`/`GlyphRaster` were
  added to expose the *coverage* of each glyph cell, which is what a renderer
  needs and what nothing had.
- **Drawing** — `lodestone-shell::hud::vanilla_font::VanillaFont`.

`lodestone_assets::font` was one of this repo's longest-lived **islands**: complete,
tested, and consumed only by its own tests. `RasterFont` is the seam that closed it.

## How it works

### Load

`VanillaFont::shared()` resolves `minecraft:default` once per process from the same
`client.jar` the block/GUI/item atlases come from, via a `OnceLock`. It is
**fail-open**: a jar-less run (headless gates, the demo world) gets `None` and every
caller keeps the debug font. `HudRenderer::new` calls it, so no `attach_*` call and
no GPU resource is involved — unlike the atlases, a font needs neither.

Pack discovery in `vanilla_font::pack_root` duplicates `resources.rs`'s rule
(`LODESTONE_ASSETS`, else the highest-sorting complete `.cache/mc/<version>` under
any ancestor of the cwd) because `resources::vanilla_manager` is `#[cfg(test)]`.
Dropping that attribute and calling it is the right end state.

### Metrics

Nothing here invents a width. `FontLoader` derives each glyph's advance from the
**rightmost non-transparent column of its sheet cell** plus one, exactly as
vanilla's `BitmapProvider.getActualGlyphWidth` does (it tests the alpha channel).
`RasterFont::load_raster` then decodes each referenced sheet once, so ink and
advance come from the same cell **by construction** rather than by coincidence.

Measured against the real 26.2 jar: 2414 codepoints, 3 bitmap sheets (ascii,
accented, nonlatin_european), `i`/`l`/`W` = 2/3/6 logical px.

Vertical placement is `GlyphBitmap.getTop() == 7 - ascent`
(`metrics::BEARING_TOP_BASE`), measured from the **top of the line**, not the
baseline. The ascii sheet (`ascent: 7`) sits flush; the accented sheet
(`ascent: 10`) hangs 3 px above.

### Draw

Glyph coverage is emitted as **quads on the HUD's existing colour stream**
(`item_icon::ColourStream`), run-length merged along each row — an 8×8 cell costs at
most 8 quads instead of 64, pixel-identically, since every texel in a run shares one
colour.

No font atlas, no texture upload, no fifth bind group. This matters: the model
shader is already at wgpu's 4-bind-group floor (see `CLAUDE.md`), and text is the
one HUD element whose colour is per-draw rather than per-texel. A textured path
would be fewer vertices but needs a new pipeline, a new upload, and a new attach
point in `app.rs`.

The shadow is **two passes over the whole (already decoded and bidi-reordered)
glyph list**: the offset copy first, at 25 % of the colour, then the text.
Whole-list-first is what keeps a following glyph's ink on top of the previous
glyph's shadow, matching vanilla's two-layer batch. The offset is **per glyph**
(`Font::shadow_offset`, looked up in `draw_resolved`) — `SHADOW_OFFSET` (1
logical px on both axes) for a sheet glyph, half that for a unihex one — not
one constant added before either pass began; see
[Unihex glyphs](./unihex-font-glyphs.md).

`shadow_of` takes the quarter in **gamma space** (`ARGB.scaleRGB(color, 0.25F)` →
`0xFF3F3F3F`, 63/255). The HUD's colour convention is sRGB 0..1 written verbatim
(`hud::legacy_rgb` divides vanilla's hex codes by 255), so the quarter is taken
directly on those floats. In linear space the shadow would land at ~54 % on screen —
a grey outline instead of vanilla's near-black one.

### Styling

`resolve_legacy` (the `§`-coded path `draw_legacy`/`text_legacy` use — chat, the
action bar, the held-item name, container tooltips once they exist) tracks a
`GlyphStyle { bold, italic, underline, strikethrough, obfuscated }` across the
run alongside the existing colour tracking, with the same reset rule
`lodestone-model`'s `apply_legacy_code` already uses: a colour code or `§r`
clears every flag, not just the one it names
(`crates/lodestone-model/src/text.rs`), and resolves each character into a
`ResolvedGlyph` rather than drawing it immediately — decoding and drawing are
now two separate passes, with `bidi_reorder_glyphs` (UAX #9) running between
them. `draw_resolved` then draws each glyph via `glyph_styled`, which — unlike
the plain path's `glyph` — also emits the underline/strikethrough bar and
reports a bold-adjusted advance, so it runs for **every** resolved character,
ink or not (a space still needs its underline segment and its bold-widened
advance).

All five rules are transcribed from `.cache/mc/26.2/client-src`, not
invented:

- **Bold** (`BakedSheetGlyph.renderChar`):
  redraw the *same* glyph a second time, offset `+metrics::BOLD_OFFSET` in x —
  not a font-weight variant. Applies independently to the shadow pass and the
  main pass (each gets its own doubled draw), which falls out for free here
  because the two passes are already two separate `draw_resolved` calls with
  different per-glyph `(x, y)`. The advance also grows by `BOLD_OFFSET`
  (`GlyphInfo.getAdvance(bold)`) for *every* glyph,
  drawable or not.
- **Italic** (`BakedSheetGlyph.shearTop`/`shearBottom`,
  both `1.0F - 0.25F * v`): vanilla shears a
  glyph as one quad with two sheared edges — a continuous linear function of
  `v`, the edge's logical-pixel offset from the line's top. This renderer
  draws ink as per-texel-row quads instead of one quad, so `draw_ink`
  evaluates that same affine function **per row**, at the row's own centre
  (`r.top() + (ty + 0.5) * texel_size()`), rather than only at the two glyph
  edges. `metrics::ITALIC_SHEAR` (`1.0`) and the newly-added
  `metrics::ITALIC_SHEAR_SLOPE` (`0.25`) are the formula's two constants. For
  the ordinary ascii sheet (`up = 0`, `down = 8`) this resolves to the top row
  shifting `+1` px and the bottom row `-1` px — a **2 px** lean across an 8 px
  glyph, not the 1 px the old doc comment here used to say (fixed alongside
  this change).
- **Underline / strikethrough** (`Font.PreparedTextBuilder.accept`): a
  `metrics::EFFECT_THICKNESS`-tall (`1.0` px) bar per glyph, from that glyph's
  pen position to `pen + advance`, extended `metrics::EFFECT_LEAD_IN` (`1.0`
  px) further left **only for the first glyph of the run**
  (`effectX0 = position == 0 ? x - 1.0F : x`). Strikethrough's bar bottom sits
  at `metrics::STRIKETHROUGH_Y` (`4.5`) logical px below the line's top;
  underline's at `metrics::UNDERLINE_Y` (`9.0`) — two different constants, not
  one "draw a line" helper parameterised by a boolean that happens to share a
  y.
- **Obfuscated** (`Font.getGlyph`, and `FontSet`'s
  `glyphsByWidth`): every draw call swaps in a
  **same-width-class** replacement codepoint's pixels — width class is
  `ceil(original_advance)` — while the *advance* stays the original
  codepoint's. `VanillaFont::obfuscation_pool` builds that width→codepoints
  map once at load time, restricted to codepoints this renderer can actually
  draw (vanilla's own pool also includes the non-rasterisable `space`
  provider; including it here would occasionally "obfuscate" a glyph into
  invisible whitespace, so it is left out — a small, documented divergence).
  `VanillaFont::obfuscation_rng` is a free-running `AtomicU64` advanced once
  per obfuscated glyph, mirroring `Font.random`
  (`RandomSource.create()`, **never reseeded**) — every frame's
  draw call advances the same stream further, which is what makes `§k` read as
  continuously animated with no timer anywhere. Space is never a candidate for
  replacement (`codepoint != 32`, in `Font.getGlyph`) and never receives one
  either, since it has no raster to begin with.

### Wiring, and why it is not an island

`HudRenderer::render_with_item_models` is the single `HudGeometry::build_inner` call
site in the renderer, and it passes `self.font`. `render()` delegates to it, so
**every** HUD render path — `app.rs::WindowApp::redraw`, `gpu.rs`, `scoreboard.rs`, `tablist.rs` —
gets vanilla text with no call-site change.

Crucially, **measurement and drawing read the same field**. `Builder::text_width` /
`Builder::legacy_width` pick the proportional or the fixed measure to match whatever
`Builder::text` will actually draw with, so a centred or right-aligned string can
never be laid out against a font other than the one that renders it. Every layout
site was converted from the free `text_w` to `b.text_width`.

## How to change it, and the gotchas

- **Do not make the font a hard requirement.** The headless and demo paths have no
  jar, and `hud/item_icon.rs`'s pixel gates assert against the fixed-width fallback.
  `HudGeometry::build` stays jar-free and byte-deterministic on purpose; use
  `build_with_font` when you want vanilla text from pure geometry.
- **Bold, italic, underline, strikethrough and obfuscated draw real geometry**,
  in `VanillaFont::resolve_legacy`/`glyph_styled`/`draw_ink` — see
  [Styling](#styling) below. This used to be the module's one documented gap: the
  metrics existed (`Font::advance_bold`, `metrics::ITALIC_SHEAR`) and
  `Font::legacy_width` already zero-widthed `§k`/`§l`/`§m`/`§n`/`§o` correctly for
  *layout*, but the draw side dropped every flag on the floor — a styling flag
  parsed but never applied at draw time, the exact island shape `CLAUDE.md` names
  for FANCY clouds, the skull renderer and `FluidRenderer`. `run()` (the plain
  `&str` path `draw`/`draw_plain` use for titles, the XP number, etc.) is
  deliberately untouched — a Rust `&str` can never carry a `§` code, so giving it
  the styled glyph path would cost every unstyled draw a pool lookup for nothing.
- **`unihex` rasterises; `ttf` still does not.** See
  [Unihex glyphs](./unihex-font-glyphs.md) — the short version is that CJK, Thai,
  Arabic, Hangul and most of the BMP now draw real glyphs, and the shell's
  `draw_ink` needed **no change** because a unihex glyph resolves to the same
  `GlyphRaster` a sheet cell does (at `texel_size` 0.5 instead of 1.0). What still
  boxes: astral-plane emoji and anything only a `ttf` provider would supply. Two
  traps live there and not here: it needs the **asset-object store** above the jar
  (the jar's `font/include/unifont.json` is an empty stub), and bold/shadow offsets
  are **per glyph** — 0.5 for a unihex glyph, 1.0 for a sheet one, and both
  `VanillaFont::glyph_styled` (bold) and `draw_resolved` (shadow) now look the
  offset up per glyph rather than assuming the sheet default for the whole string.
- **Right-to-left runs are reordered for display; shaping is not.**
  `VanillaFont::draw`/`draw_legacy`/`draw_spans`/`draw_plain` all decode into a
  `Vec<ResolvedGlyph>` first (`resolve_legacy`/`resolve_spans`), then run
  `bidi_reorder_glyphs` — the Unicode Bidirectional Algorithm (UAX #9), via the
  `unicode-bidi` crate — over that already-decomposed list before any glyph is
  drawn, matching vanilla's `Language.getVisualOrder` (which likewise reorders a
  decomposed `FormattedCharSequence`, never a raw `§`-coded string). This gets
  codepoint order right — an Arabic or Hebrew run now lays out right-to-left among
  surrounding LTR text — but does **not** select Arabic's per-position joining
  forms (isolated/initial/medial/final), so a reordered run draws its
  isolated-form glyphs rather than a cursively joined one. `bidi_reorder_glyphs`
  is a cheap no-op for pure-ASCII input (checked before touching `unicode_bidi` at
  all), which is the overwhelming majority of HUD strings.
- **`§` pairs are zero-width in both fonts.** `hud::strip_legacy` exists so the
  fallback measure does not over-count by two characters per code and push centred
  lines left.
- **The gates are pixel gates for a specific reason.** A font with every character
  correct and every *width* wrong satisfies `assert_eq!` on the source string, on the
  glyph count, on the vertex count, and on "did text draw". The defect is a property
  of the geometry *between* glyphs. Never replace these with content assertions.
- **`VanillaFont::draw` always adds vanilla's automatic 1px shadow — do not use it
  for a hand-rolled outline.** A live player report on the XP bar's level number
  ("too big and too high") traced to `hud::sprite_vitals` drawing that digit at
  `scale = 2.0` with a `text()`/`draw()` call, when vanilla's own
  `ContextualBar.extractExperienceLevel` draws it at
  scale 1, **five** times with `shadow = false` on every call: four ±1px-offset
  black copies (the outline) then one green copy — not vanilla's usual
  single-shadow text. `VanillaFont::draw_plain` / `Builder::text_plain` exist for
  exactly this: the unshadowed pass `AbstractContainerScreen.extractLabels`'s
  container labels also use. Reaching for `text()` for a hand-rolled outline
  layers an unwanted extra shadow under it.
- **A HUD row's y-offset from its neighbour is not a font-metrics quantity.**
  The same XP fix: the level number used to sit `(GLYPH_H + 2) * scale` above the
  bar — a value from font metrics that has nothing to do with vanilla's real
  gap, which is a flat `6` logical px fixed by `ContextualBar`'s own two
  constants (`top = guiHeight - 24 - 5`, text `y = guiHeight - 24 - 9 - 2`).
  Get the real vanilla offset from the decompile and derive it from the
  sibling element's own on-screen position (`by - 6.0`, not a restated
  constant) — the same "moving anchor" rule as the rest of this HUD cluster
  (see `CLAUDE.md`).

## Verification

```bash
cargo test -p lodestone-assets --test vanilla_font_metrics -- --ignored --nocapture
cargo test -p lodestone-shell  --test vanilla_font_pixels  -- --ignored --nocapture
cargo test -p lodestone-shell --lib \
  hud::tests::xp_level_number_is_the_right_size_and_the_right_distance_above_the_bar \
  -- --ignored --nocapture
# Styling (issue #117) — no GPU adapter needed, only the jar; see that
# module's own doc comment for why these are `#[ignore]`d anyway.
cargo test -p lodestone-shell --lib hud::vanilla_font::styling_tests -- --ignored --nocapture
# The held-item name consumer (issue #126), including the #117 tie-in.
cargo test -p lodestone-shell --test held_item_name_pixels -- --ignored --nocapture
```

Both fail closed: a missing jar is a failure, not a skip. The pixel gate asserts
`HudRenderer::font_attached()` **before measuring anything** — without that, a
missing jar silently degrades to the debug font and every "text drew something"
assertion below would still pass.

### What the pixel gate discriminates

Two hypotheses, both named as constants so a reader can see which one the assertion
separates:

| hypothesis | 10×`i` vs 10×`W` span | ratio |
|---|---|---|
| proportional (vanilla) | 40 px vs 120 px | `PROPORTIONAL_RATIO` = 3.000 |
| fixed 6 px advance (debug font) | 114 px vs 118 px | `FIXED_ADVANCE_RATIO` = 1.035 |

The band is ±8 % around 3.0, which excludes the unfixed value by a factor of ~2.7.
The control ratio is not exactly 1.0 only because the debug `i` bitmap is inset one
column on the left; the advances are identical.

Also asserted:

- **Per-glyph advance read off the framebuffer.** For a probe `"<c>W"`, `W`'s ink
  begins at column 0 of its cell, so the second run of main pixels starts exactly
  `advance(c) * scale` px along. Measured: `i` 2, `l` 3, `I` 4, `t` 4, `f` 5, `W` 6,
  `M` 6, `~` 7.
- **The shadow is an offset copy, not a blur** — set equality, not "the region is
  darker". The shadow pixel set must be *exactly* the main set translated
  `(+1, +1)` logical px minus whatever main covers: 560 px expected, 560 observed. A
  blur lights the other neighbours too and fails by inclusion. Plus a non-empty
  check, so the set equality cannot pass vacuously.
- **The shadow's brightness**, exactly: main peak 255, shadow peak 64. ~137 would
  mean the quarter was taken in linear space.
- **The negative control, executed.** `HudRenderer::detach_font()` restores the debug
  font in the same renderer and the same frames; `is_proportional(control)` is
  asserted **false**, and the control frame is asserted to contain no pixel between
  the shadow and main brightness thresholds.

The gate's target is deliberately `Rgba8Unorm`, **not** `Rgba8UnormSrgb`, so the
HUD's colour floats land in the framebuffer verbatim and the gamma-space quarter can
be asserted at its exact value. On an sRGB target it would read back as ~54 % and an
exact assertion would be impossible to state honestly.

## Configuration

- `LODESTONE_ASSETS` — pack root containing `client.jar` and
  `generated/reports/blocks.json`. Otherwise discovered under `.cache/mc/<version>`.
- `metrics::SHADOW_OFFSET`, `metrics::SHADOW_BRIGHTNESS`,
  `metrics::BEARING_TOP_BASE` in `lodestone-assets::font`.
- `metrics::BOLD_OFFSET`, `metrics::ITALIC_SHEAR`, `metrics::ITALIC_SHEAR_SLOPE`,
  `metrics::STRIKETHROUGH_Y`, `metrics::UNDERLINE_Y`, `metrics::EFFECT_THICKNESS`,
  `metrics::EFFECT_LEAD_IN` — the styling constants, same module, same "named so
  nothing hardcodes a magic number" rationale.
- GUI scale is still hard-coded (`let scale = 2.0` in `hud::build_inner`); the font
  takes `scale` per call and does not care, but nothing exposes it yet.

## Dependencies

- `lodestone-assets`: `font` (`Font`, `FontLoader`, `RasterFont`, `GlyphRaster`),
  `ResourceManager`/`ZipSource` for the jar, `Image::decode_png` for the sheets.
- `lodestone-shell`: `hud::item_icon::ColourStream` (the colour vertex stream),
  `hud::font` (the fixed-advance fallback), `hud::legacy_rgb` (`§` colours).
- The vanilla `client.jar` for 26.2. No GPU resources.
