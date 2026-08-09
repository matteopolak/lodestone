# Spawn eggs

## What it is

The rule layer for `*_spawn_egg` items — `crates/lodestone-server/src/spawn_egg.rs`: which entity a
given egg names, where a right-click puts it, and when the click is refused. It is the cheapest way
for a player to make any mob appear, and it was completely inert before: no handler existed anywhere
in the tree, so an egg fell through to the block-placement branch, found the clicked cell
non-replaceable and did nothing.

## How it works

Three questions, in vanilla's own order (`SpawnEggItem.useOn`).

### 1. Which entity? The mapping is derived, and the derivation is checked

**There is no generated spawn-egg table.** `lodestone-data`'s `block_items` census names spawn eggs
explicitly as one of the item families it excludes ("spawn eggs and minecarts spawning entities …
each needs its own mechanism"), and nothing else in the crate carries the item→entity link. Vanilla
holds it per registration — `Items.registerSpawnEgg(id, type)` stores the `EntityType` in the item's
`spawnEgg` property and `SpawnEggItem.getType` reads it back — so "the names match" is a hypothesis
about 88 independent registrations, not a rule.

It was checked against the pinned 26.2 decompile by extracting every
`registerSpawnEgg(ItemIds.X, EntityTypes.Y)` pair and comparing `X` minus its `_spawn_egg` suffix
against `Y`: **88 registrations, zero mismatches.** So `entity_type_for_egg` strips the suffix — and
then requires the result to be a real entry in `lodestone_data::entity_types`, so a misspelled or
modded egg refuses rather than naming an entity nothing can render.
`the_derivation_resolves_real_eggs_and_refuses_invented_ones` pins both halves, including
`zombie_villager_spawn_egg`, where a naive `split('_').next()` answers `zombie`.

### 2. Where? Two halves, and the second is the one that goes wrong quietly

`SpawnEggItem.useOn` picks the **cell**; `EntityType.create` picks the **sub-cell height**.

1. `blockState.getCollisionShape(...).isEmpty()` → the clicked cell itself, otherwise the neighbour
   across the clicked face. An egg used on tall grass spawns *in* the grass.
2. `movedUp = pos != spawnPos && clickedFace == UP` — only the top face of a cell that had collision.
3. `yOff = 1.0 + Shapes.collide(Y, entityBox, collisions, movedUp ? -2.0 : -1.0)`, over the
   collisions inside `AABB(spawnPos)`, expanded one cell **downward** when `movedUp`.

`y_offset` re-expresses step 3 without a sweep. `Shapes.collide` returns the achieved displacement,
so for a box starting at relative `y = 1` it is `max(limit, top - 1.0)`; adding the 1.0 back leaves
`max(0.0, top)` for a side click and `max(-1.0, top)` for a top click, where `top` is the highest
collision surface in the searched cells relative to `spawnPos.y`.

The discriminating input is a **bottom slab**, not a full cube. Clicking the top of a full block puts
the mob at `spawnPos.y` exactly, which a hardcoded `0.0` also produces; clicking the top of a bottom
slab must put it at `spawnPos.y - 0.5`, which only the real search does. Both are gated.

### 3. Refused, or not mine?

`SpawnEggUse` has three arms and the distinction between two of them is load-bearing:

| arm | meaning | caller |
|---|---|---|
| `NotSpawnEgg` | the held item is not an egg | fall through to block placement |
| `Refused` | vanilla `FAIL` — unknown type, or Peaceful forbids the species | return; **do not** consume the stack, **do not** place a block |
| `Spawn` | create it | spawn, then consume one |

Returning `NotSpawnEgg` for a refusal would make a refused egg place a block.

The Peaceful test is `EntityType.canSpawn`: `isAllowedInPeaceful() || difficulty != PEACEFUL`, keyed
on the per-type flag in `mob_spawn::allowed_in_peaceful` — see
[`world-state.md`](./world-state.md#what-difficulty-changes) for why the category is not a substitute.

### The composition

`apply_spawn_egg` is `use_spawn_egg` plus `MobSim::spawn_species`, named on purpose: a decision
function and a spawn function can each be correct while the seam between them is where the defect
lives, and a seam with no name has nothing to point a test at.
`a_spawn_egg_puts_a_real_entity_into_the_snapshot_set` asserts against `MobSim::snapshots()` — what
`EntityStreamer::sync` actually diffs into `ADD_ENTITY` — rather than `MobSim::iter`, so a mob that
exists in the sim and never reaches the wire fails.

`spawn_species`, not a bare spawn: it resolves the species' attributes, pathfinding shape, goal set
and mob category, so an egg-spawned mob is the same object a natural spawn produces. That matters
because natural spawning is already verified to reach a client and draw
([`natural-mob-spawning.md`](./natural-mob-spawning.md)), so an identical object needs no separate
rendering claim.

## How to change it

* **A new egg** needs nothing: the derivation covers it as soon as the entity type is in the registry
  table.
* **A dispenser** (`SpawnEggItemBehavior`) reuses `entity_type_for_egg` and nothing else — its
  placement rule is the dispenser's facing, not a clicked face.
* **Clicking a spawner block** re-keys the block entity instead of spawning. Vanilla puts it in the
  same method, but it belongs with [`block-entities.md`](./block-entities.md); this module reports
  `Spawn` for such a click, so **the caller must test for a spawner first**.

### Gotchas

* **The dispatcher must sit between the block's own `useWithoutItem` and `BlockItem.place`** — that
  is vanilla's order in `Player.useItemOn`. Ahead of `hand_use` and a lever click would spawn nothing
  and eat the egg; behind the placement branch and an egg held over air would place a block.
* **The stack shrinks only on success.** `spawnMob` consumes one *after* `type.spawn` returned
  non-null.
* **No random yaw.** `EntityType.create` snaps to `wrapDegrees(random.nextFloat() * 360)` and copies
  it into `yHeadRot`/`yBodyRot`. There is no RNG stream at this seam and `MobSim` exposes no rotation
  setter, so an egg-spawned mob faces the sim's default. Cosmetic, and stated rather than
  approximated.
* **No `Mob.finalizeSpawn`.** Vanilla passes the *regional* difficulty at the spawn position, which
  is what gives a zombie its chance of armour and a spider its potion effect. Neither regional
  difficulty nor mob equipment is modelled, so nothing is applied.

## What is not wired

**The right-click dispatch.** `crates/lodestone-server/src/server.rs`'s `apply_use_item_on` is the
only place a `use_item_on` packet reaches, and it needs a three-hunk patch: a `difficulty: Difficulty`
parameter, the one call site passing `world.difficulty().0`, and the match arm between the `hand_use`
block and `let neighbour = relative(pos, face);`. Until that lands, everything here is reachable only
from tests. The patch is short and mechanical precisely because `apply_spawn_egg` is the composition.

**Spawner blocks.** `minecraft:spawner` is still a `BlockEntity::Opaque` — its NBT round-trips
through a save and it never ticks. `BaseSpawner.serverTick` needs a nearby-player test, the
delay/count/range fields, and `MobSim` access from the block-entity tick, none of which
`BlockEntityRegistry::tick_all` has today (it takes no world and no mob handle). That is a
block-tick-plumbing change, not a rule change.

## Configuration

None. No env vars, no game rule — vanilla gates spawner *blocks* on
`ServerLevel.isSpawnerBlockEnabled`, which has no analogue here yet, and gates eggs on nothing but
difficulty.

## Dependencies

`lodestone_data::entity_types` (the registry check),
`lodestone_data::block_states`/`collision_shapes` (the landing height), `lodestone_model`
(`BlockFace`/`BlockPos`/`Difficulty`/`Vec3`), `mob_spawn::allowed_in_peaceful` (the Peaceful guard)
and `MobHandle`/`MobSim::spawn_species` for the composition. No protocol, no packet id, no world
handle — the caller supplies a block-state reader.
