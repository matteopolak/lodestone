# Unihex font glyphs

## What it is

Rasterisation of vanilla's `unihex` glyph provider — the GNU Unifont HEX bitmaps in
`font/unifont.zip` — so that CJK, Hangul, Thai, Arabic, Cyrillic extensions, box
drawing and most of the rest of the Basic Multilingual Plane draw real glyphs
instead of the hollow missing-glyph box. It takes the `minecraft:default` font from
**2,414 codepoints to 114,432**.

## How it works

### The gap it closes

`minecraft:default` chains four provider kinds. Three were already implemented:
`space`, `bitmap` (the `ascii`, `accented` and `nonlatin_european` PNG sheets) and
`reference`. Together those supply 2,414 codepoints. The fourth, `unihex`, was
*parsed and discarded*, so **every codepoint outside those 2,414 rendered as a
square** — the user-visible symptom that opened this work.

Vanilla declares the unihex provider last in `font/default.json`, i.e. at the
lowest priority, because it is the broad fallback: it covers the bitmap sheets'
codepoints too, and the sheets win them by being declared first.

### The HEX line format

`UnihexProvider.readFromStream` in `.cache/mc/26.2/client-src`:

```text
2713:00000000010102024444282810100000
4E2D:01000100010001003FF8210821082108210821083FF821080100010001000100
```

A codepoint field of **4, 5 or 6** hex digits, a colon, then the bitmap as hex
digits, then a newline. **The bitmap's digit count is the glyph's width**, and only
four counts are legal:

| digits | bit width | reader | count in vanilla's 26.2 file |
|---|---|---|---|
| 32 | 8 (half-width) | `ByteContents.read` | 12,582 |
| 64 | 16 (full-width) | `ShortContents.read` | 101,850 |
| 96 | 24 | `IntContents.read24` | 0 |
| 128 | 32 | `IntContents.read32` | 0 |

Every form is exactly **16 rows** tall (`UnihexProvider.GLYPH_HEIGHT`); the digits
divide evenly into 16 rows, most significant digit leftmost, so a row's leftmost
pixel is its most significant bit. Vanilla normalises all four widths to a 32-bit
row **left-aligned in the word** (`byte << 24`, `short << 16`, `v << 8`, `v`), and
`UnihexBitmap::rows` stores exactly that: bit 31 is column 0 at every width. Keeping
the alignment rather than the raw width is what lets one trimming rule serve all
four. **A fixed stride is therefore wrong** — the 24- and 32-wide arms are dead in
vanilla's own file but live for a resource pack, and are covered by fixture.

### The trimming rule

`left`/`right` are inclusive column bounds from **one of two places, never both**:

1. a `size_overrides` range containing the codepoint. Vanilla applies these *first*,
   in declaration order, `remove`-ing each codepoint from the pending map as it goes
   — so the **earliest** matching range wins and a later overlapping one cannot
   reclaim it. A range naming codepoints the file does not contain contributes
   nothing rather than inventing blanks.
2. otherwise the ink's own extent (`LineData.calculateWidth`):
   `left = leading_zeros(mask)`, `right = 32 - trailing_zeros(mask) - 1`, where
   `mask` is the OR of all 16 rows.

The all-empty case is the one that catches people: `mask == 0` yields
`left = 0, right = bitWidth` — **one past** the last real column, not `bitWidth - 1`.
So a blank 8-wide glyph is 9 columns wide and advances 5.5, not 5.0.

Everything else follows:

| quantity | value | source |
|---|---|---|
| `width` | `right - left + 1` | `UnihexProvider.Glyph.width` |
| advance | `width / 2 + 1` | the anonymous `GlyphInfo` in `Glyph.info` |
| oversample | 2.0 | `GlyphBitmap.getOversample` on `Glyph.bake` |
| drawn size | `width / 2` × 8 logical px | 16 rows at `1 / oversample` |
| box top | 0 logical px below the line's top | `getBearingTop` default 7, `getTop = 7 - 7` |
| bold offset | **0.5** | `Glyph.info`'s `getBoldOffset` |
| shadow offset | **0.5** | `Glyph.info`'s `getShadowOffset` |

The last two are per *glyph*, not per font: a sheet glyph keeps 1.0. Reading a font
constant instead makes bold CJK measure 0.5 px per glyph too wide.

A bound an override pushes outside the source row pads with **blank** columns —
`unpackBitsToBytes` writes 0 for any bit index outside `0..32`. That guard is
load-bearing, because the CJK ranges force `right = 15` onto glyphs whose ink stops
earlier and onto 8-bit rows that never held bit 15.

### Where the file comes from — the trap

`font/unifont.zip` is **not in `client.jar`**, and neither is the only
`font/include/unifont.json` that declares a unihex provider. The jar ships a
**29-byte stub of that JSON whose `providers` array is empty**; the real 3,993-byte
file and the 1,559,654-byte zip are asset-object-store objects. So a
`ResourceManager` built from the jar alone resolves the stub, loads zero unihex
providers, logs a perfectly healthy "loaded the vanilla default font", and draws
squares. `hud::vanilla_font::jar_manager` pushes `AssetObjectStore` **above** the
jar for exactly this reason (its own module doc in
`crates/lodestone-shell/src/asset_objects.rs` states the rule: for any name present
in both, prefer the store).

A missing `hex_file` is a deliberate **soft skip** returning zero glyphs, not an
error. Making it fatal would take a store-less install from "CJK boxes, as it always
did" to "the font fails to load and every glyph in the game changes".

### Where it reaches pixels

`lodestone_assets::font::GlyphRaster` is an enum internally
(`RasterKind::Sheet` / `RasterKind::Unihex`) with an unchanged public surface, so
`VanillaFont::draw_ink` — which walks `cell_width × cell_height` asking `is_ink` and
emits run-length-merged quads on the HUD's existing colour stream — drew unihex
glyphs correctly with **no change at all**.

## The atlas decision, and why there is no atlas

Vanilla stitches glyphs into a GPU atlas on demand. **This does not, and should
not.** Three reasons, in order of weight:

1. **The consumer does not sample a texture.** The HUD emits glyph coverage as
   quads, not as textured geometry. There is no glyph atlas to populate lazily or
   eagerly, no upload path, and no fifth bind group — which matters because the
   model shader is already at wgpu's 4-bind-group floor and the browser is far more
   likely than this machine to report exactly 4. A textured font path would be
   fewer vertices but is a separate piece of work with its own pipeline; nothing
   here is blocked on it.
2. **The CPU-side store is small enough to be eager, and vanilla parses eagerly
   too.** `UnihexProvider.Definition.loadData` reads every `.hex` member up front
   (only *baking* is lazy), and it has to: `getSupportedGlyphs` needs the full key
   set. 114,432 glyphs × (16 × `u32` rows + two bounds) is ~11 MB resident with **no
   per-glyph allocation**, against a 7.7 MB text payload that had to be decompressed
   and scanned anyway.
3. **Nothing is compiled in.** The zip is read at runtime out of the asset-object
   store, so the wasm bundle — already over its gzip ceiling, ~76% static tables —
   gains only the parser's code.

The browser is deliberately left at bitmap-only coverage: `platform::assets::Bundle`
carries the jar and the blocks report, and adding a 1.5 MB `unifont.zip` fetch to an
over-ceiling bundle is a decision for whoever owns that budget, not a side effect of
this change.

## How to change it

- **The glyph type is `font.rs`'s `UnihexGlyph`; the reader is `read_hex_entries`.**
  The reader is a faithful port with two relaxations, both documented on it: a
  trailing `\r` is dropped and a blank line is skipped, where vanilla would throw.
- **`Glyph` is a public enum** (`Bitmap`, `Unihex`, `Ttf`, `Space` — see
  `docs/ttf-font-glyphs.md` for the fourth). Adding another kind means teaching
  `GlyphRaster`'s `RasterKind`, `Glyph::advance`/`bold_offset`/`shadow_offset` and
  the census gate in `tests/font_unihex.rs` about it. The compiler finds the
  exhaustive matches; the census names what changed.
- **Do not add a fourth `ResourceManager` for fonts.** `jar_manager` is the one
  place the store is stacked; a second stack would drift.
- **The shadow offset is now applied per glyph.** `VanillaFont::draw_resolved`
  (`crates/lodestone-shell/src/hud/vanilla_font.rs`) looks up
  `Font::shadow_offset(codepoint)` for each glyph's own shadow copy, rather than
  adding one `metrics::SHADOW_OFFSET` before either drawing pass began — a string
  mixing a unihex glyph with a sheet glyph now gets 0.5 px for the former and 1 px
  for the latter, instead of one offset applied to every codepoint.

### Gotchas that cost time here

- **A test codepoint present in *both* the bitmap sheets and unifont proves
  nothing.** The sheet wins by priority, so the assertion passes with unihex
  entirely absent. Pick one on each side: `U+2713` ✓ is unihex-only, `U+2714` ✔ is
  in `nonlatin_european.png` — visually near-identical, opposite sides of the seam.
- **A codepoint whose override and derived bounds coincide is not a test of
  `size_overrides`.** `U+4E2D` 中 derives 6.5 and overrides to 9.0; `U+FF5E`
  derives 7.5 and overrides to 9.0. Boundary pairs need a codepoint just outside
  too — `U+FF5F` is one past `FF5E` and must stay 5.5.
- **`unwrap_or(f32::NAN)` makes an advance assertion vacuous**, because every
  comparison against NaN is false and an *absent* glyph passes. Compare the
  `Option`.

## Configuration

- `LODESTONE_ASSET_ROOT` / `LODESTONE_ASSETS` — either names the asset-object root
  (see the `asset_objects` module doc); without one, discovery walks ancestors for
  `.cache/mc/<version>`.
- `cargo run -p xtask -- fetch-assets --version 26.2` fetches the two objects. Both
  are in `REQUIRED_OBJECT_NAMES`/the jar-shadowed set, so no extra flag is needed.
- The `jp` font option selects `unifont_jp.zip` instead; that object is not fetched
  by default and the option is not wired to any setting yet.

## Dependencies

- `zip` (already a `lodestone-assets` dependency) to read the `hex_file` archive.
- `lodestone_shell::asset_objects` for the store, read-only.
- Vanilla `UnihexProvider`, `GlyphBitmap` and `GlyphInfo` in `.cache/mc/26.2/client-src`
  as the record.

## Gates

| gate | what it pins |
|---|---|
| `lodestone-assets/tests/font_unihex.rs` (12, hermetic) | all four digit widths, the derived and override trimming rules, the all-empty case, override boundaries and overlaps, provider priority, blank-column padding, per-glyph bold/shadow offsets, malformed-line rejection, the soft skip, and `texel_size` 0.5 |
| `lodestone-assets/tests/unihex_vanilla_oracle.rs` (4, `#[ignore]`d) | the real `unifont.zip`: 114,432 codepoints / 112,018 from unihex, ten hand-derived advances, half-vs-full-width strides and exact ink counts, plus the jar-only control |

Both suites were run against a **neutered** loader (`load_unihex` returning zero)
and 13 of the 16 fired — which is how the `unwrap_or(f32::NAN)` vacuity above was
found.
