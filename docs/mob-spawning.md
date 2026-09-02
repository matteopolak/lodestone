# Mob spawning

## What it is

Everything that puts a mob into a live world and gives it a life after that: the natural spawn
cycle and its per-biome tables, what a naturally spawned mob holds/wears, species-aware body and
goal resolution, spawn eggs, breeding and baby growth, taming, and the registry for adding a
wholly custom entity type. Most of it lives under `crates/lodestone-server/src/mobs/` and
`crates/lodestone-server/src/natural_spawn.rs`, with entity-side timing state in
`crates/lodestone-entity/src/ai/navigating_mob.rs`.

## How it works

### Natural spawn cycle

`tick::run_tick_loop`, once per tick, gated on the `spawn_mobs` game rule and skipped when no
player is loaded: `MobSim::census` rebuilds `SpawnState` from the live population; for each chunk
still under its per-`MobCategory` cap, `NaturalSpawner::cluster` runs vanilla's own
per-chunk spawn-category algorithm and returns a **group**, not a single candidate — the RNG draw order and
count *is* the spawn rate, so the cap is applied as the group is consumed rather than mid-draw.
Each candidate becomes a real mob through `MobSim::spawn_species`, so it gets the species' real
dimensions, attributes and goals; the spawn **category** comes from the biome list's own key, not
a hostile/friendly guess. `MobSim::despawn_pass` runs beside it against the nearest player. Caps
scale with the tick area actually simulated (49 columns → 11 monsters, 1 creature), not vanilla's
289-column figure.

**Peaceful** is two gates keyed on the per-type peaceful-exemption flag
(`mob_spawn::allowed_in_peaceful`), never on `MobCategory == MONSTER` — vanilla keeps seven
monsters alive on Peaceful (`piglin`, `shulker`, `ender_dragon`, `zombie_horse`,
`zombie_nautilus`, `camel_husk`, `sulfur_cube`). One refuses the candidate before the species'
predicate runs (order matters, since the predicate draws from the RNG); the other,
`MobSim::remove_monsters`, evicts what's already alive using the same classification.

**Light.** Every monster rule is a light test. `natural_spawn` computes it via
`lodestone_world::compute_column_light` over the column's palette indices (one lookup per palette
entry), bounded by `LIGHT_BUDGET_PER_CYCLE` (4 columns/tick) and `LIGHT_TTL_TICKS` (200 ticks,
then dropped wholesale — there's no per-block relight in this tree, so a torch suppresses spawns
within ~10 s rather than instantly). An unlit column returns `None`, meaning **do not spawn**,
never "treat as dark" — that would turn the budget into a spawn-rate multiplier.

**Slime chunks** are the one predicate that's two alternatives rather than a conjunction: a
swamp-surface arm (`swamp`/`mangrove_swamp`, `50 < y < 70`, `nextFloat() < surfaceSlimeSpawnChance`
then a light draw) and a slime-chunk arm (`seedSlimeChunk(cx, cz, worldSeed, 987234911).nextInt(10)
== 0`, `y < 40`, plus an unconditional `nextInt(10) == 0` consumed even in an ordinary chunk).
`SURFACE_SLIME_SPAWN_CHANCE` is a moon-phase attribute, not a constant — `0.0` at new moon (the
surface arm can't fire) up to `0.5` at full moon. Two seeds reach the spawner: a fixed
`NATURAL_SPAWN_SEED` literal for the spawn stream, and the real world-gen seed for
`is_slime_chunk` (must match the seed the terrain generated under, via a process-global — there's
no `world_seed()` on `ChunkSource`).

Known omissions: the drowned/water-ambient biome-tag rate modifiers, the nether-fortress list
override (needs a live structure manager), the Nether-only `spawn_costs` calculator (parsed,
unread), ambient sky darkening (no world clock reaches the spawner, so brightness always reads as
day — conservative, since brighter only ever suppresses spawns). Open-to-LAN spawns nothing (its
`MobHandle` has no terrain to read). `is_valid_spawn_surface` approximates a full sturdy-face test
as "a full collision cube emitting under 14 light" — rejects slabs/stairs vanilla would accept.

### Biome spawn tables

`crates/lodestone-worldgen/src/spawners.rs` parses the `spawners`/`spawn_costs` fields every 26.2
biome document carries, from the same `Resolver::biome_document` value the climate parser already
consumes. `OverworldGenerator::biome_spawners(biome)` exposes the per-biome answer; a biome with
neither field is **absent** from the map, not stored empty. Measured across the 66 bundled
documents: 795 spawner entries, non-empty in `monster` (63 biomes), `ambient` (54),
`underground_water_creature` (53), `creature` (43), `water_ambient` (13), `water_creature` (11),
`axolotls` (1), `misc` (0); `spawn_costs` non-empty in 5 (all Nether).

`MobCategory::parse` panics on an unknown key rather than dropping it silently. `weight` belongs
to the outer `WeightedList` wrapper, not vanilla's `SpawnerData` record, though this table
flattens the two; fields are read by name, so record order versus the JSON's alphabetical keys is
inert. Not modelled: `SpawnerData`'s `MISC`→`PIG` rewrite (unreachable anyway, since every 26.2
`misc` list is empty) and count validation (embedded generated assets, so a violation would be a
build defect). Use `MobCategory::ALL` for declaration order, not incidental map order.

This is data, not a runtime spawner — the chunk-generation `SPAWN` stage, light/ground
re-validation, and an entity-persistence decision are separate, larger work, mostly because most
`Creature` rules need block light and an un-persisted spawn would silently re-run on every regen.

### Spawn equipment

`lodestone_entity::spawn_equipment` ports `Mob.populateDefaultEquipmentSlots` and its overrides,
one function per species family:

| species | calls `super`? | own addition |
|---|---|---|
| unlisted (fallback) | — | `base_armor_roll` alone |
| zombie / husk / zombie_villager | yes | 1%/5%(Hard) chance of iron sword/spear/shovel, weights 1/6, 1/6, 4/6 |
| drowned | no | 10% chance of a weapon, 10/16 of that a trident (6.25% overall), else a fishing rod |
| skeleton / stray / bogged / parched | yes | unconditional bow |
| wither_skeleton | no | unconditional stone sword |
| pillager | no | unconditional crossbow |

`base_armor_roll`: `0.15 * special_multiplier` chance of any armour, an `armor_type` in `0..=5`
(`nextInt(3)` plus up to three `+1` bumps at 10.87% each), then a walk over
`[Head, Chest, Legs, Feet]` stopping at the first slot filled (10% Hard / 25% otherwise chance),
never overwriting an occupied slot. `EquipRandom` is the RNG seam so this crate stays free of a
concrete RNG type. The drowned's trident goal is gated at *runtime* on holding a trident rather
than by conditional registration (`MobController::main_hand_item()` plus
`RangedAttackGoal::with_required_main_hand("trident")`). `MobSim::spawn_species` rolls equipment on
its own `equipment_rng` stream (`EQUIPMENT_ROLL_SEED`), isolated from despawn/orb/tame streams so
one roll can't shift another's outcome. Not modelled: enchanted spawn gear (no enchantment model
exists at all) and equipment surviving save/load (no equipment NBT); `Items.IRON_SPEAR` has no
entry in `equipment::weapon_attack_damage` (a combat-stats gap, not an equipping one).

### Species-aware spawning

`MobSim::spawn_species` resolves a mob's body, combat stats and baseline goals from its real
species, replacing the old hardcoded-to-`minecraft:zombie` path. It folds `default_attributes`
(health/attack/armor from a hand-verified `type_spec` table), `species_shape` (the 26.2 dimension
census plus `SCALE`/`STEP_HEIGHT`, falling back to `MobShape::land(0.6, 1.95)` for an unknown
species), and `is_hostile_species` (a coarse classifier deciding only spawn category and despawn
persistence — per-species goal sets belong to `lodestone_entity::ai::roster`). `species_shape`
also sets `can_open_doors`/`can_float`/`malus_overrides` per species from that species' own
constructor or `finalizeSpawn`; before this every mob used `MobShape::land`'s defaults
(no door-opening, no floating, no fire/lava/water aversion) unconditionally. `is_hostile_species`
is checked against a jar-cited table so an unclassified roster species fails loudly rather than
defaulting silently.

**Movement speed is not read as blocks/tick directly.** Vanilla's `Mob.setSpeed` sets both the
per-tick speed scale *and* the forward-input magnitude a mob's move vector multiplies, so real
per-tick thrust is the *square* of `speedModifier * movement_speed`, converging under friction
(ground `0.6`, air drag `0.91`) to `requested_speed² / (1 - 0.6 * 0.91)`. `ai_ground_speed`
implements that conversion for the kinematic follower's `step_per_tick`; roster goals still
receive the *unconverted* attribute. Checked live: a zombie (`0.23`) chasing a stationary villager
measured ≈0.118 blocks/tick against a predicted `0.1165` — the unconverted attribute is roughly
double either figure, matching a long-standing "mobs move too fast" report.

Also resolved per tick from a real `DifficultyInstance`: the zombie family's door-breaking coin
flip (rolled once at spawn, preserved across baby/adult shape changes) and its Hard-only
reinforcement call (randomized per mob at spawn, rolled on a landed hit, resolved through a
simplified 50-candidate placement search). Not modelled: the "leader zombie" bonus that can also
force door-breaking or boost stats.

### Spawn eggs

`spawn_egg.rs` answers three questions in vanilla's order (`SpawnEggItem.useOn`): **which entity**
(`entity_type_for_egg` strips the `_spawn_egg` suffix and requires a real `entity_types` entry —
checked against all 88 vanilla `registerSpawnEgg` pairs, zero mismatches, refusing rather than
naming something nothing can render); **where** (the clicked cell if empty of collision, else the
neighbour across the clicked face; sub-cell height `y_offset` is `max(0.0, top)` for a side click
and `max(-1.0, top)` for a top click, where `top` is the highest collision surface found — a
bottom slab, not a full cube, is what actually discriminates this from a hardcoded `0.0`); and
**refused or not mine** (`NotSpawnEgg` falls through to block placement; `Refused` — unknown type,
or Peaceful — consumes nothing and places nothing; `Spawn` proceeds, consuming the stack only on
success).

`apply_spawn_egg` composes the decision with `MobSim::spawn_species`, so an egg-spawned mob is the
same object a natural spawn produces. Not modelled: random spawn yaw, and `Mob.finalizeSpawn`'s
regional-difficulty equipment pass. A dispenser reuses `entity_type_for_egg` alone; clicking a
spawner block re-keys the block entity instead and must be tested for **before** this dispatch,
since it still reports `Spawn` for that click. The right-click path is wired end to end
(`ServerBound::InteractEntity`/`INTERACT` → `MobSim::interact`). Spawner blocks remain a
`BlockEntity::Opaque` with no tick — `BaseSpawner.serverTick` needs block-entity access to
`MobSim` that doesn't exist yet.

### Breeding and aging

`NavigatingMob` owns the timing state vanilla keeps on `Animal`/`AgeableMob`: `love_ticks`
(`LOVE_TICKS` = 600, decremented every tick unconditionally); `age` (negative while a baby, from
`BABY_START_AGE` = -24,000; positive as a post-breeding cooldown, from `PARENT_AGE_AFTER_BREEDING`
= 6,000; `is_baby()` is `age < 0`); `age_locked`; and `partner_candidate`/`parent_candidate`,
host-injected once per tick since this crate can't search a mob population itself.
`MobSim::feed_perception` performs that search; `MobSim::resolve_breeding` resolves a drained
`take_bred()` into a real child spawn, the parent-age cooldown on both parents, and an experience
orb (1–7 XP, gated on `mob_drops`; constructed directly rather than through `award_experience`,
which would silently merge with a nearby existing orb).

**Baby shape and speed.** A baby used to keep its species' adult hitbox and speed forever.
`species_shape` now takes `is_baby: bool`: a species with a `baby_dimensions` entry uses that
literal (a baby zombie is 0.49×0.98, not a halved 0.6×1.95); anything else falls back to
`DEFAULT_BABY_AGE_SCALE` (0.5). `SimMob::set_age` detects a baby/adult boundary crossing and
pushes the new shape/speed into the live mob, so spawn and breeding share one update point.
`baby_speed_multiplier` carries the zombie family's `0.5` `ADD_MULTIPLIED_BASE` modifier
(`base * 1.5`) — the only baby speed change among species this sim breeds; every breedable
`Animal` (cow, sheep, pig, chicken, rabbit, wolf) only shrinks. `combat_defaults` deliberately did
**not** gain an `is_baby` parameter — checked against every attribute builder, health/attack/armor
never vary with age. `MetadataField::Baby` is pushed unconditionally, not only while true (a baby
that grows up must tell already-connected clients), for the breedable-animal and zombie families,
at metadata index 16 — which also carries `Creeper.DATA_SWELL_DIR`, so the guard lives in the
producer (`SimMob::snapshot`'s species switch), not the encoder.

Not modelled: a persisted "held partner" (selection is a fresh nearest-candidate search every
tick, which can thrash with several same-species animals in love at once); XP/stat/advancement
triggers beyond the orb; food-item feeding (`set_in_love()` must be called directly unless reached
through the taming arm below).

### Taming

Taming and breeding share one seam: a player identity travels alongside `PlayerPerception` as
`PerceivedPlayer { identity: Option<PlayerIdentity>, perception }`, carrying a `uuid` (what
ownership is keyed on, the only identity that survives a reconnect) and an `entity_id` handle
(what this sim's `i32`-keyed state, like attack targets, actually speaks). An unidentified player
owns nothing. Ownership (`owner: Option<MobOwner>`) and tameness (`tame: bool`) are deliberately
separate — a tamed pet whose owner logged out keeps its owner but has no resolvable position, and
is still tame.

Four taming mechanisms, transcribed per species:

| species | trigger | roll | sits? |
|---|---|---|---|
| wolf | `Items.BONE`, not while angry | `nextInt(3) == 0` | yes |
| cat | `#cat_food` (raw cod/salmon) | `nextInt(3) == 0` | yes |
| parrot | `#parrot_food` (six seeds) | `nextInt(10) == 0` | no |
| horse family | being ridden, not fed | `nextInt(getMaxTemper()) < getTemper()` | n/a |

The wolf's taming item is in **none** of its own food tags, so `breeding_food` and
`tame_mechanism`'s item sets must stay separate. `Parrot.tryToTame` uniquely omits sitting on
tame. Whether a species must be tamed before it can breed depends on whether its taming item
overlaps its food tag: an untamed wolf fed meat misses the bone arm and really can fall in love;
an untamed cat fed cod always attempts a tame instead. The horse's roll is a function of a
persisted `Temper` counter (certain to fail at 0, succeed at 100, each failure adding 5) that
doesn't derive from `#horse_food` — `hay_block` is horse food and grants no temper, `red_mushroom`
grants 3 without being in the tag — so `horse_temper_gain` is its own table. Horse breeding needs
`GOLDEN_CARROT`/`GOLDEN_APPLE`/`ENCHANTED_GOLDEN_APPLE` specifically, so its `breeding_food` row is
empty. With no passenger model here, the temper roll is attempted once per mount rather than
behind vanilla's 1-in-50 ridden tick gate — the roll arithmetic itself is unchanged.

`MobSim::interact` transcribes each species' `mobInteract` override in the **same clause order**:
feeding a hurt tame wolf meat must heal it before the same item can put it in love, and the sit
toggle is the last arm, so anything above it suppresses the toggle. The love gate is `age == 0`,
not `!is_baby()` — a parent in its post-breeding cooldown is not a baby and still cannot fall in
love.

Two roster goals make ownership observable: `SitWhenOrderedToGoal` (a tame pet with no resolvable
owner sits on its own — "pets settle when you log out") and `FollowOwnerGoal`, whose
`(speed, startDistance, stopDistance)` are arguments because vanilla's aren't uniform (wolf
1.0/10/2, cat 1.0/10/5, parrot 1.0/5/1). The sitting *order* (persisted, NBT round-trips as
`Sitting`) and sitting *pose* (the synced flag bit) are separate state, matching vanilla —
collapsing them loses the order whenever the goal is preempted by a higher-priority flag holder.

The right-click reaches a real client, and a tame/love/fail cue reaches the wire as a particle
burst — a disclosed substitution for vanilla's client-expanded entity event. The collar itself is
on the wire (`MetadataField::TamableFlags`/`HorseFlags`, index 18 — the most crowded index in the
game, four `BYTE` claimants each with a different bit per class, so a shared variant would set an
*unnamed* bit and silently read as untamed), but nothing past the server decodes it: the client's
metadata reader, event model, ECS ingest and texture resolver have no tame/sit path, even though a
tame wolf texture already exists with no production caller — see
[`entity-rendering.md`](./entity-rendering.md).

### Custom entity types

`lodestone_data::entity_disguise` maps a plugin's own entity kind (`myplugin:sentry`) to the real
vanilla type it streams as on the wire (`minecraft:armor_stand`) — there's no room in the protocol
for a novel registry entry, and vanilla itself has no such mechanism either. Resolution order: a
real vanilla type resolves to itself, a registered disguise resolves to its target, anything else
is `None` — **never** a fallback, because network type id `0` is `minecraft:acacia_boat`, and an
unresolved type used to stream as one with no error anywhere. `EntityDisguises::register` resolves
the target eagerly and refuses rather than allow a disguise that would silently become a boat, and
a custom kind is barred from the `minecraft:` namespace so it can never shadow a real type. It
lives in `lodestone_data` because that's the only crate both the client and `lodestone-server` can
reach.

**This registry has no production consumer yet.** The add-entity encoder still resolves a type
with the unchecked fallback this registry exists to replace, and nothing between a mob's snapshot
and the encoder applies a disguise; `SimMob::set_entity_type` is a cruder per-entity override
available today. Until wired — and until a server-side spawn API exists at all — a plugin can
define and validate a custom kind but cannot spawn one.

## How to change it

* **Adding a spawn rule**: every row transcribes `SpawnPlacements.java`'s registration plus the
  `check*SpawnRules` body it names — read the predicate, families genuinely differ (a wolf wants a
  block tag and brightness > 8; a bat wants base stone below, `nextBoolean()`, brightness ≤
  `nextInt(4)`; a zombified piglin has no light test at all). A species absent from the table is
  deliberately inert, not a fallback to "no restrictions" — a fallback would spawn guardians on
  land.
* **Adding a tameable species**: an arm in `tame_mechanism`, a row in `breeding_food`, and — if it
  should sit/follow — a roster entry at that species' own priorities/distances (the horse family
  is tameable but has no roster entry, since `AbstractHorse` isn't a `TamableAnimal` at all — not a
  gap). A new taming *mechanism* is a new `TameMechanism` variant, not a constant on an existing
  one — the four differ in trigger, roll shape and side effects.
* **Never derive an item set from a tag without checking the method it actually gates**, and a
  tame or spawn chance needs a driven RNG in its gate. Three traps here are exactly a tag and a
  Java method disagreeing (bone vs. `#wolf_food`, `hay_block` vs. `handleEating`'s temper grant,
  `red_mushroom` vs. `#horse_food`) — read `handleEating`/`mobInteract` first, use the tag only as
  a cross-check. A chance gate needs a seed where the two mechanisms being separated actually
  disagree — one chosen for `next_int(3)` proves nothing about `next_int(10)`.
* **Entity metadata indices are not hand-countable, and RNG call order in a port must match
  exactly.** Any new per-species metadata must take its index from the entity-data-index oracle
  dump and check every class sharing it — assuming a previous collision's guard generalises is
  exactly how a wrongly-classified mob ships. A reordered pair of `next_f32`/`next_int` calls
  (equipment, placement, taming) changes what a fixed seed produces, even without any promise of
  matching a real vanilla server byte-for-byte.
* **NBT field names collide across entity/item types, and a name-keyed schema must not assume they
  mean the same thing.** `Age` is a `Short` on `minecraft:item` (ticks alive) but an `Int` on a mob
  (breeding age, negative for a baby); `Health` is a `Float` on a mob and a fixed `Short` on an
  item. Deciding which fields to keep by checking a static modelled-field name list — rather than
  whether the decode for *that type* actually consumed the field — silently drops a field it
  failed to decode under the wrong type; this is the exact shape that once turned every saved baby
  mob into an adult on load.

## Configuration

| knob | where | default |
|---|---|---|
| `spawn_mobs` game rule | `world_state::WorldStateHandle` | `true` |
| `LIGHT_BUDGET_PER_CYCLE` / `LIGHT_TTL_TICKS` | `natural_spawn.rs` | 4 columns/tick / 200 ticks |
| `NATURAL_SPAWN_SEED` | `tick.rs` | fixed literal (reproducible, not world-derived) |
| `EQUIPMENT_ROLL_SEED`, `TAME_ROLL_SEED` / `BREED_XP_SEED` | `mobs/mod.rs` | default RNG seeds |
| `set_tame_rng` / `set_equipment_rng` | `MobSim` | override for a gate needing a known first draw |
| `set_spawn_difficulty(special_multiplier, hard)` | `MobSim` | feeds equipment, door-breaking, reinforcements |
| `set_temper` | `MobSim` | stage a horse at a chosen temper directly |
| `mob_drops` game rule | — | gates the breeding experience orb |
| `LOVE_TICKS` / `BABY_START_AGE` / `PARENT_AGE_AFTER_BREEDING` | `lodestone_entity::ai` | 600 / -24,000 / 6,000 |
| `DEFAULT_BABY_AGE_SCALE` | `lodestone_entity::ai` | 0.5 |

No feature flags anywhere in this area. Spawner blocks have no analogue of vanilla's
`isSpawnerBlockEnabled` game setting yet.

## Dependencies

* `lodestone_world` — the column light engine `natural_spawn` uses for monster rules.
* `lodestone_data` — `light_props`, `block_states`, `collision_shapes`, `entity_dimensions`,
  `entity_types`, `entity_disguise`.
* `lodestone_worldgen::spawners` via `worldgen_data::bundled_biome_spawners()` — the per-biome
  lists, parsed once and cached. `lodestone_entity::attribute` — `default_attributes`/`type_spec`.
* `lodestone_entity::ai` — `MobController`, `NavigatingMob`, `roster` goal tables (`BreedGoal`,
  `FollowParentGoal`, `SitWhenOrderedToGoal`, `FollowOwnerGoal`, `RangedAttackGoal`).
* `crate::mob_spawn` (cap/despawn engine, `SpawnRng`), `crate::mobs` (`MobSim`/`SimMob`, the
  production host for all of the above), `crate::effects::WorldEffect` and
  `crate::tick::run_tick_loop` (taming particle burst, per-tick difficulty feed).
* `.cache/mc/26.2/src/net/minecraft/world/entity/` — the pinned decompile every table above is
  checked against.
* [`combat.md`](./combat.md) (the attribute-modifier fold and damage/knockback once a mob is
  live), [`mob-ai.md`](./mob-ai.md) (the goal scheduler), [`entity-rendering.md`](./entity-rendering.md)
  (what a client does with a spawned mob's metadata).
