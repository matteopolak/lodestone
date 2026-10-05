# Item model, components and rendering

## What it is

The item stack model end to end: the two `ItemStack` types (wire/model vs.
game-side inventory) and the plugin read/write surface over them, how release-selected
clientbound stacks and data-component patches are decoded, the per-item prototype
census that fills in components vanilla omits from the wire, how one item
resolves to several baked geometries (`ItemVariants`), custom (plugin-defined)
items, armour trim, goat horns, the portable clock crate, and the
entity-metadata field a dropped item's identity rides on.

## How it works

### Two `ItemStack` types, one lowering

`lodestone_model::ItemStack` (all-`pub`, with a closed typed component struct)
is what decode produces and what `Equipment`/`DisplayItem`
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

Server inventory pickups use the same effective `max_stack_size` lookup as
container clicks. The per-item prototype census supplies built-in caps, while
an explicitly modeled stack component overrides them; this keeps entity pickup
from creating impossible stacks for tools, buckets, eggs, or plugin-authored
caps. A remainder stays on the item entity when the destination slot is full.

`ComponentValue::Release` carries a boxed `ItemReleaseComponents` under the
internal `lodestone:release_components` key only when its typed values are
non-default. It preserves animations, providers, fuel/compost values, visibility,
sign faces, wax, cushion colour, full nested pot templates, instruments, and
consume/death effects through the production model → game → model conversion.
`ItemStack::release_components` and `set_release_components` expose the carrier;
it participates in stack equality and disables empty-patch click prediction.
Ordinary stacks gain no carrier allocation or extra component key.

Known gaps: `has_unmodeled` never crosses into `lodestone-game`, so a lowered
stack cannot say its component set was partial. Several older model fields,
including custom data, repair cost, charged projectiles and attack range, have
no game-side slot and retain their established lowering defaults. A retained
typed value does not itself implement rendering or gameplay behavior.

### Data-component decode, and why an unmodelled component halts the packet

The trusted clientbound patch codec writes each
component **raw, with no length prefix** — the length-prefixed
variant is serverbound-only, precisely so a *server* can skip a hostile
client's junk. So **the only way to stop a component being a decode cliff is
to model it** — components no vanilla server ever sends
(`max_stack_size`/`max_damage`) are decoded anyway for this reason. Decoding
returns `DecodedStack::{Complete, Partial}(Option<ItemStack>)`, never a bare
`Option` with a separate completeness flag, after a `bool`-and-value shape
let one list caller (merchant offers) ignore the flag and read the interior
of an undecoded component as the next offer's fields.

`read_component_patch` in the shared compatibility adapter
covers the 26.2 component bodies and the 13 added 26.3 identities.
`can_place_on`/`can_break` are deferred deliberately:
their predicate is a second, independently-registered dispatch that can
recurse into itself with no length prefix anywhere to fall back on — a
general-purpose predicate interpreter, not one more reader. Recurring width
traps: integer and float scalars can be fixed-width rather than VarInts;
a bare registry reference differs from the `0`
(inline)/`id + 1` (reference) holder shape, and a holder set offsets only its size —
an enchantment map key uses the bare form. `equippable`'s eleven
fields must all be consumed even though only the slot (not an enum ordinal —
wire id 5 is `OffHand`) is kept; `custom_model_data` is four
separately-counted lists (float/bool/string/colour), not a legacy integer;
`attack_range` is six independent unprefixed floats; and the derived-NBT
family (`custom_data`, `recipes`, `lock`, …) has no length prefix at all, so
reading it as a bare compound is wrong for `recipes` (a list tag) as often as
right elsewhere. Test gotcha: never use a component about to be modelled as a
test's "unmodelled" stand-in, and a single-item fixture cannot see a list
caller that ignores the decode verdict.

The generated `minecraft:data_component_type` census has its own narrow
built-in boundary: `lodestone_data::data_component_types::DataComponentTypeId`.
The canonical census keeps the 111-entry 26.2 prefix and appends 13 names,
including the two identities removed from 26.3's 122-entry wire registry.
`StackCodecContext` translates each added or removed wire ID through the selected
`ProtocolDialect` before resolving its canonical component name. The 26.2 wire
boundary remains 111 even though the canonical type accepts 124 identities.
Unknown fixed IDs fail explicitly. A known unmodeled 26.2 payload can still
produce a partial stack; an unmodeled 26.3 payload returns `Unsupported` so
incomplete decoding cannot count as reviewed Play support.

### One context for stacks and synchronized holders

`StackCodecContext` carries the selected dialect and a borrowed
`ClientRegistries` snapshot. Inventory packets hold that snapshot once and pass
the context through stack patches, recipes, merchant results, and nested item
templates. Metadata and particle readers use the same context-bearing entry
points. The fixed `read_item_stack` wrapper is specifically for 26.2 callers;
it cannot resolve a synchronized holder without an explicit registry view.

An ordinary optional stack starts with count, then item ID and patch. A template
starts with item ID, then count and patch. Both translate the selected release's
item ID before looking up its name or prototype. Tool block sets, repair item
sets, entity restrictions, attributes, sound references, menus, statistics, and
map decorations also translate their fixed registry IDs at their boundaries.

Trim materials, trim patterns, banner patterns, instruments, block transformers,
and pottery patterns resolve against the Configuration entry order. There are
no assumed alphabetic trim or banner tables. A missing snapshot or out-of-range
holder fails instead of selecting a different decoration. An inline 26.3 trim
material retains its palette identifier and description; the 26.2 inline form
retains its asset suffix and per-armour overrides.

The 26.3 component model retains attack/interact animation kind and duration,
transformer and pottery keys, villager nutrition, fuel/compost constants or named
context providers, targeting-entity visibility, both four-line sign faces and
their filtered alternatives, wax, and cushion colour. Pot decorations retain
four optional full templates in back/left/right/front order. Inline instruments
retain sound, duration, range, durability damage, and description. Random
teleport effects retain the directional-particle flag; the 26.2 body supplies
`false` because it carries no flag.

Advancement entries in 26.3 carry coordinates after their holder body, including
entries without a display. `AdvancementEntry::position` preserves their float
bits and the decoder also updates display coordinates when present. The 26.2
reader consumes coordinates within the optional display as before.

To extend a component, change `inventory/components_26_3.rs` or the shared patch
reader and the corresponding `lodestone_model::item` carrier. Add an independent
byte fixture with asymmetric IDs and values and require an empty reader at the
end. The context must follow any new nested path; it must never be rebuilt from
an inferred default adapter. Extend `ItemReleaseComponents` and the game-side
conversion together when a new field lacks its own `ComponentValue` slot.
Component retention and lowering do not establish rendering, gameplay execution,
or outbound writer support; each consumer remains a separate integration gate.

The hosted server's stack encoder writes every component the server itself puts
on a stack: custom data, damage, enchantments, custom name, lore, dyed colour,
repair cost, potion contents, a referenced instrument and the two book
components. Each component's type id goes through the connection's release.
Enchantment and instrument ids are holder positions in the registry that release
sends, so one the registry lacks is left out rather than sent under another id.
An enchanted book's list travels as `minecraft:stored_enchantments`, and the
client decoder puts that back into the same `enchantments` field. Custom data is
written only when its bytes are one complete compound-root network-NBT value, so a
malformed value cannot swallow the next component. An unstyled literal name or
lore line is a bare NBT string at the root (`Text::to_nbt`); inside `with`/`extra`
lists every element stays a compound, because list elements share one tag type.

The witness is `crates/versions/26.3/tests/item_components_wire.rs`. It compares
our bytes with `fixtures/item_components_26_3.json`, which
`capture_item_components.py` records by having the official 26.3 server `give`
one single-component stack per case. Compound keys are compared as a set, because
the reference server writes them in hash order. Prototype-derived fields
(`max_damage`, `equippable`, …) are the item's defaults on both sides and are
never sent.

### Item nesting is sender-chosen, so the decoders bound it

Container-shaped components hold item stacks, and a contained stack declares its
own component patch, so the wire structure is a cycle: patch → contained stack →
patch. Nothing on the wire closes it. There is no length prefix and no declared
level count anywhere in the chain, so the nesting depth is whatever the sender
wrote — which made one crafted stack from any server a player joined enough to
exhaust the decoding thread's stack and abort the process, on the headless path
as much as the playable one.

Four routes reach the cycle in the 26.2 decoder, and every one of them passes
through `read_component_patch`: `minecraft:container` and
`minecraft:use_remainder` (and `sulfur_cube_content`) via
`read_item_stack_template_tolerant`, `minecraft:bundle_contents` and
`minecraft:charged_projectiles` via their own per-entry readers. The bound
therefore lives in one place — a `Depth` budget entered at the top of
`read_component_patch`, not incremented at the call sites that descend — so a
nesting component added to that match inherits it without its author having to
remember anything. `read_slot_display`, the recipe-display walk, enters the same
budget and shares it, since a display can contain a stack and a stack's
component can contain a display. `Depth` has no arithmetic and no constructor
from a number: `Depth::ROOT` and the checked descent are the only ways to get
one.

The cap is **19**, and it is the deepest nesting the game itself will construct,
summed per route:

| term | where it comes from |
|---|---|
| 16 | bundle-in-bundle wraps: a nested bundle costs a flat 1/16 of a bundle's weight budget of 1, so the *n*-th in a chain weighs `(n-1)/16` and the 17th insert is refused. The only route that nests repeatedly. |
| +1 | the one container-item level able to hold such a chain — a container item refuses to hold *another* container item (the fit-inside-a-container-item rule is false for a shulker-box block item), so this level cannot repeat. |
| +1 | a stack named by a prototype component (a use remainder, a sulfur cube's content) enclosing the whole thing. |

A payload deeper than that is not a large inventory; it is one no server
following the game's own rules can produce, and refusing it costs a packet.

**The cap has to be reachable, and that constrains how generous it can be.**
A cap the decoder overflows *before* hitting is a crash behind an accepted input
rather than a bound, so the `nesting_budget` gates decode at exactly the cap as
well as one past it. `ItemComponents` is over 1.7 KB and the thread that decodes
packets gets the platform default stack of 2 MiB, so holding it as a by-value
local cost a copy per match arm and put the measured survivable depth at
**19 levels**;
boxing it inside `read_component_patch` and dropping the `ItemStack` that
`read_item_stack_template_tolerant` built and every caller discarded moved that
to **48 levels and not 64**. That is why the borrowed ceiling here is *not* the
512-level serialized-structure limit the NBT reader enforces: NBT frames are a
rounding error beside these, and a cap of 512 would have been unreachable in
every build.

The same cycle exists in the 1.20.6-era slot decoder (`Slot::decode` →
`read_component_payload` → `skip_component_payload` → `Slot::decode`, reached by
that era's three list-of-nested-stacks payloads) and is bounded the same way,
with the check at `decode_nested`'s top. Its frames are small enough that
reachability was never in question.

Regression coverage lives in `fuzz/seeds/v26_2_clientbound_decode/`: the
original crash input plus a pair at the cap and one past it, so `fuzz/smoke.sh`
replays all three on every push. A clean fuzz *run* is evidence about that run's
mutation path and not about the code — the committed seeds are the durable half.

### Consuming a component's bytes is not the same as keeping its value

A component arm has two independent jobs: advance the reader by exactly the
right number of bytes, and put the value somewhere. Only the first has a
failure mode any instrument can see. Get the width wrong and the rest of the
packet decodes as garbage; read the width correctly and drop the value, and the
packet still decodes, still emits, and still scores fully connected under
`cargo xtask connectedness` — a round trip cannot see it either, because both
halves agree about a field neither retains.

So the rule for a new arm is: **either the value lands in `ItemComponents`, or
the arm says at the site why dropping it is correct**. There is no third state,
and "no consumer yet" is not the second one — a field with no consumer is
cheap, and a value silently dropped at decode time cannot be recovered by a
consumer added later.

The components whose value is retained beyond what a renderer reads:

| component | field | shape note |
|---|---|---|
| `minecraft:repairable` | `repairable_items` | a `RegistrySet` |
| `minecraft:equippable` | `equippable_allowed_entities` | a `RegistrySet`, patch-shaped (unlike the effective `equippable` slot) |
| `minecraft:damage_resistant` | `damage_resistant` | a `RegistrySet` |
| `minecraft:blocks_attacks` | `blocks_attacks` | `BlocksAttacks`, floats as raw bits behind accessors |
| `minecraft:provides_banner_patterns` | `provides_banner_patterns` | a `RegistrySet` |
| `minecraft:consumable` | `consume_effects` | `Vec<ConsumeEffect>`; the component's animation/sound/timing fields are alignment-only |
| `minecraft:death_protection` | `death_protection_effects` | `Vec<ConsumeEffect>` |
| `minecraft:trim` (inline form) | `ArmorTrim`'s four inline-only fields | descriptions, per-armour asset overrides, decal flag |

`RegistrySet` is the one shape worth understanding before touching any of them.
Every registry set on this wire is a single leading VarInt: `0` then a tag
name, or `n` then `n - 1` bare registry ids. **The tag name is part of the
value, not framing.** A tag's membership is server-side data that never reaches
the client, so a decoder that reduced the tag arm to its (empty) id list turns
"every item in `#minecraft:planks`" into "no item at all" — and the tag arm is
what a real server sends for vanilla's own repair materials, saddle-equippable
entities and banner-pattern unlocks, so it is the common case rather than the
exotic one. `RecipePropertySetsUpdated`'s stonecutter ingredient is the one
place still narrowed to explicit ids; widening it reaches consumers outside the
version crate.

Two things are still consumed for alignment on purpose, and both say so at
their site: a **sound reference** (an inline definition or a session-scoped
registry id — nothing here plays a sound sourced from an item component), and a
mob-effect instance's nested **hidden effect** (the weaker effect to restore
when a stronger one expires — holder-side bookkeeping whose own record omits
the effect id, so `MobEffectInstance` cannot represent it).

### Item prototypes: what the wire omits

A clientbound stack's patch is a **delta** from the item's built-in prototype
map, and vanilla keeps `max_stack_size`, `max_damage` and `equippable` in
that map — so `/give … diamond_helmet` is an empty patch. Missing, these
broke armour equip slots (only `MAINHAND` accepted anything), stack-size
prediction (everything read 64), and stacking (two damaged swords merged). A
1,658-row canonical table retains all 1,537 captured 26.2 rows and appends
121 authenticated 26.3 report-derived rows. The shared scalar prototypes agree
in both complete inputs. `item_prototypes::prototype_for_version` and
`GameDataVersion::item_prototype` gate lookup through release-specific item
support; a latest-only item is absent in 26.2, not silently assigned a default.
For example, the 26.3 poplar boat has stack cap 1 while poplar planks have cap
64. Generation and drift checks use the [complete union workflow](./data-behavior-codegen.md),
not the old prefix-only source regeneration test.
`read_component_patch` seeds the three
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

Vanilla's own post-spawn-finalization pre-broken-horn roll (10% chance, then a coin flip)
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
entity-metadata field (index 8, the item-stack serializer) — its spawn packet
carries no item id at all. Decoding it uses the same context-bearing
stack and component readers as the container path. For 26.2, one
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
bool) and `update_advancements` (the display flag word is a raw
big-endian integer with three live bits; frame ordinals are task,
challenge, goal — reading them task/goal/challenge swaps the two rarest
frames). Both fold into **session** state (`SessionMaps`/
`SessionAdvancements`), not per-entity state — a map can be held by several
players at once, and the advancement tree is the local player's own.
`encode_update_advancements` always writes the display body absent, since
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

`--protocol <n>` (`Config::protocol`) selects a dialect and gameplay-data
release; synchronized holders use that connection's Configuration registry
snapshot. The `live` feature compiles a family
into the registry at all. `LODESTONE_REGEN=1` on the relevant `#[ignore]`d
test regenerates a committed table from a fresh JVM dump.
Item prototypes use `behavior_union.py --runtime-install` instead; prefix-only
prototype regeneration is rejected once the union census is active.

## Dependencies

`lodestone-model` for the wire vocabulary (`ItemStack`, `ItemComponents`,
`ToolPatch`, `ArmorTrim`, `EquipmentSlot`); `lodestone-data` for every
generated census (`item_prototypes`, `data_component_types`, `items`);
`ProtocolDialect` for selected fixed-ID mappings and `ClientRegistries` for
synchronized holder order;
`lodestone-assets` for `item_model`/`icon`/`bake`; `lodestone-ecs::entity::ItemUse`
for local held-item use state; `web-time` (the sole dependency of
`lodestone-time`). No component-decode path names a protocol version outside
`crates/versions/`.
