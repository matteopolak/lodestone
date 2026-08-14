# TTF font glyphs

## What it is

Rasterisation of vanilla's `ttf` glyph provider — an embedded TrueType/OpenType
face in a resource pack's font definition — so that a pack declaring one draws
real glyphs instead of the hollow missing-glyph box. This is the last box on
"full Unicode text": `unihex` (CJK, Hangul, Thai, Arabic, Cyrillic extensions and
most of the rest of the BMP) and bidi/RTL ordering already landed; `ttf` closes
the gap for anything neither the three vanilla bitmap sheets nor `unifont.zip`
covers — chiefly astral-plane glyphs a pack supplies its own font for. Vanilla's
own `default.json` declares no `ttf` provider, so this affects resource packs
only, never the base game's own text.

## How it works

### The provider, ported

`net.minecraft.client.gui.font.providers.TrueTypeGlyphProviderDefinition` and
`com.mojang.blaze3d.font.TrueTypeGlyphProvider` in `.cache/mc/26.2/client-src`.
Vanilla loads the face with FreeType; this port uses
[`fontdue`](https://docs.rs/fontdue) — pure Rust (it wraps `ttf-parser`, itself
pure Rust), `no_std` + `alloc`, no filesystem or clock access. See "Why fontdue"
below for how that was chosen.

Five fields, all in `lodestone_assets::font::ProviderDef::Ttf`:

| field | vanilla | default |
|---|---|---|
| `file` | `location` | required |
| `size` | `size` | 11.0 |
| `oversample` | `oversample` | 1.0 |
| `shift` | `shift.x, shift.y` | `[0.0, 0.0]` |
| `skip` | `skip` (a string or a list of one-char strings, joined) | empty |

`pixels_per_em = round(size * oversample)` is the pixel size every rasterisation
call for that provider uses (`FT_Set_Pixel_Sizes`'s argument in vanilla). Every
codepoint the face's own `cmap` supplies — minus `skip` — becomes a
[`lodestone_assets::font::TtfGlyph`], unless an earlier-declared provider
already won that codepoint (`FontLoader::load`'s first-declared-provider-wins
priority, shared with `bitmap`/`unihex`/`space`).

### Metrics: what gets divided by oversample, and what doesn't

`TrueTypeGlyphProvider`'s Java constructor computes `bearingX = left /
oversample`, `bearingY = top / oversample` from FreeType's own `left`/`top`,
with `shift` already folded into those through `FT_Set_Transform` before this
port ever sees them. `fontdue` has no equivalent transform hook, so this port
applies `shift` itself, after the fact:

| quantity | formula | why |
|---|---|---|
| `advance` | `metrics.advance_width / oversample` | direct — `fontdue`'s advance already matches FreeType's `scaledAdvance` at the requested px |
| `bearing_left` | `metrics.xmin / oversample + shift.x` | `+shift.x` folds in directly, same sign as vanilla's `transformX = shiftX * oversample` |
| `bearing_top` | `(metrics.ymin + metrics.height) / oversample - shift.y` | `metrics.ymin + metrics.height` reconstructs FreeType's `bitmap_top` (offset from baseline to the bitmap's *top*, positive up) from `fontdue`'s bottom-relative `ymin`; `-shift.y` because glyph space grows up and screen space grows down, matching vanilla's `transformY = -shiftY * oversample` |

`bearing_top` feeds the same `top = 7.0 - bearingTop` formula
(`com.mojang.blaze3d.font.GlyphBitmap.getTop`) that a bitmap-sheet `ascent` and
a unihex glyph's fixed 7.0 already used — a `ttf` glyph is simply a third
source for the same quantity, not a special case downstream (see
[`docs/unihex-font-glyphs.md`](./unihex-font-glyphs.md) for the other two).

A zero-contour glyph — `TrueTypeGlyphProvider.loadGlyph`'s `EmptyGlyph` case —
resolves to `Glyph::Space { advance }` here: a real advance, no bitmap to bake,
same as an explicit `space` provider entry. `Font::ttf_count` still counts it
as "won" by the `ttf` provider, because it came from the face's own `cmap`.

### Where it reaches pixels — the one new thing `GlyphRaster` needed

`unihex` needed nothing new from `GlyphRaster` (see that doc's "no atlas"
section): every quantity the HUD's quad emitter already asked for had a unihex
answer. `ttf` needed one addition: **`GlyphRaster::left`**, the glyph box's left
edge relative to the pen position (`GlyphBitmap.getBearingLeft()`). A
bitmap-sheet cell and a unihex glyph are both flush with the pen position
(`left` is the interface default, 0.0); a TrueType outline generally is not —
`j`'s bowl sits right of the pen, `f`'s crossbar can sit left of it. Without
this, every `ttf` glyph would draw with its bearing silently dropped.

The shell's `VanillaFont::draw_ink` (`crates/lodestone-shell/src/hud/vanilla_font.rs`)
now does `let x = x + r.left() * scale;` before walking texel runs — a one-line
hunk, since `left()` is 0.0 for the two existing kinds and the run-length quad
emitter otherwise needed no change (mirrors `BakedSheetGlyph`'s own `x0 = x +
this.left`).

Coverage is antialiased 0..255 in `fontdue`'s output, but this renderer draws
opaque merged quads (no per-texel alpha — the same reason a bitmap sheet's ink
test is a bare `!= 0`), so `GlyphRaster::is_ink` binarises a `ttf` glyph's
coverage at `TTF_INK_THRESHOLD` (127). This reproduces the glyph's *shape*, not
vanilla's antialiasing — the same class of approximation the "atlas decision"
in the unihex doc accepted for a different reason (no textured draw path at
all).

### Why fontdue

No font-related crate in this workspace already pulled a rasteriser before this
change (`lodestone-assets` previously depended only on `png`/`zip` for texture
and archive decoding). `fontdue` was chosen over `ab_glyph`/`rusttype`/`swash`
for one reason that mattered concretely here: it, and its one dependency
(`ttf-parser`), are pure Rust with no C toolchain requirement, and both compile
cleanly for `wasm32-unknown-unknown` with default features (`hashbrown`,
`simd` — `simd` is inert off x86/x86_64, gated in `fontdue`'s own `Cargo.toml`).
Confirmed empirically, not assumed: a scratch crate depending on `fontdue`
built for `wasm32-unknown-unknown` before this landed in the workspace. Neither
crate touches the filesystem or a clock — `fontdue::Font::from_bytes` parses an
in-memory byte slice and `rasterize_indexed` is pure computation — so nothing
here needed a `cfg(not(target_arch = "wasm32"))` guard, and `just wasm-check`'s
`lodestone-assets fs-confinement` rule passes unchanged.

## How to change it

- **The glyph type is `font.rs`'s `TtfGlyph`; the loader is
  `FontLoader::load_ttf`.** It mirrors `load_unihex`'s shape: read the file
  (soft-skip if absent, same reasoning as unihex's missing `hex_file` — a
  `ttf` provider is a pack addition, so an absent file should degrade to "this
  pack's extra glyphs are missing", not fail the whole font), parse it, walk
  every codepoint the face supplies, skip what `skip` or an earlier provider
  already claims, insert the rest.
- **`RasterFont` holds the resident faces**, `ttf_faces: HashMap<ResourceLocation,
  fontdue::Font>`, populated in `FontLoader::load_raster` the same way
  `sheets` is populated for bitmap textures — decoded once per distinct file,
  not once per glyph. `RasterFont::raster` rasterises a `ttf` glyph's bitmap
  fresh on each call (no cross-call cache); that is a known perf tradeoff, not
  a correctness gap — `ttf` is presently pack-only and not on any default hot
  path the way ASCII HUD text is.
- **`Glyph` is a public enum** (`Bitmap`, `Unihex`, `Ttf`, `Space`). Adding
  another kind means teaching `GlyphRaster`'s `RasterKind`,
  `Glyph::advance`/`bold_offset`/`shadow_offset`, `GlyphRaster::left`/`top` and
  the glyph-kind census in `tests/font_unihex.rs` about it — the compiler finds
  the exhaustive matches, the census names what changed.
- **`GlyphRaster` is no longer `Copy`.** A `ttf` glyph's rasterised bitmap is an
  owned `Vec<u8>` inside `RasterKind::Ttf`, produced fresh by
  `RasterFont::raster`; cloning a `GlyphRaster` would re-rasterise rather than
  share pixels, so it stayed `Clone` only.

### Gotchas that cost time here

- **A single filled rectangle cannot test rasterisation orientation.**
  `fontdue`'s rasterised cell is always the tight bounding box of its own ink,
  so a solid rectangle's cell is entirely ink by construction — there is no
  "top vs bottom" to compare. `tests/font_ttf.rs`'s orientation gate uses two
  disjoint contours (a wide dense block, a narrow sparse one, a real gap
  between) specifically so the assertion has something to fail against; it was
  run against a deliberately Y-flipped `is_ink` and caught it (top/bottom ink
  counts inverted).
- **`loca`'s first two entries are `.notdef`'s start *and* end, not one shared
  entry.** A loop that pushes one offset per real glyph after a single leading
  `0` is one entry short of `numGlyphs + 1`, silently aliasing `.notdef`'s
  (empty) span onto the first real glyph's and shifting every glyph index
  after it by one. Caught because two glyphs in the test fixture were given
  deliberately different bounds/advances — see the next point.
- **Two glyphs with the same bounds and advance cannot catch a glyph-index
  mix-up.** The fixture's `A` and `B` were originally identical rectangles
  with the same `hmtx` advance; a `loca` table bug that fed `B`'s bytes to
  glyph index 1 (`A`) still passed every metrics assertion, because the two
  hypotheses (right glyph vs. wrong glyph) coincided on every measured
  quantity. Fixed by giving every fixture glyph its own bounds and advance.
- **`bearing_top` depends only on the glyph's top edge (`ymax`), not its
  bottom (`ymin`).** `(ymin + height)` telescopes back to `ymax * scale`
  algebraically, so two glyphs sharing a top edge but differing in how far
  down they reach report the *same* `bearing_top` — expected, not a bug (the
  quantity is "how far below the line's top does this glyph's own top sit",
  which does not care how tall the glyph is), but worth knowing before reading
  a coincidence as a defect.

## Configuration

- No new environment variables or flags. `size`/`oversample`/`shift`/`skip` are
  all per-provider pack JSON, read the same way `bitmap`/`unihex`/`space` are.

## Dependencies

- `fontdue` (new `lodestone-assets` dependency) for parsing and rasterisation.
- `ttf-parser` (already `fontdue`'s own dependency; added as a
  `lodestone-assets` **dev**-dependency) for the test suite's independent
  oracle — reading `hmtx`/`glyf` directly, a different code path than the
  `fontdue`-based production loader, so the expected values in
  `tests/font_ttf.rs` do not originate from the code under test.
- Vanilla `TrueTypeGlyphProviderDefinition` and `TrueTypeGlyphProvider` in
  `.cache/mc/26.2/client-src` as the record.

## Gates

| gate | what it pins |
|---|---|
| `lodestone-assets/tests/font_ttf.rs` (7, hermetic, hand-built `sfnt` fixture) | metrics against an independent `ttf-parser` + spec-formula oracle, rasterisation orientation (a real Y-flip control, run and confirmed to fail), provider priority against an earlier bitmap sheet, the zero-contour `EmptyGlyph`→`Space` fallback, the `skip` list (with a control proving the face really does map the skipped codepoint), the missing-file soft skip, and an end-to-end pack→provider→`RasterFont`→ink trace |

No `#[ignore]`d live-oracle gate: vanilla ships no `ttf` provider to gate
against, so there is no equivalent to `unihex_vanilla_oracle.rs` here — the
hand-built fixture *is* the fixture, not a stand-in for one vanilla supplies.
