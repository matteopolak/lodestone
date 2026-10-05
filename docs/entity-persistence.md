# Entity persistence

## What it is

How a live mob survives a world reload: `MobSim::saved_entities` writes each mob as a `SavedEntity` (pose, health, plus an `extra` list of vanilla-named NBT fields) into the per-dimension `entities/` region set, and `MobSim::restore_saved` puts it back. The same records are what the Anvil import and export path reads and writes.

## How it works

`SavedEntity::extra` is built from three sources, in `mobs/sim_persistence.rs` and `mobs/sim_persistence_state.rs`:

1. **Owned fields**: state the sim models, decoded onto `SimMob` on restore and re-encoded from it on save.
2. **Carried fields**: everything else in the incoming record, kept verbatim in `SimMob::passthrough` and written back unchanged. This is what keeps variants, coat/collar colours, shear state, custom names, equipment, attributes and unmodeled brain memories alive across a reload even though the sim never reads them.
3. The dividing line is `OWNED_FIELDS`. A field listed there is dropped from the carried set, so a stale copy is never written beside the live value.

Field names and encodings are the ones a real server writes. They are pinned against the vanilla-written corpus in `crates/lodestone-server/tests/entity_nbt_vanilla_oracle.rs` (`a_real_vanilla_mob_keeps_its_modeled_state_through_the_sim`, ignored; it reads `.cache/mc/survival/world`).

### What is persisted

| State | NBT field(s) | Notes |
|---|---|---|
| Pose, motion, health | `Pos`, `Motion`, `Rotation`, `Health` | always |
| Growth | `Age`, `AgeLocked` | a baby comes back with its hitbox and step |
| Villager profession, level | `VillagerData.profession` / `level` | the biome `type` is carried |
| Villager xp | `Xp`, `VillagerDataFinalized` | |
| Villager trades | `Offers.Recipes[]` (`uses`, `demand`, `specialPrice`), `LastRestock`, `RestocksToday` | matched by buy/sell item against the rebuilt offer list; an offer the table does not contain is dropped |
| Villager gossip | `Gossips[]` (`Target`, `Type`, `Value`), `LastGossipDecay` | |
| Villager job site, bed, bell | `Brain.memories` `job_site` / `home` / `meeting_point` | re-claimed on restore, see below |
| Tame owner | `Owner` (uuid int-array) | player or mob owner; a uuid naming a restored mob becomes a mob owner |
| Sitting order | `Sitting` | written for wolf, cat, parrot |
| Horse-family taming | `Tame`, `Temper`, `Owner` | horse, donkey, mule, skeleton/zombie horse, llama |
| Leash | `leash` | `{UUID}` for a player or mob holder, an `[x,y,z]` int-array for a fence knot |
| Anger | `anger_end_time`, `angry_at` | bee, wolf, enderman, zombified piglin, iron golem, polar bear. The deadline is the sim's own tick (`-1` for none) and `angry_at` is the offender's uuid when a player's hit started it. After a load the grudge has no position until the offender is in the world; `MobSim::grudge_positions` re-resolves it each tick (and once at the end of a restore). A deadline beyond the longest grudge a hit can start is clamped on restore, and one in the past is dropped. A pack alerted by a player's hit records the same offender; a piglin alerted by another mob keeps its deadline but no uuid. |
| Love timer | `InLove` | remaining ticks, written only while in love |
| Despawn exemption | `PersistenceRequired`, `CustomName` | see below |
| Sheep wool | `Color`, `Sheared` | shears, dye and grazing change it in play; streamed as the wool byte |
| Collar | `CollarColor` | wolf and cat; owner-only dye; streamed once tamed |
| Custom name | `CustomName`, `CustomNameVisible` | a name tag with a name applies it and exempts the mob from despawn; streamed as the optional name component. Stored as plain text. |
| Spawn variant | `variant` (cat, wolf, cow, pig, chicken, frog), `Variant` (horse, llama, parrot, axolotl), `RabbitType`, `Type` (fox, mooshroom) | chosen at spawn from the biome and a uuid-derived roll (`mobs/appearance.rs`); a bred baby inherits instead (see below); every variant species is streamed as entity metadata |

### Breeding

`MobSim::resolve_breeding` calls `appearance::inherit` for the baby, so it never takes a wild roll. Rules: sheep wool is the dye the two parents' colours craft to, else a coin flip; cow, pig, chicken, fox, llama and trader llama take a parent's variant by coin flip; wolf and cat do the same plus the mixed collar, and the baby is born tame to the breeder's owner when the breeder is tame; axolotl is 1 in 1200 the rare blue, else a parent; rabbit is 1 in 20 the wild roll at its position, else a parent; mooshroom is a parent, with a 1 in 1024 flip to the other type when both parents match; horse picks colour (4/9 each parent, 1/9 random) and markings (2/5 each parent, 1/5 random) separately. Frogs do not inherit (their baby is a tadpole). Horse-donkey mules are not produced because breeding pairs same-species adults only.

### How the variant tables are checked

The biome rules in `mobs/appearance.rs` are compared in `mobs/tests/variant_data.rs` with the release's own data files (`*_variant` registries, biome tags, dye recipes) read through `lodestone_mc_cache`; the expected variant of every biome is derived from the files, and the tests skip when the cache is absent. On the wire, a holder variant is `registry id + 1`, and the ids are the order the server sends the registry in, which is alphabetical by key for every variant registry. `versions/26.2/src/entity_variants.rs` therefore holds sorted tables, pinned by `tables_match_the_captured_registry_data` to the captured 26.2 and 26.3 `registry_data` payloads. A data pack that adds a variant shifts the ids after it; the tables would then name the wrong variant.

### Restore rules worth knowing

- **Profession is the durable fact, the workstation claim is derived.** A saved `job_site` only counts if the block there still hands out the saved profession and a ticket is free. Otherwise the villager keeps its profession and `tick_villager_professions` searches for a station of that same profession on its job-search interval; it never swaps to the nearest unrelated station. Divergence from vanilla: a vanilla villager with no xp loses its profession when its station is gone, ours keeps it until a station is found.
- **Persistence flag.** Passive mobs are persistent by species in the sim, so `PersistenceRequired: 0` never makes one despawnable; only a `1` or a `CustomName` upgrades a mob. On save, the flag is written only for a persistent hostile species.
- **Clocks.** `LastRestock` and `LastGossipDecay` are absolute game times; on restore they are clamped to the sim's own tick count so a clock that restarted cannot postpone restocking or decay.

## How to change it

- Cosmetic state lives in `SimMob::appearance`; `appearance::owned_fields` lists the saved fields it owns per species and must stay in step with `mob_state_fields` and `restore_mob_state`. New wire fields need an index and serializer from the jar dump (`versions/26.2/tests/support/entity_data_index_jvm.txt`, and `versions/26.3/tests/support/entity_data_index_jvm.txt` for the hosted 777 protocol, which reuses the 26.2 encoder) and an arm in the server protocol encoder; `cosmetic_metadata_constants_match_the_jar_dump` pins them against both, and `the_777_dump_differs_from_776_only_by_the_known_rows` keeps the other metadata pins valid for 777.
- To persist a new modeled field: encode it in `mob_state_fields`, decode it in `restore_mob_state` (or a species helper), and add its key to `OWNED_FIELDS`. Skipping the last step writes a stale carried copy next to the live value.
- A mob-to-mob reference (owner, leash holder) is written as the target's uuid and resolved in `resolve_references` after the whole batch exists, so restore order does not matter.
- Use `SavedEntity::to_nbt` / `from_nbt` in tests, not the in-memory struct, so the real encoding is what is checked (`mobs/tests/persistence_state.rs`).

## The native store

`world_storage::NativeEntityRecord` is `SavedEntity` in the native store's terms: identity, type, dimension, pose, motion, and a `NativeEntityState { health, item, age, pickup_delay, fields }`. `fields` is every other saved field under its vanilla name, so mobs (name, owner, variant, anger, growth), projectiles (embedded state, pickup rule), thrown items and dropped items all round-trip, and `MobSim::native_entities` / `restore_native` are thin conversions over `saved_entities` / `restore_saved`.

On the wire (`EntityRecord` in `storage.proto`) the typed fields keep their original messages and `fields` is `state_nbt`, one named binary NBT compound. `EntityRecord.schema_version` versions the layout: absent (0) is the original pose plus `durable_state` layout, 2 (`ENTITY_SCHEMA_VERSION`) adds `state_nbt`. `world_storage::migrate_entity_body` upgrades 0 to 2 on every read and refuses an unknown version; an old body on disk is rewritten in the current layout by the next `replace_live_entities` because it no longer equals the fresh encoding. Fixtures written by the original schema are `lodestone-storage-schema/tests/fixtures/native-entity-*-v1.hex`.

Each built-in dimension has its own roster, and the integrated server keeps one mob sim per dimension (`DimensionRuntime`, reached through `WorldStateHandle::dimension_runtime`). Saves write every adopted dimension's roster in one commit (`WorldStorage::replace_live_entity_rosters`) and the seed task restores a Nether or End roster into that dimension's runtime, creating it before any player travels there; see `world-storage.md` for the adoption rules. `integrated::tests::native_store_keeps_each_dimension_population_across_a_restart` proves each population returns to its own dimension.

To add a new saved field, nothing in the native store changes: own it in `mob_state_fields` / `restore_mob_state` (or the projectile equivalents) and it travels. To change the layout itself, bump `ENTITY_SCHEMA_VERSION`, add the step to `migrate_entity_body`, regenerate the schema (`LODESTONE_STORAGE_SCHEMA_REGENERATE=1`), and keep a fixture of the previous layout. `integrated::tests::native_store_round_trips_every_entity_kind_across_a_restart` is the end-to-end proof (wolf, stuck arrow, thrown potion, dropped item through a server restart).

## Configuration

None. Entity region files live under `<world>/dimensions/<ns>/<dim>/entities/`; see `world-persistence.md` for the container and `anvil-native-entity-import.md` for import.

## Not persisted yet

- **Nether and End entities under the Anvil backend**: the integrated server opens only the Overworld `entities/` region set (`EntityStorage::new`) and saves only the primary sim, so a world without the native store loses its sibling-dimension mobs on restart. The native store keeps them.
- **Tipped-arrow contents and a few unmodeled fields** are still not saved by either store; the native store adds nothing here, it carries exactly what `SavedEntity` carries.
- **Active effects, burn time, piglin/warden/allay/sniffer/camel/armadillo/axolotl timers** are not modeled-to-NBT. Vanilla's `active_effects` and `anger_end_time` are carried verbatim from an import but the sim's own values are not written.
- **Equipment, saddles and horse armour** are carried, not modeled.
- **Variant gaps:** all-black cats (full moon, certain structures) and mooshroom brown by lightning are not modeled.
- **Name tag styling:** the name is kept as plain text, so a styled name loses its formatting.
- **A tame mob without a uuid-addressable owner** (none at present) would load wild.
- **Passengers, boats and minecarts** are not part of `saved_entities`. Arrows, tridents and thrown items are (see `projectiles.md`), but only on the Anvil path.
- **Villager trades** that the generated table does not contain (vanilla rolls its own offers) are replaced by the table's offers; their use counts are lost.

## Dependencies

`lodestone-core` (NBT), `lodestone_anvil::region` (container), `mobs::villager` (claims, gossip, profession tables), `villager_trade` (offer state).
