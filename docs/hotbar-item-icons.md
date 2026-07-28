# Hotbar item icons (the draw half)

## What it is

The **draw half** of putting an item in a hotbar slot: the HUD code that turns a
resolved item icon into pixels. Two kinds of icon reach two different pipelines:

* a **flat sprite** (`item/generated`, the majority) — one textured quad off the
  stitched [`ItemAtlas`], on the HUD's own sprite pipeline;
* a **3-D block item** — vanilla's isometric mini-block, drawn through the
  *world's* `ModelPipeline` from geometry baked at asset-load time.

The geometry half lives in `lodestone-render` and is documented separately in
[`item-gui-geometry.md`](item-gui-geometry.md). **Read that first** if you are
touching the pose, the winding, or the baking. This doc is only about the shell
side: which stream a part goes to, which passes run in which order, and what is
shared with the terrain renderer.

Everything here is in `crates/lodestone-shell/src/hud.rs`, plus four accessors in
`crates/lodestone-shell/src/gpu.rs` and the wiring in
`crates/lodestone-shell/src/app.rs`.

## How it works

### The chain

```
Sim::player_menu  ->  HudFrame::hotbar_items: &[Option<HotbarSlot>]
  hud::draw_hotbar_items          (cell layout: 16 px icon, 22 px / 40 px pitch)
    Builder::item_icon(slot, x, y, size)
      ItemAtlas::icon(item).parts
        IconPart::Sprite  -> Builder::item_sprite -> HudGeometry::item_verts
        IconPart::Model   -> Builder::item_model
                               BlockModels::item(item)      (baked quads + pose)
                               gui_item_pose(rect, transform)
                               mesh_item_quads(quads, pose, gui_light)
                               -> HudGeometry::model_verts  (CPU-posed ModelVertex)
        IconPart::Special -> nothing (code-driven renderers we do not have)
  HudRenderer::render_with_item_models  ->  three render passes  ->  pixels
```

The item atlas decides *what kind* of icon an item has; `BlockModels` supplies
the geometry. Both resolve item definitions through `GuiItemContext`, so they
agree on which items are 3-D — an item is in `BlockModels`'s item map **iff** its
icon has an `IconPart::Model`.

### Why the pose is applied on the CPU

`Builder::item_model` pre-multiplies `gui_item_pose` into every vertex before it
reaches the buffer, so the whole hotbar is **one vertex buffer and one draw**.
The GUI path has to emit vertices anyway; transforming them costs nothing over
uploading them untransformed, and the alternative (one per-slot model matrix)
would mean a uniform rebind and a draw call per occupied slot.

Indices are expanded into a flat triangle list (six vertices per quad) because
the HUD's other two streams are non-indexed. The expansion preserves winding,
which is load-bearing.

### The three passes, and why they are three

`HudRenderer::render_with_item_models` records, in order, into one encoder:

| pass | attachment | contents |
| --- | --- | --- |
| `hud-pass` | colour (`Load`) | hotbar frame, vitals, flat item sprites |
| `hud-item-model-pass` | colour (`Load`) + **depth (`Clear(1.0)`)** | the 3-D mini-blocks |
| `hud-colour-pass` | colour (`Load`) | text, stack counts, durability bars |

The middle pass exists *because of the depth attachment*: a render pass's
attachments are fixed for its lifetime, and only the item models need depth.

**Depth is cleared, not loaded.** The world's depth buffer is still resident from
the terrain pass and holds values that would swallow a GUI item sitting at clip
depth ~0.5. Nothing later in the frame reads depth, so clearing the shared buffer
here is free — see `RenderState::depth_view`.

**The ordering is not cosmetic.** Stack counts and durability bars are on the
colour stream; if the model pass ran after them, an icon would be drawn over its
own count.

### What is shared with the world renderer, and why

`HudRenderer::attach_item_models` **creates no texture and no buffer of its own**
beyond the vertex buffer and the GUI camera. It borrows four things from
`RenderState`:

| borrowed | accessor | why sharing matters |
| --- | --- | --- |
| block atlas view + sampler | `model_atlas_view` / `model_atlas_sampler` | a block item's faces *are* block textures; a second upload costs tens of MB to draw nine 16 px icons |
| tint palette | `model_palette_buffer` | a grass block's icon and the world block resolve to the *same* palette slot, so they cannot drift to different greens |
| animation slots | `model_anim_buffer` | magma / sea lantern / prismarine icons advance in lock-step with the world, off the one per-frame uniform write `update_animation` already does |
| depth buffer | `depth_view` | see above |

`wgpu` resources are `Arc`-backed and a bind group keeps its own strong
reference, so these are lent as plain `&` references at attach time and need not
outlive the call.

## How to change it

### Gotcha 1 — do not add a fifth bind group

The model shader declares exactly four: camera (0), atlas (1), palette (2),
animation (3). `wgpu`'s portable `max_bind_groups` floor is **4**. A five-group
variant validates on an adapter that reports 8 (Apple silicon does) and fails on
the floor — a real regression this repo has already shipped once, caught only by
a hermetic gate. That is why the GUI camera and its *disabled* fog block share
group 0 as a single `ModelCameraUniform` rather than fog getting its own group.

### Gotcha 2 — `render` vs `render_with_item_models`

`HudRenderer::render` is the old six-argument entry point and is kept because
`src/tablist.rs` and `tests/live_tab_scoreboard_pixels.rs` call it and have
neither a model set nor a depth buffer. It delegates with `models: None,
depth: None`, which degrades to flat sprites only. New callers that want block
icons must use `render_with_item_models` and pass **both**; either being `None`
silently skips the model geometry (it is not even built — see `want_models`).

### Gotcha 3 — geometry is passed per frame, not captured at attach

Unlike `CrackResolver::from_models`, which snapshots per-state quads at setup,
this path takes `Option<&BlockModels>` on every `render_with_item_models` call.
That is not a style choice: `BlockModels` exposes item geometry **only by key
lookup** (`item(&ResourceLocation)`), with no way to enumerate its item ids from
outside `lodestone-render`, so a snapshot cannot be taken. Nine hash lookups per
frame is cheaper than cloning ~750 items' quads anyway. If an
`items()` iterator is ever added to `BlockModels`, capturing at attach time
becomes possible and would shorten `render_with_item_models`' signature.

### Gotcha 4 — the pixel count is a band, and it cannot see an inside-out cube

`tests/hotbar_block_item_pixels.rs` asserts the lit-pixel count inside one cell
lands within ±15% of **172.5 of 256** — the analytic silhouette area of the
vanilla `[30, 225, 0] / 0.625` pose in a 16 px cell (measured: **176**). Do not
loosen that to "greater than zero"; it is what rules out a half-drawn or
mis-scaled icon.

But be clear about what it *cannot* do. If the winding flipped, the three far
faces would survive culling instead — and they project to the **same hexagon**,
so the count is unchanged. The gate catches that with a second, independent
assertion: `gui_light: Side` shades the correct visible set `{Up, East, North}`
`{1.0, 0.6, 0.8}` and the flipped set `{Down, West, South}` `{0.5, 0.6, 0.8}`, so
the top-band / bottom-band brightness ratio is ~1.4 when correct and ~0.7 when
flipped. If you change the pose so the horizontal face is no longer at the top of
the silhouette, that assertion needs rewriting, not deleting.

## Configuration

None — no env vars, flags or feature gates. The behavioural switches are all
"is a thing attached":

| condition | behaviour |
| --- | --- |
| no `ItemAtlas` (`attach_items` not called) | no icons at all; wells stay empty |
| no `attach_item_models` (demo path, no baked models) | flat sprites only; block items draw an empty well |
| no `depth` passed to `render_with_item_models` | as above |
| `HudFrame::hotbar_items` is `None` | nothing is drawn in any cell |

Cell geometry, mirrored by the gate: with the vanilla GUI atlas the icon is
`16 * 2` px at a `20 * 2` px pitch starting `3 * 2` px into the frame; without it
the procedural fallback uses a 16 px icon at a 22 px pitch.

## Dependencies

* **`lodestone-assets`** — `ItemAtlas` / `ItemIcon` / `IconPart`,
  `ResourceLocation`.
* **`lodestone-render`** — `BlockModels::item`, `gui_item_pose`, `gui_ortho`,
  `mesh_item_quads`, `ModelVertex`, `ModelPipeline`, `ModelCameraUniform`,
  `CameraUniform`, `fog::FogUniform`, `RenderLayer`.
* **`crate::gpu::RenderState`** — the four borrowed GPU resources above.

## Known gaps

* **Container and inventory screens still draw the old placeholder** — a
  hash-derived colour swatch plus a one-letter abbreviation
  (`container.rs::item_color` / `item_label`). `ContainerRenderer` has its own
  builder and its own colour-only pipeline, so giving it real icons means either
  duplicating ~350 lines of `hud.rs` or (better) extracting the sprite pass and
  the item-model pass out of `HudRenderer` into a `pub(crate)` shared
  `ItemIconPass` that both renderers own. Note that `container.rs::slot_layout`
  already handles the `MenuKind` slot-order difference correctly (`Player` has
  armour and an offhand and its hotbar is at 36; `Generic{n}` has neither and its
  hotbar is at `n + 27`) — it passes the real `menu_index` through, so there is
  no constant offset to get wrong.
* **Tint on flat sprites is still deferred** — leather armour, potions and spawn
  eggs draw untinted white. 3-D items *are* tinted, through the shared palette.
* **The enchantment glint is not drawn** (`HotbarSlot::enchanted` is carried but
  unused).
* **The 16 beds bake incompletely** (head only) — see
  [`item-gui-geometry.md`](item-gui-geometry.md); the cause is in
  `lodestone-assets`, not here.

## Files and tests

| path | role |
| --- | --- |
| `crates/lodestone-shell/src/hud.rs` | `Builder::item_icon` / `item_model`, `ItemModelHud`, `attach_item_models`, the three passes |
| `crates/lodestone-shell/src/gpu.rs` | the four borrowed-resource accessors on `RenderState` |
| `crates/lodestone-shell/src/app.rs` | attach at startup, pass models + depth per frame |
| `crates/lodestone-shell/tests/hotbar_block_item_pixels.rs` | the pixel gate (GPU + `client.jar`, `#[ignore]`d) |

```
cargo test -p lodestone-shell --lib
cargo test -p lodestone-shell --test hotbar_block_item_pixels -- --ignored --nocapture
```
