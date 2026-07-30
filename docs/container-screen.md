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

The "Inventory" title for the local inventory screen (`ui.is_container_open()`
with no server menu) is still a Lodestone-chosen literal, not
`container.inventory` — there is no server component to resolve in that case.

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

Note the container overlay is drawn **before** the HUD in `WindowApp`, and both
model passes clear depth, so the order is safe. Use the `_scaled` variant, not
`render_with_icons`: the plain one lays out against `AUTO_GUI_SCALE` and would
disagree with `hit_test_with_scale` about where the slots are.

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

## Configuration

None. The behavioural switches are all "is a thing attached":

| condition | behaviour |
| --- | --- |
| `attach_items` not called | colour-swatch + letter fallback in every occupied slot |
| `attach_item_models` not called | flat sprites draw; block items draw nothing |
| no `depth` passed to `render_with_icons` | as above |
| `ContainerFrame::menu` is `None` | nothing is drawn at all, and `widget_rect` is `None` |
| `ContainerFrame::cursor` is `None` (the default) | `menu.carried()`, even if `Some`, draws nowhere |

## Dependencies

* **`lodestone-game`** — `Menu`, `MenuKind`, `CraftLayout`, `ItemStack`, and the
  `DAMAGE_COMPONENT` / `MAX_DAMAGE_COMPONENT` component keys.
* **`crate::hud::item_icon`** — `draw_item_icon`, `ColourStream`, `IconRenderer`,
  `IconAssets`, `IconSink`.
* **`lodestone-render`** — `BlockModels`, `ModelVertex` (through the shared pass).
* **`crate::gpu::RenderState`** — the borrowed block atlas, palette, animation
  slots and depth buffer.

## Files and tests

| path | role |
| --- | --- |
| `crates/lodestone-shell/src/container.rs` | everything above |
| `crates/lodestone-shell/tests/container_screen.rs` | layout tests (player, generic, crafting) + a coverage gate |
| `crates/lodestone-shell/tests/container_item_pixels.rs` | the icon pixel gate (GPU + `client.jar`, `#[ignore]`d) |
| `crates/lodestone-shell/tests/live_container_render.rs` | end-to-end against a live server |
