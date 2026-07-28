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

## How to change it

### Wiring it into the live app (still outstanding)

`ContainerRenderer` can draw icons, but `app.rs` has not been updated to attach
them, so the running client still shows the colour-swatch fallback. The change is
mechanical — mirror what is already done for `hud` a few lines above, in
`WindowApp::resumed`:

```rust
let mut container = ContainerRenderer::new(gpu.device(), format);
if let Some(items) = crate::resources::load_item_atlas() {
    container.attach_items(gpu.device(), gpu.queue(), format, items);
}
if let (Some(v), Some(s), Some(p), Some(a)) = (
    render.model_atlas_view(),
    render.model_atlas_sampler(),
    render.model_palette_buffer(),
    render.model_anim_buffer(),
) {
    container.attach_item_models(gpu.device(), format, v, s, p, a);
}
```

and in the per-frame draw, swap the `render` call for:

```rust
container_renderer.render_with_icons(
    device, queue, frame.view(), Some(render.depth_view()),
    &container_frame, item_models, w, h,
);
```

where `item_models` is the same `self.sim.vanilla_atlas().and_then(|a| a.models())`
the HUD call already computes. Note the container overlay is drawn **before** the
HUD in `WindowApp`, and both model passes clear depth, so the order is safe.

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

## Configuration

None. The behavioural switches are all "is a thing attached":

| condition | behaviour |
| --- | --- |
| `attach_items` not called | colour-swatch + letter fallback in every occupied slot |
| `attach_item_models` not called | flat sprites draw; block items draw nothing |
| no `depth` passed to `render_with_icons` | as above |
| `ContainerFrame::menu` is `None` | nothing is drawn at all, and `widget_rect` is `None` |

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
