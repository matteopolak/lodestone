# Item prototype components

## What it is

The per-item `minecraft:max_stack_size`, `minecraft:max_damage` and
`minecraft:equippable` values for protocol 776 (Minecraft 26.2) — three data
components that a clientbound item stack **never carries**, dumped from the real
26.2 server and committed as a generated table.

Companion to [`tool-mining.md`](./tool-mining.md), which does exactly this for
`minecraft:tool`. Same reason, same pattern, same `LODESTONE_REGEN=1` flow.

## Why the wire is not enough

A clientbound `ItemStack` is `(count, item registry id, DataComponentPatch)`, and
that patch is the **delta** from the item's built-in prototype component map.
Vanilla keeps all three of these components in that map, so `/give …
diamond_helmet` arrives as an *empty patch* and the client is expected to already
know them. No packet capture, at any level of effort, can supply them, because
they are never on the wire. `registries.json` carries item names and ids but no
components; `blocks.json` is block properties only.

What each one broke while it was missing (all three were, until this landed):

| component | consequence |
| --- | --- |
| `minecraft:equippable` | `ArmorSlot.mayPlace` is `owner.isEquippableInSlot(stack, slot)` (`ArmorSlot.java`) → `slot == equippable.slot() && …` (`LivingEntity.java`). With no component the only accepting slot is `MAINHAND`: **no armour was equippable by any click type.** |
| `minecraft:max_stack_size` | Every stack reported 64, so a drag distributing water buckets, eggs or shulker boxes over-filled the prediction and was corrected by the server. |
| `minecraft:max_damage` | `ItemStack.isDamageableItem` is `has(MAX_DAMAGE) && !has(UNBREAKABLE) && has(DAMAGE)` (`ItemStack.java`), which gates `isStackable` (`ItemStack.java`). Without it two identically-componented swords merged into a stack of 2. |

## How it works

### The dump

`crates/lodestone-data/oracle-java/ItemPrototypeOracle.java` boots the real 26.2
server, binds the vanilla datapack's tags, runs the data-component initializers,
and walks `BuiltInRegistries.ITEM` in **registry-id order**. One record per item:

```text
P <registryId> <itemName> <maxStackSize> <maxDamage|-> <hasDamage 0|1> <equipSlot|-> <allowedEntities -|#tag|=a,b>
```

Both bootstrap steps are load-bearing and copied from `ToolOracle.java`: tags are
datapack content (some component initializers resolve them), and in 26.2 an
item's prototype component map is baked at datapack reload rather than class-init
— `Item.components()` throws *"Components not bound yet"* until
`DATA_COMPONENT_INITIALIZERS` has been built and applied.

Committed at `crates/lodestone-data/tests/support/item_prototype_jvm.txt`
(1,537 rows, 67 KB) as the external anchor.

### Regenerating

```bash
CACHE="$(cd .cache/mc/26.2 && pwd)"
HERE="$(cd crates/lodestone-data/oracle-java && pwd)"
docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
  CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
  cp /oracle/ItemPrototypeOracle.java /work/ && javac -cp "$CP" -d /work /work/ItemPrototypeOracle.java
  java -cp "/work:$CP" ItemPrototypeOracle'
# copy stdout over tests/support/item_prototype_jvm.txt, keeping the `#` header, then:
LODESTONE_REGEN=1 cargo test -p lodestone-v770 --test item_prototypes \
    committed_table_matches_dump -- --ignored --nocapture
```

### The table and the lookup

`crates/lodestone-data/src/generated/item_prototypes.rs` is a flat
`[ItemPrototypeDef; 1537]` **indexed by network registry id** — no
de-duplication, because 1,537 entries of five small fields is ~12 KiB of rodata
and a direct index keeps the hot lookup a single bounds check.

`crates/lodestone-data/src/item_prototypes.rs` exposes:

- `prototype_by_id(i32)` — O(1), the hot path (the stack decoder already holds
  the registry id);
- `prototype(&str)` — resolves the name through `items::item_id`, one linear scan,
  deliberately reusing the existing name table rather than minting a second index
  that could drift;
- `model_prototype(&str)` — the version-free `lodestone_model::ItemPrototype`.

### The two seams it reaches consumers through

1. **`ItemComponents`' effective fields** (`crates/lodestone-model/src/item.rs`).
   `read_component_patch` in `crates/protocol/v770/src/adapter/inventory.rs` seeds
   `max_stack_size` / `max_damage` / `equippable` from the census *before* reading
   the patch, then lets the patch override. This is the route a consumer holding a
   stack should use.

   These three are **effective** fields — prototype already folded with patch —
   unlike `tool`, which is the raw `ToolPatch`. The asymmetry is deliberate:
   evaluating a tool needs the version's block tags and block-registry ids and so
   cannot happen at decode time, whereas a stack cap and an equip slot are plain
   scalars. `None` means "this adapter has no census", never a guessed default.

2. **`VersionAdapter::item_prototype(&str)`**
   (`crates/lodestone-model/src/adapter.rs`), for callers with no stack in hand —
   a creative-menu entry, a recipe output, a slot cap computed before anything is
   in the slot.

### Patch overrides that *are* decoded

`minecraft:max_stack_size` and `minecraft:max_damage` are both
`ByteBufCodecs.VAR_INT` (`DataComponents.java`), so the decoder reads
them. Not because servers send them — they essentially never do — but because
an unmodeled component halts patch decoding, which would leave the seeded
prototype value silently stale.

Removals are handled too, and the removal semantics are *not* "fall back to the
prototype": a removal clears the component to nothing, and vanilla's own fallback
with no `minecraft:max_stack_size` at all is **1**, not 64
(`ItemInstance.java`). So `/give …[!minecraft:max_stack_size]` makes an item
unstackable, and the decoder writes `Some(1)`.

## Gotchas

### 1. `EquipmentSlot::Body` is not chest armour

Vanilla's humanoid-armour gate is `eqSlot.getType() == EquipmentSlot.Type.HUMANOID_ARMOR`
(`InventoryMenu.java`), and `HUMANOID_ARMOR` covers only
`FEET`/`LEGS`/`CHEST`/`HEAD` (`EquipmentSlot.java`). `BODY` is
`ANIMAL_ARMOR` and `SADDLE` is its own type; `EquipmentSlot.isArmor()` is the
*union* of humanoid and animal armour (`EquipmentSlot.java`) and is
therefore **not** the predicate a player armour slot wants.

Folding `"body"` into `Chest` makes these placeable in a player's chestplate
slot, and the census now makes that reachable:

| item | slot | `allowedEntities` |
| --- | --- | --- |
| `minecraft:wolf_armor` | `body` | `=minecraft:wolf` |
| `minecraft:leather_horse_armor` … `diamond_horse_armor` | `body` | `#minecraft:can_wear_horse_armor` |
| `minecraft:saddle` | `saddle` | `#minecraft:can_equip_saddle` |

`animal_armour_is_body_not_chest` in `crates/lodestone-data/tests/item_prototypes.rs`
pins all of them.

### 2. Only the slot is carried, not `allowedEntities`

`ArmorSlot.mayPlace` also requires `equippable.canBeEquippedBy(entityType)`
(`Equippable.java`). `ItemPrototype` carries only *whether*
`allowedEntities` is empty, not the set. That is safe today because every
entity-restricted item in 26.2 is already in a non-humanoid slot, so the slot
check alone excludes it from a player armour slot — and
`animal_armour_is_body_not_chest` asserts exactly that over the whole dump, so a
future version that puts a restriction on a humanoid-slot item fails the test
instead of silently letting a player wear it.

The other nine `Equippable` fields (`equipSound`, `assetId`, `cameraOverlay`,
`dispensable`, `swappable`, `damageOnHurt`, `equipOnInteract`, `canBeSheared`,
`shearingSound`) are not carried at all.

### 3. `minecraft:equippable` in a patch is reported as unknown, not stale

`Equippable`'s stream codec is an eleven-field record with a
`HolderSet<EntityType>`; it is not decoded. When the patch carries it, decoding
halts (as it does for any unmodeled component) — but the decoder additionally
sets `equippable = None`, because the seeded prototype value is *known* to be
overridden. Reporting unknown beats reporting a value we can see is wrong.

### 4. `has_damage` is carried but unread

`isDamageableItem` needs `MAX_DAMAGE` *and* `DAMAGE`. In 26.2 the two agree for
all 1,537 items, which is why `ItemPrototype` exposes only `max_damage`.
`max_damage_and_damage_components_always_agree` asserts the agreement, so a
version where they diverge fails there rather than mis-answering
`isDamageableItem`.

## Evidence

`committed_table_matches_the_committed_dump` walks all 1,537 items and compares
every field through the public accessor against the committed server dump, by id
*and* by name. `dump_ids_and_names_match_the_registries_json_table` reconciles the
dump's (id, name) pairs against `items::item_name`, generated from Mojang's own
`registries.json` — two independently produced artifacts that must agree, neither
restating the other. That cross-check is what makes indexing by registry id safe;
its absence for the *block* registry is what let `block_type_name` mis-name every
block until `875f452`.

`crates/protocol/v770/tests/prototype_shape_seams.rs` covers the *seam* rather
than the table — every call bound as `&dyn VersionAdapter` first, plus
`decoded_stacks_carry_the_prototype_effective_fields`, which pushes a real
`set_cursor_item` payload with an **empty component patch** through the adapter and
asserts the resulting stack still reports `max_stack_size: Some(1)`,
`max_damage: Some(363)` and `equippable: Some(Head)`. That is the check that
distinguishes "the census exists" from "the census reaches a decoded stack".

Values hand-checked against the decompiled source and cited in the tests:
`ItemInstance.java` (the fallback is 1, not 64),
`DataComponents.java` (`COMMON_ITEM_COMPONENTS` sets 64),
`ItemStack.java` (`isStackable`/`isDamageableItem`),
`ArmorSlot.java` and `LivingEntity.java` (`mayPlace`),
`InventoryMenu.java` (the `HUMANOID_ARMOR` gate),
`EquipmentSlot.java` (slot types and serialized names),
`Equippable.java` (`canBeEquippedBy`).

## Configuration

| knob | where | effect |
| --- | --- | --- |
| `--protocol <n>` | `Config::protocol` | which version family's census is resolved |
| `live` feature | `lodestone-shell/Cargo.toml` | compiles a version family in at all |
| `LODESTONE_REGEN=1` | env var on the `#[ignore]`d `committed_table_matches_dump` | regenerates the table instead of asserting against it |

## Dependencies

- `lodestone_model::{ItemComponents, ItemPrototype, EquipmentSlot}` — the
  version-free carriers.
- `lodestone_model::VersionAdapter::item_prototype` — the only route a
  version-free consumer has to the census without naming `lodestone-v770`.
- `crates/lodestone-data/src/items.rs` — the `registries.json`-derived item name
  table, used both for the by-name lookup and as the cross-check artifact.
- `crates/lodestone-data/src/data_component_types.rs` — component type ids, for
  the patch overrides.

## Not yet consumed

`lodestone-game`'s `From<&lodestone_model::ItemStack> for ItemStack`
(`crates/lodestone-game/src/item.rs`) does **not** yet read the three new
effective fields, so `container::equippable_slot` and `ItemStack::max_stack_size`
still answer from an empty component map. Until that conversion inserts
`minecraft:equippable` (as `ComponentValue::Str(slot)`) and
`minecraft:max_stack_size` / `minecraft:max_damage` (as `ComponentValue::Int`),
this census reaches zero pixels — see CLAUDE.md on islands. The canary test
`canary_wire_stacks_carry_no_prototype_components` in
`crates/lodestone-game/src/menu.rs` still passes for the same reason, and is the
thing that should go red when the conversion lands.
