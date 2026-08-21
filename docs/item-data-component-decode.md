# Item data-component decoding, and the partial-stack contract

## What it is

How protocol 776 clientbound item stacks have their `DataComponentPatch` decoded, which of
26.2's 111 data-component types this build consumes, and the type-level contract a caller
must honour when a component it does not model ends a packet early. All of it lives in
`read_component_patch` / `read_item_stack` in `crates/protocol/v770/src/adapter/inventory.rs`.

`docs/armour-trim-decode.md` first recorded why skipping an unknown component is impossible;
this doc is the general case, the census, and the contract.

## Why an unmodeled component ends the packet

26.2 ships two patch codecs. `DataComponentPatch.STREAM_CODEC` writes each component's
payload **raw**, and `DELIMITED_STREAM_CODEC` length-prefixes it. Clientbound stacks use
`ItemStack.OPTIONAL_STREAM_CODEC`, built on the undelimited one; the delimited variant is
`OPTIONAL_UNTRUSTED_STREAM_CODEC`, i.e. serverbound only. So a component this build does not
recognise has no length to skip and no self-describing framing to walk.

The consequence is the thing to keep in mind: **every unmodeled component is a latent
truncation of the whole packet, not a lost field.** Modeling one is the only fix, which is
why components no vanilla server ever sends (`minecraft:max_stack_size`) are decoded anyway.

## The contract, and why it is an enum

`read_item_stack` returns

```rust
enum DecodedStack {
    Complete(Option<ItemStack>),
    Partial(Option<ItemStack>),
}
```

It used to be `struct DecodedStack { stack, complete: bool }`, and `decode_merchant_offers`
wrote `read_item_stack(reader)?.stack` — dropping the verdict and reading the offer's
remaining eight fields, and then the *next* offer, out of the interior of a component it
could not decode. A `bool` beside the value you actually want is an affordance to ignore it.
The enum has none: there is no way to reach the stack without naming which case you are in,
so adding a seventh caller forces the question at compile time.

**Do not add `fn stack(self) -> Option<ItemStack>`.** That reintroduces exactly the shape
that caused the bug.

Two mechanical backstops sit behind the type:

* **The reader is drained** on the bail-out inside `read_component_patch`. A caller that
  matches `Partial` and reads on anyway can now only raise `UnexpectedEof` — one dropped
  packet, which the client driver survives — rather than consuming payload bytes as ids and
  lengths. It also makes a trailing-bytes assertion pass instead of firing spuriously, so
  `read_trailing_item_stack` skips it for clarity rather than necessity.
* **A malformed or unmodelable payload must never end the session.** `Driver::run` in
  `lodestone-client` fails **open** on `AdapterError::Decode` and fatally on everything
  else, so the whole degradation story rests on decode errors staying decode errors. Turning
  one into a transport error is a session loss; see `docs/packet-framing.md` for a measured
  case where that happened for an unrelated reason.

## The census

109 of 111 component types are consumed (drifted upward from an earlier count of 51 — a sweep
closed a backlog of 60 unmodelled components down to 2, dispatched by what a real server was
observed sending live: `death_protection`, `block_entity_data` and `consumable` were the
proven-live three, each caught mid-session — `item="minecraft:spawner"
component="minecraft:block_entity_data"` and siblings. Re-verified by scanning the
`read_component_patch` source rather than carrying any figure forward; regenerate the list with
a scan of the added-component match arms against
`lodestone_data::data_component_types::DATA_COMPONENT_TYPE_NAMES`.

Consumed, grouped by the wire shape they share — each group is one rule, not one arm per id:

| family | wire | components |
|---|---|---|
| derived NBT | one `FriendlyByteBuf.writeNbt` tag | `custom_data`, `intangible_projectile`, `map_decorations`, `debug_stick_state`, `recipes`, `lock`, `container_loot` |
| unit | **zero bytes** (`StreamCodec.unit`) | `unbreakable`, `creative_slot_lock`, `glider` |
| VarInt | one VarInt | `max_stack_size`, `max_damage`, `damage`, `rarity`, `repair_cost`, `additional_trade_cost`, `ominous_bottle_amplifier`, `enchantable`, `dye`, `base_color`, `map_post_processing`, `map_id` |
| bare `Holder`/enum VarInt | one VarInt, no offset — either `holderRegistry` (synced-registry `Holder<T>`) or `idMapper` (`StringRepresentable` enum ordinal); indistinguishable on the wire, so one arm covers both | `damage_type` and 27 mob/fish/bucket-item fields: `villager/variant`, `wolf/variant`, `wolf/sound_variant`, `wolf/collar`, `fox/variant`, `salmon/size`, `parrot/variant`, `tropical_fish/pattern`, `tropical_fish/base_color`, `tropical_fish/pattern_color`, `mooshroom/variant`, `rabbit/variant`, `pig/variant`, `pig/sound_variant`, `cow/variant`, `cow/sound_variant`, `chicken/variant`, `chicken/sound_variant`, `zombie_nautilus/variant`, `frog/variant`, `horse/variant`, `llama/variant`, `axolotl/variant`, `cat/variant`, `cat/sound_variant`, `cat/collar`, `sheep/color`, `shulker/color` |
| fixed-width | `INT` / `FLOAT` / `BOOL` | `dyed_color`, `map_color`, `minimum_attack_charge`, `potion_duration_scale`, `enchantment_glint_override` |
| six floats | `FLOAT` × 6, no length prefix | `attack_range` |
| identifier | one UTF-8 string | `item_model`, `tooltip_style`, `note_block_sound` |
| chat component | network NBT | `custom_name`, `item_name` |
| `TypedEntityData` | registry-scoped VarInt then network NBT ([`read_typed_entity_data`]) | `entity_data`, `block_entity_data` |
| `CustomData.STREAM_CODEC` | network NBT only, no leading id (unlike plain `custom_data`, which has no `networkSynchronized` at all) | `bucket_entity_data` |
| `ByteBufCodecs.holder` (`0` inline / `id + 1` reference) | see the reader for each inline body | `instrument`, `provides_trim_material`, `jukebox_playable`, `painting/variant`, plus the pre-existing `trim`, `equippable`'s two sound fields |
| `HolderSet<T>` | [`read_holder_set`] | `damage_resistant`, `provides_banner_patterns`, plus the pre-existing `repairable` |
| `List<ConsumeEffect>` | [`read_consume_effects`] — dispatches through the 5-entry `consume_effect_type` registry | `death_protection`, and the tail of `consumable` |
| `ItemStackTemplate`, truncation-tolerant | [`read_item_stack_template_tolerant`] | `use_remainder`, `sulfur_cube_content`, each entry of `container` |
| composite | see the reader | `enchantments`, `stored_enchantments`, `tool`, `pot_decorations`, `lore`, `custom_model_data`, `tooltip_display`, `attribute_modifiers`, `potion_contents`, `profile`, `writable_book_content`, `written_book_content`, `bundle_contents`, `charged_projectiles`, `use_effects`, `food`, `consumable`, `use_cooldown`, `weapon`, `blocks_attacks`, `piercing_weapon`, `kinetic_weapon`, `swing_animation`, `suspicious_stew_effects`, `lodestone_tracker`, `firework_explosion`, `fireworks`, `container`, `block_state`, `bees`, `break_sound` |

`custom_name`, `damage`, `enchantments`, `dyed_color`, `trim`, `map_id`, `pot_decorations`,
`profile`, `writable_book_content`, `written_book_content`, `bundle_contents`,
`charged_projectiles` and `attack_range` are **surfaced** into `ItemComponents` as-decoded;
`potion_contents` is surfaced already mixed into an opaque colour (`potion_color`), and
`tool`/`max_stack_size`/`max_damage`/`equippable` as prototype-folded **effective** values
(see that type's own doc for the patch-vs-effective split). `custom_data` is carried as an
opaque byte blob. **Every component in the batch that closed the 60-unmodelled backlog is
consumed for alignment only** — none gained an `ItemComponents` field, matching the majority
of the pre-existing census. The value being worthless and consuming the right number of bytes
being worth a whole packet is the entire point; add a field only when a consumer needs one.

### The batch that closed the backlog, and the two that are left

`death_protection`, `block_entity_data` and `consumable` were caught live and modeled first;
the remaining 55 were worked through the full enumeration afterward. Four small shared readers
carry most of it rather than one arm per id: [`read_consume_effects`] (the `ConsumeEffect`
dispatch `consumable`/`death_protection` share), [`read_typed_entity_data`] (the
registry-VarInt-then-NBT shape `entity_data`/`block_entity_data`/each `bees` occupant share),
[`read_item_stack_template_tolerant`] (a nested stack that degrades the same way
`bundle_contents`/`charged_projectiles` already do, rather than the hard-failing
[`read_item_stack_template`] the advancement-icon path uses), and [`read_firework_explosion`]
(shared by the top-level `firework_explosion` component and each entry of `fireworks`' list).

`can_place_on`/`can_break` are the two still deferred, deliberately. `AdventureModePredicate`'s
`BlockPredicate` carries a `DataComponentMatchers`, whose `partial` half dispatches through a
*second*, independent registry (`data_component_predicate_type`, 15 entries) — several of which
(`container`, `bundle_contents`) embed an item/collection predicate that recurses back into
another `DataComponentMatchers`. That is not "one more component reader"; it is a
general-purpose predicate interpreter with no length prefix anywhere in the chain to fall back
on if one of its own sub-types is itself unrecognised — the same class of cliff `explode`'s
unmodelled `explosionParticle` registry ids are (see `docs/particle-catalogue.md` and
`lodestone_data::particle_types::is_simple_particle_type`, which exists for exactly that
sibling problem). `UNMODELED_COMPONENT` in `crates/protocol/v770/tests/item_components.rs` and
`crates/protocol/v770/tests/item_entity_metadata.rs` now names `minecraft:can_place_on` for
this reason — durable, not merely unfinished.

### `charged_projectiles` shares `bundle_contents`' reader, not its own

`minecraft:charged_projectiles` (a loaded crossbow's arrow(s)/firework) and
`minecraft:bundle_contents` both carry a list of whole `ItemStackTemplate`s — item id, count,
then a recursive nested `DataComponentPatch` per entry, with no length prefix on any of it.
`read_charged_projectiles` is a near-duplicate of `read_bundle_contents` rather than a shared
generic: the two differ only in their cap (1024 vs. 64, each the source codec's own declared
maximum) and in which `ItemComponents` field they write. An unmodeled component inside a
charged stack degrades the same way as inside a bundled one — it stops the list, flags the
outer stack `has_unmodeled`, and drops the rest of the packet, never a fatal error.

### `minecraft:attack_range`'s six floats are all independent, and none is length-prefixed

`AttackRange` (`world/item/component/AttackRange.java`) is a plain six-field record —
`min_reach`, `max_reach`, `min_creative_reach`, `max_creative_reach`, `hitbox_margin`,
`mob_factor`, all `f32`, in that wire order — **not** a single scalar. Stored on
`ItemComponents` as bits (`AttackRange::new`/accessor pattern, the same `f32`-is-not-`Eq`
convention `ItemTool::default_mining_speed` documents) so the struct keeps its `Eq` impl.

### `repairable` is one HolderSet; `equippable` must consume ten discarded fields

`Repairable.STREAM_CODEC` is a single `ByteBufCodecs.holderSet(Registries.ITEM)`. The item
ids are not useful to a consumer today, but the set is consumed so an apple with an explicit
repair material list does not become a packet cliff.

`Equippable.STREAM_CODEC` is, in order: an `EquipmentSlot` `idMapper`, a `Holder<SoundEvent>`,
optional equipment-asset `ResourceKey`, optional camera-overlay `Identifier`, optional
`HolderSet<EntityType>`, five booleans, and another `Holder<SoundEvent>`. Only the slot is
surfaced as the prototype-folded effective `ItemComponents::equippable`; the other ten fields
are consumed and discarded. The slot is **not an enum ordinal**: `EquipmentSlot.BY_ID` spells
wire id 5 as `OffHand`, whereas declaration ordinal 5 is `Head`. A sound holder uses the
opposite shape from a holder set: `0` introduces an inline `(Identifier, optional f32)` sound,
and a positive value is a reference id plus one. `holderSet` uses `0` for a named tag and
otherwise writes `count + 1` followed by bare ids.

### `minecraft:enchantments`' map key is a bare registry id, not a holder offset

`read_enchantments` (shared by `minecraft:enchantments` and `minecraft:stored_enchantments`)
used to read its per-entry key as `id + 1` and reject `0` as an unsupported "inline holder" —
a mis-transcription of `ByteBufCodecs.holder`'s two-shape codec (`0` = a full inline
definition, `id + 1` = a registry reference), which `Enchantment.STREAM_CODEC` does **not**
use. It is `ByteBufCodecs.holderRegistry(Registries.ENCHANTMENT)`, built on the plain
`registry()` helper: a bare id, no offset, no inline arm. The fix reads the id as-is; there is
no "inline enchantment" case to handle at all. This was found on a live `SET_EQUIPMENT`
packet: any entity wearing an item enchanted with whatever occupies registry id 0 lost its
entire equipment list, and every other enchanted item was silently decoding to the wrong
enchantment (off by one). Same trap `read_bundle_contents`' sibling functions warn about for
width, one level over — confusing which `ByteBufCodecs` *helper* a component's `Holder` uses,
not just how wide its payload is.

### The derived-NBT family is easy to miss

A component registered with `persistent(codec)` and **no** `networkSynchronized(...)` gets
its stream codec from `DataComponentType.Builder.build`'s fallback,
`ByteBufCodecs.fromCodecWithRegistries(codec)` — which serialises through `NbtOps` and writes
the result with `FriendlyByteBuf.writeNbt`. One network-NBT tag, nameless root, no length
prefix. `minecraft:custom_data` is in this family, and reading `CustomData.STREAM_CODEC`
instead is a trap: that field is `@Deprecated` and is what `bucket_entity_data` uses, not
`custom_data`. Reading the family as a bare *compound* is also wrong — `recipes` encodes to a
list tag and the `Unit`-valued members to an empty compound.

### Deferred

| component | cost |
|---|---|
| `can_place_on` / `can_break` | `AdventureModePredicate`'s `BlockPredicate` carries a `DataComponentMatchers`, whose `partial` half is a second, independently-registered predicate-type dispatch that can recurse into another `DataComponentMatchers` through an item/collection predicate. A general-purpose predicate interpreter, not one more component reader — see the "batch that closed the backlog" section above |

## How to change it

To model a component, add an arm to the added-component `match` in `read_component_patch`
and **read its stream codec in the jar first**. Getting a width wrong is worse than leaving
the component unmodeled: an honest bail-out becomes silent misalignment, and every field
after it decodes to a plausible wrong value. The recurring widths to check:

* `ByteBufCodecs.INT` / `DOUBLE` / `FLOAT` are **fixed-width**, not VarInts.
  `dyed_color`, `map_color` and `attribute_modifiers`' amount are all in this trap.
* `ByteBufCodecs.holderRegistry` writes a **bare** id; `ByteBufCodecs.holder` writes `0` for
  an inline definition and `id + 1` otherwise. `holderSet` offsets only the **size**.
  **`enchantments`/`stored_enchantments` shipped violating this exact rule** — `read_enchantments`
  read a bare `holderRegistry` id (`Enchantment.STREAM_CODEC`) as if it were the offset `holder`
  form, off-by-one on every non-zero id and a fatal "unsupported inline holder" error on `0`,
  which is an ordinary reference under the correct reading. The rule was written down correctly
  here the whole time; the arm just did not follow it. Re-check every `Holder<T>` arm against
  which helper its jar codec actually calls, not against this table from memory.
* `idMapper` is a bare VarInt with no offset and no sentinel.

Two gotchas about the tests:

* **Do not use a component you are about to model as a test's "unmodeled" stand-in.** Six
  gates named `minecraft:custom_data` and all six went green asserting the opposite of their
  intent the moment it was modeled. `UNMODELED_COMPONENT` in
  `crates/protocol/v770/tests/item_components.rs` is now one constant with a control
  (`the_unmodeled_stand_in_is_still_unmodeled`) that fails by name if it stops being true.
* **A single-item fixture cannot see a list caller that ignores the verdict.** Every gate in
  that file used a one-slot packet, which is why the merchant-offers bug survived. The
  discriminating fixture is multi-item with the unmodeled component on a middle entry, and
  pairwise-distinct item ids and counts so a transposition cannot pass.

## Configuration

None. No feature gates, no env vars.

## Dependencies

* `lodestone_data::data_component_types` — the generated id → name table (111 entries,
  regenerated by `cargo xtask gen-registries`). Never hardcode a numeric component id.
* `lodestone_data::item_prototypes` — seeds the three *effective* fields before the patch is
  read; see `docs/item-prototypes.md`.
* `lodestone_core::read_network_nbt` — the derived-NBT family and every chat component.
* `lodestone_model::ItemComponents` — the surfaced fields. `custom_data` is stored as raw
  bytes rather than a parsed `Nbt` so the struct keeps its `Eq` (NBT carries floats).
