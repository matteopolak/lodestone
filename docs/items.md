# Item model, components and rendering

## What it is

The item stack model end to end: the two `ItemStack` types (wire/model vs.
game-side inventory) and the plugin read/write surface over them, how a 26.2
clientbound stack's data-component patch is decoded, the per-item prototype
census that fills in components vanilla omits from the wire, how one item
resolves to several baked geometries (`ItemVariants`), custom (plugin-defined)
items, armour trim, goat horns, the portable clock crate, and the
entity-metadata field a dropped item's identity rides on.

## How it works

### Two `ItemStack` types, one lowering

`lodestone_model::ItemStack` (all-`pub`, a closed struct of nine typed
component fields) is what decode produces and what `Equipment`/`DisplayItem`
carry. `lodestone_game::item::ItemStack` (private fields, an opaque
`BTreeMap<Identifier, ComponentValue>`) is what every container/HUD path
holds. Typed accessor pairs funnel through one private `write_component`, so
a plugin-built stack and a decoded one compare equal and merge. The lowering
(`impl From<&game::ItemStack> for lodestone_model::ItemStack`) follows two
rules: **clearing removes, never zeroes** (an empty component list deletes
rather than stores empty — two otherwise-identical stacks must still merge),
and **`ToolPatch::Inherited` is not a value** — setting it removes the
component, an absent component reads back as `Inherited`. Getting that
backwards makes every pickaxe mine at fist speed.
`lodestone_model::EquipmentSlot` and `lodestone_game::container::EquipmentSlot`
are distinct same-named types; the lowering resolves by name
(`EquipmentSlot::from_name`), not through `container::equippable_slot`.

Known gaps: `has_unmodeled` never crosses into `lodestone-game`, so a lowered
stack cannot say its component set was partial. Only 8 of 111 data components
are modelled on the game side — an unmodelled one is a deliberate escape
hatch, not an oversight. `ComponentValue::Opaque`/`::Bool` have zero
constructors, and dropped-item entities carry no components at all
(`TrackedStack` is `{ id, count }`).

### Data-component decode, and why an unmodelled component halts the packet

26.2 ships two patch codecs: `DataComponentPatch::STREAM_CODEC` writes each
component **raw, with no length prefix**, and clientbound stacks
(`ItemStack.OPTIONAL_STREAM_CODEC`) are built on it — the length-prefixed
variant is serverbound-only, precisely so a *server* can skip a hostile
client's junk. So **the only way to stop a component being a decode cliff is
to model it** — components no vanilla server ever sends
(`max_stack_size`/`max_damage`) are decoded anyway for this reason. Decoding
returns `DecodedStack::{Complete, Partial}(Option<ItemStack>)`, never a bare
`Option` with a separate completeness flag, after a `bool`-and-value shape
let one list caller (merchant offers) ignore the flag and read the interior
of an undecoded component as the next offer's fields.

`read_component_patch` (`crates/protocol/v770/src/adapter/inventory.rs`)
covers 109 of 111 types. `can_place_on`/`can_break` are deferred deliberately:
their predicate is a second, independently-registered dispatch that can
recurse into itself with no length prefix anywhere to fall back on — a
general-purpose predicate interpreter, not one more reader. Recurring width
traps: `ByteBufCodecs.INT`/`FLOAT` are fixed-width, not VarInts;
`holderRegistry` writes a bare id while `.holder` writes `0`
(inline)/`id + 1` (reference) and `holderSet` offsets only its size —
`enchantments` shipped reading a bare `holderRegistry` id as the offset
`holder` form, off-by-one on every id and fatal on `0`; `equippable`'s eleven
fields must all be consumed even though only the slot (not an enum ordinal —
wire id 5 is `OffHand`) is kept; `custom_model_data` is four
separately-counted lists (float/bool/string/colour), not a legacy integer;
`attack_range` is six independent unprefixed floats; and the derived-NBT
family (`custom_data`, `recipes`, `lock`, …) has no length prefix at all, so
reading it as a bare compound is wrong for `recipes` (a list tag) as often as
right elsewhere. Test gotcha: never use a component about to be modelled as a
test's "unmodelled" stand-in, and a single-item fixture cannot see a list
caller that ignores the decode verdict.

### Item prototypes: what the wire omits

A clientbound stack's patch is a **delta** from the item's built-in prototype
map, and vanilla keeps `max_stack_size`, `max_damage` and `equippable` in
that map — so `/give … diamond_helmet` is an empty patch. Missing, these
broke armour equip slots (only `MAINHAND` accepted anything), stack-size
prediction (everything read 64), and stacking (two damaged swords merged). A
1,537-row table dumped from the real 26.2 server
(`ItemPrototypeOracle.java` → `crates/lodestone-data/tests/support/item_prototype_jvm.txt`,
regenerated with `LODESTONE_REGEN=1 cargo test -p lodestone-v770 --test
item_prototypes committed_table_matches_dump -- --ignored --nocapture`) is
indexed by registry id and exposed via `prototype_by_id`/`prototype`/
`VersionAdapter::item_prototype`. `read_component_patch` seeds the three
effective fields from this census before the patch, and a **removal** falls
back to vanilla's real default of `1`, not 64.

Gotchas: `EquipmentSlot::Body` is **not** chest armour — humanoid armour is
`feet/legs/chest/head` only, while `Body` is animal armour (wolf/horse
armour, saddles). Only the equip *slot* is carried from a patch's
`equippable`, never `allowedEntities` — safe today because every
entity-restricted item already sits in a non-humanoid slot. This census is
not yet consumed by `lodestone-game`'s own stack lowering, so
`container::equippable_slot` and stack caps still answer from an empty
component map downstream of the model boundary.

### Item variants: one item, several baked models

A stack's `minecraft:item_model` selects a **selector tree**
(`condition`/`select`/`range_dispatch`/`composite`) whose leaves name concrete
models — a bow is `item/bow` at rest and `item/bow_pulling_0/1/2` drawing; a
spyglass is a flat sprite in a slot and a 3-D tube in hand. The resolver
(`lodestone_assets::item_model`) was always complete; the bug was one layer
down — `BlockModels::build` baked each definition **once**, against a static
GUI-only context, so 84 items with more than one reachable model flattened to
their inventory form (wrong in-hand geometry *and* pose). `ItemVariants`
bakes every model an item's tree can reach at load time and resolves against
live state per draw (`ItemVariants::resolve(&ItemStateContext)`), falling
back to the inventory form.

`ItemStateContext` sources only what the shell has: display context,
`using_item`/`use_duration`/`crossbow/pull` off `ItemUse` state, and index 0
of `custom_model_data`'s float list. Anything needing unmodelled per-stack
data (`trim_material`, `damage`, `count`, …) reads as unset, routing to the
item's default appearance. The one trap in the family: **`use_duration`
counts up** (fed directly from `ItemUse::ticks`, no inversion) while
**`use_cycle` counts down** (needs a per-item duration this crate does not
model) — a symmetric "obvious" inversion pins a drawn bow at full draw
forever. `item_model` selection also gates which stack an equipment producer
resolves *before* it becomes a `ResourceLocation` — get this wrong and the
in-world hand can show the vanilla item while GUI/first-person show a pack's
replacement.

### Custom (plugin-defined) items

The wire carries an item as a registry **index**, so a genuinely novel item
id has nowhere to live. `CustomItem` names a real vanilla **base item** it is
made of on the wire, plus an identity tag (`lodestone:item_id`, deliberately
outside `minecraft:` so a real server never tries to resolve it and a future
decoder never mistakes it for a real component). `CustomItem::validate`
enforces both directions: the custom id must **not** be `minecraft:`, the
base item **must** be. `identify` is a pure function of the stack's own
components — no slot, no side table — because a stack that round-trips to a
real server and back must be recognisable from itself alone. `CustomItems` is
a shared ECS resource so one plugin can ask whether a stack belongs to
another. Known gap: the identity tag does not survive a game → model round
trip (the model's `ItemComponents` has no unmodelled-component slot), so a
custom item's identity is lost crossing a real server; anything
vanilla-shaped on it survives regardless.

### Armour trim decode, and the components modelled for the same reason

`minecraft:trim` decodes as two holders (`0` = inline definition, positive =
registry reference `- 1`) — both forms must be read even though only the
inline path is exercised, or the byte count desyncs everything after.
`TRIM_MATERIAL_IDS`/`TRIM_PATTERN_IDS` are vanilla's bootstrap order, since
the trim registries are *dynamic* and synced only during Configuration, which
this client does not store — exact for vanilla, provisional for a modded
server. Do not read these from `lodestone_assets::trim`'s own tables: one
happens to be registry-ordered, the other alphabetical, and that agreement is
coincidence. `minecraft:map_id` and `minecraft:pot_decorations` were modelled
for the identical reason as `trim` — not because their render exists, but
because leaving them unmodelled truncated the packet they rode in (a filled
map in any inventory; a decorated-pot advancement icon inside
`update_advancements`, whose `ItemStackTemplate` turns an incomplete patch
into a *fatal* error on world join).

### Goat horns

`Goat.finalizeSpawn`'s pre-broken-horn roll (10% chance, then a coin flip)
happens once at spawn (`goat_horn_spawn_roll`, `MobSim::spawn_species`),
carried on `SimMob::has_left_horn`/`has_right_horn` and pushed unconditionally
into `MetadataField::GoatHorns`, encoded as two booleans at wire indices
19/20. A horn never breaks mid-game — vanilla's ram-into-a-tagged-block
trigger has no block-state read in this crate's Brain seam — so the field is
fully wired but nothing after spawn flips it. The screaming-goat flag (index
18) is a separate, still-unwired field.

### The portable clock (`lodestone-time`)

The one sanctioned way to read a clock in this workspace. It wraps
`web-time`'s `Instant`/`epoch_duration()` rather than `std::time::Instant`/
`SystemTime`, because both of the latter **compile** for
`wasm32-unknown-unknown` and **panic at runtime** — a tab-killing crash under
this workspace's `panic = "abort"` release profile, invisible to `cargo
check`. On native, `lodestone_time::Instant` *is* `std::time::Instant` (not a
newtype); `Duration` is not wrapped at all. Every dependent crate must go
through `lodestone-time`, never `web_time`/`std::time` directly —
`scripts/wasm-check.sh` bans the raw paths per-crate, with a short exception
list for crates whose only clock call sites are already structurally confined
off the wasm build (a `#[cfg(not(target_arch = "wasm32"))]` module, a
`#[cfg(test)]` block, a dev-dependency-only crate).

### The item metadata field, and dropped-item identity

A dropped item (`minecraft:item`) carries its entire visible identity in one
entity-metadata field (index 8, `ITEM_STACK` serializer) — its spawn packet
carries no item id at all. Decoding it calls the same
`read_item_stack`/`read_component_patch` the container path uses. One
asymmetry: an unmodelled component still ends that *packet*, but metadata is
a stream of indexed fields terminated by a `0xFF` sentinel with no way to
resume mid-stream once desynced, so decode **abandons the rest of the field
list** rather than erroring — the caller must never turn that into a dropped
packet, which would throw away an item identity already decoded exactly. An
undecodable item must always produce a partial or absent stack, never a
propagated error: this path once failed *closed*, and because the driver
treats a decode error as fatal, equipping any component-bearing tool ended
the whole session. `EntityMetadataUpdate.item` is nested
`Option<Option<ItemStack>>` (outer: field present in this update; inner: is a
stack set) and flattens the two `None`s into "draw nothing" only at the
interpolator — every layer before it must treat "field absent" as "leave the
last value alone."

### Filled maps and advancements — the wire half

Two more clientbound decode gaps sharing the "field order is not the obvious
one" trap: `map_item_data` (a dirty-rectangle patch — width, height, startX,
startY, in that order, "absent" spelled as a zero-width byte with no leading
bool) and `update_advancements` (`DisplayInfo`'s flag word is a raw
big-endian `int` with three live bits; `AdvancementType` ordinals are `TASK,
CHALLENGE, GOAL` — reading them task/goal/challenge swaps the two rarest
frames). Both fold into **session** state (`SessionMaps`/
`SessionAdvancements`), not per-entity state — a map can be held by several
players at once, and the advancement tree is the local player's own.
`encode_update_advancements` always writes `DisplayInfo` absent, since
`lodestone-server`'s advancement model carries no presentation.

## How to change it

- Adding a data component: read its stream codec in the jar first, add the
  arm to `read_component_patch`, and extend the whole-struct round-trip test
  rather than writing a per-field one — a whole-struct lowering can drop a
  neighbour a narrower test cannot see.
- Adding an item-model property: teach `ItemStateContext` to answer it and
  drop it from the unsourced-property roster; nothing in the baking pass
  changes, since every variant bakes regardless of what selects it.
- Adding a custom-item field: touch `CustomItem::apply_to` and its round-trip
  test together, or a definition silently drops the field.
- A new item-variant draw site: resolve through `ItemVariants::resolve`, not
  `BlockModels::item` (the inventory-only accessor).

## Configuration

`--protocol <n>` (`Config::protocol`) selects which family's census
(prototypes, trim tables) is resolved; the `live` feature compiles a family
into the registry at all. `LODESTONE_REGEN=1` on the relevant `#[ignore]`d
test regenerates a committed table from a fresh JVM dump.

## Dependencies

`lodestone-model` for the wire vocabulary (`ItemStack`, `ItemComponents`,
`ToolPatch`, `ArmorTrim`, `EquipmentSlot`); `lodestone-data` for every
generated census (`item_prototypes`, `data_component_types`, `items`);
`lodestone-assets` for `item_model`/`icon`/`bake`; `lodestone-ecs::entity::ItemUse`
for local held-item use state; `web-time` (the sole dependency of
`lodestone-time`). No component-decode path names a protocol version outside
`crates/protocol/`.
