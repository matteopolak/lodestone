# Held block-entity items

## What it is

The path that draws a **chest, shulker box or skull the player is holding**. These
items have no item model and no block model in vanilla — every triangle comes from a
block-entity renderer — so they need a rig, not baked quads, and the 3-D surfaces had
no way to ask for one. A held chest drew *nothing at all*: the hand showed a bare arm
as if the slot were empty, while the inventory slot right below it drew a real chest.

## How it works

Two joined halves, and both were missing.

```text
items/<id>.json  ─►  ItemModel tree  ─►  resolve(ctx)
                                          ├─ Model   ─► ItemGeometry (baked quads)  ── the old path
                                          └─ Special ─► SpecialItemForm { kind, display }
                                                            │
                                        special_item_rig(kind, item_path)
                                                            │
                                                  (rig name, sheet stem)
                                                            │
                                     BlockEntityModelSet + the block-entity sheets
                                                            │
                                          EntityPipeline, two bind groups
```

* **`ItemVariants::resolve_special`** (`lodestone-render/src/block_models.rs`) is the
  accessor `resolve` structurally cannot be. A caller that draws item geometry must
  ask **both**, baked first.
* **`special_item_rig`** (`lodestone-render/src/block_entity.rs`) is the single owner
  of `kind` + item path → (rig, sheet), shared by every surface.
* **`BlockEntityModelSet::resolve_special_item`** is the single owner of
  `(kind, item path, placement) → BlockEntityInstance` — the rig lookup, the sheet,
  the rest-pose part transforms and the cull AABB, so the three world surfaces differ
  only in the placement they pass.
* **`FirstPersonHand::Special`** (`lodestone-shell/src/gpu/first_person.rs`) and the
  three `*_special_item` methods in `gpu/entity_passes.rs` are the consumers.

### The bug was three `None`s in a row, not one

The held-item path was `items.get(item).and_then(|v| v.resolve(&ctx))`, and for a
chest every link in it yielded nothing:

1. `BlockModels::items` did not contain `minecraft:chest` **at all**. The regroup at
   the end of `BlockModels::build` dropped any item with no bakeable variant, and a
   chest's definition is one `special` node.
2. `resolve`'s `Special` arm is `None` by construction.
3. Its `or_else(gui())` fallback is `None` too, because `plan.gui` is `None` for a
   special item.

So no single fix downstream could have worked. `by_model` is now allowed to be empty
when the item has special forms, which leaves every geometry accessor answering
exactly as before (`BlockModels::item` still returns `None` for a chest — the GUI
stream is untouched) while `resolve_special` becomes reachable.

### There is no flat-sprite fallback, and believing there was one hid this

`IconPart::Special` carries a `base` model, and its own doc used to say the renderer
"can fall back to [it] as a flat sprite". It cannot: **every one of the ten special
`base` models in 26.2 has no `elements` and no `layer0`**, only a `particle` texture
naming a *block* texture that is not in the item atlas. The fallback draws the same
zero pixels as no fallback. What `base` really carries is the `display` map — a
chest's `firstperson_righthand` pose is authored on `item/template_chest` — and that
is the only reason it is resolved.

The same doc also asserted "the renderer's block-entity path draws it", which was a
plan stated as a fact for as long as the four discard sites in `block_models.rs`
existed. Both claims are corrected in place.

### Why the hand cannot use the model pipeline

Not the geometry — a `BlockEntityMesh`'s `part_transforms` takes an arbitrary
placement matrix, so `first_person_item_matrix` slots in exactly where the GUI's
`gui_item_pose` and the world's `block_entity_placement_matrix` go. It is the
**texture**: a chest's UVs are `[0,1]` against a standalone 64×64
`entity/chest/*.png`, while the model pipeline binds the stitched *block* atlas,
which contains nothing under `textures/entity/`. And the model pipeline cannot bind a
second texture — it spends all four bind groups, wgpu's portable `max_bind_groups`
floor.

So the hand draws through the **block-entity pass's** `EntityPipeline`, which spends
two. `BlockEntityRenderer` gained its own `hand_cam_bind_group` for that, exactly as
`EntityRenderer` has one, and **not** by borrowing the entity pass's: a bind group
belongs to the layout object it was created from, and this pass owns its own
`EntityPipeline`. Borrowing would be a validation error on some backends and silently
fine on others.

### The pose is the ordinary held-item pose

`first_person_item_matrix(ARM, swing, dip, transform)` — the same matrix the baked
branch feeds `first_person_item_mesh`, with `transform` from the `base` model's own
`display` map. That is what makes a chest and a pickaxe swing on the same arc and dip
on the same ramp. `part_transforms(placement, &[])` takes **no** pose overrides: a
held chest's lid is shut, because `ChestSpecialRenderer` carries no `openness` at all.

### The seasonal chest

`items/chest.json` is a `minecraft:select` on `minecraft:local_time` whose non-default
case is a Christmas texture, wrapping the ordinary special node. Nothing here supplies
a `local_time` property, so `ItemPropertyContext::select` returns `None` and
`resolve_node` takes the `fallback` — the plain chest. That is the correct default and
a bounded, asserted shortfall: **a chest on 25 December draws the ordinary sheet.**
`with_no_date_property_the_select_takes_its_plain_fallback` holds it, and checks the
tree really does contain both nodes so the assertion is not vacuous.

## How to change it

### Adding a `kind`

One match arm in `special_item_rig`, and every surface that consumes the seam gets it.
Which kinds resolve there, and which need their own entry point instead, is a table in
that function's own doc — read it there rather than here, so there is one copy.

**Check for existing geometry before writing any.** Four kinds — `conduit`,
`decorated_pot`, `copper_golem_statue` and `trident` — resolved to `None` and drew
nothing in hand, and in every one of the four the rig was **already in the tree**: the
meshes were baked, registered, and their sheets already in the preload list. The gap
was the resolver, not the geometry. Writing a second rig beside a working one is worse
than doing nothing, because both then look plausible to the next reader, so grep for a
mesh and a sheet stem before adding either.

Two traps that specifically cost time there, both recorded because the wrong answer
looked authoritative:

* **Read the *special* renderer, not the block-entity one of the same name.** A doc
  table here once said the conduit rig was unported because `ConduitRenderer` calls
  `bakeLayer` four times. That is the block entity. `ConduitSpecialRenderer.bake`
  takes `CONDUIT_SHELL` alone and submits one part — an item conduit is never active,
  so it is a plain `(model, sheet)` pair and always was.
* **A `kind` may need more than one pair, or a mesh from the other corpus.** A banner
  is two meshes plus translucent layers, a decorated pot is five independently sheeted
  parts, and a trident's mesh lives in the **entity** corpus rather than
  `BLOCK_ENTITY_MODELS`. Each has its own entry point — `banner_item_rig`,
  `decorated_pot_item_rig`, `trident_item_rig` — because `special_item_rig`'s
  `&'static str` return *means* "a key into `BLOCK_ENTITY_MODELS`". Returning an
  entity-corpus name from it type-checks, resolves to nothing in every caller, and
  draws exactly the empty hand you were trying to fill.

**Do not add a second copy of the mapping.** If you find yourself writing a chest rig
twice, that is the defect: it is how the inventory slot and the hand come to disagree.

### Proving a new rig is not an island

A rig keyed on a `kind` nothing produces draws nothing, and every hermetic gate in the
crate still passes — those supply the `kind` as a literal, which is a closed loop.
`crates/lodestone-render/tests/special_item_hand_rig_resolution.rs` breaks it: it reads
the pinned 26.2 jar and asserts the **real** item definition reaches the expected `kind`
in a first-person hand. Add your subject there.

It also carries the reciprocal for the trident, which is worth copying whenever a
definition is a `select` on `minecraft:display_context`: `trident.json` sends
`gui`/`ground`/`fixed`/`on_shelf` to the flat `item/trident` sprite and only the
fallback to a rig, so drawing a rig in the inventory would be the regression rather
than the fix.

### Adding a surface

All five of vanilla's surfaces are now wired:

| surface | where | pose |
|---|---|---|
| first-person hand | `gpu/first_person.rs`'s `prepare_special_hand` | `first_person_item_matrix` |
| inventory / hotbar slot | `hud/item_icon.rs`'s `SpecialIcons` | `gui_item_pose` |
| dropped stack in the world | `entity_passes.rs`'s `dropped_special_item` | `dropped_item_matrix` + `special_item_hover_lift` |
| another entity's hand | `entity_passes.rs`'s `held_special_item` | `held_item_matrix` off the holder's arm |
| item frame | `entity_passes.rs`'s `framed_special_item` | `framed_item_matrix` (one of four framed-item producers — see `docs/item-frame-rendering.md`) |

The three world surfaces share one shape, and the shared part is a function rather
than a convention: `ItemVariants::resolve_special` finds the form, `special_item_rig`
maps `kind` + item path to `(rig, sheet)`, and
**`BlockEntityModelSet::resolve_special_item`** turns a placement matrix into a
`BlockEntityInstance`. Each surface contributes only its own placement. They batch by
`(model, sheet)` alongside the *placed* block entities and coalesce with them for
free — a dropped chest and a placed chest are the same mesh and the same sheet, so
one draw covers both.

`prepare_block_entities` therefore takes the `entities` slice now. Its
seven-source emptiness condition **grew an eighth term**: without it, a chest lying
on the floor of a room with no chests, skulls, bells, shulkers, banners, lecterns or
enchanting tables in it would hit the early return and draw nothing — the same
island the seven existing terms each record.

### Poses: the three numbers that are not what they look like

* **The hover lift comes from the rig's AABB, not from quads.** `item_hover_lift`
  measures a `BakedQuad` list; a rig has none, so `special_item_hover_lift` takes
  `BlockEntityMesh::local_min`/`local_max` and transforms **all eight corners**.
  Transforming `local_min` alone agrees exactly whenever the display transform has no
  rotation, which is true of every `ground` transform in the game — so the shortcut
  looks correct and is only wrong for the case that would ever expose it.
* **`display_matrix` centres the model, and the lift is not `0.0625`.** Its trailing
  `translate(-0.5, -0.5, -0.5)` is `ItemTransform.apply`'s, taken even by
  `NO_TRANSFORM`. A rig whose bottom sits at local `y = 0` poses at `y = -0.5`, so a
  drop's lift is `0.5 + 0.0625`. This is also why a gate that probes the pose at
  `Vec3::ZERO` measures a *corner*: the point that maps to the pose origin is
  `Vec3::splat(0.5)`.
* **A framed item gets `0.5` from the frame *and* whatever `display.fixed` says.**
  Vanilla scales `0.5` and then applies the fixed transform, which is itself `0.5`
  for a chest and `1.0` for a skull. Copying `prepare_framed_maps`' `FRAMED_MAP_SCALE`
  of `1.0` is the trap — a map has its own full-block branch, and a chest at `1.0`
  would be twice the size of the frame around it.

### What the world surfaces deliberately do not do

* **A dropped stack draws one copy, not up to five.** Vanilla's
  `submitMultipleFromCount` picks between a 3-axis jitter and a `z` fan using the
  *posed model's own depth*, which is the quad-list measurement a rig cannot supply —
  and the wrong branch would fan a chest along `z` like a sprite.

Both of the item-frame gaps this section used to list — the undecoded eight-step
`rotation`, and an ordinary framed item drawing nothing — are **closed**, along with
the frame's own body, which had no producer at all. `docs/item-frame-rendering.md`
carries that surface now; `framed_item_matrix` takes a `rotation` and an `invisible`
flag, and its lift is the one this section's third bullet above still describes
correctly.

### Gotcha — a presence assertion cannot see this bug

"Something drew" is satisfied by a flat sprite, which is precisely why the GUI looked
fine for so long. Assert a **quad count characteristic of the rig**: a single chest is
`18` quads (bottom + lid + lock, six faces each), against `6` for a block-item cube
and `0` for the sprite fallback. And pick a rig that is not cube-like — a closed
shulker box is nearly a cube, so a chest's lid and lock are the better subject.

## Configuration

None. No new constants, no options, no feature gates.

`RenderStats::special_item_drops_drawn`, `..._hands_drawn` and `..._frames_drawn` are
**three** counters rather than one total, because the three surfaces failed
independently: each was its own island, and a single number would have read
"special items are drawing" while two of the three still drew nothing.

`RenderStats::first_person_item_drawn` is `true` for a held special item as well as a
held baked one: the question it answers is "is the hand holding something visible",
and reporting `false` for exactly the case this path exists to fix is the shape of an
island counter.

Degradations, all fail-open:

| condition | result |
|---|---|
| no vanilla pack / no baked models | nothing draws; the hand falls through to the bare arm |
| jar has no `entity/chest/*.png` | nothing draws rather than an untextured box — the same asymmetry the world pass uses |
| a `kind` with no rig (a datapack one) | nothing draws, which is the behaviour before this existed |
| a held **decorated pot** carrying sherds | draws with the four *default* side sprites. `lodestone_model` decodes `minecraft:pot_decorations` into a real `PotDecorations`, but the shell's hotbar record does not carry it through to `MainHandItem`, so it cannot reach the rig — the same one-field-per-layer walk `banner_patterns` and `base_color` already made. An **un**decorated pot is exactly right, not approximate: vanilla's own `submit` uses `PotDecorations.EMPTY` for a stack with no component |
| a held **custom head** whose skin has not been fetched yet | draws the **default Steve** sheet rather than nothing, unlike every other row here. A head that lost its texture still looks like a head, so the decline is invisible at the draw site and is logged instead — see `docs/player-skins.md`. Once `remote_skins` finishes, the same stack draws its own face with no further plumbing |
| a held **copper golem statue** whose stack names a pose | draws **standing**. `special_item_rig`'s `(kind, item_path)` signature has no room for a `minecraft:block_state` component, and standing is what vanilla's own `select` falls back to for an ordinary stack |
| an enchanted held chest | draws **unglinted**; the glint pipeline rasterises the model shader's vertex layout, and this is instanced entity geometry. Same shortfall as the inventory slot, for the same reason |

## Dependencies

* `lodestone-assets` — `IconPart::Special`, `ItemIconBuilder::part_for` (the entry
  point that returns a special node's `display` map), `DisplayTransforms`.
* `lodestone-render` — `special_item_rig`, `banner_item_rig`,
  `decorated_pot_item_rig`, `trident_item_rig`, `SpecialItemForm`,
  `ItemVariants::resolve_special`, `BlockEntityModelSet`, `EntityModelSet` (the
  trident's own corpus), `BlockEntityMesh::part_transforms`,
  `Skeleton::rest_pose` (its entity-corpus equivalent), `EntityPipeline`,
  `entity::first_person_item_matrix`.
* `lodestone-shell` — `gpu/block_entities.rs`'s `BlockEntityRenderer` (rigs, sheets
  and the hand camera), `gpu/first_person.rs`'s `FirstPersonHand`,
  `hud/item_icon.rs`'s `SpecialIcons` for the GUI half.
* [`block-entity-renderers.md`](./block-entity-renderers.md) for the rigs themselves,
  and [`gui-item-icons.md`](./gui-item-icons.md) for the inventory-slot pass.
