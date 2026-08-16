# The cost-based container screens: anvil, grindstone, smithing, enchanting

## What it is

The client half of issues #253 (enchanting table), #254 (anvil / grindstone)
and #255 (smithing table): decoding, modelling and drawing the four
"item-combiner" screens whose result depends on a cost the server computes.

**This is the client half only.** The server computes the anvil's XP level
cost, the enchanting table's three offers, and the smithing/grindstone
results; none of that logic lives here or should. What this covers: the wire
packets are decoded, the menu models the right slot *kinds* and *positions*,
the screen draws with vanilla's real layout and background art, clicks
hit-test correctly, and — see "The cost numbers: wired" below — the anvil's
XP-cost text and the enchanting table's three per-row costs also reach
pixels. What is genuinely still missing on the client side is the input-slot
`mayPlace` predicates (smithing's recipe check, the grindstone's
damageable/enchanted check) and the enchanting names' cipher font; see
"Deliberately not modelled" below.

## How it works

### The model: `Menu::item_combiner` / `Menu::enchanting_table`

All four screens share `AnvilMenu`/`GrindstoneMenu`/`SmithingMenu`'s vanilla
base, `ItemCombinerMenu`, which is a plain `Generic { container_size }`
positionally — `getInventorySlotStart() == result_slot + 1`, so the quick-move
regions are exactly what `Menu::generic(container_size)` already produces. The
enchanting table isn't an `ItemCombinerMenu` in vanilla but is positionally
identical to a `Generic { 2 }` too (`EnchantmentMenu.quickMoveStack`'s own
`moveItemStackTo(stack, 2, 38, true)` confirms it).

So the model layer (`crates/lodestone-game/src/menu.rs`) adds two
constructors that build on `Menu::generic` and only change **slot kinds**:

- `Menu::item_combiner(container_size, result_slot, layout: SpecialLayout)` —
  marks `result_slot` as `SlotKind::Output` (take-only, matching
  `ItemCombinerMenu.createResultSlot`'s `mayPlace` override). Used for the
  anvil (`3, 2, Anvil`), grindstone (`3, 2, Grindstone`) and smithing table
  (`4, 3, Smithing`) — the anvil and grindstone are mechanically identical
  (same size, same result index) and are told apart only by `layout`.
- `Menu::enchanting_table()` — marks slot 1 as `SlotKind::LapisOnly` (a new
  `SlotKind` variant, `crates/lodestone-game/src/container.rs`), matching
  `EnchantmentMenu`'s anonymous `Slot` whose `mayPlace` checks
  `itemStack.is(Items.LAPIS_LAZULI)`.

`lodestone_game::menus::build_menu` dispatches to these by the wire
`menu_type`, with the same defensive `container_size` check
`is_crafting`/`Menu::crafting` already uses: if the server's actual content
length disagrees with what a real anvil/grindstone/smithing/enchanting screen
has, it falls back to a plain `Menu::generic` rather than building a menu that
contradicts the packet.

**Deliberately not modelled**: the input-slot `mayPlace` predicates these
menus actually declare (smithing's per-slot `RecipePropertySet` tests, the
grindstone's damageable-or-enchanted check) are server data this tree does not
have — the same "genuinely different, left on generic order" call
`build_menu`'s own doc comment already makes for the furnace and brewing
stand. Accepting anything client-side and letting the server's own
`container_set_slot` correct a wrong placement is the same bounded,
self-correcting cost: a visible flicker, not a desync.

### The pixel layout: `Menu::special_layout`

Vanilla's real slot positions for these four screens are not
`generic_layout`'s left-to-right grid — the anvil's two inputs sit at
`(27,47)`/`(76,47)` with the result at `(134,47)`, not three-in-a-row at
`y = 18`. A `SpecialLayout` enum (`Anvil | Grindstone | Smithing | Enchanting`)
is carried on `Menu` — the same "extra routing on `Menu`, not a new
`MenuKind`" pattern `CraftLayout` already established — and
`lodestone-shell/src/container.rs`'s `slot_layout(menu)` checks it first:

```rust
pub fn slot_layout(menu: &Menu) -> SlotLayout {
    if let Some(layout) = special_layout_positions(menu) {
        return layout;
    }
    match menu.kind() { /* ... unchanged ... */ }
}
```

| `SpecialLayout` | slots (menu index @ x,y) | source |
|---|---|---|
| `Anvil` | `0@27,47` `1@76,47` `2@134,47` | `AnvilMenu.createInputSlotDefinitions` |
| `Grindstone` | `0@49,19` `1@49,40` `2@129,34` | `GrindstoneMenu`'s constructor |
| `Smithing` | `0@8,48` `1@26,48` `2@44,48` `3@98,48` | `SmithingMenu.createInputSlotDefinitions` |
| `Enchanting` | `0@15,47` `1@35,47` | `EnchantmentMenu`'s constructor |

All four use vanilla's fixed `addStandardInventorySlots(inventory, 8, 84)` for
the player section, so `main_y = 84.0` is a constant, not derived from the top
section's row count the way `generic_layout`/`crafting_layout` compute it.

**Why this lives on `Menu` and not behind a `menu_type` parameter passed
separately to the draw path.** The first shape tried added a
`menu_type_slot_layout(menu_type, menu)` override, called only from
`build_inner` (mirroring `menu_type_title_anchor`, which is exactly this
shape and is correct for *that* case). It was wrong here specifically because
`slot_layout` has a **second** caller: `hit_test`/`hit_test_with_scale`, whose
own call sites are in `app.rs`. A `menu_type` threaded only into drawing would
have made the screen draw at vanilla's real anvil positions while clicks kept
hit-testing the old generic grid — this module's own documented failure class,
"clicks land one slot off... invisible in any screenshot." Putting the
discriminator on `Menu` instead means both callers see it for free, with zero
new parameters and zero new `app.rs` call sites.

### The background art

`ContainerBackground` (`lodestone-shell/src/container.rs`) now stitches four
more whole-panel `176×166` sheets — `gui/container/{anvil,grindstone,smithing,
enchanting_table}.png` — into the same hand-built atlas that already carries
`inventory.png`/`crafting_table.png`/`generic_54.png` (these are
`textures/gui/container/**`, not `textures/gui/sprites/**`, so they are not in
`GuiAtlas`; see that struct's own doc comment for why). `background_kind(menu)`
checks `menu.special_layout()` first, mirroring `slot_layout`'s own dispatch,
so the same `Menu` naturally selects both its real slot positions and its
real background with no separate wiring.

## The cost numbers: wired (this section used to say otherwise)

**Stale — re-verified rather than assumed.** This section
used to describe the anvil/enchanting cost feed as an island: decoded
(`ClientEvent::ContainerData`) and folded (`Menus::container_data`), but with
`lodestone_client::state::OpenMenuSnapshot` carrying no `data` field and
`Sim::open_menu` never populating one, so nothing in `lodestone-shell` could
reach it. That gap is closed — `OpenMenuSnapshot::data: Vec<(i32, i32)>`
exists (`crates/lodestone-client/src/state.rs`), `Sim::open_menu` fills it
from `menus.opened_data().to_vec()` (`crates/lodestone-shell/src/sim/session.rs::Sim::open_menu`),
and `app.rs`'s `ContainerFrame::with_cost_context` call already reads
`open_menu.data.as_slice()` through to `ContainerFrame::cost_data`. Re-checked
directly against the current source rather than trusted from this doc's own
prior claim — the exact staleness class `CLAUDE.md`'s rule 2 warns about.

That same feed is what let the furnace-family lit/burn bars and
brewing-stand fuel/brew/bubble bars (`container-screen.md`'s "The six more
`special_layout` screens" section) draw with **zero** further `app.rs`/`sim.rs`
changes: `frame.cost_data` was already the live `container_set_data` properties
by the time that work started, so `AbstractFurnaceMenu`'s `litTime`/
`litDuration`/`cookingProgress`/`cookingTotalTime` (properties `0..4`) and
`BrewingStandMenu`'s `brewingTicks`/`fuel` (properties `0..2`) needed only a
`menu.special_layout()` match in `container.rs`'s `build_inner`, the same place
the anvil/enchanting cost lines already read from.

**Drawing the numbers themselves is also done, not merely "comparatively
small" — a second stale claim in this same section, caught the same way.**
`draw_anvil_cost`/`draw_enchanting_costs` (`container.rs::geometry::draw_anvil_cost`,
`container.rs::geometry::draw_enchanting_costs`) are
real, non-stub implementations: the anvil's is/isn't-affordable colouring
(`AnvilMenu::mayPickup`), its `>= 40` "Too Expensive!" branch, and the right-
aligned backdrop text at `AnvilScreen.extractLabels`'s own `tx`/`ty`; the
enchanting table's three per-row costs at `EnchantmentScreen.extractBackground`'s
positions, deliberately **not** drawing the enchantment-name cipher text
(`EnchantmentNames`' Standard Galactic Alphabet font is a separate,
unstarted subsystem). Both read `frame.cost_data` — the now-confirmed-live
feed above — and both draw with the `VanillaFont`/`Builder::shadowed_label`
machinery [`vanilla-hud-text.md`](vanilla-hud-text.md) documents.

**The enchanting table's three offer rows are clickable, not just drawn (issue #613's
`ContainerButtonClick` remainder).** `crates/lodestone-shell/src/container/enchant.rs`:
`offer_rect` is the exact same local-widget-pixel geometry `draw_enchanting_costs` already
draws at (`xo + 60, yo + 14 + 19*i, 108, 19`), so the clickable area and the drawn button can
never disagree; `offer_clickable` transcribes `EnchantmentMenu.clickMenuButton`'s client-visible
gate (lapis count, offer cost, experience level, all skipped under `has_infinite_materials`) —
the same predicate vanilla's own client-side menu mirror runs before sending anything, since
its `access.execute` is a no-op there. `WindowApp::handle_enchant_click`
(`crates/lodestone-shell/src/app/container_input.rs`) wires it into the same first-refusal click
chain the beacon buttons use (`app/lifecycle.rs`), and `Sim::send_container_button_click`
(`crates/lodestone-shell/src/sim/session.rs`) sends `ClientAction::ContainerButtonClick`. Unlike
the beacon buttons there is no local pending state — a clickable hit *is* the send, and the
screen stays open afterwards (vanilla's `EnchantmentScreen` never closes on an offer press).
Vanilla's other two `ContainerButtonClick` screens (stonecutter recipe list, loom pattern list)
are out of scope: both need a server-populated selectable-list this tree has no registry sync
for (`StonecutterMenu`/`LoomMenu`'s `selectableRecipes`), a different shape from the
container-data-driven enchant offers.

## How to change it

- Add a fifth `SpecialLayout` variant for a new screen the same way: a
  `menu.rs` constructor that builds on `Menu::generic`, a `slot_layout`/
  `background_kind` case in `container.rs`, and (if the panel differs) a new
  sheet loaded in `ContainerBackground::build`.
- **Never add a case to `MenuKind`.** It is matched exhaustively across this
  crate; `CraftLayout` and `SpecialLayout` exist specifically so new screens
  never need to.
- If a screen's input slots ever need a real `mayPlace` predicate (matching
  vanilla's `RecipePropertySet`/damage checks), that needs a data source this
  tree does not have yet — see "Deliberately not modelled" above before
  guessing one.

## Configuration

None — no flags or env vars gate this.

## Dependencies

- `crates/lodestone-game/src/{menu.rs,menus.rs,container.rs}` — the model.
- `crates/lodestone-shell/src/container.rs` — layout, background, draw.
- `crates/lodestone-shell/src/container/enchant.rs`,
  `crates/lodestone-shell/src/app/container_input.rs`'s `handle_enchant_click`,
  `crates/lodestone-shell/src/sim/session.rs`'s `send_container_button_click` — the
  enchant-offer click producer.
- `crates/protocol/v770/src/adapter/inventory.rs`'s `handle_play_inventory` — `CONTAINER_SET_DATA` decode (already
  present, unmodified by this work).
- [`container-screen.md`](container-screen.md) — the general container-screen
  machinery this builds on.
- [`vanilla-hud-text.md`](vanilla-hud-text.md) — the font/outline machinery a
  future cost-number draw would use.

## Verification

```bash
cargo test -p lodestone-game --lib --no-fail-fast -- menu:: menus::
cargo test -p lodestone-shell --lib --no-fail-fast -- container::
cargo test -p lodestone-shell --test container_special_layout_pixels -- --ignored --nocapture
```
