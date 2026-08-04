# Humanoid armour rendering

## What it is

The path from "the server says this zombie is wearing a diamond chestplate" to a
posed, textured armour layer on screen. Four cuboid meshes baked at **two
different inflations**, painted by per-material sheets resolved through the
`equipment_asset` chain, drawn through the entity pipeline over the wearer's own
already-animated part matrices.

Landed across:

| file | what it owns |
| --- | --- |
| `crates/lodestone-assets/src/equipment.rs` | the two inflations, the four slot meshes, the item→asset table, texture paths, the undyed-leather colour |
| `crates/lodestone-render/src/entity.rs` | `ArmourMesh`/`ArmourModelSet`, `attach`, `armour_layers`, `wearer_carries_armour`, `armour_layer_tint`/`armour_layer_tint_with_dye` |
| `crates/lodestone-render/src/entity_pipeline.rs` | the per-instance tint attribute, `EntityPipeline::armour_pipeline` |
| `crates/lodestone-shell/src/gpu.rs` | sheet loading, `prepare_armour`, the draw, `RenderStats::armour_layers_drawn` |
| `crates/lodestone-shell/src/sim.rs` | `Sim::third_person_body_state` — the local player's own equipment, including its four armour slots |
| `crates/lodestone-shell/tests/armour_pixels.rs` | the pixel gate — real render path, analytic lower bound, negative control |

Companion to [`entity-rendering.md`](./entity-rendering.md) (the mob pipeline this
layers over) and [`item-prototypes.md`](./item-prototypes.md) (why the item→asset
mapping cannot be derived).

## What draws today

* **Plain armour on all four humanoid slots**, for all eight materials
  (leather / copper / chainmail / iron / gold / diamond / netherite /
  turtle_scute), on any rig that classifies as `AnimFamily::Humanoid` — player,
  zombie family, skeleton family, armour stand, and so on.
* **Undyed leather**, tinted in gamma space, including its second
  `leather_overlay` detail layer.
* Mob armour reaches pixels **with no wiring outside these files**: the chain
  `SET_EQUIPMENT` → `EntityView::equipment` → `net.rs::entity_snapshot` →
  `EntitySnapshot` → `entities.rs::occupied_equipment` → `EntityDraw::equipment`
  already carried all eight slots; only `MainHand`/`OffHand` were being consumed.
* **The local player's own armour in third person.** Landed in `22dc0ee`
  (`Sim::third_person_body_state`, `crates/lodestone-shell/src/sim.rs`) —
  this doc previously listed it under "Wiring still needed" and that line was
  stale; `ARMOUR_NATIVE_SLOTS` there already reads native indices
  `39/38/37/36` into `EquipmentSlot::{Head,Chest,Legs,Feet}` and
  `ThirdPersonBodyState::into_draw` copies them into `EntityDraw::equipment`
  verbatim, exactly as originally planned. Remote players needed nothing new
  — they are ordinary tracked entities.
* **A pixel gate.** Landed alongside the wiring above, in
  `crates/lodestone-shell/tests/armour_pixels.rs`
  (`a_fully_armoured_zombie_draws_more_silhouette_than_a_bare_one`) — see
  "Gates" below. That file is owned by another agent's in-flight work; read
  it, do not edit it here.

## What does not

* **Trims.** Designed, not landed — see "Trims" below.
* **A stack's actual dye colour.** The render-side formula
  (`armour_layer_tint_with_dye`) landed and is hermetically tested, but
  nothing upstream feeds it a real value yet, so the undyed default still
  draws in practice — see "Dye" below.
* **Baby armour meshes**, **enchantment glint**, **`Body`/`Saddle` (animal)
  armour**, **elytra**, **skull/pumpkin heads**. Each is a different vanilla
  layer with its own model; see "Deliberately out of scope".

## How it works

### The two inflations, and why this is the detail ports get wrong

Vanilla bakes the humanoid armour mesh set **twice** and hands the *inner* one to
the legs slot alone:

```java
// LayerDefinitions.java:162-163
private static final CubeDeformation OUTER_ARMOR_DEFORMATION = new CubeDeformation(1.0F);
private static final CubeDeformation INNER_ARMOR_DEFORMATION = new CubeDeformation(0.5F);
// LayerDefinitions.java:173
HumanoidModel.createArmorMeshSet(INNER_ARMOR_DEFORMATION, OUTER_ARMOR_DEFORMATION)
```

`HumanoidModel.createArmorMeshSet` (`HumanoidModel.java:129-144`) then applies
them per slot: `head`, `chest` and `feet` get **outer**, `legs` gets **inner**.
With one inflation the leggings and the chestplate both draw a `body` cube over
the same torso at the same radius, and which one you see is decided by z-fighting
— the classic "looks fine on a fully-armoured mob, wrong on a mob wearing
leggings only" bug.

Two further per-cube adjustments sit on top, both read from source:

* **Legs are 0.1 texels thinner than their slot.** `createBaseArmorMesh`
  (`HumanoidModel.java:146-160`) re-adds `right_leg`/`left_leg` with
  `g.extend(-0.1F)` — `HumanoidModel.LEGGINGS_OVERLAY_SCALE`
  (`HumanoidModel.java:33`).
* **The helmet keeps a `hat` shell at +0.5.** The head slot uses
  `retainPartsAndChildren({"head"})`, which retains a part *with its subtree*,
  and `hat` is authored at `g.extend(0.5F)` (`HumanoidModel.java:93`).

So the **effective** inflations are:

| slot | parts | inflation |
| --- | --- | --- |
| head | `head` | 1.0 |
| head | `hat` | 1.5 |
| chest | `body`, `right_arm`, `left_arm` | 1.0 |
| legs | `body` | 0.5 |
| legs | `right_leg`, `left_leg` | 0.4 |
| feet | `right_leg`, `left_leg` | 0.9 |

`legs_are_a_tenth_of_a_texel_thinner_than_their_slot` and
`the_leggings_body_cube_sits_inside_the_chestplate_body_cube` pin these on the
**baked geometry**, not on the constants, so a change that drops the override
anywhere between the table and the bake goes red.

**The `hat` shell draws zero pixels, and that is measured, not assumed.** Its
cubes unwrap onto `x ∈ [32, 64)`, `y ∈ [0, 16)` of the 64×32 sheet, and that
region is fully transparent in all nine of 26.2's humanoid armour textures
(counted per texel against the real PNGs). It is kept because vanilla keeps it.
Also: every humanoid armour sheet is **strictly binary alpha** (only 0 and 255
appear), which is why the shared entity shader's `0.5` cutout is safe here even
though vanilla's `ARMOR_CUTOUT_NO_CULL` uses `ALPHA_CUTOUT 0.1F`.

### Sheets are 64×32

`LayerDefinition.create(mesh, 64, 32)` — *not* the 64×64 a modern player skin
uses. A 64×64 assumption halves every V coordinate and paints the legs with the
helmet's pixels, which reads as a texture-resolution bug rather than a mesh one.
`armour_sheets_are_sixty_four_by_thirty_two` pins it.

### Every piece is posed off the *wearer's* part matrix

This is the core design decision and it is the faithful one.

Vanilla's armour model is an instance of the **wearer's own model class**:
`AbstractZombieRenderer` builds an `ArmorModelSet<M extends ZombieModel>`
(`AbstractZombieRenderer.java:14-22`), and `EquipmentLayerRenderer.renderLayers`
submits it with the wearer's render state, so `setupAnim` runs
`animateZombieArms` on the *chestplate*. A zombie's armour reaches out in front
because the armour ran the same animator.

There is one animator per mesh here, so the equivalent is to run **no second
pose at all**:

```rust
// ArmourMesh::attach — crates/lodestone-render/src/entity.rs
for (range, wearer_index) in armour_mesh.attach(&wearer.skeleton) {
    let m = instance.part_transforms[wearer_index];   // read only
    // draw `range` instanced over `m`
}
```

`ArmourMesh`'s vertices are **part-local** and its pivots come from the same
`lodestone_assets::entity_models::humanoid_root` builder the wearer's rig uses
(shared `pub(crate)` for exactly this reason — vanilla shares it too, via
`createBaseArmorMesh` calling `createMesh`). So the wearer's matrix for the part
of the same name is *exactly* the right transform, with nothing appended.

**Nothing is written back to `part_transforms`.** That is the same discipline
`EntityInstance::hand_transforms` exists to enforce: there, folding a held item's
pivot shift into `part_transforms` would have dragged the mob's visible arm along
with the sword. Armour needs the matrix unmodified, so there is nothing to fold
in — but the rule is the same one, and a future "optimisation" that poses armour
by mutating the wearer's transforms would break the mob, not the armour.

Two consequences worth knowing, both sub-texel and both deliberate:

* `skeleton`/`stray`/`wither_skeleton` pivot their legs at `x = ±2.0` where
  `HumanoidModel` has `±1.9`, so skeleton leg armour sits 0.1 texel
  (0.00625 blocks) further out than vanilla draws it.
* `player_slim` pivots its arms 0.5 texel lower than the wide rig, and vanilla
  bakes only **one** player armour set (`PlayerModel.createArmorMeshSet` takes no
  slim flag and adds only empty `left_sleeve`/`right_sleeve`/`*_pants`/`jacket`
  nodes), so a slim player's arm armour sits 0.5 texel (0.03 blocks) low.

Following the visible limb is worth more than matching vanilla's pivot to a
thirtieth of a block. The alternative — posing a second skeleton — would
reintroduce the zombie-arm divergence vanilla avoids by construction.

### The humanoid gate is the animation family, not part names

`wearer_carries_armour(skeleton)` is `family() == AnimFamily::Humanoid`, i.e.
"has both arms and both legs" — which is what `HumanoidModel` means, and which
matches which renderers own a `HumanoidArmorLayer`.

**A pig has both `head` and `body`.** A chestplate keyed on part names alone
attaches its `body` cube to a pig's torso and draws a floating breastplate on a
farm animal: geometry that resolves perfectly and is completely wrong. The gate
lives *inside* `ArmourMesh::attach` so a caller cannot forget it, and
`a_pig_attaches_no_armour_despite_having_a_body_part` carries the control
asserting the name lookup would otherwise have succeeded.

### Item → asset → texture

An armour item does not name its texture. It carries `minecraft:equippable`,
whose `assetId` keys the `equipment_asset` registry
(`ArmorMaterials.java` → `EquipmentAssets.java`), and the client reads
`assets/<ns>/equipment/<asset>.json` for a per-layer-type list of texture layers.
`EquipmentClientInfo.Layer.getTextureLocation` (`EquipmentClientInfo.java:105-107`)
builds the path:

```text
textures/entity/equipment/<layer_type>/<texture>.png
```

`layer_type` is `humanoid` for head/chest/feet and `humanoid_leggings` for legs
(`HumanoidArmorLayer.usesInnerModel` is `slot == LEGS`).

**`assetId` is not on the wire and not in the item-prototype census.** A
clientbound `/give diamond_helmet` arrives with an *empty* component patch, and
the committed census carries only `equippable.slot()` (see
[`item-prototypes.md`](./item-prototypes.md), "Only the slot is carried"). So
`equipment::ARMOUR_ITEMS` is a 29-row table transcribed from `ArmorMaterials`.
The gotcha it exists to prevent: the item is **`golden_helmet`** while the asset
is **`gold`**, so stripping the piece suffix would look up a nonexistent
`equipment/golden.json`.

29, not 38: 26.2 has 38 items in a `HUMANOID_ARMOR` slot, but
`HumanoidArmorLayer.shouldRender` requires an `assetId`
(`HumanoidArmorLayer.java:38-45`). The other nine are drawn by other layers —
`carved_pumpkin` and the seven skulls by `CustomHeadLayer`, `elytra` by
`WingsLayer`. `non_armour_head_items_do_not_resolve` keeps a pumpkin from
rendering as a helmet-shaped shell.

Slot equality is enforced too: `armour_layers(slot, item)` returns empty unless
the item's *own* declared slot matches, which is `shouldRender`'s
`equippable.slot() == slot`. A plugin putting a helmet in the boots slot draws
nothing, as vanilla does.

### Dye, and the gamma-space multiply

Leather's layer list is two entries (`equipment/leather.json`):

```json
"humanoid": [
  { "dyeable": { "color_when_undyed": -6265536 }, "texture": "minecraft:leather" },
  { "texture": "minecraft:leather_overlay" }
]
```

`-6265536` is `0xFF_A0_65_40` — `(160, 101, 64)` after `ARGB::opaque`. The base
sheet is **near-greyscale**: 589 of `humanoid/leather.png`'s 660 opaque texels
are exactly grey, measured against the real PNG. A port that skips the tint
renders leather armour as pale iron.

**The real-dye rule, and the render side of it landed.**
`armour_layer_tint_with_dye(layer, dyed_color: Option<u32>)`
(`crates/lodestone-render/src/entity.rs`) transcribes
`EquipmentLayerRenderer.getColorForLayer` exactly:

```java
// EquipmentLayerRenderer.java:113-121
private static int getColorForLayer(Layer layer, int dyeColor) {
   Optional<Dyeable> dyeable = layer.dyeable();
   if (dyeable.isPresent()) {
      int colorWhenUndyed = dyeable.get().colorWhenUndyed().map(ARGB::opaque).orElse(0);
      return dyeColor != 0 ? dyeColor : colorWhenUndyed;
   } else {
      return -1;
   }
}
```

where `dyeColor` is:

```java
// DyedItemColor.java:27-30
public static int getOrDefault(final ItemStack itemStack, final int defaultColor) {
   DyedItemColor color = itemStack.get(DataComponents.DYED_COLOR);
   return color != null ? ARGB.opaque(color.rgb()) : defaultColor;
}
```

called as `DyedItemColor.getOrDefault(itemStack, 0)` — so "component absent"
and "default `0`" are the same input to `getColorForLayer`, and the function
does not distinguish them.

**A leather piece dyed pure black reads as undyed**, and this is vanilla's own
behaviour: `ARGB.opaque` only forces the alpha byte, so a `0x000000` dye still
has RGB `0`, `dyeColor != 0` is false, and the ternary falls through to
`colorWhenUndyed` exactly as if no dye were applied at all. Pinned by
`dyed_color_zero_reads_as_undyed` so a future "fix" that special-cases black
does not quietly diverge from the game it ports.

Three hops separated this from a real value reaching it. **Two are closed:**

1. **Closed.** `crates/protocol/v770/src/adapter.rs::read_component_patch` now
   decodes `minecraft:dyed_color` (registry id 44, `DyedItemColor.STREAM_CODEC`
   — a bare `ByteBufCodecs.INT`, i.e. `reader.i32()`, not a `VarInt` like every
   other scalar component here) into `ItemComponents::dyed_color: Option<u32>`.
   Hermetically tested against a hand-built wire vector
   (`container_set_slot_decodes_a_dyed_leather_helmet`,
   `crates/protocol/v770/tests/container_inventory.rs`) — the expected rgb is a
   literal chosen to prove the fixed-width read, not round-tripped through our
   own encoder.
2. **Additive, not the wide-tuple shape originally sketched here.** Widening
   `EntitySnapshot::equipment`/`EntityDraw::equipment` to a 3-tuple would have
   touched every existing call site that destructures them (several in
   `gpu.rs` alone) for no reason — nothing about item *identity* needed to
   change. Instead `EntitySnapshot::equipment_dye: Vec<(EquipmentSlot, u32)>`
   and `EntityDraw::equipment_dye` (same shape) ride *alongside* `equipment`,
   entities.rs-side plumbing (`RenderEquipmentDye` component, folded in
   `spawn_track`/`update_track`, read in `extract_entity_draws`) is done. The
   one remaining piece is `net.rs::entity_snapshot` populating
   `equipment_dye` from `view.equipment`'s `ItemStack.components.dyed_color`
   — a brokered-file patch, not yet landed as of this writing; the exact diff
   is in the commit message of whichever commit lands this doc update.
3. **Closed** (once hop 2's net.rs half lands). `crates/lodestone-shell/src/
   gpu.rs`'s `prepare_armour` now looks up the current slot's dye in
   `draw.equipment_dye` and calls
   `armour_layer_tint_with_dye(layer, dye)`. `armour_layer_tint` is kept as
   the `None`-dye convenience wrapper (still used by the crate's own hermetic
   armour-pixel gates, which do not exercise dye).

The tint rides the **instance buffer** as a packed `0x00RRGGBB` word at shader
location 9, not a bind group — the model shader is at wgpu's 4-group floor and a
fifth group compiles on an M5 (8 groups) while crashing at startup on any
4-group adapter. A vertex attribute has no such ceiling.

It is multiplied **in gamma space**, inside the *same* transfer round-trip the
directional and world-light shades already use:

```wgsl
let lit = srgb_to_linear(linear_to_srgb(tex_col.rgb) * in.tint * diffuse * in.light_term);
```

Vanilla is not colour-managed: its `submitModel(..., color, ...)` becomes a
vertex colour multiplying the gamma-encoded texel byte. Doing the dye multiply in
linear light pulls every factor toward 1.0 and washes it out — the same trap
`CLAUDE.md` records for tint and shade. `tint_defaults_to_white_and_packs_rgb_in_order`
also pins that an untinted instance is **white, not zero**: zero would be black
armour, and every mob in the game goes through `EntityInstanceRaw::new`.

### Depth: why armour has its own pipeline

`EntityPipeline::armour_pipeline` is a second `wgpu::RenderPipeline` over the
*same* bind-group layout objects. It was created because it needed a different
`depth_compare` from the base pipeline; **since issue #21 the two are
depth-identical** (both `LessEqual`) and it survives for its label and to keep
the armour pass's requirement explicit at its own call site.

Vanilla's entity depth state is
`DepthStencilState.DEFAULT = (GREATER_THAN_OR_EQUAL, writeDepth = true)`
(`DepthStencilState.java:6`), inherited by every entity render type from
`ENTITY_SNIPPET` (`RenderPipelines.java:49-56`) with no override anywhere —
`ENTITY_SOLID` (`:232`), `ENTITY_CUTOUT` (`:245`), `ENTITY_CUTOUT_CULL` (`:238`),
`ENTITY_TRANSLUCENT` (`:274`). Under this engine's `[0,1]` DirectX-style depth —
vanilla is reversed-Z — that is **`LessEqual`**.

This section used to say the base pipeline's `Less` "is the one that departs from
vanilla. It is left alone rather than 'fixed': changing it alters how every mob's
coplanar geometry resolves, and this work has no pixel gate to prove that safe."
That was accurate when written, and it named its own missing prerequisite. Issue
#21 built the gate — `lodestone-render/tests/entity_depth_coincident_pixels.rs`,
which measured the bug directly (two coincident quads, red drawn first, blue
second: the frame read `[189, 0, 0]` with blue covering **0 of 16384** pixels, so
the *first* draw was winning) — and then made the change.

Armour needed the faithful value first because leather's two layers are
**coplanar at one inflation**. Under `Less`, the `leather_overlay` pass fails the
depth test against the base at every texel and is silently invisible.
`armour_layers_drawn` counts *layers* rather than pieces so a regression here is
legible: a count that drops to one per piece means resolution broke, a count that
stays at two with no overlay on screen means depth did. The same mechanism was
costing every mob its coincident geometry one layer up, which is what #21 fixed.

Sharing `self`'s layout objects (rather than creating equivalent descriptors)
means every camera and texture bind group already built through the base pipeline
is valid on the armour one, with no reliance on wgpu deduplicating structurally
identical layouts.

### Draw order

Armour is drawn in the same render pass, **immediately after the mob bodies** and
before dropped items / crack / translucent water. The pieces are physically
outside the body (smallest inflation +0.4 texels) so depth sorts body against
armour on its own.

`prepare_armour` walks `ArmourSlot::ALL` — which is `HumanoidArmorLayer.submit`'s
own order, chest → legs → feet → head (`HumanoidArmorLayer.java:48-52`) — rather
than the equipment list, so the order is deterministic regardless of what order
the server sent. Batches accumulate into a `Vec`, never a `HashMap`, because
insertion order *is* the layer order that makes the leather overlay visible.

### Batching

Four meshes, ~17 textures, uploaded once at startup:

* **Meshes are per slot, not per material** — the geometry depends only on the
  inflation, so eight materials do not mean eight helmets.
* **Textures are keyed `(texture name, layer type)`** — 9 `humanoid` sheets and
  8 `humanoid_leggings` ones (no turtle leggings exist). Leather shares one layer
  list between both layer types, so the loader dedupes.

Per frame, instances group by `(slot, texture)` and then by armour part, so a
field of armoured zombies is a handful of instanced draws rather than one per
mob.

## How to change it, and the gotchas

* **Never fold `EquipmentSlot::Body` into `Chest`.** `humanoid_armour_slot` is
  the gate and it maps exactly the four `HUMANOID_ARMOR` slots. `BODY` is
  `ANIMAL_ARMOR` — wolf armour and horse barding live there — and `SADDLE` is
  its own type. `EquipmentSlot::isArmor` is the *union* of humanoid and animal
  armour (`EquipmentSlot.java:73-75`) and is therefore also the wrong predicate.
  The visible symptom of getting this wrong is a player wearing a horse's diamond
  barding as a chestplate. `only_the_four_humanoid_slots_map_to_armour` pins it,
  including a count assertion so a new vanilla slot fails here rather than being
  silently ignored.
* **Never pose armour by mutating `part_transforms`.** See "Every piece is posed
  off the wearer's part matrix".
* **A new material** is one row in `ARMOUR_ASSETS` plus its items in
  `ARMOUR_ITEMS`; nothing in the render or shell layer changes. The
  `humanoid_armour_items_cover_every_material` test requires every asset to be
  reachable from an item and every item's slot to declare layers.
* **Armour has no synthetic-texture fallback**, unlike a mob's own sheet. With no
  vanilla pack `armour_textures` is empty and `prepare_armour` returns
  immediately. That asymmetry is deliberate: a flat-magenta mob reads as "this
  mob's sheet is missing", whereas a flat-coloured shell over a mob's head reads
  as a rendering bug, and the offline demo has no armour to draw anyway.
* **The sheet loader duplicates `resources.rs`'s pack discovery, and should not
  have to.** `resources::asset_root`/`open_client_jar` are private and
  `resources::vanilla_manager` is `#[cfg(test)]`, so production code elsewhere
  cannot reach any of them; `hud::vanilla_font::jar_manager` already carries an
  identical copy and says the same thing. The right end state is one
  `pub(crate) fn vanilla_manager()` in `resources.rs` with all three callers
  going through it — a one-line attribute change in a file this pass did not own.
  Until then the discovery rule is duplicated *exactly*.

## Wiring still needed (outside this change's files)

**The local player's own armour in third person landed in `22dc0ee`** — see
"What draws today" above. This section previously described it as
outstanding; that was stale by the time it was re-read for this pass, per
`CLAUDE.md`'s note that the written record is the most common source of
stale claims in this repo. The only item still open:

1. **Real dye colours** — `Dyeable.colorWhenUndyed` draws until this lands,
   which is correct for an undyed piece and wrong for a dyed one. See "Dye"
   above for the current state: the protocol decode and the render/gpu.rs
   sides are closed, `entities.rs`'s additive `equipment_dye` plumbing is
   done, and the one remaining piece is a one-line brokered patch to
   `crates/lodestone-shell/src/net.rs::entity_snapshot` (populate
   `equipment_dye` from `view.equipment`'s `ItemStack.components.dyed_color`).

## Trims: designed, not landed, and why

Deliberately deferred — the blockers are the same off-limits files item 2's
wiring needs (`crates/protocol/v770` for the component, `gpu.rs`/
`entity_pipeline.rs` for a third pipeline) plus an atlas-stitching capability
this crate does not have yet, so a partial landing would cost more to review
than the current honest absence. What it would take, so the next pass does not
have to re-read the client:

* **Input does not exist.** `minecraft:trim` is an `ArmorTrim` record
  (pattern + material holders); nothing in `crates/protocol/v770` decodes it, and
  `entity_snapshot` drops components anyway — the same gap dye has, one step
  worse because the component itself is unmodeled.
* **A third depth mode.** `RenderPipelines.ARMOR_DECAL_CUTOUT_NO_CULL` is
  `DepthStencilState(CompareOp.EQUAL, false)` — depth compare **equal, no depth
  write** — plus `LayeringTransform.VIEW_OFFSET_Z_LAYERING`. Under `[0,1]` depth
  that is `CompareFunction::Equal` with `depth_write_enabled: false`; it is a
  third pipeline, not a variant of the two that exist.
* **A stitched trim atlas, and the material palette is per-*wearer*-material,
  not global.** `ArmorTrim.layerAssetId`:

  ```java
  // ArmorTrim.java:41-44
  public Identifier layerAssetId(final String layerAssetPrefix, final ResourceKey<EquipmentAsset> equipmentAsset) {
     MaterialAssetGroup.AssetInfo materialAsset = this.material().value().assets().assetId(equipmentAsset);
     return this.pattern().value().assetId().withPath(patternPath -> layerAssetPrefix + "/" + patternPath + "_" + materialAsset.suffix());
  }
  ```

  called with `layerAssetPrefix = layerType.trimAssetPrefix()` = `"trims/entity/" + id`
  (`EquipmentClientInfo.java:140-142`, e.g. `trims/entity/humanoid`), so the
  final path is `trims/entity/<layer_type>/<pattern>_<material suffix>`. The
  interesting part is `assetId(equipmentAsset)`:

  ```java
  // MaterialAssetGroup.java:56-58
  public MaterialAssetGroup.AssetInfo assetId(final ResourceKey<EquipmentAsset> equipmentAssetId) {
     return this.overrides.getOrDefault(equipmentAssetId, this.base);
  }
  ```

  — the trim **material**'s own `MaterialAssetGroup` looks up an override keyed
  by the **wearer's armour** `equipmentAsset`, falling back to its plain
  `suffix` otherwise. Only five of the eleven trim materials declare any
  override at all, and each declares exactly one — itself:

  ```java
  // MaterialAssetGroup.java:37-43
  IRON = create("iron", Map.of(EquipmentAssets.IRON, "iron_darker"));
  NETHERITE = create("netherite", Map.of(EquipmentAssets.NETHERITE, "netherite_darker"));
  COPPER = create("copper", Map.of(EquipmentAssets.COPPER, "copper_darker"));
  GOLD = create("gold", Map.of(EquipmentAssets.GOLD, "gold_darker"));
  DIAMOND = create("diamond", Map.of(EquipmentAssets.DIAMOND, "diamond_darker"));
  ```

  So a **diamond**-material trim on a **diamond** chestplate resolves to the
  `..._diamond_darker` sprite, but the same diamond trim on an iron chestplate
  (or any other material) resolves to plain `..._diamond` — the override only
  fires when the trim material matches the piece it decorates, for these five
  materials (`quartz`/`redstone`/`emerald`/`lapis`/`amethyst`/`resin` never
  override). Picking the sprite from the trim material alone, ignoring the
  wearer's own armour material, is exactly the plausible-looking-but-wrong
  colour `CLAUDE.md` warns this task about. Those sprites live in the
  `armor_trims` texture atlas, so this needs an atlas stitch this crate does not
  do yet, plus `TrimPattern.decal()` to choose between two sheet variants.

A helmet that draws with the wrong trim sprite looks like a *material* bug, so
shipping this half-done would be worse than the current honest absence.

## Deliberately out of scope

| thing | vanilla layer | why not here |
| --- | --- | --- |
| baby armour | `HUMANOID_BABY` layer type, `createBabyArmorMesh` | a whole second mesh set with its own deformations (`-0.1/0.5/0.3` outer, `-0.1/0.3/0.3` inner) and its own `waist`/`*_foot` parts. A baby wears adult armour scaled by the mob's 0.5 uniform scale — visibly close, not vanilla. |
| enchantment glint | `armorEntityGlint` | `ItemStack.hasFoil` is not on this side of the wire. |
| animal armour (`Body`) | `WolfArmorLayer`, `HorseArmorLayer`, `LlamaDecorLayer` | different meshes, different layer types (`wolf_body`, `horse_body`, `llama_body`). Not humanoid — see the `Body` gotcha above. |
| saddles (`Saddle`) | eleven per-mount saddle layer types | same. |
| elytra | `WingsLayer` / `ElytraModel` | `chest`-slot but its own model; `equipment/elytra.json` declares only a `wings` layer. |
| skulls, `carved_pumpkin` | `CustomHeadLayer` | a block or skull *model* on the head, not an armour mesh. Has no `assetId`, so `shouldRender` is false. |
| piglin armour offsets | `PiglinModel.createArmorMeshSet(INNER, new CubeDeformation(1.02F))` | piglins use a slightly larger outer inflation and a shifted baby arm pose. Piglins are not in the model corpus. |

## Configuration

No feature flag or env var of its own. `LODESTONE_ASSETS` (or a discovered
`.cache/mc/<version>/`) must contain `client.jar`, or `armour_textures` is empty
and nothing draws.

## Gates

Hermetic:

* `lodestone-assets` `equipment` module (11 tests) — the two inflations, the
  `-0.1` leg override on baked geometry, leggings-inside-chestplate, per-slot
  part retention, hat retention only on the helmet, the 64×32 sheet, the item
  table's closure and its slot/suffix agreement, the nine non-armour head items,
  leather-only dyeability, and texture paths.
* `lodestone-render` `entity` — every slot attaches to every armour-wearing
  corpus rig; a pig attaches nothing *with* the control that its `body`/`head`
  lookup would have succeeded; every armour matrix is a positive-determinant
  wearer matrix whose composition with a **real camera's** `view_projection`
  inherits that camera's sign (derived, not asserted); layer resolution across
  slots and non-armour items; leather-only tint; and, for
  `armour_layer_tint_with_dye`, a real dye reaching the dyeable base layer but
  not the non-dyeable overlay, absent-dye falling back to
  `colorWhenUndyed` (and agreeing with the zero-argument wrapper), the
  `dyed_color == 0` reads-as-undyed quirk, and a non-dyeable layer ignoring a
  present dye.
* `lodestone-render` `entity_pipeline` — the instance record is 72 bytes with the
  tint at location 9, and an untinted instance is white rather than zero.
* `lodestone-shell` `gpu` — `only_the_four_humanoid_slots_map_to_armour`; a fully
  armoured zombie's `EntityDraw` resolves 5 layers over 10 attach points, with
  `Body` horse armour contributing nothing.
* `lodestone-shell` `gpu`, `#[ignore]`d, needs the pack —
  `every_humanoid_armour_sheet_decodes_from_the_real_jar`: all 17 sheets decode
  out of the real jar at 64×32. Run it with
  `cargo test -p lodestone-shell --lib every_humanoid_armour_sheet -- --ignored`.

**Pixels are covered too, `#[ignore]`d**, in
`crates/lodestone-shell/tests/armour_pixels.rs`:
`a_fully_armoured_zombie_draws_more_silhouette_than_a_bare_one` drives the real
`RenderState::render` path (not a closed unit-test loop) and measures the
non-sky pixel delta between an armoured zombie and a bare negative control
against an analytically-projected lower bound derived from the chest part's
real baked vertices and `ArmourSlot::Chest::inflation()` — plus an exact
`armour_layers_drawn` count (4 on the subject, 0 on the control). Run it with
`cargo test -p lodestone-shell --test armour_pixels -- --ignored --nocapture`.
This closes what an earlier revision of this doc called "the honest gap".

**Still not covered:** dye and trims have no pixel gate, because neither
reaches a pixel yet (see "What does not" above) — a gate over an unimplemented
feature would be vacuous by construction.

## Dependencies

* `lodestone_assets::equipment` — the version-pinned data module (inflations,
  meshes, item table, texture paths).
* `lodestone_assets::entity_models::humanoid_root` — shared `pub(crate)` so the
  armour pivots are the wearer's pivots by construction.
* `lodestone_assets::entity::bake_entity_parts` — part-local quad baking.
* `lodestone_render::entity_anim::{Skeleton, AnimFamily}` — `index_of` for
  attachment, `family()` for the humanoid gate. Read only; no edits.
* `lodestone_model::event::EquipmentSlot` — the wire-side slot enum the shell
  maps onto `ArmourSlot`.
* `crates/lodestone-shell/src/entities.rs` — `EntityDraw::equipment`, read (not
  edited); it already carried all eight slots.

## See also

* [`entity-rendering.md`](./entity-rendering.md) — the mob pipeline this layers
  over.
* [`third-person-player-body.md`](./third-person-player-body.md) — the local
  player's avatar, which now wears armour (see "What draws today" above).
* [`item-prototypes.md`](./item-prototypes.md) — why `assetId` cannot be derived
  from the wire, and the `Body`-is-not-`Chest` rule on the census side.
