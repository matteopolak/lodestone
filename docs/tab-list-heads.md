# Tab-list player heads

## What it is

The tab-list overlay shows each listed player’s 8×8 skin face to the left of
their name, including the optional hat layer. A profile without a usable remote
skin still gets a deterministic packaged fallback.

## How it works

`crate::tablist::tab_list_view` resolves the profile’s `textures` property
through the shared remote-skin decoder and records only the URL, fallback sheet,
and hat visibility. Frame gather sends the URL to the idempotent asynchronous
remote-skin request path. The HUD geometry records each face placement in
column-major row order. `hud::item_icon::TabHeadRenderer` groups placements by
resolved sheet, uploads newly available images, and samples the face rectangle
(`8..16, 8..16`) plus the hat rectangle (`40..48, 8..16`) with nearest filtering.

## How to change it

Keep URL resolution and network scheduling outside the render pass. Add new
skin sources to `TabListHead` and the frame-gather request chain, then preserve
the per-row fallback when a fetch is pending or refused. Any layout change must
update both `TabPanel::new`’s head width and the head placement emitted by
`HudGeometry::build_inner`.

## Configuration

No new flags or environment variables. The existing resource-pack stack supplies
packaged fallback sheets; the existing remote-texture host allow list controls
which profile URLs may be fetched.

## Dependencies

The feature depends on `lodestone-game::tablist` profile state,
`crate::remote_skins` for decode/fetch/cache, `crate::resources` for packaged
fallback sheets, and the HUD textured-quad pipeline for drawing.
