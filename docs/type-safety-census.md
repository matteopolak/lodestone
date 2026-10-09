# Type-safety census

## What it is

The policy for where public primitive APIs (`i32`, `usize`, `String`) must be replaced by a semantic type, and where the primitive is the correct interface. Nothing in CI checks it; it is a discovery recipe and a set of typed-boundary rules.

## How it works

Candidates are found with two conservative scans over production Rust under `crates/` (excluding `tests/` and `benches/`):

```text
numeric: public functions with state_id, block_state, effect_id, item_id, entity_id,
         window_id, slot, mode, kind, rotation or sequence primitive parameters
text:    public String fields whose names end in url, dimension, potion, effect,
         state, kind, mode, key or id
```

Each hit is either a migration candidate or an intentional primitive. The scan is a discovery guard, not a claim that every integer or string needs a wrapper.

Migration families still open: `entity-network-id` (server simulation and client compatibility APIs), `inventory-menu-slot`, `typed-discriminator`, and `dimension-resource-url`.

Intentional primitives are those where the representation is the interface: wire bytes and strings in version packet crates, storage and import formats (`lodestone-anvil`), external identity strings, ring-buffer indices, observability labels, secrets, and user-authored or format-defined text.

### Typed boundaries in place

| Domain | Type | Rule |
|---|---|---|
| Canonical block state | `lodestone_data::block_states::StateId` | raw values only at decode, chunk-store or version-adapter boundaries, validated before use |
| Potion | `PotionId` | names parsed at item/NBT boundaries with `PotionId::from_name`; unknown names stay unresolved |
| Serialized block state | `BlockStateValue` | unknown spellings kept as an explicit extension value at text import/export |
| Container sync state | `lodestone_model::ContainerStateId` | convert only at `from_wire` / `as_wire` |
| Recipe item ids | `lodestone_model::ItemId` | require `ItemId::canonical_raw()` before indexing a generated item table; unknown non-negative ids stay protocol-local |
| Entity ids | `lodestone_model::EntityNetworkId` | server ids classified at ingress with `from_wire`; negative plugin ids come only from the plugin allocator; `raw()` only at wire or GPU POD boundaries |

Worldgen configs (`CodedBlock`, `OreTarget`, `SpringCfg`, `ReplaceBlobsCfg`, `BlockBlobCfg`) carry `CanonicalStateId`, and the End podium, gravity settlement and block reactions carry `StateId`.

`BlockAtlas::state_id_of` returns `Option<StateId>` from a textual key; the shell's collision adapter calls `StateId::raw()` only when filling its packed list. Other consumers call `StateId::from_state_str` directly rather than recovering a raw id to validate it again. An unknown block name stays unresolved and is never treated as a solid built-in state.

## How to change it

- Migrate a family as a unit: change every site in it, then rerun both scans.
- Move a primitive to the intentional category only when its boundary role is documented and tested.
- Keep unknown inputs unresolved rather than mapping them to a plausible id.

## Dependencies

`rg` for the scans, and the model types at each protocol, storage or plugin boundary.
