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

* **Trims — landed.** See "Trims" below. The one remaining hole is
  the *local player's own* trim in third person, which is a
  `lodestone-game` `ComponentMap` gap rather than a rendering one — that
  boundary drops `trim`, exactly as it drops the local player's dye.
* **Baby armour meshes**, **enchantment glint**, **`Body`/`Saddle` (animal)
  armour**, **elytra**, **skull/pumpkin heads**. Each is a different vanilla
  layer with its own model; see "Deliberately out of scope".

**A stack's actual dye colour now draws** — this list previously carried it
as a gap ("nothing upstream feeds it a real value yet"); that was true when
written and closed as of `64cfdcb` (see "Dye" below for the full three-hop
chain). The undyed default (`colorWhenUndyed`) is still what draws for a
genuinely undyed piece, which is correct, not a residual gap.

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

Three hops separated this from a real value reaching it. **All three are now
closed** (`64cfdcb`) — this section previously said hop 2's `net.rs` half was
"not yet landed", which was stale by the time an unrelated pass re-checked it
directly against the working tree rather than trusting the note: real dye
colours draw today, end to end.

1. **Closed.** `crates/protocol/v770/src/adapter/inventory.rs::read_component_patch` now
   decodes `minecraft:dyed_color` (registry id 44, `DyedItemColor.STREAM_CODEC`
   — a bare `ByteBufCodecs.INT`, i.e. `reader.i32()`, not a `VarInt` like every
   other scalar component here) into `ItemComponents::dyed_color: Option<u32>`.
   Hermetically tested against a hand-built wire vector
   (`container_set_slot_decodes_a_dyed_leather_helmet`,
   `crates/protocol/v770/tests/container_inventory.rs`) — the expected rgb is a
   literal chosen to prove the fixed-width read, not round-tripped through our
   own encoder.
2. **Closed.** Additive, not the wide-tuple shape originally sketched here.
   Widening `EntitySnapshot::equipment`/`EntityDraw::equipment` to a 3-tuple
   would have touched every existing call site that destructures them
   (several in `gpu.rs` alone) for no reason — nothing about item *identity*
   needed to change. Instead `EntitySnapshot::equipment_dye:
   Vec<(EquipmentSlot, u32)>` and `EntityDraw::equipment_dye` (same shape)
   ride *alongside* `equipment`; entities.rs-side plumbing
   (`RenderEquipmentDye` component, folded in `spawn_track`/`update_track`,
   read in `extract_entity_draws`) landed together with `net.rs::
   entity_snapshot` populating `equipment_dye` from `view.equipment`'s
   `ItemStack.components.dyed_color` (`net.rs`, narrowed the same way
   `equipment` itself is: a slot only carries a dye if its item is present
   *and* its `ResourceLocation` validates — a slot `equipment` dropped can
   never emit a dye for an item the renderer was never told about). Pinned by
   `entity_snapshot_carries_equipment_dye_through` (`net.rs`'s own test
   module).
3. **Closed.** `crates/lodestone-shell/src/
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
`depth_compare` from the base pipeline; **the two are now depth-identical**
(both `LessEqual`) and it survives for its label and to keep the armour
pass's requirement explicit at its own call site.

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
That was accurate when written, and it named its own missing prerequisite. A
follow-up built the gate — `lodestone-render/tests/entity_depth_coincident_pixels.rs`,
which measured the bug directly (two coincident quads, red drawn first, blue
second: the frame read `[189, 0, 0]` with blue covering **0 of 16384** pixels, so
the *first* draw was winning) — and then made the change.

Armour needed the faithful value first because leather's two layers are
**coplanar at one inflation**. Under `Less`, the `leather_overlay` pass fails the
depth test against the base at every texel and is silently invisible.
`armour_layers_drawn` counts *layers* rather than pieces so a regression here is
legible: a count that drops to one per piece means resolution broke, a count that
stays at two with no overlay on screen means depth did. The same mechanism was
costing every mob its coincident geometry one layer up, which the depth-compare
fix above resolved.

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

**Both items this section used to list are closed.** The local player's own
armour in third person landed in `22dc0ee` (see "What draws today" above),
and real dye colours landed in `64cfdcb` (see "Dye" above for the full
three-hop chain: protocol decode, `entities.rs`'s additive `equipment_dye`
plumbing, and `net.rs::entity_snapshot` populating it from
`view.equipment`'s `ItemStack.components.dyed_color` — all three closed, not
one still pending). This section previously described the dye hop as a
one-line brokered patch "not yet landed as of this writing"; a later pass
re-read `net.rs` directly rather than trusting that note and found the patch
already present. **Nothing is open here anymore** — see "Trims" below for
what remains in this doc's scope.

## Trims: the render-side capability is built; it is a named island

This section previously said trims were "designed, not landed" and listed
three blockers: the undecoded wire component, a third depth mode, and "an
atlas-stitching capability this crate does not have yet". Re-investigated
directly against `.cache/mc/26.2/client-src` and `client.jar` rather than
trusted from the old write-up — one of the three turned out to be a wrong
diagnosis, and the other two are now built. **What remains is entirely outside
`lodestone-render`/`lodestone-assets`'s ownership**, so this is filed as a
named island rather than shipped further.

### What actually needed building, corrected

The old write-up said the sprites "live in the `armor_trims` texture atlas,
so this needs an atlas stitch this crate does not do yet" — true that the
capability was missing, wrong about what it was. `client.jar` has exactly one
loose PNG per pattern per layer type (`trims/entity/humanoid/sentry.png`, 36
files total for 18 patterns × 2 layer types) — there is no per-material file
on disk to load. `assets/minecraft/atlases/armor_trims.json` is a
`minecraft:paletted_permutations` source, not a `directory` one: each base
PNG is an **eight-step greyscale index image** (verified against the real
`sentry.png` — its only four opaque colours are all members of the eight-entry
reference strip `trims/color_palettes/trim_palette.png`), and a material's
final sprite is produced *at load time* by substituting each pixel's grey for
the same-indexed colour in that material's own 8×1 palette strip
(`trims/color_palettes/iron.png`, `iron_darker.png`, …) — `PalettedPermutations.java`'s
`createPaletteMapping`/`NativeImage.mappedCopy`. So the missing capability
was never atlas *packing* (no UV sub-rects are needed — a resolved trim
sprite is a full 64×32 image, same shape as any other `ArmourLayer` texture);
it was this **palette-swap pixel generation**, which
[`crate::atlas_source`]'s own module docs already flagged as parsed-but-not-baked
("the actual palette-swap pixel generation is a bake step and is
intentionally left to the atlas-baking layer").

**Landed, in `lodestone-assets`:**

* `lodestone_assets::palette_bake::recolor_by_palette` — the pixel transform
  itself, ported byte-for-byte from `PalettedPermutations.java`'s lambda
  (alpha-`0` pixels pass through untouched *before* any lookup; a reference
  palette entry with alpha `0` can never be matched; a matched pixel's alpha
  is `pixel.alpha * target.alpha / 255`; an unmapped colour is — worked
  through the same formula — a byte-exact pass-through, not a forced-opaque
  substitution, which is easy to get backwards reading the Java quickly).
* `lodestone_assets::palette_bake::bake_paletted_permutations` — drives it
  against a real [`AtlasSource::PalettedPermutations`] and a
  [`ResourceManager`], producing one decoded `Image` per derived sprite id,
  with a report (missing textures, decode errors, per-palette errors) rather
  than a hard failure — the same softness `BannerPatternAtlas` already uses
  for its own per-sprite misses.
* `lodestone_assets::trim` — the two hand-transcribed registry tables this
  *cannot* discover from a generic atlas descriptor (`TRIM_PATTERNS`' `decal`
  flag, `TRIM_MATERIALS`' wearer-keyed override table — genuine
  `MaterialAssetGroup.java` statics, not resource files) plus `TrimAtlas`,
  which loads the real `armor_trims.json`, bakes it, and resolves
  `(pattern, material, layer type, wearer armour asset)` → sprite exactly like
  `ArmorTrim.layerAssetId`/`MaterialAssetGroup.assetId` do — the wearer-aware
  override (diamond trim darkens only on diamond armour, plain elsewhere) is
  hermetically pinned (`diamond_trim_darkens_only_on_diamond_armour`) and then
  proven against real baked pixels from the real jar (see "Gates").

**Landed, in `lodestone-render`:** `EntityPipeline::trim_decal_pipeline`
(`entity_pipeline.rs`) — the genuine third depth mode,
`CompareFunction::Equal`/`depth_write_enabled: false`, translated from
`RenderPipelines.ARMOR_DECAL_CUTOUT_NO_CULL`'s `DepthStencilState(CompareOp.EQUAL,
false)`. Unlike every other depth translation in this doc, `EQUAL` needed no
sign flip for `[0,1]` depth — equality has no "direction" the way
`GREATER_THAN_OR_EQUAL`/`LessEqual` does. Reuses `camera_layout`/
`texture_layout` exactly like `armour_pipeline`/`banner_layer_pipeline`/
`flame_pipeline` — still two bind groups, nowhere near the 4-group floor.

**A gotcha worth keeping**: `TrimPattern.decal()` selects *this* pipeline vs.
the ordinary armour one, and **every one of 26.2's 18 patterns has
`"decal": false`** (checked directly against every
`data/minecraft/trim_pattern/*.json` in `client.jar`). So `trim_decal_pipeline`
is real, tested to exist and be selectable, and currently unreachable by any
vanilla content — the fork still has to exist because `decal` is registry
data a resource pack or a future version can set, not a constant this engine
is free to assume.

### How a trim reaches the screen — all three hops, landed

**This section used to say all three of these were missing. Every claim in it
was stale by the time it was read** — most usefully the first, which named a
decoder that had already landed — so it is rewritten as the live description.

1. **The wire component.** `minecraft:trim` decodes into
   `lodestone_model::ItemComponents::trim` as `ArmorTrim { material, pattern }`,
   both bare registry paths. It is decoded rather than left unmodelled because
   the clientbound `DataComponentPatch` codec writes payloads **raw with no
   length prefix**, so an unknown component truncates the rest of the packet
   — see `docs/armour-trim-decode.md`.
2. **The carry path.** `entities::resolve_entity_facts` lifts it beside the
   dye, and it travels `EntityFacts::equipment_trim` →
   `RenderEquipmentTrim` → `EntityDraw::equipment_trim`. A *third* component
   beside `RenderEquipment`/`RenderEquipmentDye` rather than a wider tuple,
   because a piece can be dyed **and** trimmed at once, and the two reach the
   GPU by different routes.
   (`net::entity_snapshot` is gone — a later change deleted it; every
   reference to it in an older revision of this doc is stale.)
3. **The draw.** `gpu::entities::load_trim_sprites` bakes every trim sprite out
   of the jar at startup into `EntityRenderer::trim_textures`, keyed by
   `trim_sprite_id`'s `ResourceLocation`. `prepare_armour` appends one batch per
   `(slot, trim sprite)` **after** that slot's own armour layers, and `frame.rs`
   binds it at group 1.

Three things about (3) are easy to get wrong:

* **A trim is a texture, not a tint**, so it cannot ride an instance row the way
  the dye does. That is why `ArmourDrawBatch::texture` is an `ArmourTextureKey`
  enum rather than the old `(&str, ArmourLayerType)` tuple.
* **Order is load-bearing.** The trim batch must follow its slot's layers or the
  coplanar `LessEqual` depth compare rejects it. `accum` is an insertion-ordered
  `Vec`, never a `HashMap`, which is what makes that free.
* **It draws through `armour_pipeline`, not `trim_decal_pipeline`.** All eighteen
  of 26.2's trim patterns are `decal: false`; the decal pipeline is the
  `decal: true` variant (depth `Equal`, no write) and stays selectable and
  unused. Reading its name as "the pipeline trims use" is the trap.
* **The trim instances are untinted white.** The sprite is already the
  material's colour (`TrimAtlas` palette-swaps it), so applying the slot's dye
  would tint gold trim green on dyed leather.

**Still open within trim scope:** the local player's own trim in third person.
`ThirdPersonBodyState` reads the inventory through `lodestone_game`'s
`ComponentMap`, whose `From<&lodestone_model::ItemStack>` sets `trim: None` —
the same boundary that drops the local player's dye. One shared fix, in a crate
outside this doc's subject.

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

**Still not covered:** trims have no pixel gate, because the feature itself
does not reach a pixel yet (see "Trims" above) — a gate over an
unimplemented feature would be vacuous by construction. **Dye is different
from this note's previous wording**: dye now reaches real pixels (see "Dye"
above), and has hermetic coverage
(`entity_snapshot_carries_equipment_dye_through` in `net.rs`, plus
`armour_layer_tint_with_dye`'s own tests in `lodestone-render`'s `entity`
module) but no dedicated *pixel* gate proving a dyed piece renders a
different colour than an undyed one through the real GPU path — filed as a
follow-up rather than built here, since `armour_pixels.rs` (the file a dye
pixel gate would naturally extend) is another agent's in-flight work.

**Trims' own capability is covered, up to the island boundary.** Hermetic,
`lodestone-assets`:

* `palette_bake` module (6 tests) — the recolour transform itself: a
  referenced grey maps to the same-indexed target colour; a fully-transparent
  pixel passes through unchanged even when its RGB would otherwise match; a
  reference entry with zero alpha can never be matched; an unmapped colour is
  a byte-exact pass-through, not forced opaque; alpha scales by the target's
  own alpha (integer division, matching Java); a later duplicate reference
  entry wins the lookup (the `HashMap` overwrite vanilla's own code produces).
* `trim` module (7 tests) — the 18-pattern/11-material table closures, every
  pattern's `decal` flag being `false` in 26.2, the five overriding materials
  each overriding only their own armour and no other, the six non-overriding
  materials never changing suffix, `trim_sprite_id` matching
  `ArmorTrim.layerAssetId` for both a plain and an overridden suffix and both
  layer types, and a missing-descriptor pack reporting an error rather than
  panicking.

Jar-backed, `#[ignore]`d, `crates/lodestone-assets/tests/trim_atlas_gate.rs`
(`cargo test -p lodestone-assets --test trim_atlas_gate -- --ignored --nocapture`):

* `every_trim_sprite_bakes_cleanly_against_the_real_jar` — all 576 sprites (18
  patterns × 16 suffixes × 2 layer types) bake with zero missing textures,
  decode errors, or palette errors against a real `client.jar`.
* `a_hand_verified_pixel_recolours_to_irons_own_first_palette_entry` — a
  specific pixel (`sentry.png`'s `(11, 0)`, independently confirmed to be the
  reference palette's index-0 grey) recolours to iron's own index-0 colour,
  read directly off the real PNGs (not round-tripped through this crate's own
  encoder — the expected bytes came from decoding the jar's palette strips
  with Pillow, outside this codebase, while investigating the feature).
* `the_same_pixel_differs_between_the_overridden_and_plain_suffix` — the
  wearer-aware override end to end: the identical pixel differs between a
  diamond-worn (plain `iron`) and an iron-worn (`iron_darker`) resolution,
  with both exact byte values asserted, not merely that they differ.
* `background_pixels_outside_the_pattern_stay_fully_transparent` — a control
  that the palette swap does not accidentally paint the transparent
  background opaque.

No pipeline-level test exists for `trim_decal_pipeline` itself, matching this
file's existing convention: `armour_pipeline`/`banner_layer_pipeline`/
`flame_pipeline` have none either (pipeline construction needs a real
`wgpu::Device`; their correctness is proven downstream by the `#[ignore]`d GPU
pixel gates above, which this pipeline cannot reach yet — see "Trims").

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
* `lodestone_assets::trim` — `TrimPattern`/`TrimMaterial`/`TRIM_PATTERNS`/
  `TRIM_MATERIALS`/`TrimAtlas`/`trim_sprite_id`, the registry tables and
  atlas wrapper this pass added for trims.
* `lodestone_assets::palette_bake` — `recolor_by_palette`/
  `bake_paletted_permutations`, the generic `paletted_permutations` bake step
  `trim` is built on; also the one [`crate::atlas_source`] flagged as
  parsed-but-not-baked before this pass.
* `lodestone_render::entity_pipeline::EntityPipeline::trim_decal_pipeline` —
  the third depth mode for a `decal: true` trim pattern.

## See also

* [`entity-rendering.md`](./entity-rendering.md) — the mob pipeline this layers
  over.
* [`third-person-player-body.md`](./third-person-player-body.md) — the local
  player's avatar, which now wears armour (see "What draws today" above).
* [`item-prototypes.md`](./item-prototypes.md) — why `assetId` cannot be derived
  from the wire, and the `Body`-is-not-`Chest` rule on the census side.
