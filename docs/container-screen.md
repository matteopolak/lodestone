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

`slot_layout(&Menu) -> SlotLayout` dispatches twice:

```
match menu.kind() {
    MenuKind::Player            => player_layout(),
    MenuKind::Generic { size }  => match menu.craft_layout() {
        Some(craft) => crafting_layout(craft, size),   // crafting table
        None        => generic_layout(size),           // chest, barrel, ...
    },
}
```

The second dispatch is **additive on `Menu`, not a new `MenuKind`**, and that is
deliberate. A crafting table *is* a generic container as far as sizing and
quick-move regions are concerned — vanilla's `CraftingMenu` is positionally
identical to a `Generic { container_size: 10 }`. Only its slot *kinds* and its
screen differ. `MenuKind` is matched exhaustively across this crate, so a new
variant would have broken every match; `Menu::craft_layout()` was added for
exactly this.

Every `SlotRect` carries the **real `menu_index`**. There is no constant offset
anywhere, and none should be reintroduced:

| menu | slot indices |
| --- | --- |
| `Player` (window 0) | `0` result, `1..=4` 2×2 craft, `5..=8` armour, `9..=35` main, `36..=44` hotbar, `45` offhand |
| `Generic { n }` | `0..n` container, `n..n+27` main, `n+27..n+36` hotbar — **no** armour, **no** offhand |
| crafting table | a `Generic { 10 }`: `0` result, `1..=9` grid, `10..=36` main, `37..=45` hotbar |

`crafting_layout` uses vanilla's `crafting_table.png` slot origins for the 3×3
case — grid at `(30, 17)`, result at `(124, 35)`, main at `(8, 84)`, hotbar at
`(8, 142)`, panel `176×166` — expressed in terms of the grid's real dimensions so
a differently sized grid still lands somewhere sane.

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

Not modelled: `AbstractFurnaceScreen.java:39` centres its title
(`(imageWidth - font.width(title)) / 2`), the only vanilla anchor that depends on
the text itself. There is no furnace `MenuKind` yet (issue #28) — a furnace
arrives as a `Generic` and gets `x = 8`. Adding that branch means giving
`label_layout` the font, which it deliberately does not take today.

### The panel is real vanilla art now (issue #51)

`ContainerBackground` (`container.rs`) loads and stitches vanilla's three real
`textures/gui/container/*.png` sheets — `generic_54`, `crafting_table`,
`inventory` — and `ContainerRenderer::attach_background` binds them, exactly
the "is a thing attached" pattern `attach_items`/`attach_item_models` already
use. Covers every menu `slot_layout` already lays out (`MenuKind::Player`,
plain `Generic`, and `Generic` with a `craft_layout`); it does **not** add
furnace/hopper/anvil/etc. backgrounds, because this crate has no slot layout
for those screens yet (issue #28).

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
swatch fallback). "Last" matters: it is appended after the per-slot loop on all
three streams, and every pass draws its stream in append order, so the carried
stack lands on top of whatever slot the cursor happens to be over.

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

## How to change it

### Wiring it into the live app — two of three steps are done

**Read this before believing any claim about what is unwired here.** This section
described three outstanding steps; two of them have since landed, and the stale
version was cited as the cause of
[#50](https://github.com/matteopolak/lodestone/issues/50) ("block items render flat
in container screens — 3D geometry only reaches the hotbar").

| step | state |
|---|---|
| `container.attach_items(...)` in `WindowApp::resumed` | **done** (`app.rs:1488`) |
| `container.attach_item_models(...)` in `WindowApp::resumed` | **done** (`app.rs:1496`) |
| pass `models` + `depth` in the per-frame draw | **outstanding** — this is #50 |

So the container's 3-D item pass is fully constructed and fully bound, and then
**never fed**. `app.rs:1273` calls

```rust
container_renderer.render_scaled(device, queue, frame.view(), &container_frame, gui_scale, w, h);
```

and `ContainerRenderer::render_scaled` hardcodes `depth: None, models: None`
(`container.rs:1278`). `render_with_icons_scaled` then computes
`want_models = self.icons.models_attached() && depth.is_some()` — `false` — and
`IconAssets { models: None }` reaches `push_item_model`, which returns early. Flat
sprite icons still draw (they need only `attach_items`), which is precisely why the
symptom reads as "block items render **flat**" rather than "nothing renders": the
sprite stream is unaffected and only the mini-blocks vanish.

This is the *island* shape at its purest — a complete, attached, tested capability
with nothing calling it. It has cost this repo eleven confirmed instances.

The fix is one call swap at `app.rs:1273`:

```rust
// `item_models` is the same value the HUD call at app.rs:1359 already computes;
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

The third step, the **carried stack**, is also **done**: `app.rs:1270` builds the
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
