# Registries: synchronized data, canonical block states, and generated tables

## What it is

How data-driven registries reach the client and server: the Configuration-phase `registry_data` packets, the `lodestone-canonical` bridge from legacy block ids to canonical block states, the generated registry enums and validated id newtypes, and the `lodestone-data` crate that owns the generated game-data censuses.

## How it works

### Registry data ingest

During Configuration the server sends one `registry_data` packet per synchronized registry (29). The authoritative list is the game jar's own synchronized-registry list, not `registries.json` (which omits `minecraft:dimension_type` and `minecraft:world_clock`).

- Each packet is a registry id plus ordered `(entry id, optional NBT)` pairs. **Entry order is the holder-id space**: `login`, `respawn` and `set_time` reference entries by bare VarInt index. Dynamic registries are typically alphabetical by resource location, not bootstrap order; settle order against a captured fixture.
- Only `dimension_type` and `world_clock` are parsed into typed values (chunk height, sky-light default, day clock); the other 27 keep ordered names. Add a typed arm when a third registry becomes load-bearing, not a generic NBT cache. An elided or unparseable entry keeps its slot as `Option<T>`; a resent registry replaces the whole set; both preserve holder ids.
- `login`/`respawn` keep the dimension-type holder as a raw signed VarInt. `DimensionTypeHolderId::from_wire` rejects negatives once at the v26 adapter ingress; a non-negative unknown id stays an unresolved lookup and follows the level-name fallback, never coerced to overworld.

The server mirror, `ServerProtocol::encode_registry_data`, emits the same burst (`select_known_packs` with an empty list, all 29 registries, `update_tags`); `dimension_type` and `world_clock` are hand-built, the other 27 opaque bytes captured from a real server.

### `lodestone-canonical`: the pre-Flattening bridge

Every pre-1.13 family maps block ids through this shared crate (it names no protocol family; one table serves them all since older id spaces are subsets of 1.12.2's; `v1-14` bakes its own, see [multi-protocol seam](./multi-protocol-seam.md)). Two modules in series: `flattening::lookup(old_block_id, meta)` gives a 1.13-era name plus properties, dumped from the 1.13.2 server jar's own data fixer, and distinguishes resolved, no table entry, requires-additional-context (flower pots, skulls, double-plant upper halves need block-entity data) and out-of-bounds; `canonical` bridges that to a canonical `StateId` (`CanonicalBlockState::Resolved`) via a small hand-verified rename table and property fixups. Neither substitutes air: the consuming family decides in its chunk decoder and counts it on a `FallbackTally`.

### Registry types: generated enums

`Block`, `Item` and `EntityType` (`lodestone_data`) are generated `#[repr]` enums whose discriminant is the wire registry id, with no `Custom` variant or `#[non_exhaustive]`, so a version bump breaks incomplete matches at compile time. Plugin/custom values live in a `*Ref` wrapper (`u32`: below the built-in count a registry id, at or above an index into a host-owned interner). Validate the raw integer once at the edge, then use total lookups:

| type | validate with | notes |
|---|---|---|
| `Block` | `u16` then `Block::from_registry_id` | custom keys stay identifiers in their dynamic registry |
| `Item` | `ItemId::canonical_raw()`, then `Item::from_registry_id` | `ItemId` keeps unknown/custom entries; writers use `Item::from_name`/`registry_id` |
| `EntityType` | `from_resource_key` / `from_name` | `EntitySnapshot` keeps a `ResourceKey` (legacy families share the trait); `add_entity` fallback is `EntityType::AcaciaBoat` |
| `BlockEntityType` | newtype over 49 fixed entries | writers call `raw` only at the VarInt |
| `StateId` | `StateId::new`, `StateId::from_state_str` | `raw` only where a packet writes the global id |
| `ParticleTypeId`, `PotionId`, `AttributeId`, `MenuId`, `DataComponentTypeId` | `new` / `from_registry_id` | unknown wire values are decode failures, never a default |

- **`Block` and `StateId` are different id spaces**: 1,196 values in registration order versus 32,366 in name-sorted order; cross only via `StateId::block` and `Block::default_state`. The raw state table's first column is the alphabetical block index, resolved through `generated_block_enum::REGISTRY_IDS_BY_NAME` into `BLOCK_REGISTRY_NAMES`; air (registry 0, alphabetical 19) and stone (1, 975) tell the orders apart.
- The spawner block's state is `minecraft:spawner` but the block-entity registry and chunk-NBT key is `minecraft:mob_spawner` (id 9); the decoder accepts the former only as an alias for older worlds.
- `block_states::state_id` resolves in three tiers: exact, default plus named overrides, default alone. The default comes from the block's identity data, not the lowest state id.
- Sound events keep one id-indexed name column (1,968 names) with a sparse `(u32, f32)` fixed-range table (currently empty). Potion base keys: a 46-entry `u8` table maps each potion to its base (24 unique, from the committed `potion_effect_bases_26_2.txt`); never infer a base by stripping `long_`/`strong_`; `ItemComponents` keeps the holder raw so unknown holders survive and consumers fail closed.
- An unknown data-component id in an item patch's additions makes the stack partial (no generic length to skip); unknown removals are safe. Controls: `custom_data = 0`, `tool = 28`, `shulker/color = 110`.
- `Identifier` stores `SmolStr` (`new_borrowed` avoids allocation); generated tables stay `&'static str`. Legacy `block_action` has no metadata, so the adapter scans the legacy table for one resolved `StateId`, falls back to `block_states::air_state`, and uses `StateId::block`.

### `lodestone-data`: the censuses

About twenty generated game-data tables (block states, hardness, collision shapes, solidity, item prototypes, entity census and dimensions, tools, sounds, particles, menus, components...) describe the game, not the wire (`packet_ids` stays in `v26-2`). Each has a generated `src/generated/*.rs`, a hand-written `src/*.rs` lookup returning `lodestone-model` types, and a dump program under `oracle-java/`. Two provenance shapes: **registry-report tables** (`attribute_types`, `entity_types`, `block_states`, `sound_events`, `particle_types`, `menus`, `items`, `data_component_types`) parse `registries.json`/`blocks.json`; **JVM-walked tables** (`hardness`, `collision_shapes`, `block_solidity`, `entity_census`, `entity_dimensions`, `item_prototypes`, `outline_shapes`, `path_types`, `snow_support`, `tools`, `block_entity_types`) boot a real headless server and walk it because the fact has no getter (block-entity coverage comes from the per-type state-validity check). The `v26-2` adapter delegates each data-shaped `VersionAdapter` method into this crate; `v1-8`/`v1-9`/`v1-14` keep their own translation tables, not a second census.

## How to change it

- Never hand-edit a generated file: `LODESTONE_REGEN=1 cargo test -p <crate> --test <name> <fn> -- --ignored --nocapture` (exact invocation in each test header; the potion base table omits `--ignored`: `... --test potion_effect_ids committed_table_matches_the_committed_fixture -- --nocapture`).
- Large arrays are sharded: `path_types.rs` keeps the public `STATE_PATH_TYPE`, the generator emits 1,024-state files assembled by a const copy; the drift guard compares every shard and rejects stale ones.
- A census keyed by a built-in registry reuses its canonical names (blast/fire facts map `Block` id to fact index; the generator checks the exact `0..BLOCK_COUNT` permutation).
- `cargo xtask gen-registries` follows the current release (`mc-version`) and owns only the menu table; sounds, particles, items and components are append-only canonical censuses emitted by `crates/lodestone-data/tools/identity_staging.py`, `behavior_union.py` and `crates/versions/26.3/tools/fixed_registry_maps.py --emit-data-tables`. `gen-registries --check` detects drift; `cargo xtask conformance --family <family>` defaults version and protocol to the current release.
- Attributes, menus, mob effects and potions are identical in 26.2 and 26.3 (one table; `tests/release_registries.rs`, ignored, pins names to `registries.json`). Damage types (51, 36 tags) and enchantments (43) come from the current datapack (`just regen-damage-types`, `tests/enchantments.rs`). The worldgen biome enum stays on 26.2 ([worldgen biome types](./worldgen-biome-types.md)).
- New typed registry field (e.g. `DimensionType`): add it to the wire struct, the `lodestone-model` carrier if version-free consumers need it, and the building adapter; check whether it moved into the generic `attributes`/component map first.
- New census: a dump program or report parser, a generated raw table, a lookup file and a `lib.rs` module; `tests/generated_string_columns.rs` fails unless a new `&'static str` column is classified in its `ALLOWED` table.
- Canonical bridge renames and fixups are hand-written and need a justification checked against the reference source. `cargo xtask connectedness` cannot see a canonicalisation defect (it proves a wire is connected, not what flows); verify decoded block ids with a captured section or live server.

## Configuration

`LODESTONE_REGEN=1` switches every generator from assert to write. The `live-registry` Cargo feature gates the live capture test; `LODESTONE_CAPTURE_FIXTURES=1` rewrites captured passthrough-registry fixtures. Tables are `&'static` rodata.

## Dependencies

`lodestone-data` depends on `lodestone-model` for returned types and nothing else; `lodestone-canonical` depends only on `lodestone-data` (used by `v1-8`/`v1-9`); `lodestone-ecs`, `-client`, `-shell` consume the typed `dimension_type`/`world_clock`; the reference cache (`generated/reports`, `client-src`) and the 1.13.2 server jar ([oracles and benchmarks](./oracles-and-benchmarks.md)).
