# GUI item icons (the draw half)

## What it is

The **draw half** of putting an item in a GUI slot: the shell code that turns a
resolved item icon into pixels. It serves **both** screens that have slots — the
hotbar (`hud.rs`) and the container / inventory / crafting screen
(`container.rs`) — from one shared module, `hud/item_icon.rs`. Two kinds of icon
reach two different pipelines:

* a **flat sprite** (`item/generated`, the majority) — one textured quad off the
  stitched [`ItemAtlas`], on the HUD's own sprite pipeline;
* a **3-D block item** — vanilla's isometric mini-block, drawn through the
  *world's* `ModelPipeline` from geometry baked at asset-load time.

The geometry half lives in `lodestone-render` and is documented separately in
[`item-gui-geometry.md`](item-gui-geometry.md). **Read that first** if you are
touching the pose, the winding, or the baking. This doc is only about the shell
side: which stream a part goes to, which passes run in which order, and what is
shared with the terrain renderer.

Everything here is in `crates/lodestone-shell/src/hud/item_icon.rs`, with the two
consumers in `hud.rs` and `container.rs`, four accessors in
`crates/lodestone-shell/src/gpu.rs`, and the hotbar's wiring in
`crates/lodestone-shell/src/app.rs`.

### Why it is shared rather than copied

A second copy of the pipelines is not a style problem, it is a second ~30 MB
atlas upload and a second tint palette that can silently drift green from the
world's. `IconRenderer` owns the flat item-sprite pipeline and the 3-D
item-model pass; `HudRenderer` and `ContainerRenderer` each hold one and delegate
their `attach_items` / `attach_item_models` to it.

## How it works

### The chain

```
Sim::player_menu -> HudFrame::hotbar_items      Menus::active -> ContainerFrame
  hud::draw_hotbar_items                          container::slot_layout
   (16 px icon, 22 px / 40 px pitch)               (16 px cell, 18 px pitch)
        \                                              /
         `------------>  item_icon::draw_item_icon  <--'
                           ItemAtlas::icon(item).parts
                             IconPart::Sprite  -> IconSink::sprite (item atlas)
                             IconPart::Model   -> BlockModels::item(item)
                                                  gui_item_pose(rect, transform)
                                                  mesh_item_quads(..., gui_light)
                                                  -> IconSink::model (posed verts)
                             IconPart::Special -> nothing
                           count + durability   -> IconSink::colour
                             (count draws through the caller's `Option<&VanillaFont>`,
                              same fallback rule as every other HUD string)
        ,----------------------------------------------.
  HudRenderer::render_with_item_models    ContainerRenderer::render_with_icons
        `-------------> IconRenderer (upload / draw) -> pixels
```

`ItemIcon` (re-exported as `hud::HotbarSlot`) is the per-slot draw record both
screens build: item id, count, damage/max_damage, enchanted.

The item atlas decides *what kind* of icon an item has; `BlockModels` supplies
the geometry. Both resolve item definitions through `GuiItemContext`, so they
agree on which items are 3-D — an item is in `BlockModels`'s item map **iff** its
icon has an `IconPart::Model`.

### Why the pose is applied on the CPU

`item_icon::push_item_model` pre-multiplies `gui_item_pose` into every vertex before it
reaches the buffer, so the whole hotbar is **one vertex buffer and one draw**.
The GUI path has to emit vertices anyway; transforming them costs nothing over
uploading them untransformed, and the alternative (one per-slot model matrix)
would mean a uniform rebind and a draw call per occupied slot.

Indices are expanded into a flat triangle list (six vertices per quad) because
the other two streams are non-indexed. The expansion preserves winding, which is
load-bearing.

### The three passes, and why they are three

`HudRenderer::render_with_item_models` records, in order, into one encoder:

| pass | attachment | contents |
| --- | --- | --- |
| `hud-pass` | colour (`Load`) | hotbar frame, vitals, flat item sprites |
| `hud-item-model-pass` | colour (`Load`) + **depth (`Clear(1.0)`)** | the 3-D mini-blocks |
| `hud-colour-pass` | colour (`Load`) | text, stack counts, durability bars |

`ContainerRenderer::render_with_icons` mirrors it, with one wrinkle: its *chrome*
(panel, wells, title) and its *overlay* (stack counts, durability bars) are both
on the same colour stream, and the icons go **between** them. So the colour
stream is emitted in two runs — all the wells first, then everything that sits on
an icon — and `ContainerGeometry::chrome_vertex_count` records the split, which
the renderer draws as two ranges of one buffer:

| pass | attachment | contents |
| --- | --- | --- |
| `container-pass` | colour (`Load`) | panel, title, slot wells (`0..chrome`) |
| `container-item-model-pass` | colour (`Load`) + **depth (`Clear(1.0)`)** | the 3-D mini-blocks |
| `container-item-pass` | colour (`Load`) | flat sprites, then `chrome..n` (counts, bars) |

If you add anything to the container's colour stream, put it in the loop that
matches its layer, not wherever is convenient — a stack count emitted in the
wells loop ends up *underneath* the sprite it counts.

### Stack count text and font

`draw_item_icon` takes `font: Option<&VanillaFont>`, threaded from each
screen's own `Builder` (`HudRenderer`/`ContainerRenderer` both resolve
`VanillaFont::shared()` once in `new()`, exactly like `HudRenderer` already
did for every other string). With a font attached, the count draws through
`VanillaFont::draw` — real glyph widths and vanilla's 1 px / 25%-brightness
drop shadow. Without one it falls back to the fixed-advance 5×7 debug font,
now with the same 25%-brightness shadow colour (`vanilla_font::shadow_of`)
instead of a pure black one.

**The text scale is `size / 16.0`, the same factor the icon itself draws
at — not a separate multiplier.** An earlier version scaled the fallback
digits by an extra 2x on top of that, which is what actually made stack
counts look oversized: the count grew relative to the icon it sits on, not
just relative to the slot. Keep it this way once a real `gui_scale` lands —
scaling every one of these sizes uniformly is exactly what keeps the count
proportioned correctly relative to the icon without a special case.

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

### Gotcha 2 — plain `render` vs the icon-aware entry point

`HudRenderer::render` is the old six-argument entry point and is kept because
`src/tablist.rs` and `tests/live_tab_scoreboard_pixels.rs` call it and have
neither a model set nor a depth buffer. It delegates with `models: None,
depth: None`, which degrades to flat sprites only. New callers that want block
icons must use `render_with_item_models` and pass **both**; either being `None`
silently skips the model geometry (it is not even built — see `want_models`).

`ContainerRenderer` has the same split for the same reason:
`ContainerRenderer::render` keeps the old six-argument shape (and the
colour-swatch fallback), `render_with_icons` takes `depth` and `models`.

### Gotcha 3 — geometry is passed per frame, not captured at attach

Unlike `CrackResolver::from_models`, which snapshots per-state quads at setup,
this path takes `Option<&BlockModels>` on every render call. Historically that
was forced: `BlockModels` exposed item geometry **only** by key lookup
(`item(&ResourceLocation)`), and block *states* are enumerable only because their
ids are a dense `0..n`, which item `ResourceLocation`s are not.

`BlockModels::items()` now exists and yields `(&ResourceLocation,
&ItemGeometry)`, so an attach-time snapshot *is* possible — `gpu.rs`'s dropped-item
renderer takes one. This path deliberately still does not: a handful of hash
lookups for the visible slots per frame is cheaper than cloning ~750 items'
quads, and the borrow keeps the icons in lock-step with a reloaded pack.

### Gotcha 4 — the pixel count is a band, and it cannot see an inside-out cube

`tests/hotbar_block_item_pixels.rs` and `tests/container_item_pixels.rs` assert
the lit-pixel count inside one cell lands within ±15% of **172.5 of 256** — the analytic silhouette area of the
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

* **The container screen is not wired in `app.rs` yet.** `ContainerRenderer` can
  draw icons — `attach_items`, `attach_item_models`, `render_with_icons` all
  exist and are proved by `tests/container_item_pixels.rs` — but `app.rs` still
  calls the plain `ContainerRenderer::new` + `render`, so the live inventory keeps
  the colour-swatch fallback. See [`container-screen.md`](container-screen.md)
  for the exact four-line change.
* **Tint on flat sprites is still deferred** — leather armour, potions and spawn
  eggs draw untinted white. 3-D items *are* tinted, through the shared palette.
* **The enchantment glint is not drawn** (`ItemIcon::enchanted` is carried but
  unused).
* **The 16 beds bake incompletely** (head only) — see
  [`item-gui-geometry.md`](item-gui-geometry.md); the cause is in
  `lodestone-assets`, not here.
* **The item atlas is uploaded twice** once both screens attach — it is small
  (one 2-D sprite sheet) next to the block atlas, which is *not* duplicated, but
  a single shared upload would be the tidier end state.

## Files and tests

| path | role |
| --- | --- |
| `crates/lodestone-shell/src/hud/item_icon.rs` | `ItemIcon`, `draw_item_icon`, `ColourStream`, `IconRenderer` (both pipelines, upload, draws) |
| `crates/lodestone-shell/src/hud.rs` | the hotbar consumer: cell layout, `Builder::item_icon`, the three passes |
| `crates/lodestone-shell/src/container.rs` | the container consumer: slot layout, chrome/overlay split, `render_with_icons` |
| `crates/lodestone-shell/src/gpu.rs` | the four borrowed-resource accessors on `RenderState` |
| `crates/lodestone-shell/src/app.rs` | attach at startup, pass models + depth per frame (**hotbar only** so far) |
| `crates/lodestone-shell/tests/hotbar_block_item_pixels.rs` | hotbar pixel gate (GPU + `client.jar`, `#[ignore]`d) — 176 / 0 / 0 |
| `crates/lodestone-shell/tests/container_item_pixels.rs` | container pixel gate (same) — 176 block / 120 sprite / 0 empty / 0 control |
