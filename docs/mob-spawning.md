# Mob spawning

## What it is

Everything that puts a mob into a live world and gives it a life afterwards: the natural spawn cycle and biome tables, spawn equipment, species-aware body and goal resolution, spawn eggs, breeding and growth, taming, and the registry for custom entity types. It lives under `crates/lodestone-server/src/mobs/` and `natural_spawn.rs`, with timing state in `crates/lodestone-entity/src/ai/navigating_mob.rs`.

## How it works

### Module layout

Under `mobs/`, `sim_config_spawn` builds and populates the simulation, `sim_tick` owns per-tick owner batches, the other `sim_*` modules cover interactions, combat, persistence, lifecycle effects and snapshots, `sim_mob` has one-mob accessors, `handle` has the shared production wrappers and `collision` is the live shape-aware sweep. Put a new public operation beside its consuming phase and keep cross-phase helpers `pub(super)`.

### Natural spawn cycle

Once per tick, gated on the `spawn_mobs` game rule and skipped when no player is loaded, `tick::run_tick_loop` has `MobSim::census` rebuild `SpawnState`; for each chunk still under its category cap, `NaturalSpawner::cluster` runs the per-chunk algorithm and returns a group. RNG draw order and count are the spawn rate, so the cap applies as the group is consumed. Each candidate becomes a real mob via `MobSim::spawn_species`.

- **Category** is the species' registered one (`species::category_of`: fish water ambient, squid and dolphins water creature, bats ambient, villagers and golems misc), applied to natural spawns, generation-time animals, spawners and restored mobs.
- **Despawn** measures each mob against its nearest same-dimension player. A leashed mob never despawns (`DespawnCtx::requires_custom_persistence`). Peaceful eviction is separate: `MobSim::remove_monsters`.
- **Caps** are `max x chunks / 289`, where `chunks` is `FollowArea::spawn_cap_chunks`, the union of 17x17 squares around every player, resident or not. One player gets the full maxima (70 monsters, 10 creatures, 15 ambient, 5 each of axolotl, underground water creature and water creature, 20 water ambient). It deliberately is not the simulated follow area (49 columns), where `5 x 49 / 289` rounds small categories to zero. The trade-off: the connection streams only `CONCURRENT_TICK_RADIUS` (3) columns, so caps fill about six times denser than the reference; widening the streamed radius restores density.
- **Census** counts every mob by category except persistence-required ones (`SimMob::is_persistence_required`: name tag or saved flag). A persistent category such as a cow still counts. `SimMob::is_persistent` is the wider never-despawns flag.
- **Persistent categories** (creatures) only attempt on ticks divisible by 400; the rest every tick. Land animals mostly come from generation ([`worldgen-mob-generation-spawn.md`](worldgen-mob-generation-spawn.md)).
- **Position** reads the retained surface heightmap via `ChunkWorld::surface_y`: stored height is a relative first-free cell, so highest occupied Y is `min_y + stored_height - 1`. Water, leaves and plants count as surface; air does not. Columns without a map use a scalar fallback.
- **Peaceful** is two gates keyed on the per-type exemption flag (`mob_spawn::allowed_in_peaceful`), not on category: seven monsters survive Peaceful (`piglin`, `shulker`, `ender_dragon`, `zombie_horse`, `zombie_nautilus`, `camel_husk`, `sulfur_cube`). One refuses the candidate before the species predicate (which draws RNG); the other evicts the living.

**Light.** `natural_spawn` samples light with `lodestone_world::compute_column_light` over palette indices, bounded by `LIGHT_BUDGET_PER_CYCLE` (4 columns per tick) and `LIGHT_TTL_TICKS` (200 ticks, then dropped wholesale, so a torch suppresses spawns within about 10 s). An unlit column returns `None`, meaning do not spawn, never "dark", or the budget becomes a spawn-rate multiplier.

`NaturalSpawner::set_environment` receives dimension and rain/thunder intensities from the shared tick owner. Sky light is absent in the Nether. The Overworld's 24,000-tick sky-level track has multiplier 1 at ticks 133 and 11867 and 0.26666668 at 13670 and 22330, interpolated across the boundary; times 15 that is daylight 15 and night 4. Rain blends toward 4 with alpha 0.3125, thunder 0.52734375 (thunder intensity times rain intensity; ordinary rain the remainder). Sky darkening is the integer truncation of `15 - sky_level`.

Dark-monster rules keep the preliminary raw-sky random test, then use per-dimension block-light limits and final thresholds: Overworld 0 and uniform `[0,7]`, Nether 15 and constant 7, End 0 and constant 15. Brightness is the maximum of block light and sky light minus darkening; effective thunder above 0.9 forces darkening 10. Bats and surface slimes use ordinary darkening; animals and glow squid sample undarkened light. A dimension change clears cached light. See [dimension runtime](dimension-runtime.md).

**Slime chunks** are two alternatives: a swamp arm (`swamp`/`mangrove_swamp`, `50 < y < 70`, a draw under the moon-phase chance, then a light draw) and a slime-chunk arm (seed-derived stream mixed with `987234911`, one in ten; `y < 40`; plus an unconditional one-in-ten draw consumed even in ordinary chunks). The surface chance is a moon-phase attribute, 0.0 at new moon to 0.5 at full. Two seeds reach the spawner: a fixed `NATURAL_SPAWN_SEED` for the stream and the real world seed (via a process-global) for `is_slime_chunk`.

**Water placement.** Surface water animals (cod, salmon, pufferfish, squid, dolphin) need water below, plain water above (`SpawnRule::water_above`) and Y in `[sea - 13, sea]`; nautilus `[sea - 25, sea - 5]`; glow squid `y <= sea - 33` in darkness; tropical fish the surface band except in lush caves. Drowned: one in 15 with no depth gate in `river`/`frozen_river`, otherwise one in 40 and strictly below `sea - 5`. River biomes skip 98% of water-ambient picks. `SEA_LEVEL` is 63.

Known omissions: the Nether-fortress list override (needs a live structure manager), the Nether `spawn_costs` calculator (parsed, unread), spawning in Open-to-LAN (no terrain to read), and `is_valid_spawn_surface`, which approximates a sturdy-face test as a full collision cube emitting under 14 light and so rejects slabs and stairs the reference accepts.

### Biome spawn tables

`crates/lodestone-worldgen/src/spawners.rs` parses the `spawners`/`spawn_costs` of every biome document from the same `Resolver::biome_document` as the climate parser. Answers live in a `BuiltinBiome`-indexed table; a biome with neither field is absent, not empty. Across the 66 bundled documents: 795 entries, non-empty in `monster` (63 biomes), `ambient` (54), `underground_water_creature` (53), `creature` (43), `water_ambient` (13), `water_creature` (11), `axolotls` (1), `misc` (0); `spawn_costs` in 5, all Nether. `MobCategory::parse` panics on an unknown key; use `MobCategory::ALL` for declaration order.

### Spawn equipment

`lodestone_entity::spawn_equipment` has one function per species family:

| species | calls base? | own addition |
|---|---|---|
| unlisted | n/a | `base_armor_roll` alone |
| zombie / husk / zombie_villager | yes | 1% (5% Hard) iron sword/spear/shovel, weights 1/6, 1/6, 4/6 |
| drowned | no | 10% chance of a weapon, 10/16 of that a trident (6.25% overall), else a fishing rod |
| skeleton / stray / bogged / parched | yes | bow |
| wither_skeleton | no | stone sword |
| pillager | no | crossbow |

`base_armor_roll`: `0.15 * special_multiplier` chance of armour, an `armor_type` in `0..=5` (a roll among three bases plus up to three +1 bumps at 10.87%), then a walk over head, chest, legs, feet stopping at the first filled slot (10% Hard, else 25%) without overwriting. `EquipRandom` keeps the crate free of a concrete RNG. The drowned's trident goal is gated at runtime on holding a trident (`RangedAttackGoal::with_required_main_hand("trident")`). `MobSim::spawn_species` rolls on its own `equipment_rng` stream (`EQUIPMENT_ROLL_SEED`) so one roll cannot shift another stream. Not modelled: enchanted gear, equipment surviving save/load, and an iron-spear entry in `equipment::weapon_attack_damage`.

`EquipmentSlots` stores `lodestone_data::item::Item`; `equipment::apply_equipment` takes it typed, with the string form as the dynamic player-inventory boundary. Built-in keys resolve to the generated `EntityType`; extension keys take a generic inherited-equipment fallback.

### Species-aware spawning

`MobSim::spawn_species` resolves body, stats and baseline goals from the real species by folding `default_attributes` (from the verified `type_spec` table), `species_shape` (dimension census plus scale and step height, falling back to `MobShape::land(0.6, 1.95)`; also per-species door opening, floating, fence walking and pathing maluses) and `is_hostile_species` (a coarse classifier for category and despawn persistence only; goal sets live in `lodestone_entity::ai::roster`). An unclassified roster species fails loudly.

The attribute table covers every built-in species with a natural-spawn registration. `movement_speed` is seeded explicitly (some correctly stay at the registry 0.7, so verify the instance exists as well as its value). When adding a natural-spawn species, update `type_spec` and the coverage cases in `lodestone_entity::attribute` from its own definition.

**Movement speed is not blocks per tick.** The navigator holds the movement-speed attribute (times the baby multiplier); goals pass `modifier * attribute`, and `lodestone_entity::ai::locomotion` turns that into thrust (speed squared on ground, `0.02 * speed` airborne or in fluid; scaled up on slippery blocks). Velocity carries between ticks and decays by block friction times 0.91 on the ground (0.91 air, 0.8 water, 0.5 lava), after the block speed factor (soul sand, honey). Cruise on stone is `speed^2 / 0.454`; live, a zombie (0.23) chasing a villager measured about 0.118 blocks per tick against 0.1165. Knockback is a velocity and decays the same way; `displace` is the one-shot position nudge for pushes, leashes and pistons.

The zombie family's door-breaking coin flip (rolled once at spawn, kept across baby/adult changes) and Hard-only reinforcements (rolled on a landed hit through a simplified 50-candidate search) use the world's real difficulty. The leader-zombie bonus is not modelled.

### Spawn eggs

`spawn_egg.rs` answers three questions in click-handling order. **Which entity:** `entity_type_for_egg` strips `_spawn_egg` and requires a real `entity_types` entry (all 88 egg-to-entity pairs match). **Where:** the clicked cell if collision-free, else the neighbour across the face; sub-cell height `y_offset` is `max(0.0, top)` for a side click and `max(-1.0, top)` for a top click, where `top` is the highest collision surface. **Refused or not mine:** `NotSpawnEgg` falls through to placement; `Refused` (unknown type or Peaceful) consumes and places nothing; `Spawn` consumes the stack only on success.

`apply_spawn_egg` composes this with `MobSim::spawn_species`. Test for a spawner block click before this dispatch, since it still reports `Spawn`; it re-keys the block entity instead. A dispenser reuses `entity_type_for_egg` alone. Right-click is wired end to end (`ServerBound::InteractEntity` to `MobSim::interact`). A spawner block always has a `BlockEntity::Spawner` (data-less is an empty one); the tick loop's spawner pass ticks each resident spawner, and one waiting on a cold column does not block the rest. `spawner_blocks_work` gates them. Trial spawners are not modelled; random spawn yaw and the regional-difficulty equipment pass are missing.

### Breeding and aging

`NavigatingMob` owns `love_ticks` (`LOVE_TICKS` 600, decremented unconditionally), `age` (negative as a baby from `BABY_START_AGE` -24,000; positive post-breeding cooldown from `PARENT_AGE_AFTER_BREEDING` 6,000; `is_baby()` is `age < 0`), `age_locked`, and host-injected `partner_candidate`/`parent_candidate`. A golden dandelion on a baby toggles `age_locked` via the `AgeLockToggled` outcome, resetting age to `BABY_START_AGE` and starting a 40-tick cooldown; villagers and the two undead horses refuse it (`cannot_be_age_locked`). Timer and lock persist as `Age`/`AgeLocked` through `MobSim::saved_entities`/`restore_saved`. `MobSim::feed_perception` does the candidate search; `MobSim::resolve_breeding` turns a drained `take_bred()` into a child, a parent cooldown on both and a 1-7 XP orb (gated on `mob_drops`, constructed directly because `award_experience` would merge it with a nearby orb).

**Baby shape and speed.** `species_shape` takes `is_baby`: a species with a `baby_dimensions` entry uses it (a baby zombie is 0.49x0.98), otherwise `DEFAULT_BABY_AGE_SCALE` (0.5). `SimMob::set_age` pushes new shape and speed on a baby/adult crossing, so spawn and breeding share one update point. `baby_speed_multiplier` gives the zombie family its +0.5 multiplicative bonus (base x 1.5); other breedables only shrink. Health, attack and armor do not vary with age. `MetadataField::Baby` is pushed unconditionally (a grown-up baby must update connected clients) at metadata index 16, which also carries the creeper's swell direction, so the species guard is in `SimMob::snapshot`, not the encoder.

Not modelled: a persisted held partner (selection re-searches every tick and can thrash), advancements, and food-item feeding (call `set_in_love()` directly).

### Taming

Taming and breeding share `PerceivedPlayer { identity: Option<PlayerIdentity>, perception }`: a `uuid` (what ownership keys on, surviving reconnects) and an `entity_id` handle. Ownership (`owner: Option<MobOwner>`) and `tame: bool` are separate, since a tame pet whose owner logged out has no resolvable position but stays tame.

| species | trigger | roll | sits? |
|---|---|---|---|
| wolf | bone, not while angry | 1 in 3 | yes |
| cat | `#cat_food` (raw cod/salmon) | 1 in 3 | yes |
| parrot | `#parrot_food` (six seeds) | 1 in 10 | no |
| horse family | being ridden, not fed | roll against current vs max temper | n/a |

- The wolf's taming item is in none of its food tags, so `breeding_food` and `tame_mechanism` item sets stay separate. Whether a species must be tamed to breed depends on whether the taming item overlaps its food tag: an untamed wolf fed meat can fall in love; an untamed cat fed cod always tries to tame.
- The horse roll uses a persisted temper counter (certain to fail at 0, succeed at 100, +5 per failure) that does not follow `#horse_food`: `hay_block` grants none and `red_mushroom` grants 3, so `horse_temper_gain` is its own table. Horse breeding needs `GOLDEN_CARROT`/`GOLDEN_APPLE`/`ENCHANTED_GOLDEN_APPLE`, so its `breeding_food` row is empty. With no passenger model the roll is attempted once per mount rather than behind the 1-in-50 ridden tick gate.
- `MobSim::interact` keeps each species' clause order: feeding a hurt tame wolf meat heals it before it can fall in love, and the sit toggle is last, so anything above suppresses it. The love gate is `age == 0`, not `!is_baby()`.
- `SitWhenOrderedToGoal` (a tame pet with no resolvable owner sits) and `FollowOwnerGoal` with `(speed, start_distance, stop_distance)` per species (wolf 1.0/10/2, cat 1.0/10/5, parrot 1.0/5/1) make ownership observable. The sitting order (persisted as `Sitting`) and pose (synced flag bit) are separate state.
- The right-click reaches a real client with a particle burst as a disclosed substitute for the client-expanded entity event. The collar is on the wire (`MetadataField::TamableFlags`/`HorseFlags`, index 18, with four byte claimants each using a different bit, so a shared variant would set an unnamed bit), but the client has no tame/sit decode or texture path; see [`entity-rendering.md`](./entity-rendering.md).

### Custom entity types

`lodestone_ecs::entity_spawn::CustomEntityRegistry` (via `App::add_custom_entity_type`) maps a plugin kind (`myplugin:sentry`) to the real type it streams as (`minecraft:armor_stand`). Registration refuses a `minecraft:` custom kind, a duplicate, and any disguise that is not a real entity type (network id 0 is a boat, so an unresolvable disguise must fail at registration). `spawn_custom_entity` carries the disguise in the entity's kind; `lodestone-mob-spawner` exercises it.

## How to change it

- **Spawn rule:** each row transcribes a per-species registration plus its placement predicate; read the predicate, because families differ (wolf: block tag and brightness above 8; bat: stone below, a coin flip and brightness at most a `0..4` draw; zombified piglin: no light test). A species absent from the table is deliberately inert; a fallback would spawn guardians on land.
- **Tameable species:** an arm in `tame_mechanism`, a row in `breeding_food`, and a roster entry for sit/follow. A new mechanism is a new `TameMechanism` variant. The horse family is tameable with no roster entry because its base class is outside the tamable hierarchy.
- **Never derive an item set from a tag** without checking the logic it gates; three traps were tags disagreeing with the real gating (bone vs `#wolf_food`, `hay_block`, `red_mushroom`). A chance gate needs a seed where the mechanisms being separated disagree.
- **Entity metadata indices are not hand-countable** and RNG call order must match exactly: take indices from the entity-data-index oracle and check every class sharing one; reordering `next_f32`/`next_int` calls changes a fixed seed.
- **NBT names collide across types** (`Age` is a `Short` on an item and an `Int` on a mob; `Health` a `Float` vs a fixed `Short`). Deciding which fields to keep from a static name list, rather than whether that type's decode consumed the field, once turned every saved baby into an adult on load.
- `cargo test -p lodestone-server --test generation_population_client --no-fail-fast` drives generation candidates through the production tick, protocol 776 and the client ECS, with a no-batch control. It does not establish generator selection, wire parity or pixels.

## Configuration

| knob | where | default |
|---|---|---|
| `spawn_mobs`, `mob_drops`, `spawner_blocks_work` | game rules (`world_state::WorldStateHandle`) | on |
| `LIGHT_BUDGET_PER_CYCLE` / `LIGHT_TTL_TICKS` | `natural_spawn.rs` | 4 columns per tick / 200 ticks |
| `NATURAL_SPAWN_SEED` | `tick.rs` | fixed literal |
| `set_environment(dimension, rain, thunder)` | `NaturalSpawner` | Overworld, clear |
| `EQUIPMENT_ROLL_SEED`, `TAME_ROLL_SEED`, `BREED_XP_SEED` | `mobs/mod.rs` | default seeds |
| `set_tame_rng`, `set_equipment_rng`, `set_temper`, `set_spawn_difficulty(special_multiplier, hard)` | `MobSim` | test overrides and difficulty feed |
| `LOVE_TICKS` / `BABY_START_AGE` / `PARENT_AGE_AFTER_BREEDING` / `DEFAULT_BABY_AGE_SCALE` | `lodestone_entity::ai` | 600 / -24,000 / 6,000 / 0.5 |

## Dependencies

- `lodestone_world` (column light), `lodestone_data` (`light_props`, `block_states`, `collision_shapes`, `entity_dimensions`, `entity_types`), `lodestone_worldgen::spawners` via `worldgen_data::bundled_biome_spawners()`, `lodestone_entity::attribute` and `lodestone_entity::ai` (`MobController`, `NavigatingMob`, roster goals).
- `crate::mob_spawn` (cap/despawn engine, `SpawnRng`), `crate::mobs` (`MobSim`/`SimMob`), `crate::effects::WorldEffect`, `crate::tick::run_tick_loop`.
- `.cache/mc/` pinned decompile that the tables are checked against. Related: [`combat.md`](./combat.md), [`mob-ai.md`](./mob-ai.md), [`entity-rendering.md`](./entity-rendering.md).
