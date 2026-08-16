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

46 of 111 component types are consumed (drifted upward from an earlier count of 42 —
`profile`, `potion_contents`, `writable_book_content`, `written_book_content` and
`bundle_contents` were the ones this doc had lost track of; re-verified against the match
arms directly rather than carried forward). The 65 that are not are each still a truncation
point. Regenerate the list with a scan of the added-component match arms against
`lodestone_data::data_component_types::DATA_COMPONENT_TYPE_NAMES`.

Consumed, grouped by the wire shape they share — each group is one rule, not one arm per id:

| family | wire | components |
|---|---|---|
| derived NBT | one `FriendlyByteBuf.writeNbt` tag | `custom_data`, `intangible_projectile`, `map_decorations`, `debug_stick_state`, `recipes`, `lock`, `container_loot` |
| unit | **zero bytes** (`StreamCodec.unit`) | `unbreakable`, `creative_slot_lock`, `glider` |
| VarInt | one VarInt | `max_stack_size`, `max_damage`, `damage`, `rarity`, `repair_cost`, `additional_trade_cost`, `ominous_bottle_amplifier`, `enchantable`, `dye`, `base_color`, `map_post_processing`, `map_id` |
| fixed-width | `INT` / `FLOAT` / `BOOL` | `dyed_color`, `map_color`, `minimum_attack_charge`, `potion_duration_scale`, `enchantment_glint_override` |
| identifier | one UTF-8 string | `item_model`, `tooltip_style`, `note_block_sound` |
| chat component | network NBT | `custom_name`, `item_name` |
| composite | see the reader | `enchantments`, `stored_enchantments`, `tool`, `trim`, `pot_decorations`, `lore`, `custom_model_data`, `tooltip_display`, `attribute_modifiers`, `potion_contents`, `profile`, `writable_book_content`, `written_book_content`, `bundle_contents` |

`custom_name`, `damage`, `enchantments`, `dyed_color`, `trim`, `map_id`, `pot_decorations`,
`profile`, `writable_book_content`, `written_book_content` and `bundle_contents` are
**surfaced** into `ItemComponents` as-decoded; `potion_contents` is surfaced already mixed
into an opaque colour (`potion_color`), and `tool`/`max_stack_size`/`max_damage`/`equippable`
as prototype-folded **effective** values (see that type's own doc for the patch-vs-effective
split). `custom_data` is carried as an opaque byte blob. The rest are consumed for alignment
and thrown away, which is the entire point — the value is worthless and consuming the right
number of bytes is worth a whole packet.

### The derived-NBT family is easy to miss

A component registered with `persistent(codec)` and **no** `networkSynchronized(...)` gets
its stream codec from `DataComponentType.Builder.build`'s fallback,
`ByteBufCodecs.fromCodecWithRegistries(codec)` — which serialises through `NbtOps` and writes
the result with `FriendlyByteBuf.writeNbt`. One network-NBT tag, nameless root, no length
prefix. `minecraft:custom_data` is in this family, and reading `CustomData.STREAM_CODEC`
instead is a trap: that field is `@Deprecated` and is what `bucket_entity_data` uses, not
`custom_data`. Reading the family as a bare *compound* is also wrong — `recipes` encodes to a
list tag and the `Unit`-valued members to an empty compound.

### Deferred, highest value first

| component | cost |
|---|---|
| `can_place_on` / `can_break` | `AdventureModePredicate`: a list of `BlockPredicate`s, each with state/NBT matchers. Adventure-mode servers send them |
| `container` / `charged_projectiles` | lists of whole `ItemStack`s — recursive through this same decoder, the same shape `bundle_contents` (now modeled, see the composite row above and `read_bundle_contents`) used to occupy here |
| `food` / `consumable` / `use_cooldown` / `use_remainder` / `weapon` / `blocks_attacks` | multi-field records with nested effect lists |
| `entity_data` / `block_entity_data` / `bucket_entity_data` | `TypedEntityData`: a registry id then NBT. Cheap, but each uses a *different* registry codec |
| the 29 `*/variant`, `*/collar`, `*/color`, `salmon/size` ids | individually trivial (holder VarInt or enum VarInt) but each needs its registry checked for static vs dynamic, since a dynamic-registry `Holder` uses the inline-`0` sentinel and a static one does not. Only mob buckets and spawn eggs carry them |

## How to change it

To model a component, add an arm to the added-component `match` in `read_component_patch`
and **read its stream codec in the jar first**. Getting a width wrong is worse than leaving
the component unmodeled: an honest bail-out becomes silent misalignment, and every field
after it decodes to a plausible wrong value. The recurring widths to check:

* `ByteBufCodecs.INT` / `DOUBLE` / `FLOAT` are **fixed-width**, not VarInts.
  `dyed_color`, `map_color` and `attribute_modifiers`' amount are all in this trap.
* `ByteBufCodecs.holderRegistry` writes a **bare** id; `ByteBufCodecs.holder` writes `0` for
  an inline definition and `id + 1` otherwise. `holderSet` offsets only the **size**.
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
