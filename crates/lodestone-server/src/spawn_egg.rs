//! Spawn eggs — the decision half of vanilla's `SpawnEggItem.useOn`.
//!
//! # What it is
//!
//! Given the item a player right-clicked with, the block they clicked and the
//! face they clicked, this answers **which entity type to create and exactly
//! where its feet go** — or that the click is not a spawn egg at all, so the
//! caller falls through to ordinary block placement. It performs no world
//! mutation and no spawning; the caller hands the answer to
//! [`crate::MobSim::spawn_species`].
//!
//! Same shape as [`crate::composter`]/[`crate::bone_meal`]: a pure function
//! returning an outcome enum with an explicit "not mine" arm, so the
//! right-click dispatcher stays a match rather than growing a fifth family of
//! inline logic.
//!
//! # The item → entity mapping is derived, not tabulated, and that is checked
//!
//! **There is no generated spawn-egg table in `lodestone-data`.** Its
//! `block_items` census names spawn eggs explicitly as one of the item families it
//! excludes ("spawn eggs and minecarts spawning entities … each needs its own
//! mechanism"), and nothing else carries the mapping. So this module needed a
//! source, and the honest one is the jar.
//!
//! Vanilla holds it as a per-registration field: `Items.registerSpawnEgg(id, type)`
//! puts the `EntityType` into the item's `spawnEgg` property, and
//! `SpawnEggItem.getType` reads it back — so "the name matches" is a *hypothesis*
//! about 88 independent registrations, not a rule. It was checked against the
//! pinned 26.2 decompile by extracting every
//! `registerSpawnEgg(ItemIds.X, EntityTypes.Y)` pair and comparing `X` minus its
//! `_spawn_egg` suffix against `Y`: **88 registrations, zero mismatches.** So the
//! derivation is exact for this version, and
//! [`entity_type_for_egg`] additionally requires the derived name to be a real
//! entry in [`lodestone_data::entity_types`] — a misspelled or modded
//! `*_spawn_egg` therefore refuses rather than proposing an entity type nothing
//! can render.
//!
//! `every_jar_registered_spawn_egg_resolves` drives all 88 of those item ids
//! through the derivation, so an egg the player can actually hold and this
//! function cannot resolve is a failing test rather than an inert item. The list
//! is the extraction's own output, committed with its provenance, so the gate does
//! not depend on `.cache/` being present.
//!
//! # Where the mob lands
//!
//! `SpawnEggItem.useOn` picks the cell, then `EntityType.create` picks the
//! sub-cell height. Both halves are here, and the second one is the part that is
//! easy to get almost right:
//!
//! 1. **The cell.** `blockState.getCollisionShape(level, pos).isEmpty()` → the
//!    clicked cell itself; otherwise the neighbour across the clicked face. So an
//!    egg used on tall grass spawns *in* the grass, and one used on stone spawns
//!    beside or on top of it.
//! 2. **`movedUp`** is `pos != spawnPos && clickedFace == UP` — true only for the
//!    top face of a cell that had collision.
//! 3. **The height** is `spawnPos.y + yOff`, where
//!    `yOff = 1.0 + Shapes.collide(Y, entityBox, collisions, movedUp ? -2.0 : -1.0)`
//!    and the collisions are those inside `AABB(spawnPos)`, expanded one cell
//!    **downward** when `movedUp`. The entity box starts one cell above `spawnPos`
//!    and falls, bounded. Re-expressed without a sweep — see [`y_offset`] — that
//!    is `max(highest collision top in the searched cells, movedUp ? -1.0 : 0.0)`,
//!    relative to `spawnPos.y`.
//!
//! The common case is the one worth checking by hand: clicking the **top** of a
//! full block gives `spawnPos = pos + up`, `movedUp = true`, and the clicked
//! block's own top is `spawnPos.y - 1 + 1.0 = spawnPos.y`, so `yOff = 0.0` and the
//! mob stands on the clicked face. Do the same with a bottom slab underfoot and
//! its top is `spawnPos.y - 0.5`, so `yOff = -0.5` and the mob stands on the slab
//! rather than half a block above it. A gate that only ever clicked full cubes
//! cannot tell those two apart from a hardcoded `0.0`, which is why
//! [`y_offset`]'s tests use both.
//!
//! # How to change it
//!
//! * **A new egg** needs nothing: the derivation covers it as soon as the entity
//!   type is in the registry table.
//! * **The dispenser behaviour** (`SpawnEggItemBehavior`) reuses
//!   [`entity_type_for_egg`] and nothing else here — its placement rule is the
//!   dispenser's facing, not a clicked face.
//! * **Clicking a spawner block** is deliberately *not* this module's job even
//!   though vanilla puts it in the same method: it re-keys the block entity
//!   instead of spawning, so it belongs with [`crate::block_entities`]. This
//!   module reports [`SpawnEggUse::Spawn`] for a click on a spawner, and the
//!   caller must check for a spawner first — see the gotcha below.
//!
//! ## Gotchas
//!
//! * **A spawner check must come before this.** `SpawnEggItem.useOn` tests
//!   `level.getBlockEntity(pos) instanceof Spawner` *first*, and that branch
//!   returns without creating anything. Nothing here can see block entities, so
//!   ordering is the caller's responsibility.
//! * **Peaceful refuses.** `type.canSpawn(level)` is
//!   `isAllowedInPeaceful() || difficulty != PEACEFUL`, and a `FAIL` there means
//!   the stack is *not* consumed. [`SpawnEggUse::Refused`] carries that
//!   distinction — returning `NotSpawnEgg` instead would make the egg place a
//!   block.
//! * **The stack shrinks only on success.** `spawnMob` consumes one *after*
//!   `type.spawn` returned non-null, so a caller must not decrement before the
//!   spawn.
//!
//! # Dependencies
//!
//! [`lodestone_data::entity_types`] for the registry check,
//! [`lodestone_data::block_states`]/[`lodestone_data::collision_shapes`] for the
//! landing height, [`lodestone_model`] for the geometry types, and
//! [`crate::mob_spawn::allowed_in_peaceful`] for the peaceful guard. No protocol,
//! no world handle — the caller supplies a block-state reader.

use lodestone_data::{block_states, collision_shapes, entity_types};
use lodestone_model::{BlockFace, BlockPos, Difficulty, ResourceKey, Vec3};

/// What a right-click with the held item means for this module.
#[derive(Debug, Clone, PartialEq)]
pub enum SpawnEggUse {
    /// The held item is not a spawn egg. The caller continues to whatever it
    /// would have done — block placement, most often.
    NotSpawnEgg,
    /// A spawn egg, but vanilla's `useOn` returns `FAIL`: the derived entity type
    /// is not in the registry, or the difficulty is `Peaceful` and the species is
    /// `notInPeaceful`. **The stack is not consumed**, and no block is placed.
    Refused,
    /// Create `entity_type` with its feet at `position`, then consume one from the
    /// stack.
    Spawn {
        /// The entity type the egg names.
        entity_type: ResourceKey,
        /// Feet position: cell centre horizontally, [`y_offset`] vertically.
        position: Vec3,
    },
}

/// The entity type a spawn-egg item id names, or `None` when `item` is not a
/// spawn egg or names no registered entity type.
///
/// `item` is the full item id (`"minecraft:sheep_spawn_egg"`). The namespace is
/// carried across to the entity key, so a non-`minecraft` egg resolves against a
/// non-`minecraft` entity type — which then fails the registry check, because
/// [`lodestone_data::entity_types`] holds only vanilla's.
#[must_use]
pub fn entity_type_for_egg(item: &str) -> Option<ResourceKey> {
    let (namespace, path) = match item.split_once(':') {
        Some((ns, path)) => (ns, path),
        // A bare name is `minecraft`, matching how every other id in this crate
        // is normalised.
        None => ("minecraft", item),
    };
    let species = path.strip_suffix("_spawn_egg")?;
    if species.is_empty() {
        return None;
    }
    let key = ResourceKey::new(namespace, species).ok()?;
    // The registry check that turns a name derivation into a validated answer.
    entity_types::entity_type_id(&key.to_string())?;
    Some(key)
}

/// Resolves one right-click with `item` into [`SpawnEggUse`].
///
/// * `clicked` is the block position from the `use_item_on` packet and `face` the
///   clicked face.
/// * `difficulty` is the **world** difficulty, for `EntityType.canSpawn`.
/// * `block_state` reads a full block-state string at world coordinates — the
///   same closure shape the item-settling pass takes, so the caller passes its
///   live `ChunkSource` rather than a snapshot.
#[must_use]
pub fn use_spawn_egg(
    item: &str,
    difficulty: Difficulty,
    clicked: BlockPos,
    face: BlockFace,
    block_state: &dyn Fn(i32, i32, i32) -> String,
) -> SpawnEggUse {
    if !item
        .split_once(':')
        .map_or(item, |(_, path)| path)
        .ends_with("_spawn_egg")
    {
        return SpawnEggUse::NotSpawnEgg;
    }
    let Some(entity_type) = entity_type_for_egg(item) else {
        return SpawnEggUse::Refused;
    };
    // `EntityType.canSpawn(level)`.
    if difficulty == Difficulty::Peaceful
        && !crate::mob_spawn::allowed_in_peaceful(entity_type.path())
    {
        return SpawnEggUse::Refused;
    }

    let clicked_state = block_state(clicked.x, clicked.y, clicked.z);
    let clicked_empty = collision_boxes_for(&clicked_state).is_empty();
    let spawn_pos = if clicked_empty {
        clicked
    } else {
        offset(clicked, face)
    };
    let moved_up = spawn_pos != clicked && face == BlockFace::Up;

    // `getCollisions(null, AABB(spawnPos))`, plus the cell below when `movedUp`
    // expands the box downward. Tops are expressed relative to `spawnPos.y`.
    let mut top = None;
    let mut consider = |cell: BlockPos, base: f64| {
        for b in collision_boxes_for(&block_state(cell.x, cell.y, cell.z)) {
            let candidate = base + f64::from(b.max[1]);
            if top.is_none_or(|t| candidate > t) {
                top = Some(candidate);
            }
        }
    };
    consider(spawn_pos, 0.0);
    if moved_up {
        consider(
            BlockPos::new(spawn_pos.x, spawn_pos.y - 1, spawn_pos.z),
            -1.0,
        );
    }

    SpawnEggUse::Spawn {
        entity_type,
        position: Vec3::new(
            f64::from(spawn_pos.x) + 0.5,
            f64::from(spawn_pos.y) + y_offset(top, moved_up),
            f64::from(spawn_pos.z) + 0.5,
        ),
    }
}

/// What [`apply_spawn_egg`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum SpawnEggApplied {
    /// Not a spawn egg; the caller continues to block placement.
    NotSpawnEgg,
    /// A spawn egg vanilla refuses (`FAIL`). No entity, and **the stack is not
    /// consumed** — but the caller must still not fall through to placement, or a
    /// refused egg would place a block.
    Refused,
    /// The entity exists in the sim. The caller now consumes one from the stack
    /// (`itemStack.consume(1, user)`, which vanilla does *after* the spawn
    /// succeeded) and tells the client its hotbar slot changed.
    Spawned {
        /// The new entity's network id, for a caller that wants to log or track it.
        entity_id: i32,
        /// What was created.
        entity_type: ResourceKey,
        /// Where its feet went.
        position: Vec3,
    },
}

/// [`use_spawn_egg`] **plus the spawn** — the composition, named, so a gate has a
/// subject and the right-click dispatcher stays a five-line match.
///
/// This exists for the reason this repo keeps rediscovering: a decision function
/// and a spawn function can each be correct while the seam between them is where
/// the defect lives, and a seam with no name has nothing to point a test at. It is
/// also what keeps the version-aware dispatcher small — it needs to know only
/// "consume one, or don't".
///
/// # What is deliberately not modelled
///
/// * **The random yaw.** `EntityType.create` snaps the entity to
///   `Mth.wrapDegrees(random.nextFloat() * 360)`, then copies it into `yHeadRot`
///   and `yBodyRot`. This has no RNG stream to draw from and [`crate::MobSim`]
///   exposes no rotation setter, so an egg-spawned mob faces the sim's default
///   until one of those exists. Cosmetic, and stated rather than silently
///   approximated.
/// * **`Mob.finalizeSpawn`.** Vanilla calls it with the *regional* difficulty at
///   the spawn position, which is what gives a zombie its chance of armour and a
///   spider its potion effect. Neither regional difficulty nor mob equipment is
///   modelled here, so nothing is passed and nothing is applied.
pub fn apply_spawn_egg(
    item: &str,
    difficulty: Difficulty,
    clicked: BlockPos,
    face: BlockFace,
    block_state: &dyn Fn(i32, i32, i32) -> String,
    mobs: &crate::MobHandle,
) -> SpawnEggApplied {
    match use_spawn_egg(item, difficulty, clicked, face, block_state) {
        SpawnEggUse::NotSpawnEgg => SpawnEggApplied::NotSpawnEgg,
        SpawnEggUse::Refused => SpawnEggApplied::Refused,
        SpawnEggUse::Spawn {
            entity_type,
            position,
        } => {
            // `spawn_species`, not a bare `spawn`: it resolves the species'
            // attributes, its pathfinding shape, its goal set and its mob
            // category, so an egg-spawned mob is the same object a natural spawn
            // produces. Anything less puts a shaped, AI-less entity on the wire.
            let entity_id =
                mobs.with(|sim| sim.spawn_species(entity_type.clone(), position).id());
            SpawnEggApplied::Spawned {
                entity_id,
                entity_type,
                position,
            }
        }
    }
}

/// `EntityType.getYOffset` re-expressed without a sweep.
///
/// Vanilla places the entity's box one cell **above** `spawnPos` and sweeps it
/// down by at most `movedUp ? 2.0 : 1.0`, then adds 1.0:
///
/// ```java
/// return 1.0 + Shapes.collide(Direction.Axis.Y, entityBox, shapes, movedUp ? -2.0 : -1.0);
/// ```
///
/// `Shapes.collide` returns the *achieved* displacement, which for a box starting
/// at relative `y = 1` is `max(limit, top - 1.0)` — the fall stops on the highest
/// surface it meets, or runs out of budget. Adding the 1.0 back leaves
/// `max(1.0 + limit, top)`, i.e. `max(0.0, top)` for a side click and
/// `max(-1.0, top)` for a top click. `top` is `None` when no searched cell has
/// any collision at all, in which case the box falls the whole budget.
///
/// The two limits are what make this worth a named function: a top click can drop
/// the mob *below* `spawnPos` (onto a slab in the cell beneath), and a side click
/// never can.
#[must_use]
pub fn y_offset(top: Option<f64>, moved_up: bool) -> f64 {
    let floor = if moved_up { -1.0 } else { 0.0 };
    match top {
        Some(top) if top > floor => top,
        _ => floor,
    }
}

/// The collision boxes of a full block-state string, empty for air, a fluid, tall
/// grass, or a name outside the table.
///
/// Resolution is `block_state_id` then `block_states::state_id`, deliberately —
/// **never** `block_state_id_or_default`, which answers a bare name with the
/// block's *lowest* state id rather than its default. That distinction is what
/// decides whether a bare `minecraft:oak_slab` is a bottom slab or a full cube,
/// and it is the same trap `crate::mobs`' item-settling probe documents.
///
/// `pub(crate)`: `crate::mob_spawner`'s spawner-tick collision check
/// (`crate::tick::run_tick_loop`'s call site) reuses this rather than a second
/// copy of the same resolution order.
pub(crate) fn collision_boxes_for(state: &str) -> &'static [collision_shapes::Aabb] {
    let id = crate::mobs::block_state_id(state).or_else(|| block_states::state_id(state));
    id.and_then(collision_shapes::collision_boxes)
        .unwrap_or(&[])
}

/// `BlockPos.relative(direction)`.
fn offset(pos: BlockPos, face: BlockFace) -> BlockPos {
    match face {
        BlockFace::Down => BlockPos::new(pos.x, pos.y - 1, pos.z),
        BlockFace::Up => BlockPos::new(pos.x, pos.y + 1, pos.z),
        BlockFace::North => BlockPos::new(pos.x, pos.y, pos.z - 1),
        BlockFace::South => BlockPos::new(pos.x, pos.y, pos.z + 1),
        BlockFace::West => BlockPos::new(pos.x - 1, pos.y, pos.z),
        BlockFace::East => BlockPos::new(pos.x + 1, pos.y, pos.z),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A world of `minecraft:stone` at `y <= 64`, air above, unless a cell is
    /// overridden.
    fn world(overrides: Vec<(BlockPos, &'static str)>) -> impl Fn(i32, i32, i32) -> String {
        move |x, y, z| {
            let at = BlockPos::new(x, y, z);
            if let Some((_, name)) = overrides.iter().find(|(p, _)| *p == at) {
                return (*name).to_owned();
            }
            if y <= 64 {
                "minecraft:stone".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        }
    }

    #[test]
    fn a_non_egg_item_is_not_this_modules_business() {
        assert_eq!(
            use_spawn_egg(
                "minecraft:stone",
                Difficulty::Normal,
                BlockPos::new(0, 64, 0),
                BlockFace::Up,
                &world(vec![])
            ),
            SpawnEggUse::NotSpawnEgg
        );
    }

    /// The item → entity derivation, including the two ways it must refuse.
    #[test]
    fn the_derivation_resolves_real_eggs_and_refuses_invented_ones() {
        assert_eq!(
            entity_type_for_egg("minecraft:sheep_spawn_egg").map(|k| k.to_string()),
            Some("minecraft:sheep".to_owned())
        );
        // A multi-word species, where a naive `split('_').next()` would answer
        // `zombie`.
        assert_eq!(
            entity_type_for_egg("minecraft:zombie_villager_spawn_egg").map(|k| k.to_string()),
            Some("minecraft:zombie_villager".to_owned())
        );
        assert_eq!(entity_type_for_egg("minecraft:stone"), None);
        assert_eq!(entity_type_for_egg("minecraft:_spawn_egg"), None);
        // The registry check doing its job: the suffix is right and the species is
        // not a thing.
        assert_eq!(entity_type_for_egg("minecraft:wumpus_spawn_egg"), None);
        // Vanilla has no `player_spawn_egg`, and `minecraft:player` *is* a
        // registered entity type — so this row is about the item side, and it must
        // still resolve, because the derivation's job is the name and the caller's
        // is whether the player holds such an item. Recorded rather than asserted
        // as a refusal, because inventing a refusal here would be a rule vanilla
        // does not have.
        assert!(entity_type_for_egg("minecraft:player_spawn_egg").is_some());
    }

    /// **Clicking the top of a solid block puts the mob on top of it.** The number
    /// is derived rather than guessed: `spawnPos` is `(0, 65, 0)`, the clicked
    /// block's top is `65.0`, so `yOff = 0.0` and the feet are at `y = 65`.
    #[test]
    fn a_top_click_stands_the_mob_on_the_clicked_face() {
        let out = use_spawn_egg(
            "minecraft:pig_spawn_egg",
            Difficulty::Normal,
            BlockPos::new(0, 64, 0),
            BlockFace::Up,
            &world(vec![]),
        );
        assert_eq!(
            out,
            SpawnEggUse::Spawn {
                entity_type: ResourceKey::new("minecraft", "pig").unwrap(),
                position: Vec3::new(0.5, 65.0, 0.5),
            }
        );
    }

    /// **The discriminating input for [`y_offset`]: a bottom slab under the spawn
    /// cell.** A `movedUp` click searches the cell below too, and the slab's top is
    /// `spawnPos.y - 0.5`, so the mob stands at `64.5` — half a block *below*
    /// `spawnPos`. A hardcoded `0.0` offset, or a search that skipped the lower
    /// cell, yields `65.0` and passes the full-cube test above.
    #[test]
    fn a_top_click_on_a_slab_lands_the_mob_on_the_slab() {
        let slab = BlockPos::new(3, 64, 3);
        let out = use_spawn_egg(
            "minecraft:pig_spawn_egg",
            Difficulty::Normal,
            slab,
            BlockFace::Up,
            &world(vec![(slab, "minecraft:oak_slab[type=bottom,waterlogged=false]")]),
        );
        let SpawnEggUse::Spawn { position, .. } = out else {
            panic!("a slab has collision, so the egg spawns above it: {out:?}");
        };
        assert!(
            (position.y - 64.5).abs() < 1e-9,
            "a bottom slab's top is 64.5, so the feet go there, not at 65.0: {position:?}"
        );
    }

    /// A cell with **no** collision at all is spawned into directly, and the mob
    /// does not fall through the floor: the searched cell is empty and a side-click
    /// floor of `0.0` holds it at `spawnPos.y`.
    #[test]
    fn an_egg_used_on_a_collisionless_block_spawns_in_that_cell() {
        let grass = BlockPos::new(0, 65, 0);
        let out = use_spawn_egg(
            "minecraft:cow_spawn_egg",
            Difficulty::Normal,
            grass,
            BlockFace::Up,
            &world(vec![(grass, "minecraft:short_grass")]),
        );
        let SpawnEggUse::Spawn { position, .. } = out else {
            panic!("short grass has an empty collision shape: {out:?}");
        };
        // `spawnPos == clicked`, so `movedUp` is false and the floor is 0.0.
        assert_eq!(position, Vec3::new(0.5, 65.0, 0.5));
    }

    /// A side click spawns in the neighbouring cell, at that cell's own level.
    #[test]
    fn a_side_click_spawns_beside_the_block() {
        let out = use_spawn_egg(
            "minecraft:chicken_spawn_egg",
            Difficulty::Normal,
            BlockPos::new(0, 64, 0),
            BlockFace::East,
            &world(vec![]),
        );
        let SpawnEggUse::Spawn { position, .. } = out else {
            panic!("{out:?}");
        };
        // The neighbour at (1, 64, 0) is stone in this fixture, whose top is
        // `spawnPos.y + 1.0`, and a side click's budget is one cell — so the mob
        // rests on top of it at 65.0 rather than inside it.
        assert_eq!(position, Vec3::new(1.5, 65.0, 0.5));
    }

    /// **Peaceful refuses a monster egg and accepts an animal one.** The pair is
    /// the discriminating input: a gate on the monster alone passes for an
    /// implementation that refuses every egg on Peaceful.
    #[test]
    fn peaceful_refuses_a_monster_egg_only() {
        assert_eq!(
            use_spawn_egg(
                "minecraft:zombie_spawn_egg",
                Difficulty::Peaceful,
                BlockPos::new(0, 64, 0),
                BlockFace::Up,
                &world(vec![])
            ),
            SpawnEggUse::Refused,
            "EntityType.canSpawn is false for a notInPeaceful type on Peaceful"
        );
        assert!(matches!(
            use_spawn_egg(
                "minecraft:sheep_spawn_egg",
                Difficulty::Peaceful,
                BlockPos::new(0, 64, 0),
                BlockFace::Up,
                &world(vec![])
            ),
            SpawnEggUse::Spawn { .. }
        ));
        // And a MONSTER-category species vanilla *allows* in peaceful — the row
        // that fails if the guard is category-derived.
        assert!(matches!(
            use_spawn_egg(
                "minecraft:shulker_spawn_egg",
                Difficulty::Peaceful,
                BlockPos::new(0, 64, 0),
                BlockFace::Up,
                &world(vec![])
            ),
            SpawnEggUse::Spawn { .. }
        ));
        // Easy, the neighbouring difficulty, must accept the monster — otherwise
        // "peaceful refuses" would be satisfied by "everything refuses".
        assert!(matches!(
            use_spawn_egg(
                "minecraft:zombie_spawn_egg",
                Difficulty::Easy,
                BlockPos::new(0, 64, 0),
                BlockFace::Up,
                &world(vec![])
            ),
            SpawnEggUse::Spawn { .. }
        ));
    }

    /// Every `registerSpawnEgg` item id in the pinned 26.2 decompile, extracted by
    /// the same pass this module's doc describes and committed here so the gate
    /// does not need `.cache/` present. **Sorted**, so a hand edit lands where the
    /// extraction would have put it.
    static JAR_SPAWN_EGG_ITEMS: [&str; 88] = [
        "allay_spawn_egg",
        "armadillo_spawn_egg",
        "axolotl_spawn_egg",
        "bat_spawn_egg",
        "bee_spawn_egg",
        "blaze_spawn_egg",
        "bogged_spawn_egg",
        "breeze_spawn_egg",
        "camel_husk_spawn_egg",
        "camel_spawn_egg",
        "cat_spawn_egg",
        "cave_spider_spawn_egg",
        "chicken_spawn_egg",
        "cod_spawn_egg",
        "copper_golem_spawn_egg",
        "cow_spawn_egg",
        "creaking_spawn_egg",
        "creeper_spawn_egg",
        "dolphin_spawn_egg",
        "donkey_spawn_egg",
        "drowned_spawn_egg",
        "elder_guardian_spawn_egg",
        "ender_dragon_spawn_egg",
        "enderman_spawn_egg",
        "endermite_spawn_egg",
        "evoker_spawn_egg",
        "fox_spawn_egg",
        "frog_spawn_egg",
        "ghast_spawn_egg",
        "glow_squid_spawn_egg",
        "goat_spawn_egg",
        "guardian_spawn_egg",
        "happy_ghast_spawn_egg",
        "hoglin_spawn_egg",
        "horse_spawn_egg",
        "husk_spawn_egg",
        "iron_golem_spawn_egg",
        "llama_spawn_egg",
        "magma_cube_spawn_egg",
        "mooshroom_spawn_egg",
        "mule_spawn_egg",
        "nautilus_spawn_egg",
        "ocelot_spawn_egg",
        "panda_spawn_egg",
        "parched_spawn_egg",
        "parrot_spawn_egg",
        "phantom_spawn_egg",
        "pig_spawn_egg",
        "piglin_brute_spawn_egg",
        "piglin_spawn_egg",
        "pillager_spawn_egg",
        "polar_bear_spawn_egg",
        "pufferfish_spawn_egg",
        "rabbit_spawn_egg",
        "ravager_spawn_egg",
        "salmon_spawn_egg",
        "sheep_spawn_egg",
        "shulker_spawn_egg",
        "silverfish_spawn_egg",
        "skeleton_horse_spawn_egg",
        "skeleton_spawn_egg",
        "slime_spawn_egg",
        "sniffer_spawn_egg",
        "snow_golem_spawn_egg",
        "spider_spawn_egg",
        "squid_spawn_egg",
        "stray_spawn_egg",
        "strider_spawn_egg",
        "sulfur_cube_spawn_egg",
        "tadpole_spawn_egg",
        "trader_llama_spawn_egg",
        "tropical_fish_spawn_egg",
        "turtle_spawn_egg",
        "vex_spawn_egg",
        "villager_spawn_egg",
        "vindicator_spawn_egg",
        "wandering_trader_spawn_egg",
        "warden_spawn_egg",
        "witch_spawn_egg",
        "wither_skeleton_spawn_egg",
        "wither_spawn_egg",
        "wolf_spawn_egg",
        "zoglin_spawn_egg",
        "zombie_horse_spawn_egg",
        "zombie_nautilus_spawn_egg",
        "zombie_spawn_egg",
        "zombie_villager_spawn_egg",
        "zombified_piglin_spawn_egg",
    ];

    /// **Every real egg resolves.** The failures are collected rather than asserted
    /// inside the loop: an `assert!` there would abort on the first miss and prove
    /// exactly one arm, so a systematic gap (say, every two-word species) would be
    /// reported as a single name.
    #[test]
    fn every_jar_registered_spawn_egg_resolves() {
        let unresolved: Vec<&str> = JAR_SPAWN_EGG_ITEMS
            .iter()
            .copied()
            .filter(|item| entity_type_for_egg(&format!("minecraft:{item}")).is_none())
            .collect();
        assert!(
            unresolved.is_empty(),
            "{} of 88 jar-registered spawn eggs resolve to no entity type: {unresolved:?}",
            unresolved.len()
        );
        let mut sorted = JAR_SPAWN_EGG_ITEMS;
        sorted.sort_unstable();
        assert_eq!(sorted, JAR_SPAWN_EGG_ITEMS, "the list must stay sorted");
    }

    /// **The composition.** A spawn egg must reach an entity that is on the wire —
    /// the thing neither [`use_spawn_egg`] nor `spawn_species` can be tested for
    /// on its own.
    ///
    /// `snapshots()` is the assertion subject deliberately: it is what
    /// `EntityStreamer::sync` diffs to produce `ADD_ENTITY`, so a mob present in
    /// the sim but absent from the snapshot set would fail here and pass any test
    /// that looked at `MobSim::iter`.
    #[test]
    fn a_spawn_egg_puts_a_real_entity_into_the_snapshot_set() {
        let mobs = crate::MobHandle::new(crate::ChunkWorld::new(0, 128));
        let before = mobs.with(|sim| sim.snapshots().len());

        let applied = apply_spawn_egg(
            "minecraft:sheep_spawn_egg",
            Difficulty::Normal,
            BlockPos::new(0, 64, 0),
            BlockFace::Up,
            &world(vec![]),
            &mobs,
        );
        let SpawnEggApplied::Spawned {
            entity_id,
            position,
            ..
        } = applied
        else {
            panic!("a sheep egg on Normal must spawn: {applied:?}");
        };

        let snapshots = mobs.with(|sim| sim.snapshots());
        assert_eq!(
            snapshots.len(),
            before + 1,
            "exactly one new entity reaches the wire"
        );
        let spawned = snapshots
            .iter()
            .find(|s| s.id == entity_id)
            .expect("the spawned id must be in the snapshot set that becomes ADD_ENTITY");
        assert_eq!(spawned.entity_type.to_string(), "minecraft:sheep");
        assert_eq!(
            spawned.position, position,
            "the wire must carry the position the placement rule computed"
        );

        // Refused: nothing is created, so the stack must not be consumed and no
        // entity appears. Without this arm the count assertion above is satisfied
        // by an implementation that spawns unconditionally.
        let refused = apply_spawn_egg(
            "minecraft:zombie_spawn_egg",
            Difficulty::Peaceful,
            BlockPos::new(0, 64, 0),
            BlockFace::Up,
            &world(vec![]),
            &mobs,
        );
        assert_eq!(refused, SpawnEggApplied::Refused);
        assert_eq!(
            mobs.with(|sim| sim.snapshots().len()),
            before + 1,
            "a refused egg creates nothing"
        );
    }

    /// [`y_offset`]'s four cases, each against the arithmetic in its own doc
    /// comment rather than against this module's output.
    #[test]
    fn y_offset_matches_the_collide_expression() {
        // Nothing to land on: the whole budget is spent.
        assert_eq!(y_offset(None, false), 0.0);
        assert_eq!(y_offset(None, true), -1.0);
        // A surface inside the budget stops the fall there.
        assert_eq!(y_offset(Some(0.5), false), 0.5);
        assert_eq!(y_offset(Some(-0.5), true), -0.5);
        // A surface *below* the budget cannot be reached.
        assert_eq!(y_offset(Some(-0.5), false), 0.0);
        assert_eq!(y_offset(Some(-1.5), true), -1.0);
    }
}
