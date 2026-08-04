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
                             IconPart::Special -> special_icon_geometry(kind, item)
                                                  gui_item_pose(rect, display.gui)
                                                  BlockEntityMesh::part_transforms
                                                  -> IconSink::special (mesh + sheet
                                                     + placement, NOT verts)
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
| `container-item-model-pass` | colour (`Load`) + **depth (`Clear(1.0)`)** | the slots' 3-D mini-blocks (`0..slot_model`) |
| `container-item-pass` | colour (`Load`) | the slots' flat sprites (`0..slot_item`), then `chrome..slot_colour` (counts, bars) |
| `container-carried-model-pass` | colour (`Load`) + **depth (`Clear(1.0)`)** | the carried stack's 3-D mini-block (`slot_model..n`) |
| `container-carried-pass` | colour (`Load`) | the carried stack's flat sprite (`slot_item..n`), then `slot_colour..n` |

If you add anything to the container's colour stream, put it in the loop that
matches its layer, not wherever is convenient — a stack count emitted in the
wells loop ends up *underneath* the sprite it counts.

### The last two passes are a *stratum*, not a reorder (issue #377)

Reported from play: the stack on the cursor drew **under** the slot items. It was
already appended last on all three streams, and two of the four combinations were
already right — which is why this needed measuring rather than a reorder. The
cause is that within one stratum the item passes run **model first, then flat
sprites** (only the model pass needs depth, and a pass's attachments are fixed
for its lifetime), so:

| cursor holds | slot holds | before |
| --- | --- | --- |
| flat sprite | flat sprite | correct — later in the same stream |
| flat sprite | 3-D block | correct — the sprite pass runs after the model pass |
| **3-D block** | flat sprite | **wrong** — the model pass runs *before* the sprite pass |
| **3-D block** | 3-D block | **wrong** — same GUI depth, resolved against the depth buffer, not append order |

plus a third, independent mechanism in *every* combination: the slot layer's
**stack-count glyphs** are on the colour stream's second run, which also drew
after the carried icon. The measured control confirmed all three — the flat/flat
case failed at 37 px whose bounding box (`x164..173 y168..175`) is exactly where a
count sits.

So `build_inner` records three more split markers — `slot_vertex_count`,
`slot_item_vertex_count`, `slot_model_vertex_count`, plus `slot_special_count` —
and the renderer replays all three streams as a second stratum. **The second
model pass clears depth again**, and that clear is the load-bearing part: it is
what vanilla's `graphics.nextStratum()`
(`AbstractContainerScreen.java:126`, called immediately before it draws the
carried item and nowhere else on that screen) buys, and without it a slot block's
near faces still win. `IconStratum` in `hud/item_icon.rs` names the two layers.

Two things to know before touching this:

* the sprite and model streams need **no** split argument in `IconRenderer::upload`
  — they are contiguous vertex slices, so only the *draw* splits — but `special`
  does, because a special batch is *grouped* by `(model, sheet)` during upload and
  a group spanning the split would draw a carried chest in the slot stratum.
  `upload` therefore takes `special_carried_from`; the hotbar passes
  `special.len()` because it has no carried stack.
* gate: `tests/container_cursor_pixels.rs`. Its discriminator is **"nothing paints
  inside the cursor's own ink"** — the pixel set where a cursor-only render
  differs from a chrome baseline — reported as a bounding box, never a fraction.
  Its live-detector control is the complement (the slot item *must* be visible
  outside that ink, or the assertion is vacuous), and it runs the flat-sprite and
  the block cursor separately, because a flat sprite structurally cannot exercise
  the depth half.

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

#### The anchor is two constants, not two derivations (issue #384)

`COUNT_RIGHT = 17.0` / `COUNT_TOP = 9.0`, both times `scale`, straight off
`GuiGraphicsExtractor.itemCount` (`:947-952`, identical in
`SpectatorGui.java:79`):

```java
this.text(font, amount, x + 19 - 2 - font.width(amount), y + 6 + 3, -1, true);
```

Note **`19 - 2 = 17`** — one pixel *past* the 16 px icon's right edge, so this is
deliberately not `size`.

The code used to say `x + size - width` and `y + size - LINE_HEIGHT * scale`, and
**the derivation was the defect rather than the off-by-one**: both drift when the
cell size or the font's line height changes, and they agree with vanilla at no
glyph height at all. `LINE_HEIGHT` is 9, so the top came out `y + 16 - 9 = y + 7`
against vanilla's `y + 9`.

Two things worth knowing before touching this again:

* **`f.width` is not the painted width.** It sums advances, exactly like vanilla's
  `Font.width`, so the last glyph's ink ends one pixel short of it, and the shadow
  pass at `+1, +1` then reaches one pixel past. Measured: a count of `7` inks
  `local x11..16 y9..16` from an anchor whose right edge is x17. Assert the ink
  box against a one-pixel window, not an exact column — and never assert the
  anchor instead, because a player sees ink.
* **The reported symptom was half right and the horizontal half was backwards.**
  The play report was "lower and further left". Lower is real and measured: the fix
  moves the ink down 2 px (`y7..14` → `y9..16`). *Further left* is not what vanilla
  does — the old anchor was already one pixel left of vanilla's, and matching
  vanilla moves the number one pixel **right** (`x10..15` → `x11..16`). Vanilla is
  the reference, so it moved right. If the number still reads as too far right in
  play, the cause is downstream of this anchor and needs measuring separately.

Gate: `tests/stack_count_anchor_pixels.rs`. It differences a count-`7` and a
count-`64` frame against a count-**1** frame (vanilla draws no number at
`getCount() == 1`, so the icon, well, panel and dim all cancel) and reports an ink
**bounding box in slot-local pixels**, scanned over the cell grown by 8 px so a
count drawn outside the slot is seen rather than clipped. Two counts, because a
single digit cannot distinguish right alignment from a fixed left edge — the
second control below is that exact case.

Both controls watched failing:

| control | result |
| --- | --- |
| the old derived anchors | `7` → `x10..15 y7..14`, `64` → `x4..15 y7..14`; four failures, right edge and top both wrong |
| right alignment dropped (fixed left edge) | `7` → `x11..16 y9..16`, **still perfectly correct**; `64` → `x11..22`, running 6 px outside the cell |

That second row is the reason the gate measures two counts. A one-digit gate would
have passed it.

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

### Gotcha 0 — `build_sprite_pipeline` is a code dedup, not a resource one

`item_icon::build_sprite_pipeline` builds the four `wgpu` objects a textured
sprite pass needs — bind-group layout, pipeline layout, render pipeline, bind
group — plus the dynamic vertex buffer, from a shader source, a
`wgpu::TextureFormat` and a `&Atlas`. It exists because that construction used
to be hand-copied in three places: `HudRenderer::attach_gui`,
`MenuRenderer::attach_gui` and `IconRenderer::attach_items` itself (the latter
already serving both `HudRenderer::attach_items` and
`ContainerRenderer::attach_items`, which delegate to it and never duplicated
this code). Each of those three now calls the shared function instead of
repeating ~90 lines of descriptors apiece.

**It does not share a bind-group layout, pipeline or bind group *instance*
across those three call sites, and never did.** `wgpu` does not deduplicate
structurally-equal layouts (see `docs/armour-rendering.md`'s note on the
armour pipeline), and before this function existed each of the three sites
independently built its own layout/pipeline/bind group — verified by reading
each site's code before moving it. `build_sprite_pipeline` is called three
times and allocates fresh `wgpu` objects on every call, exactly as the three
call sites did before, so nothing that depended on object identity changed.
If a future caller ever needs to *share* one pipeline across screens (not just
share the code that builds it), that is a different, larger change than this
one and needs its own identity audit.

One cosmetic difference: the pre-refactor `attach_gui` sites gave each
`wgpu` object its own suffixed debug label (`"hud-sprite-bgl"`,
`"hud-sprite-pipeline"`, …); `build_sprite_pipeline` reuses one `label` for
all of them, matching the convention `IconRenderer::attach_items`'s
pre-existing `label` parameter already used for its own two callers. Labels
are debugger-only and invisible to every pixel gate in this crate.

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
| no `attach_item_models` (demo path, no baked models) | flat sprites only; block items **and** special (chest) items draw an empty well |
| no `depth` passed to `render_with_item_models` | as above |
| `HudFrame::hotbar_items` is `None` | nothing is drawn in any cell |
| jar has no `entity/chest/*.png` | `SpecialIcons::new` returns `None`; chests draw nothing rather than a placeholder |

`attach_item_models` gates **both** 3-D passes, deliberately: the special pass needs
the same depth attachment, so one `models_attached()` signal covers both and no
caller learns a second flag. It is also what makes one `attach`-less frame serve as
the negative control for both pixel gates.

The special pass is built **lazily**, on the first frame that actually contains a
chest, because it needs a `queue` to upload its 22 sheets and `attach_item_models`
has only a `device` — widening that signature would reach up through both screens
into the contended `app.rs`. A `special_tried` flag stops a jar-less run re-trying
every frame. `HudRenderer::special_icon_sheets()` reports how many sheets it loaded,
which is what separates "no chest in any slot" from "no pack, so a chest could never
draw".

Cell geometry, mirrored by the gate: with the vanilla GUI atlas the icon is
`16 * 2` px at a `20 * 2` px pitch starting `3 * 2` px into the frame; without it
the procedural fallback uses a 16 px icon at a 22 px pitch.

## Dependencies

* **`lodestone-assets`** — `ItemAtlas` / `ItemIcon` / `IconPart`,
  `ResourceLocation`.
* **`lodestone-render`** — `BlockModels::item`, `gui_item_pose`, `gui_ortho`,
  `mesh_item_quads`, `ModelVertex`, `ModelPipeline`, `ModelCameraUniform`,
  `CameraUniform`, `fog::FogUniform`, `RenderLayer`; and for the special pass
  `BlockEntityModelSet` / `BlockEntityMesh::part_transforms`, `CHEST_SINGLE`,
  `ChestMaterial::from_block_path`, `ChestHalf`, `chest_texture_stem(s)`,
  `EntityPipeline`, `GpuEntityModel::upload_parts`, `EntityCameraUniform`,
  `entity_camera_buffer`, `upload_instances`.
* **`crate::resources::load_block_entity_textures`** — the 22 chest sheets, read
  straight from `client.jar`. Shared *code* with the world's block-entity pass but
  not shared *resources*: that pass keeps its bind groups against its own
  `EntityPipeline` instance's layouts, and 22 64x64 textures are cheap enough that
  reaching across the `gpu` module boundary is not worth it.
* **`crate::gpu::RenderState`** — the four borrowed GPU resources above.
* **`docs/block-entity-renderers.md`** — the geometry side, and the authoritative
  account of what is and is not reusable between the world chest and this one.
  A change to the chest models must be checked against **both** call sites.

## Known gaps

* ~~**The container screen draws flat icons but not 3-D block items**~~ — issue
  [#50](https://github.com/matteopolak/lodestone/issues/50), **closed**. `app.rs`
  now calls `render_with_icons_scaled` with `Some(render.depth_view())` and the real
  `item_models`, so `want_models = models_attached() && depth.is_some()` is `true`
  and `push_item_model` emits geometry for block items.

  This entry is kept because it went stale **twice**, in two different ways, and the
  progression is the useful part. First it said the screen "is not wired in `app.rs`
  yet" and kept a colour-swatch fallback — wrong, because `attach_items` and
  `attach_item_models` were both already installed. Then it said the remaining gap
  was one call through `render_scaled` hardcoding `depth: None, models: None` —
  correct when written, now fixed.

  The symptom's *shape* was the diagnostic each time: "flat, not missing" means the
  sprite stream is fine and only the model stream is starved, whereas a missing
  `attach_items` would have shown swatches. Reach for that distinction before
  grepping — it localises the break to one of the two streams in a single frame.
* ~~**`IconPart::Special` draws nothing — chest, shulker, banner, shield are
  invisible**~~ — issue
  [#369](https://github.com/matteopolak/lodestone/issues/369), **chest closed, nine
  kinds remain**. The arm was literally `IconPart::Special { .. } => {}`; it now
  routes through `special_icon_geometry` and a third `EntityPipeline` pass recorded
  inside `IconRenderer::draw_models`. Gated by
  `tests/hotbar_special_item_pixels.rs`.

  **The fix is keyed on `kind`, and that is load-bearing.** The family is ten kinds
  over 91 item definitions:

  | `kind` | defs | geometry | draws today |
  |---|---|---|---|
  | `minecraft:chest` | 13 | #23, all 7 materials | **yes** |
  | `minecraft:shulker_box` | 17 | not ported | no |
  | `minecraft:banner` | 16 | not ported | no |
  | `minecraft:copper_golem_statue` | 32 | not ported | no |
  | `minecraft:head` / `player_head` | 7 | not ported | no |
  | `minecraft:shield` | 2 | not ported | no |
  | `minecraft:trident` | 2 | not ported | no |
  | `minecraft:conduit` | 1 | not ported | no |
  | `minecraft:decorated_pot` | 1 | not ported | no |

  Each remaining row is **one match arm** in `special_icon_geometry` the day its
  model lands in `BLOCK_ENTITY_MODELS`; none of the wiring changes. Note the item id
  is consulted only *within* a kind, to choose the sheet — that is what makes one
  chest arm cover trapped, ender and the four copper weathering stages.

  **Do not "simplify" this to the `base` sprite fallback.** The issue proposed it as
  the cheap route and it is not a route at all: **every one of the ten `base` models
  has no `elements` and no `layer0`**, only a `particle` texture naming a *block*
  texture that is not in the item atlas. `classify_model` yields no
  `IconPart::Sprite`, so the fallback draws the same zero pixels under a different
  arm. Measured, not assumed —
  `the_base_sprite_fallback_is_vacuous_for_every_special_kind` asserts 1 special /
  0 sprite parts for all ten through the production resolver. The `base` model's
  value is its `display` map, which is where the chest's `gui` pose
  (`[30, 45, 0]` at `0.625`, **45** not 225) comes from.
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
| `crates/lodestone-shell/src/hud/item_icon.rs` | `ItemIcon`, `draw_item_icon`, `ColourStream`, `IconStratum`, `IconRenderer` (both pipelines, upload, draws) |
| `crates/lodestone-shell/src/hud.rs` | the hotbar consumer: cell layout, `Builder::item_icon`, the three passes |
| `crates/lodestone-shell/src/container.rs` | the container consumer: slot layout, chrome/overlay/carried splits, `render_with_icons` |
| `crates/lodestone-shell/src/gpu.rs` | the four borrowed-resource accessors on `RenderState` |
| `crates/lodestone-shell/src/app.rs` | attaches both screens at startup; passes models + depth per frame to **both** — the hotbar's `render_with_item_models` and the container's `render_with_icons_scaled`. (This row used to say the container call "still omits them, which is #50"; #50 closed, and the Known-gaps entry above already recorded that. Two statements about the same fact is how that entry went stale twice.) |
| `crates/lodestone-shell/tests/hotbar_block_item_pixels.rs` | hotbar pixel gate (GPU + `client.jar`, `#[ignore]`d) — 176 / 0 / 0 |
| `crates/lodestone-shell/tests/container_item_pixels.rs` | container pixel gate (same) — 176 block / 120 sprite / 0 empty / 0 control |
| `crates/lodestone-shell/tests/container_cursor_pixels.rs` | the carried-stack stratum gate (#377, same requirements) — 0 px bleed inside the cursor's ink across three cases; the pre-fix control fails at 128 / 46 / 37 px |
