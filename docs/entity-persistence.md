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
| Despawn exemption | `PersistenceRequired`, `CustomName` | see below |

### Restore rules worth knowing

- **Profession is the durable fact, the workstation claim is derived.** A saved `job_site` only counts if the block there still hands out the saved profession and a ticket is free. Otherwise the villager keeps its profession and `tick_villager_professions` searches for a station of that same profession on its job-search interval; it never swaps to the nearest unrelated station. Divergence from vanilla: a vanilla villager with no xp loses its profession when its station is gone, ours keeps it until a station is found.
- **Persistence flag.** Passive mobs are persistent by species in the sim, so `PersistenceRequired: 0` never makes one despawnable; only a `1` or a `CustomName` upgrades a mob. On save, the flag is written only for a persistent hostile species.
- **Clocks.** `LastRestock` and `LastGossipDecay` are absolute game times; on restore they are clamped to the sim's own tick count so a clock that restarted cannot postpone restocking or decay.

## How to change it

- To persist a new modeled field: encode it in `mob_state_fields`, decode it in `restore_mob_state` (or a species helper), and add its key to `OWNED_FIELDS`. Skipping the last step writes a stale carried copy next to the live value.
- A mob-to-mob reference (owner, leash holder) is written as the target's uuid and resolved in `resolve_references` after the whole batch exists, so restore order does not matter.
- Use `SavedEntity::to_nbt` / `from_nbt` in tests, not the in-memory struct, so the real encoding is what is checked (`mobs/tests/persistence_state.rs`).

## Configuration

None. Entity region files live under `<world>/dimensions/<ns>/<dim>/entities/`; see `world-persistence.md` for the container and `anvil-native-entity-import.md` for import.

## Not persisted yet

- **The native typed store** (`world_storage::NativeEntityState`) carries only health for a living entity. Everything above, including growth, is lost when a world is written through it; the Anvil entity regions are the path that round-trips. Widening it means a storage schema change.
- **Active effects, anger deadlines, burn time, piglin/warden/allay/sniffer/camel/armadillo/axolotl timers** are not modeled-to-NBT. Vanilla's `active_effects` and `anger_end_time` are carried verbatim from an import but the sim's own values are not written.
- **Variants, sheep colour/shear, collar colour, custom names, equipment, saddles and horse armour** are carried, not modeled: the sim has no state for them, so a mob that changes them in play (dye, shears, name tag) does not write the change.
- **Love timer** is dropped; the mob must be fed again.
- **A tame mob without a uuid-addressable owner** (none at present) would load wild.
- **Passengers, projectiles, boats and minecarts** are not part of `saved_entities`.
- **Villager trades** that the generated table does not contain (vanilla rolls its own offers) are replaced by the table's offers; their use counts are lost.

## Dependencies

`lodestone-core` (NBT), `lodestone_anvil::region` (container), `mobs::villager` (claims, gossip, profession tables), `villager_trade` (offer state).
