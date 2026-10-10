# Entity persistence

## What it is

How a live mob survives a world reload: `MobSim::saved_entities` writes each mob as a `SavedEntity` (pose, health, plus an `extra` list of vanilla-named NBT fields) into the per-dimension `entities/` region set, and `MobSim::restore_saved` puts it back. The Anvil import and export path reads and writes the same records.

## How it works

`SavedEntity` and its NBT codec live in `entity_record` (compiled for wasm too: a bee stored in a hive is a `SavedEntity` compound); only the region-file I/O in `entity_storage`, the native store conversions and the villager point-of-interest claims are native-only.

`SavedEntity::extra` (built in `mobs/sim_persistence.rs` and `sim_persistence_state.rs`) combines **owned fields** (state the sim models, decoded onto `SimMob` and re-encoded on save) and **carried fields** (everything else in the incoming record, kept verbatim in `SimMob::passthrough`, which keeps variants, colours, shear state, equipment, attributes and unmodelled brain memories alive). `OWNED_FIELDS` is the dividing line: a listed field is dropped from the carried set so a stale copy never sits beside the live value. Names and encodings are the real server's, pinned against a vanilla-written corpus in `crates/lodestone-server/tests/entity_nbt_vanilla_oracle.rs` (`a_real_vanilla_mob_keeps_its_modeled_state_through_the_sim`, ignored; reads `.cache/mc/survival/world`).

| State | NBT field(s) | Notes |
|---|---|---|
| Pose, motion, health | `Pos`, `Motion`, `Rotation`, `Health` | always |
| Growth | `Age`, `AgeLocked` | a baby keeps its hitbox and step |
| Villager | `VillagerData` profession/level (biome `type` carried), `Xp`, `VillagerDataFinalized` | |
| Villager trades | `Offers.Recipes[]` (`uses`, `demand`, `specialPrice`), `LastRestock`, `RestocksToday` | matched by buy/sell item against the rebuilt list; offers the table lacks are dropped |
| Villager gossip, memories | `Gossips[]`, `LastGossipDecay`; `Brain.memories` `job_site`/`home`/`meeting_point` | claims re-made on restore |
| Tame owner, sitting, horse taming | `Owner` (uuid int-array; a uuid naming a restored mob becomes a mob owner), `Sitting` (wolf, cat, parrot), `Tame`, `Temper` | |
| Leash | `leash` | `{UUID}` for a player or mob holder, `[x,y,z]` for a fence knot |
| Anger | `anger_end_time`, `angry_at` | bee, wolf, enderman, zombified piglin, iron golem, polar bear. Deadline is the sim's tick (`-1` none); `MobSim::grudge_positions` re-resolves the offender each tick and at the end of restore; a deadline beyond the longest a hit can start is clamped, a past one dropped |
| Love timer | `InLove` | only while in love |
| Wool, collar | `Color`, `Sheared`; `CollarColor` | streamed as the wool byte / once tamed |
| Custom name | `CustomName`, `CustomNameVisible` | a name tag applies it and exempts the mob from despawn; stored as plain text |
| Spawn variant | `variant` (cat, wolf, cow, pig, chicken, frog), `Variant` (horse, llama, parrot, axolotl), `RabbitType`, `Type` (fox, mooshroom) | chosen at spawn from biome and a uuid-derived roll (`mobs/appearance.rs`); streamed as metadata |

**Breeding.** `MobSim::resolve_breeding` calls `appearance::inherit` so a baby never takes a wild roll: sheep take the dye the parents' colours craft to, else a coin flip; cow, pig, chicken, fox, llama take a parent's variant; wolf and cat likewise plus mixed collar (born tame to a tame breeder's owner); axolotl 1 in 1200 rare blue; rabbit 1 in 20 the wild roll; mooshroom a parent with 1 in 1024 flip when both match; horse picks colour (4/9 each parent, 1/9 random) and markings (2/5, 2/5, 1/5) separately. Frogs do not inherit (tadpole); mules are not produced.

**Variant tables.** Biome rules in `mobs/appearance.rs` are compared in `mobs/tests/variant_data.rs` against the release's own `*_variant` registries, biome tags and dye recipes via `lodestone_mc_cache` (skipped without the cache). On the wire a holder variant is `registry id + 1`, ids ordered alphabetically by key; `versions/26.2/src/entity_variants.rs` holds sorted tables pinned by `tables_match_the_captured_registry_data` to the captured 26.2 and 26.3 payloads. A data pack adding a variant would shift ids.

**Restore rules.**
- Profession is durable; the workstation claim is derived. A saved `job_site` counts only if the block still hands out the saved profession and a ticket is free; otherwise `tick_villager_professions` searches for a station of that profession and never swaps to an unrelated one. (Vanilla drops the profession of a xp-less villager whose station is gone; ours keeps it.)
- Passive mobs are persistent by species, so `PersistenceRequired: 0` never makes one despawnable; only `1` or a `CustomName` upgrades a mob. On save the flag is written only for a persistent hostile species.
- `LastRestock` and `LastGossipDecay` are absolute game times, clamped on restore to the sim's tick count.

### The native store

`world_storage::NativeEntityRecord` is `SavedEntity` in the native store's terms: identity, type, dimension, pose, motion and `NativeEntityState { health, item, age, pickup_delay, fields }` (`item` a whole `ItemStack`; [item save format](item-save-format.md)). `fields` is every other saved field under its vanilla name, so mobs, projectiles, thrown and dropped items round-trip; `MobSim::native_entities`/`restore_native` are thin conversions. On the wire (`EntityRecord` in `storage.proto`) `fields` is `state_nbt`; `EntityRecord.schema_version` 0 is the original pose plus `durable_state` layout and 2 (`ENTITY_SCHEMA_VERSION`) adds `state_nbt`. `world_storage::migrate_entity_body` upgrades 0 to 2 on read and refuses unknown versions; old bodies are rewritten by the next `replace_live_entities`. Fixtures: `lodestone-storage-schema/tests/fixtures/native-entity-*-v1.hex`.

Each built-in dimension has its own roster and the integrated server one mob sim per dimension (`DimensionRuntime`, via `WorldStateHandle::dimension_runtime`). Saves write every adopted dimension's roster in one commit (`WorldStorage::replace_live_entity_rosters`); the seed task restores a Nether or End roster into that runtime before any player travels there ([World storage](world-storage.md)). The Anvil backend uses one `entities/` set per dimension (`DimensionEntityStores`). Proofs: `integrated::tests::native_store_keeps_each_dimension_population_across_a_restart`, `anvil_entities_keep_each_dimension_population_across_a_restart`, `native_store_round_trips_every_entity_kind_across_a_restart`.

## How to change it

- Cosmetic state lives in `SimMob::appearance`; `appearance::owned_fields` lists the saved fields per species and must stay in step with `mob_state_fields` and `restore_mob_state`. New wire fields need an index and serializer from the jar dump (`versions/26.2/tests/support/entity_data_index_jvm.txt`, and the 26.3 one for protocol 777, which reuses the 26.2 encoder) and an arm in the server encoder; `cosmetic_metadata_constants_match_the_jar_dump` and `the_777_dump_differs_from_776_only_by_the_known_rows` pin them.
- To persist a new modelled field: encode in `mob_state_fields`, decode in `restore_mob_state` (or a species helper), and add the key to `OWNED_FIELDS` (skipping this writes a stale carried copy). Nothing in the native store changes.
- To change the layout: bump `ENTITY_SCHEMA_VERSION`, add the step to `migrate_entity_body`, regenerate the schema (`LODESTONE_STORAGE_SCHEMA_REGENERATE=1`), keep a fixture of the previous layout.
- A mob-to-mob reference (owner, leash holder) is written as a uuid and resolved in `resolve_references` after the whole batch exists, so order does not matter.
- Test through `SavedEntity::to_nbt`/`from_nbt`, not the in-memory struct (`mobs/tests/persistence_state.rs`).

## Configuration

None. Entity region files live under `<world>/dimensions/<ns>/<dim>/entities/` ([World persistence](world-persistence.md); import in `anvil-native-entity-import.md`).

## Not persisted yet

Tipped-arrow contents; active effects, burn time and piglin/warden/allay/sniffer/camel/armadillo/axolotl timers (carried verbatim from an import, but the sim's own values are not written); equipment, saddles and horse armour (carried, not modelled); all-black cats and lightning-brown mooshrooms; name-tag styling; passengers, boats and minecarts; projectiles on the native path other than via `fields`; villager trades the generated table lacks (replaced by table offers, use counts lost).

## Dependencies

`lodestone-core` (NBT), `lodestone_anvil::region`, `mobs::villager` (claims, gossip, profession tables), `villager_trade` (offer state).
