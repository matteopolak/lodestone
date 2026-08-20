# Custom Font Metrics and RGBA Design

## What it is

This change makes resource-pack bitmap fonts retain their native colour and
transparency and makes HUD layout measure text with the same font providers used
to draw it. It fixes opaque white custom-font icons and incorrectly centred
font-driven HUD content such as boss-bar titles.

## Problem

`VanillaFont::spans_width` currently measures every component with the default
font. Drawing resolves each span's `FontId` and may use a resource-pack font, so
the measured width and final pen position diverge whenever the custom glyph has
a different advance.

Bitmap glyph rasterization also exposes only a binary `is_ink` result. The HUD
then paints every non-transparent source texel using the component text colour,
discarding the bitmap's RGB channels and converting partial alpha to an opaque
quad. This turns translucent grey icon backgrounds into solid white rectangles.

## Goals

- Resolve the same font provider and fallback for measurement and drawing.
- Preserve bitmap-provider RGBA, including partial alpha.
- Continue applying component colour and opacity as a tint/modulation.
- Keep Unihex glyphs binary and keep TrueType coverage behaviour unchanged.
- Fall back to the default font per glyph when a custom provider lacks a codepoint.

## Non-goals

- Replacing the rectangle-based HUD text renderer with a texture atlas.
- Changing Minecraft component inheritance or font-provider precedence.
- Adding new resource-pack provider formats.

## Design

### Raster representation

Extend the asset font raster interface so a rasterized texel can return its
source RGBA rather than only an ink/no-ink flag. Bitmap-provider glyphs return
the decoded sheet texel. Unihex glyphs return opaque white for set bits and
transparent for unset bits. TrueType glyphs preserve the current thresholded
shape: coverage above `TTF_INK_THRESHOLD` returns opaque white and lower
coverage returns transparent.

The representation must distinguish a transparent texel from a visible black
texel; using RGB zero as an absence sentinel is therefore invalid.

### Drawing

The HUD font renderer multiplies the source texel colour and alpha by the
resolved component colour and alpha. Existing shadow and bold passes use the
same source coverage and their existing colour transforms. Contiguous-run
coalescing may continue only when adjacent texels produce the same final RGBA;
otherwise runs must split so source colour and transparency are not flattened.

### Measurement

`spans_width` resolves each span's `FontId`, then selects a provider for each
codepoint using the same selection and default-font fallback as `draw_spans`.
Advance, bold expansion, and missing-glyph handling must be shared or tested as
equivalent. The measured width is the final cursor advance, excluding only the
same terminal spacing that drawing excludes today.

Boss bars and other centred HUD consumers need no special-case correction: they
will receive the correct width from the common font API.

## Failure handling

An unavailable custom font or uncovered codepoint falls back to the default font
for that glyph. A malformed provider continues through the resource-pack loader's
existing warning path; it does not invalidate unrelated spans. Alpha-zero bitmap
pixels generate no geometry.

## Tests

- A bitmap glyph with translucent grey texels produces translucent grey output.
- Component tint and opacity multiply, rather than replace, source RGBA.
- A custom glyph's advance controls `spans_width`.
- An uncovered codepoint uses the default font's raster and advance.
- Mixed default/custom spans measure to the same final pen advance used by draw.
- Existing Unihex and TrueType coverage tests remain green.

## How to change it

Font source decoding belongs in `lodestone-assets`; font selection, measurement,
and HUD geometry belong in `lodestone-shell/src/hud/vanilla_font.rs`. New raster
formats must define both their texel colour semantics and their advance semantics.

## Configuration and dependencies

There is no new setting. Behaviour depends on resource-pack font JSON, bitmap
images, the existing custom-font cache, and Minecraft text-component `FontId`s.
