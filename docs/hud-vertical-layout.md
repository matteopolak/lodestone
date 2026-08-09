# HUD vertical layout

## What it is

Where every row of the bottom-centre HUD cluster sits — hotbar, XP bar and level
number, hearts, hunger, armour, air bubbles, action bar — and which vanilla
expression each `y` is transcribed from. One page because these rows are all
derived from two anchors, and getting either wrong moves everything above it.

## How it works

Everything is in **logical-canvas pixels**, the space
`crate::menu::render::logical_canvas` returns: the physical framebuffer divided
by the effective integer GUI scale. That canvas *is* vanilla's
`guiScaledWidth`/`guiScaledHeight`, so a vanilla `guiHeight - N` transcribes
directly as `b.h - N` with no conversion. `H` below means `b.h`.

Two anchors, both absolute:

| anchor | value | vanilla |
|---|---|---|
| `hud::HOTBAR_MARGIN` | `0.0` | `Hud.extractItemHotbar` blits the bar at `(guiWidth/2 - 91, guiHeight - 22, 182, 22)` — flush, no margin |
| `hud::vitals_line_base(H)` | `H - 39` | `Hud.extractPlayerHealth`'s `int yLineBase = graphics.guiHeight() - 39` |

Everything else hangs off those:

| element | expression | vanilla source |
|---|---|---|
| hotbar | `H - 22 - HOTBAR_MARGIN` | `extractItemHotbar` |
| hotbar selection | hotbar top `- 1`, 24×23 | `extractItemHotbar`'s second blit |
| hotbar item icons | hotbar top `+ 3`, 20 px pitch, 16 px icon | vanilla insets a 16 px icon into each 20 px slot |
| XP bar | hotbar top `- 5 - 2` = `H - 29` | `ContextualBar.top` = `guiScaledHeight - MARGIN_BOTTOM(24) - HEIGHT(5)` |
| XP level number | XP bar top `- 6` = `H - 35` | `ContextualBar.extractExperienceLevel`'s `guiHeight - 24 - 9 - 2` |
| hearts | `vitals_line_base(H)` | `extractHearts(…, yLineBase, …)` |
| hunger | `vitals_line_base(H)`, right-anchored | `extractFood(…, yLineBase, xRight)` — the **same row** as the hearts |
| armour | `vitals_line_base(H) - 10` | `yLineArmor = yLineBase - (numHealthRows - 1) * healthRowHeight - 10` |
| air bubbles | `vitals_line_base(H) - 10`, right-anchored | three terms that collapse to one — see below |
| action bar | `H - 72` | `extractOverlayMessage` translates to `guiHeight - 68` then draws at `y = -4` |
| held-item name | `H - 59`, `+ 14` in creative/spectator | `extractSelectedItemName` |

### The air row is `yLineBase - 10`, and reading only `extractPlayerHealth` says otherwise

This is the one worth writing down, because the wrong answer looks obviously
right. For an unmounted player:

| step | in | out |
|---|---|---|
| `extractPlayerHealth`: `yLineAir = yLineBase - 10` | `H-39` | `H-49` |
| `if (vehicleHearts == 0) { extractFood(…); yLineAir -= 10; }` | `H-49` | `H-59` |
| `extractAirBubbles` → `getAirBubbleYLine(0, H-59)` | `H-59` | **`H-49`** |

The third step cancels the second. `getAirBubbleYLine` computes
`rowOffset = getVisibleVehicleHeartRows(hearts) - 1`, and
`getVisibleVehicleHeartRows(0)` is `ceil(0 / 10.0) == 0`, so `rowOffset` is
**-1** and `yLineAir - rowOffset * 10` adds the ten straight back.

The second subtraction is real but unobservable without a vehicle. Its purpose is
the mounted case: there no food row draws (`vehicleHearts != 0`), and a 20-heart
mount gives `rowOffset == 1`, moving the bubbles up to `H-59` to clear the
vehicle-health row that replaced the food. Mounted vehicles are not modelled
(`HudFrame` carries no vehicle), so that branch has nothing to drive it.

So the bubbles share a line with the armour row — armour on the left (`xLeft`),
bubbles on the right (`xRight`), which is what vanilla looks like.

## How to change it

- **Derive from a vanilla expression, never by stacking upward from the row
  below.** The cluster used to be built from a `cluster_top` that started at
  `H - 6` and was pulled up by the hotbar and then by the XP bar, so the hearts
  landed on `H - 48` with an XP bar and `H - 41` without — two answers, neither
  of them vanilla's `H - 39`, and the row silently moved with the player's game
  mode. Vanilla's `yLineBase` takes no branch of any kind.
- **`HOTBAR_MARGIN` is not `HUD_MARGIN`.** The latter (6) is the chat and F3 text
  inset and has nothing to do with the hotbar; they were one number, which is why
  correcting the hotbar looked like it would move the chat.
- **Both draw paths must agree.** `sprite_vitals` (vanilla GUI atlas) and the
  procedural fallback in `HudGeometry::build_inner` both call
  `vitals_line_base`/`HOTBAR_MARGIN`. When they disagreed, the air-row pixel gate
  could not derive one rect for both and had to reproduce the stack by hand.
- **A pixel gate must call the same function, not restate the number.** Three
  gates each held their own copy of the `6.0` margin or a hardcoded `h - 19`
  heart row, and all three went red when the draw was corrected. They now call
  `hud::HOTBAR_MARGIN` and `hud::vitals_line_base`, which is the whole reason
  those two are `pub`.
- **Check whether an animation subsystem invalidates a gate's premise before
  believing it.** `xp_level_number_is_the_right_size_and_the_right_distance_above_the_bar`
  was red for an unrelated reason: it rendered `level: 0` and then `level: 5`, and
  `anim::XpFlash` reads that rise as a level-up, running the digit's green through
  `flash_toward_white(…, 1.0)` — pure white, so the gate's green-dominance test
  found nothing while the digit was painting perfectly (947 texels against the
  bar's 906). Rendering the digit *first* fixes it: `XpFlash` only triggers once
  `primed` by a previous frame.

## Configuration

None. Every value here is a transcribed vanilla constant. The GUI scale option
changes the canvas size, not these offsets; the chat options
(`hud::ChatDisplayOptions`) affect the chat block only.

## Dependencies

`crate::menu::render::logical_canvas` for the canvas size,
`crate::hud::anim` for the per-row jitter and the XP flash, and the vanilla GUI
atlas (`GuiAtlas`) for the sprites — absent it, the procedural fallback draws at
the same rows. Decompiled reference: `net.minecraft.client.gui.Hud` and
`net.minecraft.client.gui.contextualbar.ContextualBar` under
`.cache/mc/26.2/client-src`.
