# Item model, components and rendering

## What it is

The item stack model end to end: the two `ItemStack` types, how clientbound stacks and data-component patches are decoded, the per-item prototype census that fills in what the wire omits, item variants (one item, several baked models), plugin-defined items, and the portable clock crate.

## How it works

### Two `ItemStack` types, one lowering

- `lodestone_model::ItemStack` (all-`pub`, closed typed component struct) is what decode produces and what `Equipment`/`DisplayItem` carry.
- `lodestone_game::item::ItemStack` (private fields, opaque `BTreeMap<Identifier, ComponentValue>`) is what every container and HUD path holds. Typed accessors funnel through one private `write_component`, so a plugin-built and a decoded stack compare equal and merge.

The lowering (`impl From<&game::ItemStack> for lodestone_model::ItemStack`) has two rules: clearing removes and never zeroes (an empty list deletes the component, so identical stacks still merge), and `ToolPatch::Inherited` is not a value (setting it removes the component, absence reads back as `Inherited`). Getting that backwards makes every pickaxe mine at fist speed. The two `EquipmentSlot` types are distinct; lowering resolves by name via `EquipmentSlot::from_name`.

Pickups use the same effective `max_stack_size` lookup as container clicks: prototype census caps, overridden by an explicitly modelled component. A remainder stays on the item entity.

`ComponentValue::Release` carries a boxed `ItemReleaseComponents` under the internal `lodestone:release_components` key, only when non-default. It keeps animations, providers, fuel/compost values, sign faces, wax, cushion colour, pot templates, instruments and consume/death effects through model to game to model. It participates in equality and disables empty-patch click prediction.

Known gaps: `has_unmodeled` never crosses into `lodestone-game`; custom data, repair cost, charged projectiles and attack range have no game-side slot; retaining a value does not implement its rendering or gameplay.

### Decode: why an unmodelled component halts the packet

The clientbound patch codec writes each component raw, with no length prefix (the length-prefixed form is serverbound-only). The only way to avoid a decode cliff is to model the component; even ones no server sends (`max_stack_size`, `max_damage`) are decoded. Decode returns `DecodedStack::{Complete, Partial}(Option<ItemStack>)` so a list caller (merchant offers) cannot ignore completeness and read an undecoded component's interior as the next offer.

`read_component_patch` in the shared adapter covers the 26.2 bodies and the 13 added 26.3 identities. `can_place_on`/`can_break` are deliberately deferred: their predicate is an independent, self-recursive dispatch with no length prefix.

Width traps:
- Integers and floats can be fixed-width rather than VarInt.
- A bare registry reference differs from the holder shape (`0` inline, `id + 1` reference); a holder set offsets only its size; an enchantment-map key uses the bare form.
- `equippable` has eleven fields, all consumed, only the slot kept (wire id 5 is off-hand).
- `custom_model_data` is four separately counted lists (float, bool, string, colour).
- `attack_range` is six unprefixed floats.
- The derived-NBT family (`custom_data`, `recipes`, `lock`) has no length prefix; `recipes` is a list tag, not a compound.
- Never use a component about to be modelled as a test's "unmodelled" stand-in.

The canonical `lodestone_data::data_component_types::DataComponentTypeId` keeps the 111-entry 26.2 prefix plus 13 names (including two removed from 26.3's 122-entry wire registry). `StackCodecContext` translates each wire id through the selected `ProtocolDialect`; unknown ids fail explicitly. An unmodelled 26.3 payload returns `Unsupported` so incomplete decode cannot count as reviewed Play support.

### One context for stacks and holders

`StackCodecContext` carries the dialect and a borrowed `ClientRegistries` snapshot, passed through patches, recipes, merchant results, nested templates, metadata and particle readers. The fixed `read_item_stack` wrapper is 26.2-only and cannot resolve a synchronized holder.

An ordinary optional stack is count, item id, patch; a template is item id, count, patch. Both translate the item id for the release first. Trim materials and patterns, banner patterns, instruments, block transformers and pottery patterns resolve against Configuration entry order, never assumed alphabetical tables; a missing snapshot or out-of-range holder fails rather than picking another decoration.

26.3 advancement entries carry coordinates after the holder body even without a display (`AdvancementEntry::position` keeps the float bits); 26.2 reads them inside the optional display.

### Encoding

The hosted stack encoder writes every component the server puts on a stack: custom data, damage, enchantments, name, lore, dyed colour, repair cost, potion contents, instrument and the two book components, with type ids per connection release. Enchantment and instrument ids are holder positions in that release's registry; one missing is omitted, never sent under another id. An enchanted book's list travels as `minecraft:stored_enchantments` and decodes back into `enchantments`. Custom data is written only as one complete compound-root network-NBT value. An unstyled literal name or lore line is a bare NBT string at the root (`Text::to_nbt`); inside `with`/`extra` every element stays a compound.

Witness: `crates/versions/26.3/tests/item_components_wire.rs` against `fixtures/item_components_26_3.json`, recorded by `capture_item_components.py` (the official server `give`s one single-component stack per case). Compound keys compare as a set; prototype defaults are never sent.

### Nesting is sender-chosen, so decoders bound it

Container-shaped components hold stacks whose patches hold components, a cycle with no length prefix or depth count. One crafted stack once exhausted the decoder thread's stack and aborted the process.

All routes pass through `read_component_patch`, so the bound is a `Depth` budget entered at its top (not at call sites); `read_slot_display` shares the same budget. `Depth` has no arithmetic or numeric constructor: only `Depth::ROOT` and checked descent.

The cap is 19, the deepest nesting the game itself builds: 16 bundle-in-bundle wraps (each nested bundle costs 1/16 of the weight budget), plus 1 container-item level, plus 1 prototype-component stack (use remainder, sulfur cube content). The cap must be reachable: `ItemComponents` is over 1.7 KB and packets decode on a 2 MiB default stack, which made the survivable depth 19 as a by-value local and 48 after boxing it; hence not the NBT reader's 512 limit. `nesting_budget` gates exactly the cap and one past it. The 1.20.6 slot decoder is bounded the same way at `decode_nested`.

Regression seeds live in `fuzz/seeds/v26_2_clientbound_decode/` (replayed by `fuzz/smoke.sh`); a clean fuzz run is evidence about its mutation path, the seeds are the durable half.

### Consuming bytes is not keeping the value

A component arm must advance the reader exactly and either land the value in `ItemComponents` or say at the site why dropping it is right. A dropped value still decodes, emits and scores connected, and round trips cannot see it.

Retained beyond what a renderer reads: `repairable_items`, `equippable_allowed_entities`, `damage_resistant`, `provides_banner_patterns` (all `RegistrySet`), `blocks_attacks`, `consume_effects`, `death_protection_effects`, and `ArmorTrim`'s inline-only fields.

`RegistrySet` is one leading VarInt: `0` then a tag name, or `n` then `n - 1` bare ids. The tag name is part of the value: tag membership never reaches the client, so reducing it to an empty id list would turn "every plank" into "nothing", and the tag arm is the common one. Only the stonecutter ingredient in `RecipePropertySetsUpdated` is still narrowed to explicit ids. Two things are consumed for alignment on purpose: sound references and a mob-effect's hidden effect.

### Item prototypes: what the wire omits

A clientbound patch is a delta from the item's built-in prototype, so an empty patch can mean a diamond helmet. Without the census, armour accepted only the main-hand slot, stack prediction read 64 for everything and damaged swords merged.

The canonical table has 1,658 rows: 1,537 captured 26.2 rows plus 121 report-derived 26.3 rows. `item_prototypes::prototype_for_version` and `GameDataVersion::item_prototype` gate lookup by release, so a latest-only item is absent in 26.2 rather than defaulted. `read_component_patch` seeds the three effective fields (`max_stack_size`, `max_damage`, `equippable`) from it, and a removal falls back to `1`, not 64. Regeneration follows [data-behavior-codegen](./data-behavior-codegen.md).

Gotchas: `EquipmentSlot::Body` is animal armour, not chest; only the slot is carried from `equippable`, never `allowedEntities`; `lodestone-game`'s own lowering does not yet consume the census, so `container::equippable_slot` and stack caps still answer from an empty map there.

### Item variants

`minecraft:item_model` selects a selector tree (`condition`, `select`, `range_dispatch`, `composite`) whose leaves name concrete models (a bow at rest versus drawing). `ItemVariants` bakes every reachable model at load and `ItemVariants::resolve(&ItemStateContext)` picks per draw, falling back to the inventory form. Draw sites must use it, not the inventory-only `BlockModels::item`.

`ItemStateContext` sources display context, `using_item`/`use_duration`/crossbow pull from `ItemUse`, and the first `custom_model_data` float; trim material, damage and count read as unset. `use_duration` counts up (fed from `ItemUse::ticks`) while `use_cycle` counts down; inverting the wrong one pins a drawn bow at full draw. Equipment producers must select by `item_model` before forming the `ResourceLocation`, or the in-hand item shows the base item while GUI shows the pack's replacement.

### Custom (plugin-defined) items

The wire carries a registry index, so a novel id has nowhere to live. `CustomItem` names a vanilla base item plus an identity tag `lodestone:item_id` (outside `minecraft:` so no server resolves it). `CustomItem::validate` requires a non-`minecraft:` custom id and a `minecraft:` base. `identify` is a pure function of the stack's components. `CustomItems` is a shared ECS resource. Known gap: the tag does not survive game to model round trips, so identity is lost crossing a real server.

### Trim, maps, advancements

`minecraft:trim` is two holders (`0` inline, positive = reference minus 1); both must be read. `TRIM_MATERIAL_IDS`/`TRIM_PATTERN_IDS` are bootstrap order (dynamic registries synced in Configuration are not stored), exact for the stock server and provisional for modded ones; do not read `lodestone_assets::trim`'s tables, which agree only by coincidence. `map_id` and `pot_decorations` are modelled because leaving them out truncated the packets they ride in (a decorated-pot advancement icon makes join fatal).

`map_item_data` is a dirty-rectangle patch (width, height, startX, startY; "absent" is a zero-width byte). `update_advancements` display flags are a raw big-endian int with three live bits; frame ordinals are task, challenge, goal. Both fold into session state (`SessionMaps`, `SessionAdvancements`). `encode_update_advancements` always writes the display absent.

### Goat horns

A pre-broken horn is rolled once at spawn (`goat_horn_spawn_roll`, 10% then a coin flip), held on `SimMob::has_left_horn`/`has_right_horn` and sent as `MetadataField::GoatHorns` (indices 19/20). Nothing flips it after spawn; the screaming flag (index 18) is unwired.

### Dropped-item identity

A dropped item's whole identity is entity-metadata index 8; its spawn packet carries none. Metadata is a `0xFF`-terminated stream that cannot resume once desynced, so an unmodelled component abandons the rest of the field list rather than erroring, and an undecodable item must yield a partial or absent stack, never a propagated error (this once ended the session on equipping a component-bearing tool). `EntityMetadataUpdate.item` is `Option<Option<ItemStack>>`: outer is "field present in this update", inner "stack set"; flatten only at the interpolator.

### The portable clock (`lodestone-time`)

The only sanctioned clock. `std::time::Instant`/`SystemTime` compile for `wasm32-unknown-unknown` and panic at runtime (fatal under `panic = "abort"`). `lodestone_time` wraps `web-time`; on native its `Instant` is `std::time::Instant`. `scripts/wasm-check.sh` bans raw paths per crate, with a short exception list for structurally confined crates.

## How to change it

- New data component: read its codec in the jar, add an arm to `read_component_patch` (`inventory/components_26_3.rs` for 26.3) plus the `lodestone_model::item` carrier, extend the whole-struct round-trip, and add an independent byte fixture with asymmetric ids that requires an empty reader. Thread the context through new nested paths. Extend `ItemReleaseComponents` and the game conversion together when no `ComponentValue` slot exists.
- Item-model property: teach `ItemStateContext` and drop it from the unsourced roster.
- Custom-item field: change `CustomItem::apply_to` and its round-trip together.

## Configuration

`--protocol <n>` (`Config::protocol`) selects dialect and gameplay data. `LODESTONE_REGEN=1` on the relevant `#[ignore]`d test regenerates a table from a fresh JVM dump; item prototypes use `behavior_union.py --runtime-install` instead.

## Dependencies

`lodestone-model`, `lodestone-data` (`item_prototypes`, `data_component_types`, `items`), `ProtocolDialect`, `ClientRegistries`, `lodestone-assets` (`item_model`, `icon`, `bake`), `lodestone-ecs::entity::ItemUse`, `web-time`.
