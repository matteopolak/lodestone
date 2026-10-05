# Entity appearance parity

## What it is

The audit of every appearance-affecting entity field the 26.3 client renders from, and where Lodestone stands on each: decoded from the wire, carried to the ECS, and reaching pixels. It also describes the plumbing the fixes share (a `MobAppearance` block, ordered translucent layers, a self-lit eyes layer).

## How it works

Chain per field: metadata bytes, the per-class decoder in `lodestone_v26_2::packets::metadata`, `EntityMetadataUpdate`, `lodestone_ecs::ingest::apply_entity_metadata`, ECS components (`Variant`, `Tamed`, `CollarColor`, `Baby`, `Appearance`), `lodestone::entities::extract_entity_draws`, `EntityDraw`, then `RenderState::prepare_entities` and the frame passes.

- Indices are reused across unrelated classes, so every row in `packets/metadata/appearance.rs` names the `MetadataClass` it applies to. A test anchors each row to the jar-derived index dump and a control proves a drifted row is rejected. A field at its default is never sent, so absence means the vanilla default.
- Baby is read only by classes that own an age flag (`baby_index`). Piglin keeps its own index; its immunity bool at the ageable index is not a baby.
- Sheet resolution: `entity_variant_sheet_for_state` (registry and packed variants, wolf tame/angry), then `entity_appearance_sheet` (per-species ordinals, cat/frog keys, panda genes, shulker dye, bee flags). Sheets are pack files named by id; nothing is copied from Mojang data beyond ids. A missing sheet falls back to the model's own.
- Layers: `EntityDraw::overlay_sheet` (tinted collar or markings), `EntityDraw::layers` (ordered clothing layers) and `EntityDraw::eyes_sheet` all re-draw the already-resolved mesh. Eyes go through `EntityPipeline::eyes_pipeline` (`fs_main_emissive`: no diffuse, no world light, fog kept, no depth write); the others through the translucent pipeline.
- Anger (wolf, bee) is an end time compared with `WorldTime::age`, which `extract_entity_draws` reads.

## Status table

Legend: D = decoded, E = reaches ECS, R = reaches render. "Fixed" marks a gap now closed.

| Entity | Appearance inputs | D | E | R | Status |
|---|---|---|---|---|---|
| spider, cave spider | eyes layer | n/a | n/a | yes | fixed |
| enderman | eyes layer; creepy, carried block, open mouth | yes | yes | eyes only | eyes fixed; mouth and carried block remain |
| phantom | eyes layer; size | yes | yes | eyes only | eyes fixed; size scale remains |
| creaking | active flag drives eyes | yes | yes | yes | fixed |
| villager, zombie villager | biome type, profession, level | yes | yes | yes | fixed (a baby draws its baby biome layer only) |
| cat | registry breed, collar dye | yes | yes | yes | fixed |
| wolf | breed, tame, angry, collar, armour | yes | yes | yes | angry, baby and body armour fixed |
| frog | registry variant | yes | yes | yes | fixed |
| rabbit | coat ordinal, killer rabbit | yes | yes | yes | fixed |
| parrot | colour ordinal | yes | yes | yes | fixed |
| llama, trader llama | colour | yes | yes | yes | colour and carpet fixed; chest remains |
| mooshroom | red or brown | yes | yes | yes | fixed; back mushrooms remain |
| panda | main and hidden genes | yes | yes | yes | fixed; sit and sneeze poses remain |
| shulker | dye colour (16 is undyed) | yes | yes | yes | fixed |
| bee | nectar, angry | yes | yes | yes | fixed |
| pig, cow, chicken | temperature variant | yes | yes | yes | already ok; cold and warm model shapes remain |
| fox, axolotl, horse coat and markings | coat | yes | yes | yes | already ok |
| sheep | dye, sheared | yes | yes | yes | already ok |
| ageable mobs with a baby rig (zombie family, piglin, villager, pig, cow, mooshroom, chicken, sheep, wolf, cat, ocelot, horse family, llama, rabbit, fox, goat, bee, polar bear, panda, turtle, squid, dolphin, armadillo, axolotl, camel, strider, hoglin, nautilus, sniffer) | baby flag | yes (class-gated) | yes | yes | fixed: dedicated baby rig and baby sheets; see Baby rigs |
| happy ghast and other ageable mobs with no baby rig | baby flag | yes | yes | scale only | the adult mesh at the age scale |
| tropical fish | packed pattern and two colours | yes | yes | no | remaining (tinted base plus tinted pattern layer) |
| salmon, pufferfish | size variant, puff state | yes | yes | no | remaining |
| goat | screaming, left and right horn | yes | yes | no | remaining |
| snow golem | pumpkin | yes | yes | no | remaining |
| ghast, vex, wither, wither skull | shooting, charging, armour, blue skull | yes | yes | no | remaining |
| slime, magma cube, sulfur cube | size | yes | yes | no | remaining |
| strider | suffocating shiver | yes | yes | no | remaining |
| armadillo, turtle, copper golem | state, egg, weathering | yes | yes | no | remaining |
| bogged, arrow | sheared, tipped colour | yes | yes | no | remaining |
| creeper | charged aura | no | no | no | remaining |
| pig, horse, donkey, mule, skeleton and zombie horse, strider, camel, camel husk | saddle | yes (slot 7) | yes | yes | fixed; see Worn gear |
| horse family, llama, wolf | body-slot armour and carpet | yes (slot 6) | yes | yes | fixed; see Worn gear |
| nautilus, zombie nautilus | saddle, armour | yes | yes | yes | fixed; see Worn gear |
| happy ghast | harness, ropes, baby | yes | yes | yes | fixed; see Worn gear |
| drowned | outer layer | n/a | n/a | yes | fixed (item-free gear layer) |
| strider | cold (suffocating) sheet | yes | yes | yes | fixed for adult and baby |
| donkey, mule, llama | chest flag | yes | yes | yes | fixed; chests hide until flagged |
| rabbit | hop event (entity event 1) | yes | yes | yes | fixed; see `docs/keyframe-animation.md` |
| bat | roost flag | yes | yes | yes | fixed |
| frog | pose (jump, croak, tongue), water | yes | yes | yes | fixed |
| camel | pose-change stamp, dash | yes | yes | yes | fixed; the jump-cooldown head bump and the seated offset are not ported |
| armadillo | state, peek event | yes | yes | yes | fixed |
| sniffer | state | yes | yes | yes | fixed |

## Baby rigs

In 26.3 a baby is its own model, hand-proportioned and placed at scale `1.0`; the entity's age scale only sizes its hitbox, shadow and flame. Chain: the baby flag decoded per class (`baby_index`), `Baby`, `extract_entity_draws`, `EntityDraw::baby`.

- Rig: `EntityDraw::model_type_path` returns `lodestone_render::baby_model_name(type)` (the `<adult>_baby` corpus entry) and `EntityDraw::model_scale` returns `1.0` for it. Every pass that re-resolves the mesh (armour, flame, held items, heads) reads those two accessors, never `type_path` and `scale`. Types with no baby rig keep the adult mesh at `scale`.
- Sheet: the adult sheet the variant resolved to is mapped by `baby_sheet` (the name plus `_baby`; the panda puts the gene first). No variant reported means the rig's own default sheet. Markings, collars and the villager biome layer follow the same mapping; a baby villager draws only `entity/<family>/baby/<biome>`.
- Sheep wool: the baby wool is the baby body rig again, so it rides `EntityDraw::layers` with the dye tint. The adult wool mesh pass attaches only to the adult rig.
- Rig data is transcribed part for part from the client's own baby model definitions by a one-off script; the sheet sizes and every face's unwrap are checked against the real PNGs (`tests/entity/baby_models.rs`, ignored, needs the jar).
- Keyframes: the baby rabbit, camel, armadillo and fox play their own keyframe definitions through `docs/keyframe-animation.md`. Not ported: the baby axolotl (its swim, walk and idle states are chosen from render-state factors the client does not yet derive; the adult axolotl is code-driven and also not ported) and the baby wolf's sitting lean (no wolf, cat or fox draws a sitting, sleeping or crouching pose at all yet, so the baby lean needs that first).
- Not ported: the baby humanoid armour meshes (armour is the adult mesh at the draw's scale). The baby armour set has its own pivots (head 15, body 18, legs 20 with a z offset), extra parts (`waist`, `inner_body`, per-leg feet) and `humanoid_baby` equipment sheets, while armour attaches by reusing the wearer's matrices, so each baby wearer rig would need its pivots reconciled with the armour's. Do that together with a baby armour mesh set in `ArmourModelSet`.

## Worn gear

Saddle (slot 7) and body (slot 6) items are drawn as a second mesh over the animal: a rig with the animal's own part names, inflated or extended, posed by the same skeleton and textured from `textures/entity/equipment/<layer>/`. Chain: equipment packet, `Equipment` component, `extract_entity_draws` (`worn_gear`), `EntityDraw::gear`, `prepare_entities`.

- Item to layer: `lodestone_render::gear_layers(entity, slot, item)` is the table (`saddle`; `<material>_horse_armor`; `<colour>_carpet`; `wolf_armor`). Body armour comes before the saddle, as the client orders them.
- Rigs: `lodestone_assets::entity_models::gear_entries` (pig saddle at +0.5, horse and wolf armour at +0.1 and +0.2, llama carpet at +0.5 without chests, the horse saddle block and bridle, donkey and mule saddle with donkey ears, camel saddle and bridle). The strider saddle reuses the strider rig with the saddle sheet.
- Draw: each `GearOverlay` is resolved with the body's placement and animation, then joins the overlay group of its sheet and tint, so it takes the same hurt flash and light as the body.
- Dye: leather horse armour tints its base layer with the stack's dye (undyed brown when none) and draws an untinted overlay; the wolf armour's overlay draws only when dyed.
- Babies draw no gear (no gear rig is a baby rig), as in the client.
- Reins and chests: the rein lines (horse family) and reins (camel) are part of the saddle rigs and the chest boxes are part of the donkey, mule and llama rigs; `lodestone_render::hidden_parts` names the parts to collapse (the shell's `hide_parts` scales their matrices to zero) unless `EntityDraw::ridden` (the entity has a passenger, folded from the set-passengers packet) or `EntityDraw::chested` (the chest metadata flag) holds.
- Cracks: a wolf armour with damage draws `wolf_armor_cracks(remaining)` over itself, low under 0.95 of durability, medium under 0.69, high under 0.32. The durability fraction travels `EntityFacts::equipment_wear`, `RenderEquipmentWear`, `worn_gear`.
- Happy ghast: the harness is its own rig (`happy_ghast_harness`, goggles down while ridden, `_idle` goggles tipped up, plus `_baby_` pairs); ropes show while another entity is leashed to the ghast and a harness is worn (the holder set comes from every `Leashed` in `extract_entity_draws`); a worn body item scales the body part to 0.9375 through `lodestone_render::part_scales`. The baby ghast is the `happy_ghast_baby` rig (inner shell, 0.95 composite scale) and keeps its harness and ropes; every other baby draws no gear.
- Item-free layers: `lodestone_render::intrinsic_layers` (the drowned's inflated outer body, adult and baby sheets).
- Nautilus: the saddle is the upper shell block inflated by 0.2 and the armour the whole shell at 0.01, both posed in the nautilus's static rest pose (its swim keyframes are not ported, see below).
- Trader llama: the built-in blanket is `trader_llama_decor`, drawn unless a carpet replaced it, and the baby wears it too (its own baby rig, inflated by 0.2).
- Durability: a stack's `max_damage` is folded in from the item prototype by the stack decoder, so the wolf armour crack overlay works from a live packet carrying only a `damage` patch (`a_wolf_armour_stack_decodes_with_its_prototype_durability`).
- Not ported: armour trim and enchantment glint on animal gear. The pack ships trim textures only for the humanoid layer types, so a trim on animal gear has no art to draw; glint is not drawn for humanoid armour either, because the stack's foil state is not modelled.
- Gotcha: a gear sheet directory missing from `GEAR_SHEET_DIRS` is never loaded and the layer silently draws nothing. The jar test `tests/entity/gear_models.rs` (ignored) checks each rig against the real sheet.

## How to change it

- New per-species field: add a `MobAppearance` field and a row in `packets/metadata/appearance.rs` (class guard, index, serializer), then a case in `entity_appearance_sheet` or a layer function, then a wire test and a pixel gate with a control (`tests/entities/appearance_wire.rs`, `appearance_pixels.rs`).
- New sheet directory: add it to `entity_extra_sheet_dirs` or the sheet is never loaded and the layer silently draws nothing.
- A new baby rig: add its builder to `lodestone_assets::entity_models::babies` and a row in `baby_entries` (corpus name `<adult>_baby`, default sheet the adult's plus `_baby`). Keep the adult's part names: the skeleton and animation are keyed on them. Nothing else changes; `baby_model_name` discovers it from the corpus.
- Gotcha: the `extract_entity_draws` nested query is at the 16-item tuple limit; add to the existing appearance query tuple rather than a new slot.

## Configuration

None. Sheets load from the resource pack at startup; there are no flags or env vars.

## Dependencies

`lodestone-model` (`MobAppearance`), `lodestone-ecs` (`Appearance`, `WorldTime`), the v26-2 metadata decoder, `lodestone-render` (`entity_catalog`, `eyes_pipeline`, `entity.wgsl`), and `lodestone-shell` (extraction and passes). The producer side is outside this doc: the integrated server emits few of these fields, and the other protocol families' adapters do not raise `MobAppearance`.
