# Potion effects in the inventory

## What it is

`EffectsInInventory` (`26.2`): the column of active-effect widgets drawn beside
the player's own inventory screen — the effect's real icon sprite, its
translated name with a level numeral, and the remaining time — on a nine-sliced
background sprite. Drawn inside the container screen's own geometry pass, which
is where the GUI sprite atlas and the vanilla proportional font already are.

## How it works

Three layers, and the split matters:

| layer | lives in | job |
|---|---|---|
| state | `lodestone_game::effect::ActiveEffects` | what the wire folded |
| model | `crates/lodestone-shell/src/effects.rs` (`inventory_rows`) | sort, translate, format |
| draw | `crates/lodestone-shell/src/container/geometry.rs` (`draw_effect_column`) | sprites and glyphs |

The model layer is separate because it needs the **language table**, which
lives on `Sim` and is reached through `Sim::translator` at the `redraw.rs` call
site; the draw layer is inside the container because that is the only place
holding a `ContainerBackground` and a `VanillaFont`.

### Model: `effects::inventory_rows`

Ported from `EffectsInInventory.extractEffects`/`getEffectName` and
`MobEffectUtil.formatDuration`:

- **Order** is `Ordering.natural().sortedCopy(...)`, i.e.
  `MobEffectInstance.compareTo`: non-ambient before ambient, then finite before
  infinite, then shorter duration, then `MobEffect.getColor()`. Insertion order
  is *not* it. The colour tiebreaker is why
  `lodestone_data::mob_effects::mob_effect_color` exists.
- **Name** is `MobEffect.getDisplayName()` — the `effect.<namespace>.<path>`
  key through the language table — plus, for amplifier `1..=9` only, a space
  and `enchantment.level.<amplifier + 1>`. Not `potion.potency.*`; that is the
  potion *tooltip*'s key, and the two tables differ.
- **Duration** is `StringUtil.formatTickDuration(ticks, 20.0)`: `mm:ss`,
  widening to `hh:mm:ss` only past a whole hour, seconds floored. An infinite
  effect (`duration < 0`) is the translated `effect.duration.infinite`, which
  in `26.2` is `∞`.
- **`show_icon` is not consulted.** `EffectsInInventory` reads
  `getActiveEffects()` whole; only the HUD's own overlay
  (`Hud.extractEffects`) gates on `instance.showIcon()`.

### Assets: two sprite families, and only one of them is where you would look

- The backgrounds are `container/inventory/effect_background` and
  `…/effect_background_ambient`, ordinary `gui/sprites/**` art — but they are
  `nine_slice` (`32x32`, border `4`), the only sprites in the container atlas
  that are, because they are the only ones blitted at a caller-chosen width.
  `ContainerBackground::scaled_sprite_quads` reads their declared
  `gui.scaling` out of the pack at build time and decomposes through
  `lodestone_assets::gui::GuiScaling::geometry`; the whole-sprite
  `sprite_quad` would smear the border across the widget.
- **The icons are not under `gui/sprites/**` at all.**
  `assets/minecraft/atlases/gui.json` declares a *second* source directory for
  the GUI atlas — `{"type": "directory", "prefix": "mob_effect/", "source":
  "mob_effect"}` — so `Hud.getMobEffectSprite`'s `mob_effect/<id>` sprite id
  resolves to `textures/mob_effect/<id>.png`. Any enumeration that assumes one
  source directory finds none of the 41 vanilla icons, and a widget that blits
  one draws nothing. `ContainerBackground::build` enumerates that directory
  itself (fail-open per file) and `mob_effect_icon_quad` looks the result up by
  sprite id.

### Layout: real 26.2 constants

- `ICON_SIZE = 18`, `SPACING = 7`, `TEXT_X_OFFSET = 32`,
  `SPRITE_SQUARE_SIZE = 32`.
- `x0 = leftPos + imageWidth + 2`, `availableWidth = screen.width - x0`;
  nothing draws at all when `availableWidth < 32` (`canSeeEffects`).
- `maxWidth = availableWidth >= 120 ? availableWidth - 7 : 32`.
- `textureWidth = min(maxWidth, max(32 + width(name) + 7, 32 + width(duration) + 7))`
  — the widths come from the caller's own font, so this stays correct on a
  jar-less run's fallback font too.
- `yStep = 33` for five or fewer effects, `132 / (count - 1)` above that.
- Name at `(x0 + 32, y0 + 7)` in white, duration nine pixels below in
  `0xFF808080`; both with vanilla's default drop shadow (the five-argument
  `graphics.text` overload defaults `dropShadow` to `true`, unlike the two
  container labels' explicit `false`). A name wider than
  `textureWidth - 32 - 7` is clipped with `...`, per
  `ComponentRenderUtils.clipText`.

### The stale claim this doc exists partly to correct

**`EffectsInInventory` does not reposition the container panel to make room.**
That belongs to the *older* `EffectRenderingInventoryScreen` lineage, which
descriptions of "potion effects in the inventory" still carry forward. In the
real 26.2 source `InventoryScreen`'s own `leftPos` comes from the ordinary
centred (or recipe-book-shifted) layout, untouched by whether any effect is
active; `EffectsInInventory` only decides whether there is *already* enough
free canvas beside the panel. The port implements the real behaviour.

### Coordinate space

Everything is the **logical** GUI canvas — the space
`container::layout::panel_origin_with_scale` and `slot_layout` produce, which
is what makes the column scale with the panel at any GUI scale or DPI. The
recipe-book shift is already folded into the panel origin
`ContainerGeometry::build_inner` measures from, so an open book pushes the
column right along with the panel.

## How to change it

- Layout constants are named `const`s at the top of `effects.rs`
  (`INV_ICON_SIZE`, `INV_SPACING`, `INV_TEXT_X_OFFSET`, `INV_BACKGROUND`,
  `INV_Y_STEP`, `INV_CROWDED_SPAN`). Re-derive any of them from the real
  `EffectsInInventory` source rather than reaching for a round number.
- Adding a sprite the column draws means adding it to `container.rs`'s
  `GUI_SPRITES`; if it is drawn at a size other than its own, it also needs a
  row in `background.rs`'s `NINE_SLICE_SPRITES` so its declared scaling is
  read.
- The screen gate is the `MenuKind::Player` check in `redraw.rs`, where
  `effect_rows` is built — `draw_effect_column` takes the rows as data and has
  no opinion about which screen is open. Vanilla also shows this column on
  `CreativeModeInventoryScreen`, which this client does not yet route through
  the same frame.
- **The HUD's own top-right overlay is still an approximation** and is a
  different widget: `Hud.extractEffects` draws a `24x24` background sprite and
  an `18x18` icon in two rows (beneficial above, harmful below) with **no text
  at all**, plus a flashing alpha under 200 ticks remaining.
  `effects::geometry` draws a coloured chip with a name and a timer instead.
  Closing that gap needs a sprite atlas at that draw site, which
  `EffectsRenderer`'s untextured pipeline does not have — the same obstacle
  this column just stopped having, solved the same way (draw it where the
  atlas already is).

## Configuration

None. The column draws whenever the local player has at least one active
effect, the open menu is the player's own inventory, there is room beside the
panel, and a pack is attached — a jar-less run draws no column rather than a
coloured-rectangle stand-in, because a stand-in is indistinguishable from art
that failed to load.

## Dependencies

- `crates/lodestone-shell/src/effects.rs` — `InventoryEffectRow`,
  `inventory_rows`, and the layout arithmetic.
- `crates/lodestone-shell/src/container/geometry.rs` — `draw_effect_column`.
- `crates/lodestone-shell/src/container/background.rs` — the mob-effect icon
  enumeration, `mob_effect_icon_quad` and `scaled_sprite_quads`.
- `crates/lodestone-shell/src/container/frame.rs` — `ContainerFrame::effects`.
- `crates/lodestone-shell/src/app/redraw.rs` — the call site and the
  `Sim::translator` lookup.
- `lodestone_data::mob_effects::mob_effect_color` — the sort tiebreaker.

## Verification

```bash
cargo test -p lodestone-shell --lib --no-fail-fast -- effects::
cargo test -p lodestone-shell --lib effect_column_tests -- --ignored --nocapture
```

The second command is the load-bearing one and needs `client.jar` under
`.cache/mc/<ver>`; it is deliberately not hermetic. Every input comes from the
real pack — the language table, the sprite atlas, the font — because a gate
that installs its own translations and its own sprites reproduces exactly the
blindness that let this widget ship four wrong things at once.

Its assertions, and the neuter each was observed failing under:

| assertion | neuter | observed |
|---|---|---|
| the row reads `Speed II` | name resolved as `path.replace('_', " ")` | failed |
| a quad carries `mob_effect/speed`'s own atlas UVs | the `mob_effect` source directory not enumerated | failed |
| the background decomposes into more than one quad | declared scaling replaced with `Stretch` | failed |

Each neuter was applied on top of a passing run and reverted from an
md5-checked backup.
