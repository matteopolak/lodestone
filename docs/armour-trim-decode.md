# Armour trim decoding, and the component-patch decode cliff

Issue [#17](https://github.com/matteopolak/lodestone/issues/17) (partial — the wire
half; the renderer half is shell work and still outstanding).

## What it is

`minecraft:trim` now decodes off the wire into
`lodestone_model::ItemComponents::trim`, so a smithing-table armour trim reaches
the client as a `(material, pattern)` pair. The asset layer it feeds
(`lodestone_assets::trim`, `trim_decal_pipeline`) was already complete with zero
callers; this is the missing link.

The more important half is why it had to be *modeled* rather than skipped.

## The decode cliff, and why skipping is impossible

`read_component_patch` (`crates/protocol/v770/src/adapter.rs`) has an `other =>`
arm that sets `has_unmodeled` and **stops reading the rest of the packet**. That
looks like a wart worth fixing generically — skip the unknown component's payload
and continue — and it is not fixable, verified against the jar rather than assumed:

26.2 ships **two** patch codecs (`DataComponentPatch.java:62-76`):

| codec | payloads | used by |
|---|---|---|
| `STREAM_CODEC` | written **raw**, no length | `ItemStack.OPTIONAL_STREAM_CODEC` — **clientbound** |
| `DELIMITED_STREAM_CODEC` | `registryFriendlyLengthPrefixed` | `OPTIONAL_UNTRUSTED_STREAM_CODEC` — serverbound |

`ItemStack.java:124-126` is the join: clientbound stacks are built on the
**undelimited** one. So there is no length to skip and no self-describing framing
to walk, and the delimited variant exists precisely so a *server* can safely skip a
hostile client's junk — the asymmetry is deliberate.

**The only way to stop a given component being a decode cliff is to model it.**
That makes each unmodeled component a latent truncation bug for the whole packet,
not merely a lost field, and it is the reason `minecraft:max_stack_size` and
`minecraft:max_damage` are decoded despite no server ever sending them.

## How it works

`read_armor_trim` mirrors `ArmorTrim.STREAM_CODEC` (`ArmorTrim.java:26-28`): a
`Holder<TrimMaterial>` then a `Holder<TrimPattern>`. Each holder is a VarInt where
`0` introduces an **inline** definition and any positive value references the
registry at `value - 1`. Both forms are read, because both must be — consuming the
wrong byte count for the inline form desyncs the rest of the packet exactly as the
cliff above does.

Inline bodies, from the two `DIRECT_STREAM_CODEC`s:

* `TrimMaterial` — a `MaterialAssetGroup` (one UTF-8 asset suffix, then a VarInt
  count of `(key, suffix)` override pairs) then a description `Component` (network
  NBT).
* `TrimPattern` — an `Identifier`, a description `Component`, then a `bool` decal.

The result is two bare registry **paths** (`"netherite"`, `"silence"`), the form
`lodestone_assets::trim::{trim_material, trim_pattern}` keys its sprite tables by.

## How to change it, and the gotchas

* **`Registries.TRIM_MATERIAL` and `TRIM_PATTERN` are dynamic registries.** Their
  ids come from the Configuration-phase `registry_data` sync, and this client keeps
  no dynamic-registry store — so a reference-form holder has nothing to resolve
  against. `adapter.rs`'s `TRIM_MATERIAL_IDS`/`TRIM_PATTERN_IDS` are the vanilla
  **bootstrap order** (`TrimMaterials.java:25-35`, `TrimPatterns.java:31-48`), which
  is what a server without a trim datapack assigns. Exact for vanilla,
  **provisional** for a modded server — the same posture and caveat as
  `server_protocol.rs`'s `BIOME_NAMES`. An out-of-range id yields an empty string
  rather than an error: the bytes are consumed either way, which is the property
  that keeps the rest of the packet readable.
* **Do not read those tables from `lodestone_assets::trim`.** `TRIM_MATERIALS`
  there happens to be in registry order today; `TRIM_PATTERNS` beside it is
  **alphabetical**. "The asset table is in registry order" is a coincidence for one
  of the two and cannot be relied on for either.
* **The inline material carries no registry name**, only its asset suffix. That is
  what is reported, and for every vanilla material the suffix *is* the registry path
  (`MaterialAssetGroup::create(base)`); it is also the half `trim_sprite_id` needs.
* `lodestone-game`'s own `ComponentMap` has no trim representation, so
  `ItemStack -> game -> ItemStack` drops it. That is listed with the other lossy
  fields on that conversion's doc, not a silent gap.

## Configuration

None.

## Dependencies

`lodestone_model::ArmorTrim`; `lodestone_data::generated::data_component_types` for
`minecraft:trim`'s own component-type registry id (56 in 26.2, resolved by name).
Consumers: `lodestone_assets::trim` for the sprite tables, and eventually the
shell's equipment-layer renderer.
