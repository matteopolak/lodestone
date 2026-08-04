# Held-item name tooltip

## What it is

The item name that briefly appears centred above the hotbar whenever the
selected item's *identity* changes (issue #126) — vanilla's
`Hud.extractSelectedItemName` (`Hud.java:625-648` in the 26.2 client). It
fades in **instantly** (no ramp-up) and holds at full opacity, then fades out
over its last half-second.

## How it works

Three pieces, in three different crates, none of which is an island by
itself:

- **The timer/alpha model** — `lodestone_game::player_state::HeldItemHighlight`.
  A pure, tick-driven state machine mirroring `Hud.tick()`
  (`Hud.java:1190-1203`): `tick(Some((item, hover_name)))` restarts the
  40-tick timer only when the **identity** (item id *or* resolved hover name)
  actually changes — switching between two slots holding identical dirt does
  *not* restart it, only counts it down. `alpha()` is `1.0` for any
  `timer >= 10` and ramps linearly to `0.0` over the final 10 ticks
  (`Hud.java:639`, `timer * 256 / 10`, clamped to 255).
- **The styled name** — `lodestone_game::item::styled_hover_name`. Builds the
  `§`-coded string to draw: the item's hover name, forced **italic** when a
  `minecraft:custom_name` component is present
  (`Hud.java:627-630`). Depends on issue #117's styling draw to actually show
  the italic — before that landed, the code would parse but the shear would
  never appear on screen.
- **The draw** — `HudFrame::held_item: Option<(String, f32)>` in
  `lodestone-shell::hud`, drawn centred at `y = guiHeight - 59`
  (`Hud.java:634`), **unscaled** (no `×2` — the exact defect issue #256 fixed
  on the XP level number, on a second piece of HUD text this time), through
  the same `Builder::text_legacy` → `VanillaFont::draw_legacy` path chat and
  the action bar already use.

## How to change it, and the gotchas

- **Identity, not slot.** `HeldItemHighlight::tick` takes `(item id, hover
  name)`, already resolved by the caller — it only compares. Do not key this
  off `HudFrame::hotbar` (the selected slot index): vanilla's own check is
  `!selected.is(lastToolHighlight.getItem()) ||
  !selected.getHoverName().equals(...)`, which is item-and-name equality, not
  slot equality.
- **No fade-in.** `Hud.java:639`'s formula is at maximum for the *entire*
  hold phase (`timer >= 10`) and only ramps down in the last 10 ticks before
  hitting zero. A fade-in animation is a different (wrong) shape.
- **Rarity colour is not modelled.** Vanilla tints the name by the item's
  rarity (`ItemStack.getRarity().color()`); this build has no rarity data
  reaching `ItemStack` (no `minecraft:rarity` component, no per-item default
  table), so every name draws at the caller's base colour (white) — correct
  for the common case (most items are common-rarity/white) and a known,
  narrower gap than #117's for everything else. See
  `lodestone_game::item::styled_hover_name`'s doc for exactly what would need
  to change.
- **The display-name translation is a best-effort, not a verified table.**
  There is no existing `item.minecraft.*`/`block.minecraft.*` resolver
  anywhere in this tree — checked before writing this (the issue text's claim
  that one already existed to reuse did not hold up; the tracker lags the
  tree). `styled_hover_name`/`base_display_name` try
  `item.minecraft.<path>`, then `block.minecraft.<path>` (vanilla's own two
  `descriptionId` families, `Item.java:634-645` — a plain `Item` defaults to
  the former, a `BlockItem` to the latter, and nothing here classifies which
  is which per item), then a humanised fallback. Good enough to never show a
  raw snake_case key; not a byte-for-byte match to vanilla's `en_us.json` for
  every item.
- **The creative/spectator +14px offset is not modelled.** Vanilla shifts
  this label down 14px when `!canHurtPlayer()` (no health/hunger row to
  clear). No game-mode signal reaches `HudFrame` yet, so only the survival
  `y = guiHeight - 59` position draws. See `HudFrame::held_item`'s doc.
- **The live per-tick wiring is not landed by this change.** `HeldItemHighlight`
  needs to be ticked once per client tick with the currently-selected stack,
  and its `(name, alpha)` needs to reach `app.rs`'s `hud_frame.held_item =
  ...` — both outside this change's file ownership (`sim.rs`/`app.rs` are
  brokered; the natural ECS home alongside `TitleOverlay`/`ActionBarOverlay`
  is `lodestone-ecs::session`, which is a different agent's cluster
  entirely). The model and the draw are both real and both tested against
  hand-constructed input; only the frame-by-frame glue is outstanding. See
  the PR/issue thread for the drafted patch.

## Configuration

- No new env vars or flags. `HeldItemHighlight::TIMER_TICKS` (40) and
  `FADE_TICKS` (10.0) are vanilla's defaults at
  `notificationDisplayTime = 1.0`; the option itself is not modelled (no
  config surface reads it), matching the "no game-mode signal" gap above.

## Dependencies

- `lodestone-model`: `Text`/`TextStyle` (for the italic-forcing wrapper) and
  `Identifier` (item identity).
- `lodestone-game`: `item::{ItemStack, ComponentValue, CUSTOM_NAME_COMPONENT,
  styled_hover_name}`, `player_state::HeldItemHighlight`.
- `lodestone-shell`: `hud::{HudFrame, Builder::text_legacy}`, and transitively
  everything [`vanilla-hud-text.md`](./vanilla-hud-text.md) depends on for the
  actual glyphs (issue #117's styling draw, for the italic case).
