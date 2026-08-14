# The merchant/trading screen

## What it is

The client-side villager/wandering-trader trading screen — vanilla's
`MerchantScreen` over a `MerchantMenu` — reached when the server sends `OPEN_SCREEN`
with `menu_type = minecraft:merchant` followed by `MERCHANT_OFFERS`. This is issue
#245's UI half, client-side only: it renders the real panel, the two payment slots
and the take-only result slot, the seven-row scrollable trade list with prices and
availability arrows, and sends `SELECT_TRADE` when a row is clicked. It does not
generate trades, assign professions, or open the screen from an in-game right-click
— that is the server half (#245's core / V4 in `docs/plans/villager-economy.md`),
not built yet.

## How it works

### Menu shape

`Menu::merchant()` (`crates/lodestone-game/src/menu.rs`) builds vanilla's real
`MerchantMenu` shape: payment slots `0`/`1`, a take-only result slot `2`
(`SlotKind::Output`), then the player's main storage and hotbar — mechanically a
`MenuKind::Generic { container_size: 3 }` with `SpecialLayout::Merchant` attached,
the same "generic quick-move order, special pixel layout" pattern the anvil,
furnace family, brewing stand and every other `SpecialLayout` already use.
`crate::menus::build_menu` (`lodestone-game`) selects it for a real
`(menu_type == "merchant", container_size == 3)` pair — checked through the same
`ScreenOpened` → `ContainerContent` dispatch every other special layout is, not by
calling the constructor directly, with a size-guard control mirroring the
anvil/grindstone/smithing one right above it.

`MerchantMenu.quickMoveStack`'s own routing (payment slots move to the player
inventory **forwards**, not the generic order's backwards) is **not** modelled —
the same "genuinely different, left on generic order" gap the furnace and brewing
stand already carry, for the same reason: a self-correcting, bounded cost (one
round trip once a server exists to correct it against), not a guess.

### Layout and background

`crates/lodestone-shell/src/container/layout.rs`'s `special_layout_positions`
places the three real slots at vanilla's own `(136,37)`/`(162,37)`/`(220,37)`
(`MerchantMenu.java:42-44`) and the player section at `x = 108` — the **only**
special layout whose player section is not left-aligned with the panel
(`MerchantMenu.java:45`'s `addStandardInventorySlots(inventory, 108, 84)`). The
panel itself is `276×166`, not the usual `176×166`
(`MerchantScreen.java:57`). `append_main_inventory_at` generalises the shared
main-inventory-grid helper with an `x` parameter for this one case;
`append_main_inventory` is the `x = 8` convenience every other screen keeps
using.

`crates/lodestone-shell/src/container/background.rs` loads
`minecraft:gui/container/villager` (a `512×256` sheet, unlike every other sheet
in this atlas) and blits its top-left `276×166` corner — `whole_panel_sized`
already generalises to an arbitrary sub-rect regardless of the source sheet's own
size, so this needed no new drawing primitive.

### The trade list

`crates/lodestone-shell/src/container/merchant.rs` is the trade-list layout,
pricing and hit-test — vanilla calls these "fake items" (`graphics.fakeItem`)
because they are not menu slots at all, just seven rows of icons drawn at fixed
pixel offsets, scrolled by an index this client does not yet model (see
"What is not modelled" below). `row_layout(i)` gives each visible row's cost-A,
cost-B, result and arrow positions, transcribed from
`MerchantScreen.java:36-53,167-219`; `button_rect(i)`/`hit_test_local` are the
matching clickable region vanilla's seven `TradeOfferButton`s occupy.
`adjusted_cost_a_count` ports `MerchantOffer.getModifiedCostCount`
(`MerchantOffer.java:114-118`) — the demand/reputation pricing arithmetic that
makes a trade's *displayed* price differ from its base, which is what the
discount-strikethrough sprite is keyed on.

`cost_item_stack` resolves a wire `MerchantOffer`'s raw `(item registry id,
count)` cost pair into a displayable stack via
`lodestone_data::items::item_name` — the one place this client reaches into the
generated protocol-776 item table rather than reading an already-resolved
`ItemStack` off an event, because `ItemCost` carries no component data to build
one from (see `lodestone_game::trades`'s own module doc).

`crates/lodestone-shell/src/container/geometry.rs`'s `draw_merchant_trades`
draws the list: each visible offer's cost/result icons (through the same
`Builder::draw_stack` every slot icon already uses — a jar-less run gets the
same hash-swatch fallback), the discount strikethrough when a price is
demand-adjusted, the trade arrow (normal or out-of-stock), and the out-of-stock
overlay for the *selected* row only. It is a no-op for any screen without
`SpecialLayout::Merchant`, or a merchant screen with no `TradeOffers` attached
yet (`ContainerFrame::trades` is `None`) — every existing caller.

### The composed title and the second label

`MerchantScreen.extractLabels` composes the villager's level into the title
itself (`"<name> - <level word>"`, vanilla's `merchant.title` = `"%s - %s"`)
rather than merely moving the anchor, which is why `menu_type_title_anchor`
used to exclude `merchant` entirely. `container::merchant_title`
(`crates/lodestone-shell/src/container/frame.rs`) does the composition — through
`lodestone_game::text::resolve_to_string`, the same nested-translation resolver
every other server-authored title goes through — and `menu_type_title_anchor`
now has its own `merchant` branch for the level-badge title's real centring
formula (`49 + imageWidth/2 - textWidth/2`, not the plain centring the furnace
family uses). The second label, "Trades" (`merchant.trades`), has no field in
`LabelLayout` at all — it is drawn directly by `draw_merchant_trades`, at a
fixed position vanilla's own screen hardcodes rather than a per-screen anchor.

### Selecting a trade

`crates/lodestone-shell/src/app/container_input.rs`'s `handle_merchant_click`
gets first refusal on a left-press the same way the recipe-book panel does
(`WindowApp::handle_recipe_panel_click`'s established pattern in
`crates/lodestone-shell/src/app/lifecycle.rs`): resolve the click against
`merchant::button_hit_test`, remember the row in `WindowApp::merchant_selected`,
and send `ClientAction::SelectTrade` through `Sim::send_select_trade`. Both the
action variant and every protocol family's encoder already existed
(`ClientAction::SetFlying`'s outbound-island shape) — this is the first shell
caller.

## How to change it

- **Trade-row pixel constants** live at the top of `container/merchant.rs`,
  each cited against `MerchantScreen.java`. Re-derive from the real screen's
  `init`/`extractContents`, never from a summary — see the module's own "how to
  change it" note on the transposition trap this repo has hit before.
- **Adding vanilla's scroller or experience bar** needs new sprite quads in
  `geometry.rs`'s `draw_merchant_trades` (the sprite ids are already reserved in
  `container.rs`'s `MERCHANT_*` consts for the ones that *are* drawn; the
  scroller/XP-bar ids are not yet reserved) plus, for the XP bar,
  `VillagerData.getMinXpPerLevel`/`getMaxXpPerLevel` thresholds this tree does
  not carry.
- **A new `SpecialLayout` case anywhere else in this crate family** (background,
  layout, frame) should follow this one's shape: attach the discriminator to
  `Menu` via `Menu::special_layout`, never grow `MenuKind` — see
  `lodestone_game::menus::build_menu`'s own doc comment for why.

## What is not modelled (named gaps, not bugs)

- **Scrolling past the first seven offers.** `OFFER_ROWS = 7`; a merchant with
  more trades only shows and can select the first seven. Vanilla's scroller
  sprite pair is not drawn.
- **The experience/level progress bar** (`extractProgressBar`) — needs XP
  threshold data this tree does not have yet.
- **`MerchantMenu.tryMoveItems`** — vanilla auto-fills the two payment slots
  from the player's own inventory when a trade row is selected. Not predicted
  client-side; the server (once it exists) would correct an empty payment slot
  after a select regardless.
- **The real `OPEN_SCREEN` entry point.** Nothing server-side sends
  `menu_type = minecraft:merchant` yet (villager right-click, wandering trader,
  the whole of #245's core) — see `docs/plans/villager-economy.md`'s V4. Until
  it lands, this screen is reachable only by synthesizing the packets this
  module's own tests do.

## Configuration

None — no env vars, flags or config files. `OFFER_ROWS` is the one tunable
constant, and changing it without adding scrolling would just show a different
fixed prefix of a longer trade list.

## Dependencies

`lodestone_game::menu` (`Menu::merchant`, `SpecialLayout::Merchant`),
`lodestone_game::trades::TradeOffers` (the `MerchantOffersReceived` fold),
`lodestone_game::text::resolve_to_string` (title composition),
`lodestone_data::items` (registry id → item name for the two cost pairs),
`lodestone_model::event::MerchantOffer`. Rendering rides the same
`crate::hud::item_icon` / `ContainerBackground` / `Builder` machinery every
other container screen in `crates/lodestone-shell/src/container/` uses — no new
GPU pipeline.
