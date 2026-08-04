# The cost-based container screens: anvil, grindstone, smithing, enchanting

## What it is

The client half of issues #253 (enchanting table), #254 (anvil / grindstone)
and #255 (smithing table): decoding, modelling and drawing the four
"item-combiner" screens whose result depends on a cost the server computes.

**This is the client half only.** The server computes the anvil's XP level
cost, the enchanting table's three offers, and the smithing/grindstone
results; none of that logic lives here or should. What this covers: the wire
packets are decoded, the menu models the right slot *kinds* and *positions*,
the screen draws with vanilla's real layout and background art, and clicks
hit-test correctly. See "What is not yet wired" below for the gap that
remains — the numeric costs themselves do not reach pixels yet, and closing
that gap needs a change outside this crate's files.

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
| `Anvil` | `0@27,47` `1@76,47` `2@134,47` | `AnvilMenu.java:42-45,58-60` |
| `Grindstone` | `0@49,19` `1@49,40` `2@129,34` | `GrindstoneMenu.java:48-60` |
| `Smithing` | `0@8,48` `1@26,48` `2@44,48` `3@98,48` | `SmithingMenu.java:25-29,58-61` |
| `Enchanting` | `0@15,47` `1@35,47` | `EnchantmentMenu.java:55-61` |

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

## What is not yet wired: the cost numbers

The anvil's level-cost text, and the enchanting table's three per-row costs
(plus the level-requirement clue and the seed used to pick which enchant a
row offers), arrive as `container_set_data` (`ClientboundSetContainerDataPacket`
— `EnchantmentMenu`'s ten `DataSlot`s, `AnvilMenu`'s one). That packet **is**
decoded (`ClientEvent::ContainerData` in `crates/protocol/v770/src/adapter.rs`)
and **is** folded (`Menus::container_data(property) -> Option<i32>` in
`crates/lodestone-game/src/menus.rs`) — this was already true before this
session's work, for the furnace's burn/cook progress.

It is an island past that point: nothing in `lodestone-shell` reads
`Menus::container_data`. The chain breaks at
`lodestone_client::state::OpenMenuSnapshot` (`crates/lodestone-client/src/state.rs`),
which carries `window_id`/`menu_type`/`title`/`menu` but no `data`, and at
`Sim::open_menu` (`crates/lodestone-shell/src/sim.rs:2490-2500`), which builds
that snapshot. Both would need a `data: Vec<(i32, i32)>`-shaped field added and
copied through before `app.rs`'s one `ContainerFrame` call site could carry it
to a draw. `sim.rs` is outside this doc's author's file ownership for this
session (combat-agent territory) — flagged here rather than edited.

Once that plumbing exists, drawing the numbers is comparatively small: the
anvil's cost is `container_data(0)` after `Menu::special_layout() ==
Some(Anvil)`; the enchanting costs are `container_data(0..3)`. Both would draw
with the same `VanillaFont`/`Builder::text_plain` machinery
[`vanilla-hud-text.md`](vanilla-hud-text.md) documents.

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
- `crates/protocol/v770/src/adapter.rs` — `CONTAINER_SET_DATA` decode (already
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
