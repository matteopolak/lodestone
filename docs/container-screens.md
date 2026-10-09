# Container screens

## What it is

The container and inventory screen family: the shared model that draws any open `Menu` (chest, furnace, crafting table, anvil family, creative inventory, merchant), the client-side click predictor that mirrors the reference click rules, and the screens with bespoke chrome on top (cost readouts, station widgets, creative grid, merchant trades, 3-D player preview, potion-effect icons).

## How it works

### The container screen model

`crates/lodestone-shell/src/container.rs` projects a `Menu` (folded server state owned by `lodestone-game`) into rectangles and vertex streams. It never mutates; slot state is authoritative in `Menu`.

`slot_layout(&Menu) -> SlotLayout` is the single dispatch for drawing and hit-testing (`hit_test`/`hit_test_with_scale` in `app.rs`). It tries `Menu::special_layout()` (anvil, grindstone, smithing, enchanting, merchant, furnace family, brewing stand, loom, stonecutter, cartography table, dispenser/dropper), then `Menu::craft_layout()`, then a plain grid. The shapes are attached to `Menu`, not new `MenuKind` variants: `MenuKind` is matched exhaustively crate-wide, and threading a `menu_type` only into drawing once let clicks and pixels disagree on slot positions.

Every slot rect carries the real `menu_index`, no offset. `Generic { n }` is `0..n` container, `n..n+27` main, `n+27..n+36` hotbar. The player inventory (window 0) is `0` result, `1..=4` 2x2 craft, `5..=8` armour, `9..=35` main, `36..=44` hotbar, `45` offhand. Special layouts reposition slots pixel-for-pixel but keep generic quick-move regions.

Bounded slot types live in `lodestone_model`: `HotbarSlot`, `MenuSlot`, `BundleItemSlot`, `BrewingBottleSlot`, `CrafterSlot`. Raw packet values and the outside-click sentinel stay at the protocol/UI boundary; validate there and pass typed `*_at` APIs inward.

- `ContainerBackground` stitches the real `textures/gui/container/*.png` sheets as native-size sub-rect blits (not `GuiScaling` sprites, not in `GuiAtlas`). Without it, a flat fill and per-slot wells draw and the title ink switches for legibility.
- Source split: `lodestone-game/src/menu.rs` (mutable projection, click state), `menu/layout.rs` (descriptors, empty-slot icons), `menu/tests.rs` (negative-control click tests). Keep descriptors additive.
- The full-canvas dim behind any container panel is true black with straight alpha (192/255 top, 208/255 bottom) via `ALPHA_BLENDING`; zero RGB matters in caves, where a near-black tint can raise destination bytes. Creative and advancements geometry share the values.
- Potion stacks keep all four `minecraft:potion_contents` values (mixed ARGB, optional registry id, ordered custom effects, optional name suffix) from the 26.2 adapter through `lodestone-model` to the game `ItemStack`. Built-in effects precede custom ones; custom effects with no holder render under the uncraftable title with no registry id (never invent one from the colour). Title precedence: stack custom name, then name suffix, then base potion id, then the no-holder `empty` suffix. The wire order is pinned by `crates/versions/26.2/tests/fixtures/potion_contents_complete.hex`, re-captured with the ignored `live-item` gate against `scripts/live-oracles/survival.sh`.
- Draw order: dim, background, chrome (title, wells), 3-D item models (depth-tested), flat icons and text. The carried stack is a final stratum replayed after every slot (a block on the cursor needs its own depth clear); the hover tooltip rides its tail so it draws above any overlay inserted between strata.
- The crafting result slot is always read from the server. `Menus::predicted_craft_result` is only for recipe-book ghosts and must never be written into the result slot.
- Titles come from a server `Text` and must resolve through the language table (a plain-string conversion prints the raw key). Title and the second "Inventory" label derive x/y from the same expression the panel art uses, since panel height varies with rows.

### Click handling

`crates/lodestone-game/src/click.rs` and `menu.rs` reimplement the reference container-click rules locally so the screen updates before the server confirms. It is faithful to the reference quirks on purpose: a "corrected" version would predict an outcome different from the server's and desync for a round trip. The server holds an independent port as the authority; they agree by construction from the same source, and the server's `container_set_content` correction reconciles disagreement.

Seven `ContainerInput` modes: `Pickup`, `QuickMove`, `Swap` (number key or offhand), `Clone` (creative middle-click), `Throw`, `QuickCraft` (paint drag), `PickupAll` (double-click). The drag is START (arm type), ADD (record a slot), END (distribute), reset by a bad header sequence, empty cursor or invalid type, but not by painting an invalid slot (skipped, drag stays armed). The held-drag preview (translucent wash plus per-cell count) uses the same split arithmetic that finishes the drag.

Drag distribution is `QuickCraftType::{Even, One, Clone}`. `Menu::do_click` validates the raw two-bit value before arming and stores the typed value; public preview and drag entry points keep raw integers for the input layer but reject unknown values. `Clone` needs infinite materials; `3` is unused.

Quick-move destination order is per menu kind: a generic container moves into the player inventory backwards (hotbar first) and out forwards; a crafting table loads its grid first; the player inventory has eight steps (result, craft grid, armour, auto-equip armour/offhand, main to hotbar, rest) where auto-equip must be reachable from every source slot including the offhand. The furnace family has one override: if the recipe-book sync declares the item's numeric id in the screen's cooking-input property set, prediction targets slot 0 only, with no fuel guessing (a non-input item or missing set keeps generic order; an input that cannot fit waits for server reconciliation). Brewing stand routing stays generic.

The input protocol (`MenuInput`: press/drag/release/keyPress) is a separate layer, and a correct machine can still have no caller. Number keys 1-9 in a container are `Swap` on the hovered slot (the hotbar binding is swallowed while any screen is open). `Q` in a container is a click with server correction; `Q` in gameplay (`DropSelectedItem`/`DropSelectedItemStack`) has no confirmation packet and must predict locally or the count never updates.

### Cost screens (anvil, grindstone, smithing, enchanting)

Enchant cost takes an `EnchantmentOfferSlot` (`Top`, `Middle`, `Bottom`), whose order drives both the RNG draw sequence and the distinct per-row formulas, so an out-of-range row cannot borrow the bottom rule. These screens are small `Generic` menus whose slot kinds differ (take-only output, lapis-only input) with positions from a `SpecialLayout`. Input-slot placement predicates that need registry data (smithing recipe check, grindstone damageable/enchanted check) are not modelled: anything placed is accepted and corrected by the server.

Anvil XP cost and enchanting offer costs arrive as `container_data` properties folded into `ContainerFrame::cost_data` and drawn with the HUD font (see [`hud.md`](./hud.md)). The same stream drives furnace lit/burn bars and brewing fuel/brew bars via `special_layout`. Enchant offer rows are clickable using the same rect the draw code computes, gated by lapis, level and cost as the original client pre-check does.

### Station widgets (enchanting, stonecutter, loom)

Each is a "predict, then send" surface with no local pending state: a valid click is the send. Click precedence: merchant trade rows, beacon buttons, enchant offers, stonecutter grid, loom grid, recipe-book panel, ordinary slot. A new surface must be added to every later stage's guard.

The stonecutter's ordered result rows from recipe synchronization are the single source for redraw, wheel range and click validation (the bundled `RecipeBook` could draw one order and send another's button id on a datapacked server). The frame carries the server rows and the start index from the persisted wheel offset; drawing applies `skip(start).take(12)` before filtering unresolvable icons so a blank row cannot renumber a later button. The loom's offers are a small table transcribed from the pack's banner-pattern tag JSON. Scroll is wheel-only (thumb drag unwired); loom pattern icons are a disclosed cut.

### Creative inventory

A 14-tab strip, scrollable 5x9 grid, search and inventory tabs, with contents from the creative-tab table (1725 items) cross-checked against the item registry. It is not a `MenuKind` (the backing menu has no server container), so it owns its layout while sharing the vertex and draw pipeline. It opens on the `instabuild` ability flag, not a game mode.

Deliberate departures: clicking a grid cell places the item straight into the selected hotbar slot (one wire action); the saved-hotbars tab is empty (no store); clicks on the real inventory slots beneath are consumed. Search is a case-insensitive substring of the registry path, not the display name; tag queries are not modelled.

### Merchant screen

Client UI half only: reached when the server opens a `minecraft:merchant` menu followed by offers (trade generation and the open path are server work that does not exist yet). Shape is `Generic { 3 }` (two payments, take-only result) with `SpecialLayout::Merchant`; its player-inventory section is the only layout whose x-offset is not the panel's left edge. The seven-row list is fixed pixel offsets (not slots): cost and result icons, demand-adjusted price with strikethrough when discounted, in/out-of-stock arrow. A row click sends `SelectTrade`; payment auto-fill, scrolling past seven offers and the XP bar are not modelled.

### Player preview

A live 3-D player rig in the inventory with head and eyes following the mouse; the first full entity rig inside a 2-D panel and first GPU scissor rect. It reuses the same function that places every world mob (composed, not restated). Head yaw is relative to body yaw (the head turns twice as far as the body; treating both as absolute draws a permanent over-the-shoulder look). It records after the panel background and before slot items, loading default skin sheets from the staged archive when no account skin exists (keep them in native and browser staging, or the preview cannot attach before a world opens).

The hover highlight uses the pack's front sprite, else a 16 px translucent fill and border above the item, independent of a background atlas. When the recipe book is open on a canvas narrower than 379 logical pixels, its page replaces inventory contents (background and preview remain; slots, labels and slot interaction are hidden); hit testing still separates the panel from the outside so an outside click keeps its meaning. Draw-range markers remain vertex counts.

### Potion effects (inventory)

Icon, translated name with level numeral and remaining duration on a nine-slice background. Order: non-ambient before ambient, finite before infinite, shorter duration first, then effect colour. Effect icons come from a second source directory in the pack's atlas definition, so code assuming one GUI sprite directory finds none. The HUD status-effect overlay is a different widget over the same state and gates on a per-effect "show icon" flag this one ignores.

## How to change it

- Match the reference behaviour before touching a click mode or layout; derive expected test values from the reference, never this port's prior output.
- Never add a `MenuKind` variant; add an additive descriptor on `Menu` (`CraftLayout`, `SpecialLayout`).
- Derive label anchors, panel height and the second label from the expression the panel art uses; a restated constant breaks when row count varies.
- The 3-D item pass needs both `models` and a real `depth` view; an attached-but-unfed pipeline reads as "block items render flat" because flat icons need no depth.
- A server-initiated close needs its own reconciliation with the screen state machine, or the screen draws a stale player inventory.
- Furnace input routing is driven only by the live server property set: keep numeric-id resolution at the shell boundary and carry identifiers through `PlayerCtx`; add no data dependency to `lodestone-game`. Brewing-stand and merchant routing need their own authoritative inputs, not hardcoded slots.

## Configuration

No flags. Behaviour depends on what is attached to the renderer, each with a legible fallback when absent: item atlas (colour swatch), item-model pipeline (flat sprites), background art (flat panel), and font (fixed-advance debug font); and on whether a `ContainerFrame` carries a cursor, cost data or trade offers.

## Dependencies

`lodestone-game` (`Menu`, `MenuKind`, `CraftLayout`, `SpecialLayout`, `click.rs`, `menus.rs`), `lodestone-server` (independent authoritative click port, not a build dependency), `crates/lodestone-shell/src/container.rs` and `container/{background,geometry,builder,renderer,layout,enchant,stonecutter,loom,merchant,player_preview}.rs`, `crate::hud::item_icon`, `lodestone-render`/`lodestone-assets`, [`ui-framework.md`](./ui-framework.md), [`hud.md`](./hud.md).
