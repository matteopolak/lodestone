# The container / inventory / crafting screen

## What it is

`crates/lodestone-shell/src/container.rs` — the screen that draws an open
[`Menu`](../crates/lodestone-game/src/menu.rs): a panel, a well per slot, the
slot contents, and (for menus that have one) a crafting grid and result slot.

It is a *projection only*. Slot state is folded by `lodestone-game`; this module
turns a `Menu` into rectangles and vertex streams and never mutates anything.

Item icons come from the shared pass documented in
[`gui-item-icons.md`](gui-item-icons.md) — the same code, atlases and tint
palette the hotbar uses. Read that for anything about the icons themselves.

## How it works

### Layout

`slot_layout(&Menu) -> SlotLayout` dispatches three times:

```
fn slot_layout(menu: &Menu) -> SlotLayout {
    if let Some(layout) = special_layout_positions(menu) {  // anvil, grindstone,
        return layout;                                       // smithing, enchanting
    }
    match menu.kind() {
        MenuKind::Player            => player_layout(),
        MenuKind::Generic { size }  => match menu.craft_layout() {
            Some(craft) => crafting_layout(craft, size),   // crafting table
            None        => generic_layout(size),           // chest, barrel, ...
        },
    }
}
```

Both the `craft_layout` and `special_layout` checks are **additive on `Menu`, not
a new `MenuKind`**, and that is deliberate. A crafting table *is* a generic
container as far as sizing and quick-move regions are concerned — vanilla's
`CraftingMenu` is positionally identical to a `Generic { container_size: 10 }`.
The anvil, grindstone, smithing table and enchanting table (issues #253-#255) are
the same shape one level further: all four are `ItemCombinerMenu`-family menus
whose quick-move regions are exactly `Menu::generic`'s (`lodestone_game::menu::Menu::item_combiner`'s
own doc comment has the `getInventorySlotStart() == result_slot + 1` proof), so
only their **slot kinds** (`SlotKind::Output`, `SlotKind::LapisOnly`) and their
**pixel positions** differ, not their `MenuKind`. `MenuKind` is matched
exhaustively across this crate, so a new variant would have broken every match;
`Menu::craft_layout()` and `Menu::special_layout()` exist for exactly this.

**`special_layout_positions` is checked *inside* `slot_layout` itself, not via a
`menu_type`-keyed override function called separately by drawing and by
[`hit_test`]/[`hit_test_with_scale`].** That was the first shape tried and it was
wrong: `hit_test`'s callers are in `app.rs`, so a `menu_type` parameter added only
to the draw path would have made clicks land on the *old* generic-grid positions
while the screen visibly drew at the *real* anvil/grindstone/smithing/enchanting
ones — this module's own documented failure mode ("clicks land one slot off... a
bug invisible in any screenshot"). Putting the discriminator on `Menu` instead
means the one `slot_layout(menu)` call both `build_inner` and `hit_test` already
make picks it up for free.

Every `SlotRect` carries the **real `menu_index`**. There is no constant offset
anywhere, and none should be reintroduced:

| menu | slot indices |
| --- | --- |
| `Player` (window 0) | `0` result, `1..=4` 2×2 craft, `5..=8` armour, `9..=35` main, `36..=44` hotbar, `45` offhand |
| `Generic { n }` | `0..n` container, `n..n+27` main, `n+27..n+36` hotbar — **no** armour, **no** offhand |
| crafting table | a `Generic { 10 }`: `0` result, `1..=9` grid, `10..=36` main, `37..=45` hotbar |
| anvil / grindstone | a `Generic { 3 }`: `0`, `1` input, `2` result (take-only), `3..=29` main, `30..=38` hotbar |
| smithing table | a `Generic { 4 }`: `0` template, `1` base, `2` addition, `3` result (take-only), `4..=30` main, `31..=39` hotbar |
| enchanting table | a `Generic { 2 }`: `0` item, `1` lapis-only, `2..=28` main, `29..=37` hotbar |
| furnace/blast furnace/smoker | a `Generic { 3 }`: `0` ingredient, `1` fuel, `2` result (take-only), `3..=29` main, `30..=38` hotbar |
| brewing stand | a `Generic { 5 }`: `0..=2` potions, `3` ingredient, `4` fuel, `5..=31` main, `32..=40` hotbar |
| loom | a `Generic { 4 }`: `0` banner, `1` dye, `2` pattern, `3` result (take-only), `4..=30` main, `31..=39` hotbar |
| stonecutter | a `Generic { 2 }`: `0` input, `1` result (take-only), `2..=28` main, `29..=37` hotbar |
| cartography table | a `Generic { 3 }`: `0` map, `1` additional, `2` result (take-only), `3..=29` main, `30..=38` hotbar |
| dispenser/dropper | a `Generic { 9 }`: `0..=8` a 3×3 grid, `9..=35` main, `36..=44` hotbar |
| hopper | a `Generic { 5 }`: `0..=4` a row, `5..=31` main, `32..=40` hotbar |

`crafting_layout` uses vanilla's `crafting_table.png` slot origins for the 3×3
case — grid at `(30, 17)`, result at `(124, 35)`, main at `(8, 84)`, hotbar at
`(8, 142)`, panel `176×166` — expressed in terms of the grid's real dimensions so
a differently sized grid still lands somewhere sane.

`special_layout_positions` (the anvil/grindstone/smithing/enchanting case) is
simpler: all four use vanilla's **fixed** `addStandardInventorySlots(inventory,
8, 84)` for the player section, so `main_y` is a constant `84.0` rather than
computed from the top section's own row count. See
[`container-cost-screens.md`](container-cost-screens.md) for the full slot
position table, the background art, and what these screens' "cost" (the anvil's
level-cost number, the enchanting table's three offer costs) does and does not
reach yet.

### The six more `special_layout` screens (issue #28), plus a seventh found while writing this

Issue #28 ("container screens: the whole family") extended the same
`SpecialLayout`/`background_kind` pattern to the furnace family (`Furnace`,
`BlastFurnace`, `Smoker` — one variant per texture, all three sharing the same
three slot coordinates), the brewing stand, the loom, the stonecutter, the
cartography table and the dispenser/dropper (`Dispenser`, shared by both —
vanilla ships no `dropper.png` or `DropperScreen` at all). Every one of them
is mechanically a plain `Menu::generic` — same "accept anything, let the
server's own `container_set_slot`/quick-move correct a wrong guess" order the
anvil/grindstone/smithing family already established — with only a take-only
result slot marked where one exists (furnace, loom, stonecutter, cartography
table) and a `SpecialLayout` attached for the real pixel positions and
background sheet. `special_layout_positions`'s own doc comment in
`container.rs` has the full position table with `file:line` citations; it is
not repeated here to avoid a second copy that can drift.

**A seventh, `Hopper`, was found while writing that doc comment — not one of
#28's own named containers.** The comment being drafted claimed hoppers drew
`generic_54`'s ordinary chest sheet correctly, which turned out to be false:
`HopperScreen` is a real, dedicated screen at `imageHeight = 133`, not `166`
(`HopperScreen.java:15`), with five slots at `(44, 20)` step `18`
(`HopperMenu.java:24`) and `main_y = 51`, not `84`
(`HopperMenu.java:27`). Before this, a hopper silently drew a taller,
wrong-shaped chest panel — exactly the "plausible but transposed" defect
class this whole family of code exists to avoid, just discovered by writing
the documentation rather than by looking at a screenshot.

**Three of the six #28 screens knowingly draw fewer slots than vanilla's real
UI**, because the missing piece is a whole button-driven sub-feature this
tree has no data for, not a slot position: the loom's banner-pattern
selection grid, the stonecutter's recipe-selection scroll list (both need a
registry this tree does not decode, plus a `ContainerButtonClick` producer
that does not exist yet), and — a gap #28 did **not** close — the beacon and
the villager's trade screen are not modelled at all (different `imageWidth`,
no slot layout, and for the villager an entire undecoded trade-offer packet
family). See `SpecialLayout::Loom`/`SpecialLayout::Stonecutter`'s doc
comments for the specifics. This is a deliberate, honest degrade: the three
core slots each screen *does* draw are fully functional (place an item, take
the result once the server computes and sends it), and nothing is drawn that
looks clickable but silently does nothing.

**The furnace family and the brewing stand draw real, live progress
bars — not a degrade.** `AbstractFurnaceMenu`'s `litTime`/`litDuration`/
`cookingProgress`/`cookingTotalTime` and `BrewingStandMenu`'s
`brewingTicks`/`fuel` arrive as ordinary `container_set_data` properties,
the same feed `container-cost-screens.md`'s anvil/enchanting cost lines
already read through `ContainerFrame::cost_data` — so this needed no new
`app.rs`/`sim.rs` wiring, only a `menu.special_layout()` match in
`build_inner` computing the same sub-rectangle blits
`AbstractFurnaceScreen.java:53-72`/`BrewingStandScreen.java:29-52` do.
`ContainerBackground::sprite_subregion_quad` is the new primitive this
needed: every existing sprite draw in this file blits a sprite *whole*
(`sprite_quad`), but these bars grow from a partial window of a larger
sprite (e.g. the lit flame samples a `14×n` slice of a `14×14` sprite,
offset from the bottom) — see its own doc comment.

### The title goes through the language table (issue #52)

A server does not send the word "Crafting". `ClientboundOpenScreen` carries a
component — `translate("container.crafting")` — and turning that into words is the
*client's* job. The screen's title used to be built with

```rust
(Some(&open.menu), open.title.to_plain_string())   // app.rs, before
```

which drew a literal **`CONTAINER.CRAFTING`** on the panel.

`Text::to_plain_string()` is not a translator. It flattens against
`lodestone_model::text::default_translation`, a **fourteen-key stub** covering
chat, join/leave and six death messages; there is no `container.*` entry, so the
key falls through to itself. It is correct on a tree that has *no* `translate`
nodes left — notably the output of `lodestone_game::text::resolve` — and in logs
and tests. It is wrong on anything a server authored.

`container::menu_title(&Text, &translate)` is the read-boundary resolution, the
same shape `scoreboard::sidebar_from`, `tablist::player_rows` and
`overlay::boss_bars_from` use, and `app.rs` passes `Sim::translator()`:

```rust
crate::container::menu_title(&open.title, self.sim.translator().as_ref())
```

Fallback order is `table[key]` → the component's own `fallback` → the key. The
demo palette loads no `en_us.json`, and a renamed chest arrives as a plain
literal; neither may cost the title.

### The two labels (issue #370)

`AbstractContainerScreen::extractLabels` draws **two** pieces of text, not one
(`AbstractContainerScreen.java:189-191`):

```java
graphics.text(font, title,                titleLabelX,     titleLabelY,     -12566464, false);
graphics.text(font, playerInventoryTitle, inventoryLabelX, inventoryLabelY, -12566464, false);
```

`-12566464` is `0xFF404040`, a dark grey, and the trailing `false` means **no
drop shadow**. `container::label_layout(menu, layout)` returns the anchors:

| screen | `titleLabelX` | second label | vanilla source |
| --- | --- | --- | --- |
| generic container | `8` | yes | `AbstractContainerScreen.java:68-71` |
| crafting table | `29` | yes | `CraftingScreen.java:22` |
| player inventory | `97` | **no** | `InventoryScreen.java:29, 73-75` |

`titleLabelY` is `6` everywhere. The second label is always
`(8, imageHeight - 94)`, and `SlotLayout::height` *is* vanilla's `imageHeight`
(166 for the player and crafting panels, `114 + rows * 18` for a chest — both
asserted against vanilla's own constructors in `tests/container_labels.rs`), so
the anchor is derived from the same expression the panel art is blitted with.
**Never restate it as a number**: a 6-row chest's label sits 54 px below a 3-row
chest's, and `CLAUDE.md` records what hardcoding a moving anchor cost the HUD.

The player inventory screen is the **only** screen that omits the second label,
and it does so by overriding `extractLabels` to drop the second `graphics.text`
call. Removing the label globally trades one bug for another — a chest, a furnace
and a crafting table all draw it.

What was wrong before, all of it reported as one blurred "the font is wrong":

* The title was pushed through `to_ascii_uppercase()`. Vanilla never does, and
  `hud::font` has had lowercase glyphs since it was written. The visible cost was
  worst on the thing that prompted the report: a chest renamed "Loot" in an anvil
  drew as **LOOT**.
* It drew via `ColourStream::text` — the fixed-advance **5×7 debug font** — while
  `Builder` was already holding a `VanillaFont` for stack counts. Right glyphs,
  wrong typeface, wrong advances. `Builder::label` now picks the proportional
  font, through `VanillaFont::draw_plain` (added for this: every other text
  surface in the crate is shadowed, and these two labels are not).
* `titleLabelY` was `7`, and `titleLabelX` was `8` on every screen.
* The player inventory screen's title was the literal `"Inventory"`, hardcoded in
  `app.rs`. Wrong twice: vanilla's title there is
  `translatable("container.crafting")` — **"Crafting"**, naming the 2×2 grid
  (`InventoryScreen.java:28`) — and going in as the *title* it was drawn at the
  title anchor, which on that one screen is `x = 97`, not `8`.

`container::player_inventory_title` and `player_inventory_label` resolve
`container.crafting` and `container.inventory` through the language table.
The second one being a local key is **not** issue #52 repeating: vanilla reads
`playerInventoryTitle` from `Inventory.getDisplayName()`, itself the client-side
constant `translatable("container.inventory")` (`Inventory.java:55`), so there is
no server component to resolve. A container's *title* is the opposite case and
must always come from the packet.

**Stale as of issue #28 (was: "not modelled").** This used to say
`AbstractFurnaceScreen.java:39`'s centred title needed a furnace `MenuKind`
that did not exist. It never did: `menu_type_title_anchor`'s own doc comment
(`container.rs`) keys off the wire `menu_type` string directly, independent
of `MenuKind` or `SpecialLayout`, and it has carried the furnace family's
centred anchor (and eight more screens') since before #28's slot-layout work
landed. What #28 *did* add is `SpecialLayout::{Furnace,BlastFurnace,Smoker}`,
which gives the furnace family real slot positions and background art on top
of the title anchor it already had; see "The six more `special_layout`
screens (issue #28)" below.

### The panel is real vanilla art now (issue #51)

`ContainerBackground` (`container.rs`) loads and stitches vanilla's real
`textures/gui/container/*.png` sheets — `generic_54`, `crafting_table`,
`inventory`, plus (issues #253-#255 and #28) `anvil`, `grindstone`,
`smithing`, `enchanting_table`, `furnace`, `blast_furnace`, `smoker`,
`brewing_stand`, `loom`, `stonecutter`, `cartography_table` and `dispenser`
(shared by the dropper too — see "The six more `special_layout` screens"
below) — and `ContainerRenderer::attach_background` binds them, exactly the
"is a thing attached" pattern `attach_items`/`attach_item_models` already
use. **Stale as of issue #28** (was: "does not add furnace/hopper/anvil/etc.
backgrounds, because this crate has no slot layout for those screens yet") —
hoppers and shulker boxes still draw the plain `generic_54` sheet, correctly:
vanilla itself draws a hopper as an oddly-shaped **generic** container
background, not a dedicated sheet (`HopperMenu` declares no
`Screen`/texture override), so there was never a gap there to close.

These three PNGs are **not** `GuiAtlas` material: they live at
`textures/gui/container/**`, not `textures/gui/sprites/**`, carry no sibling
`.mcmeta`, and vanilla does not scale them through any of `GuiScaling`'s three
modes at all — it blits hand-placed sub-rectangles of each 256×256 sheet at
native size. The generic chest case is genuinely two blits
(`ContainerScreen.java:21-27`): a row-count-dependent top piece
(`0,0,176,rows*18+17`) and a fixed 96 px bottom piece immediately below it
sampled from `v=126` regardless of row count. `crafting_table`/`inventory`
each blit one whole `176x166` panel. `ContainerBackground::quads` computes
these UV windows by hand against the atlas's own sprite placement — see its
doc comment before reaching for `GuiScaling` on a similar problem; that type
has no "arbitrary sub-rect" mode and was never meant to grow one for this.

With a background attached, the flat panel fill and the per-slot well
rectangles this screen has always drawn are **suppressed** — the real
sheet's own art already bakes in every well at the exact pixel offsets
`slot_layout` targets (the layout constants were themselves transcribed from
vanilla's sheets), so drawing a second flat well on top would be pure noise.
Title text switches from the fallback's warm-light colour to vanilla's own
dark grey (`0xFF404040`, `InventoryScreen.java:76`) for the same reason: the
fallback colour was chosen to read against the flat dark fill, not against a
light wood panel.

### The dim behind the panel (issue #61's leftover)

`AbstractContainerScreen::isInGameUi()` overrides `true`
(`AbstractContainerScreen.java:535-538`), which routes `Screen.render`'s
`extractBackground` to `extractTransparentBackground` (`Screen.java:375-377`)
— a full-canvas **vertical gradient**, ARGB `(192,16,16,16)` top to
`(208,16,16,16)` bottom, not the pause menu's tiled
`inworld_menu_background.png` (that is the `isInGameUi() == false` branch —
see `pause-menu.md`). `Builder::gradient_rect_px` reproduces it: a two-colour
rect whose vertices the GPU interpolates, on
`ContainerGeometry::dim_vertex_count`'s own leading range of `verts`, drawn
**unconditionally** whenever a menu is open (independent of whether a real
background is attached).

This is what dims the HUD hotbar for free, per `pause-menu.md`'s "issue #61"
section: the HUD draws unconditionally behind any world-following screen
(`hud_follows_world`), and `app.rs`'s per-frame draw now runs the container
pass **after** the HUD pass (previously it ran before, which is why the dim
alone was not enough — the HUD painted right back over it every frame). The
dim is draw order, not a per-element alpha; `pause-menu.md`'s "There is no
per-element dimming here, and adding one would be the wrong shape" note is
satisfied the same way the pause overlay already satisfies it.

Pass order inside `ContainerRenderer::render_with_icons_scaled` is now four
stages, not three: **dim** (no depth) → **background texture** (no depth, if
attached) → **rest of chrome** (no depth: the flat-fill fallback when there is
no texture, the title, the wells when there is no texture) → **item models**
(depth, cleared) → **flat icons + text** (no depth). The dim has to precede
the texture — vanilla draws its own panel art *after* its dim, not the other
way around — which is why `ContainerGeometry` carries two split markers now,
`dim_vertex_count` and `chrome_vertex_count`, not one.

Proven by `tests/container_background_pixels.rs` (`#[ignore]`d, GPU +
`client.jar`): a point inside the panel differs measurably between a real
background and the flat fill (claim 1), and a hotbar-cell pixel drawn by a
real `HudRenderer` reads measurably darker once a container screen draws on
top of it than with the identical two-pass sequence and the container frame
closed (claim 2's executed negative control).

### The result slot is the server's

**Do not add a local recipe matcher to fill the result slot.** Vanilla computes
the crafting result *server-side*: `CraftingMenu.slotsChanged` sends a
`container_set_slot` for slot 0, and the vanilla client never matches recipes to
fill it. That already flows through `Menus::apply` -> `ClientMenu::reconcile`, so
reading `menu.slot_item(craft.result_slot)` **is** reading server truth.

`Menus::predicted_craft_result` exists for the recipe book and for ghost
previews, and must never be written into the result slot — it would overwrite
truth with a guess whenever the two disagree. See
[`crafting.md`](crafting.md) for the matching model itself.

### Vertex streams and pass order

`ContainerGeometry` carries three streams:

| field | format | contents |
| --- | --- | --- |
| `verts` | `[x, y, r, g, b, a]` NDC | panel, title, wells, stack counts, durability bars |
| `item_verts` | `[x, y, u, v, r, g, b, a]` | flat sprite icons off the `ItemAtlas` |
| `model_verts` | `ModelVertex` | 3-D block items, CPU-posed into GUI pixel space |

The colour stream carries things from **two different layers** — chrome that goes
under the icons and text that goes over them — so it is emitted in two runs (all
wells first, then everything per-slot) and `chrome_vertex_count` records the
split. `render_with_icons` draws it as two ranges of one buffer with the icon
passes in between. Emitting a stack count in the wells loop would bury it under
its own icon.

### The carried stack

`Menu::carried()` — the stack the player has picked up and is dragging — draws
**last**, after every slot, through the same `Builder::draw_stack` helper the
per-slot loop uses (real icon if an atlas resolves it, else the hash-derived
swatch fallback).

**"Last on the stream" was not enough, and this section used to say it was.** It
claimed "every pass draws its stream in append order, so the carried stack lands
on top of whatever slot the cursor happens to be over" — true for two of the four
combinations and false for the other two, which is issue #377 and was reported
from play as "the cursor stack draws under the slot items". The item passes run
**model first, then flat sprites** (only the model pass needs a depth
attachment), so a *3-D block* on the cursor drew under a slot's flat sprite; and
two blocks at the same GUI depth resolve against the depth buffer rather than
against append order. On top of that the slot layer's stack-count glyphs are on
the colour stream's second run and painted over the carried icon too.

The carried stack is therefore its own **stratum**: `build_inner` records
`slot_vertex_count` / `slot_item_vertex_count` / `slot_model_vertex_count` /
`slot_special_count`, and `render_with_icons_scaled` replays all three streams in
`container-carried-model-pass` + `container-carried-pass` after every slot pass.
The carried model pass **clears depth again**, which is the load-bearing part and
is exactly what vanilla's `graphics.nextStratum()`
(`AbstractContainerScreen.java:126`) buys. Full account, including the
four-case table and the measured control, in
[`gui-item-icons.md`](gui-item-icons.md); gate is
`tests/container_cursor_pixels.rs`.

It only draws when `ContainerFrame::cursor` is `Some([x, y])` — viewport
pixels, the same space `hit_test` takes, **not** local widget coordinates.
`ContainerFrame::new` leaves it `None`, so an unmodified caller (every existing
one, including `app.rs`'s current call site) draws exactly as before; a caller
that wants the carried stack to actually appear must chain
`.with_cursor(Some([x, y]))`. See "Wiring it into the live app" below for the
one line `app.rs` needs.

There is no tooltip yet, so "below the tooltip" (vanilla's actual third layer)
is currently moot — the carried stack is simply the topmost thing this screen
draws.

### The drag preview, and the one number that must not be re-derived (issue #378 part 2)

While a paint-drag is held, vanilla shows the *provisional* result of releasing
now: in each painted cell a 50%-white wash (`fill(…, -2130706433)` = `0x80FFFFFF`)
under the stack that cell would receive, and on the cursor the count it would be
left holding. This client accumulated the paint set and drew none of it.

`ContainerFrame::with_drag(Some((kind, painted)))` turns it on; `app.rs` passes
`MenuInput::drag_paint()` straight through. `None` — the default — draws nothing,
so every headless caller and gate is unchanged.

**The split arithmetic is not in this file.** `Menu::quick_craft_plan`
(`lodestone-game/src/click.rs`) produces the per-cell totals, and
`finish_quick_craft` distributes with the *same call*. Vanilla writes the formula
out three times — `doClick`'s end arm (`AbstractContainerMenu.java:377-390`),
`extractSlot` (`AbstractContainerScreen.java:202-222`) and
`recalculateQuickCraftRemaining` (`:248-267`) — and a preview that disagreed with
the outcome would be worse than no preview, so there is one copy here.

Three details are transcribed on purpose and all three are easy to lose:

* **`quickCraftSlots.size() == 1` draws nothing at all.** `extractSlot` `return`s
  before anything (`:203-205`), so the cell blanks — including whatever it already
  held. A one-cell drag is about to be re-dispatched as an ordinary click.
* **A clamped count is yellow** (`:212-215`), which is why `QuickCraftCell` carries
  a `clamped` flag rather than the caller re-deriving it. The ink is threaded
  through `item_icon::draw_item_icon_counted`; every other caller passes
  `COUNT_INK`.
* **A `CLONE` drag's previewed cursor deliberately does not match the outcome.**
  `recalculateQuickCraftRemaining` assigns `maxStackSize` outright (`:251-252`)
  while `doClick` still subtracts per cell, so `remaining` goes negative and the
  release empties the cursor. Vanilla shows a full stack anyway. Transcribed, not
  fixed — see `tests/drag_preview_agreement.rs`. The *per-cell* counts agree.

The wash goes in the **chrome** run (before `chrome_vertex_count`) so it lands
under the icon it backs; the provisional counts land past that split like every
other count.

#### Why the screen's paint set can be trusted as the divisor

`painted.len()` is the even-split divisor. The screen keeps its own paint set
(`MenuInput.drag`) and the machine keeps its own (`Menu::quick_craft_slots`, grown
from the `ADD` packets), and if their sizes ever differed the preview would divide
by a different number than the distribution. They cannot, because both grow
through **`Menu::can_drag_place_at`** — one predicate, called from both layers,
which is also what fixed issue #378 part 1. `container.rs`'s
`the_screens_paint_set_and_the_machines_stay_identical` asserts the equality
end to end rather than leaving it as an argument.

#### Proof

Two halves, because neither is sufficient alone:

* `lodestone-game/tests/drag_preview_agreement.rs` — hermetic. Reads the plan,
  then runs the **real** `perform_drag`, then asserts previewed == produced per
  cell, across 2-, 3- and 5-cell drags for both buttons plus occupied/clamped
  cells and `CLONE`. It also states the shares as literals hand-derived from
  `getQuickCraftPlaceCount`, and that is load-bearing: now that plan and outcome
  share code, agreement alone is `decode(encode(x)) == x` and would survive two
  symmetric misunderstandings. Measured — swapping the `EVEN` share to a `ceil`
  leaves the agreement assertions green and the literal fails at `left: 4,
  right: 3`.
* `lodestone-shell/tests/container_drag_preview_pixels.rs` — the pixels. Four
  frames differing only in the drag. Measured on a green tree:
  preview vs no-drag in a painted cell **256 px, bbox x160..175 y94..109** (the
  whole cell — the wash); the **unpainted** control cell **0 px**; a 3-share vs a
  2-share in the same cell **8 px, bbox x170..175 y105..108**, a 6×4 box in the
  bottom-right corner, which is exactly where a stack count sits; and two drags
  whose share is the same number by different routes (`floor(7/3)` and
  `floor(4/2)`) **0 px — pixel-identical**.

  That last pair is the assertion "a number drew in each painted cell" cannot
  make. Three controls were run one at a time and the table of which assertions
  fire is in the gate's own header — the useful part is that **no single
  assertion covers two of them**. With the preview absent entirely, the
  same-share comparison passes *vacuously* (two blanks are identical) and only
  the reaches-pixels assertion fires. With the `EVEN` share switched to `ceil`,
  reaches-pixels passes happily and the split assertion fails at 8 px in the
  count corner, while the magnitude assertion collapses to 0 px because
  `ceil(9/3)` and `ceil(7/3)` are both 3. With `cell()` ignoring its index so
  every cell previews, *only* position fires, at 256 px in a cell that was never
  painted. That last one is `CLAUDE.md`'s magnitude species in miniature: three
  assertions measuring *whether* something drew, all satisfied, and the thing was
  in the wrong place.

### Keyboard: the number keys are `SWAP`, not slot selection (issue #378 part 3)

While a container screen is open, `resolve_key`'s container arm consumes every key
— that is deliberate, so no gameplay binding fires behind an inventory. The nine
number keys fell into that swallow, which was correct for half the job and wrong
for the other half: vanilla's `1`–`9` **do not** change the selected hotbar slot
while a screen is up (that lives in `Minecraft.handleKeybinds`, gated on
`screen == null`) — they issue a `ContainerInput::SWAP` with that hotbar index
against the **hovered** slot, `AbstractContainerScreen.checkHotbarKeyPressed`
(`AbstractContainerScreen.java:506-522`).

The order inside the arm is vanilla's own, from `keyPressed`
(`AbstractContainerScreen.java:489-503`): the inventory binding closes the screen
first, *then* the hotbar keys. `KeyOutcome::ContainerSwap { button }` carries the
raw wire button number, and `App::send_container_swap` applies vanilla's two
**state** guards — an empty cursor and a hovered slot — before sending. Those
guards are in the driver, not in `resolve_key`, which only knows about keys;
failing either does nothing, which is what these keys did before, so a miss is not
a new dead end. The hover comes from the same
`active_container_menu` + `hit_test_with_scale` pair the mouse path uses, so key
and mouse cannot disagree about which slot is under the pointer.

**The off-hand key (vanilla's `F`) now works here too, and it took a deletion to
unblock.** `Click::offhand_swap` and `do_swap`'s `button == 40` arm both existed
and were tested (#27), and `send_container_swap` already had the branch — the only
missing piece was an `InputAction::SwapOffhand`, and adding it with vanilla's
default of `F` (`Options.java:663`, GLFW keysym 70) collided with this client's
Lodestone-only `key.lodestone.toggleFly` and turned `keybinds.rs`'s
conflict-free-defaults test red. **Issue #382 deleted `toggleFly`** (as one of
several bespoke debug affordances that vanilla's cheat commands cover), which
freed `F`, and `resolve_key`'s container arm now asks
`binds.is(InputAction::SwapOffhand, code)` **before** the hotbar keys — matching
`checkHotbarKeyPressed`'s own order, so rebinding the off-hand key onto a number
key swaps with slot `40` rather than that number's slot.
`app::tests::the_offhand_key_swaps_with_slot_forty_while_a_container_is_open` is
the gate.

The separate *gameplay* hand-swap — a `ServerboundPlayerActionPacket`
`SWAP_ITEM_WITH_OFFHAND` with no screen open — **landed under issue #385** and is
a different mechanism, not this one with a flag: it names no slot, does no hit
test and is not a container click at all. See
[`keybindings.md`](./keybindings.md#one-action-two-mechanisms-keyswapoffhand-issues-382-385).
The gate above now asserts the two outcomes are distinct rather than asserting the
gameplay half absent.

(This paragraph used to point at "#378's remaining half". #378 was closed, which
made that a dangling reference — the staleness class `CLAUDE.md` rule 2 is about.
It is #385.)

## How to change it

### Wiring it into the live app — two of three steps are done

**Read this before believing any claim about what is unwired here.** This section
described three outstanding steps; two of them have since landed, and the stale
version was cited as the cause of
[#50](https://github.com/matteopolak/lodestone/issues/50) ("block items render flat
in container screens — 3D geometry only reaches the hotbar").

| step | state |
|---|---|
| `container.attach_items(...)` in `WindowApp::resumed` | **done** (`app.rs::lifecycle::WindowApp::resumed`) |
| `container.attach_item_models(...)` in `WindowApp::resumed` | **done** (`app.rs::lifecycle::WindowApp::resumed`) |
| pass `models` + `depth` in the per-frame draw | **outstanding** — this is #50 |

So the container's 3-D item pass is fully constructed and fully bound, and then
**never fed**. `app.rs::WindowApp::redraw` calls

```rust
container_renderer.render_scaled(device, queue, frame.view(), &container_frame, gui_scale, w, h);
```

and `ContainerRenderer::render_scaled` hardcodes `depth: None, models: None`
(`container.rs::ContainerRenderer::render_scaled`). `render_with_icons_scaled` then computes
`want_models = self.icons.models_attached() && depth.is_some()` — `false` — and
`IconAssets { models: None }` reaches `push_item_model`, which returns early. Flat
sprite icons still draw (they need only `attach_items`), which is precisely why the
symptom reads as "block items render **flat**" rather than "nothing renders": the
sprite stream is unaffected and only the mini-blocks vanish.

This is the *island* shape at its purest — a complete, attached, tested capability
with nothing calling it. It has cost this repo eleven confirmed instances.

The fix is one call swap in `app.rs::WindowApp::redraw`:

```rust
// `item_models` is the same value the HUD call in `app.rs::WindowApp::redraw` already computes;
// it is currently created *after* this block, so hoist it above the
// `if container_menu.is_some()` guard.
let item_models = self.sim.vanilla_atlas().and_then(|a| a.models());
container_renderer.render_with_icons_scaled(
    device,
    queue,
    frame.view(),
    Some(render.depth_view()),
    &container_frame,
    item_models,
    self.nav.gui_scale(),
    w,
    h,
);
```

**Stale as of issue #51/#61's dimming fix**: the container overlay used to draw
*before* the HUD in `WindowApp`. It now draws **after** the HUD (and after the
status-effects overlay), on purpose — see "The dim behind the panel" above.
Both model passes still independently clear depth immediately before their own
draw, so the two rendering in either relative order remains safe; what changed
is which one paints over the other's *colour*, which is the whole point. Use
the `_scaled` variant, not `render_with_icons`: the plain one lays out against
`AUTO_GUI_SCALE` and would disagree with `hit_test_with_scale` about where the
slots are.

The third step, the **carried stack**, is also **done**: `app.rs::WindowApp::redraw` builds the
frame with `.with_cursor(Some([self.cursor.0, self.cursor.1]))`. Kept here because
the failure mode is worth recording — `ContainerGeometry::build_inner` checks
`frame.cursor` before it checks `menu.carried()`, so leaving the field at its `None`
default builds all the geometry correctly and draws none of it.

### Gotchas

* **The fallback still exists and still matters.** With no item atlas attached,
  occupied slots draw `item_color` (a hash-derived swatch) plus `item_label` (one
  letter). That is the jar-less/demo path and the negative control the pixel gate
  leans on. If you delete it, `container_screen.rs` starts asserting coverage
  that nothing produces.
* **Coverage is not evidence of an icon.** `container_screen.rs` measures pixels
  covered inside the widget rect — a coloured square satisfies that just as well
  as a picture of a diamond does. That is why the icons needed their own gate.
* **`ContainerRenderer::render` must keep its signature.** `app.rs` and two
  integration tests call it; `render_with_icons` was added alongside rather than
  replacing it.
* **Never hand `ContainerFrame::title` a `Text::to_plain_string()`.** It is a
  `&str`, so the resolution has to happen at the call site, and the type cannot
  stop you passing an unresolved one. Use `menu_title`. `container.crafting`
  shipped to screen once already.
* **`ContainerFrame::title` is the *screen's* title, not "the container's name".**
  On the player inventory screen it is "Crafting". If you find yourself writing
  the word "Inventory" as a title, you want `inventory_label`.
* **A label gate that reports a coverage fraction is useless here.** Both bugs
  #370 fixed were labels drawn *legibly, in the wrong place or the wrong
  typeface* — every one of which satisfies "something drew". `container_labels.rs`
  measures **bounding boxes** and isolates the ink by subtracting a build with the
  label blanked, so it can tell "20 px off" from "not drawn". It also asserts the
  premise (nothing else in the screen paints in the label colour) rather than
  assuming it.

## Configuration

None. The behavioural switches are all "is a thing attached":

| condition | behaviour |
| --- | --- |
| `attach_items` not called | colour-swatch + letter fallback in every occupied slot |
| `attach_item_models` not called | flat sprites draw; block items draw nothing |
| no `depth` passed to `render_with_icons` | as above |
| `attach_background` not called | flat programmatic panel fill + wells, instead of vanilla's real `container/*.png` art; the dim still draws either way. Also switches the label ink from vanilla's `0xFF404040` to a warm light — dark grey on the dark fallback panel would be invisible. `background_attached()` is the gate for asserting the vanilla colour |
| `VanillaFont::shared()` returns `None` (no jar) | both labels fall back to the fixed-advance 5×7 debug font: legible, so no coverage assertion can see it. `font_attached()` is the gate |
| `ContainerFrame::with_inventory_label` not called | the second label reads `"Inventory"`, `en_us.json`'s value for `container.inventory`; `app.rs` supplies the translated one |
| `ContainerFrame::menu` is `None` | nothing is drawn at all, and `widget_rect` is `None` |
| `ContainerFrame::cursor` is `None` (the default) | `menu.carried()`, even if `Some`, draws nowhere |

## Dependencies

* **`lodestone-game`** — `Menu`, `MenuKind`, `CraftLayout`, `ItemStack`, and the
  `DAMAGE_COMPONENT` / `MAX_DAMAGE_COMPONENT` component keys.
* **`crate::hud::item_icon`** — `draw_item_icon`, `ColourStream`, `IconRenderer`,
  `IconAssets`, `IconSink`, and (new) `ColourStream::gradient_rect` for the dim.
* **`lodestone-render`** — `BlockModels`, `ModelVertex` (through the shared pass),
  `GuiSpriteQuad`, `GpuAtlas` (the background texture's own small pipeline).
* **`lodestone-assets`** — `Atlas`, `AtlasBuilder` (`ContainerBackground` stitches
  the three `container/*.png` sheets directly, not through `GuiAtlas` — see "The
  panel is real vanilla art now" above for why).
* **`crate::gpu::RenderState`** — the borrowed block atlas, palette, animation
  slots and depth buffer.

## Files and tests

| path | role |
| --- | --- |
| `crates/lodestone-shell/src/container.rs` | everything above |
| `crates/lodestone-shell/tests/container_screen.rs` | layout tests (player, generic, crafting) + a coverage gate |
| `crates/lodestone-shell/tests/container_item_pixels.rs` | the icon pixel gate (GPU + `client.jar`, `#[ignore]`d) |
| `crates/lodestone-shell/tests/container_background_pixels.rs` | the panel-art and hotbar-dim pixel gate, with its negative controls (GPU + `client.jar`, `#[ignore]`d) |
| `crates/lodestone-shell/tests/live_container_render.rs` | end-to-end against a live server |
