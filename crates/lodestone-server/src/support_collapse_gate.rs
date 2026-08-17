//! Gates for the support-collapse pass: `crate::server`'s `collapse_unsupported`
//! driven against a rig world, one arm per **family shape** rather than per block.
//!
//! # What it is
//!
//! `crate::block_support`'s own unit tests check the *predicate* — which cell a
//! state depends on. That is a closed loop: the predicate can be perfect while
//! nothing calls it, which is exactly the state `crate::neighbor_update` was in
//! for its whole life (a correct cascade delivering to no reaction). These gates
//! call the **production** function, through a real [`ChunkSource`], and assert
//! that cells actually became air and that the loot actually rolled.
//!
//! Four family shapes cover the classes; sixty blocks cover the same four. The
//! arms are a single-support plant (sugar cane, plus its own vertical cascade), a
//! wall-mounted block (a wall torch), a two-block door, and a rail. A bed is a
//! fifth because its partner test is lateral rather than vertical, and it is the
//! one shape where "the support went to air" is *not* the rule.
//!
//! # How to change it
//!
//! Add an arm to [`collapse_family_shapes`], and keep collecting mismatches into
//! the vector rather than asserting inside the loop — an `assert!` in a `for`
//! aborts at the first failure, which would leave the other four arms as
//! arguments rather than observations.
//!
//! Gotcha: [`RigWorld`] must reflect its own edits, or every gate here passes
//! while proving nothing. [`the_rig_world_reflects_its_own_edits`] is that premise
//! check, and it is the reason this rig retains real [`ChunkColumn`]s instead of
//! using a simpler air-returning double.

#![cfg(test)]

use std::collections::HashMap;
use std::sync::Mutex;

use lodestone_model::BlockPos;

use crate::ChunkSource;
use crate::chunk::ChunkColumn;
use crate::server::collapse_unsupported;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
/// The rig's solid floor. Everything is placed on top of it, so "break the
/// support" is one `set_block` to air at this y.
const FLOOR_Y: i32 = 64;

struct RigWorld {
    columns: Mutex<HashMap<(i32, i32), ChunkColumn>>,
}

impl RigWorld {
    fn new() -> Self {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for x in 0..16 {
            for z in 0..16 {
                column.set_block(x, FLOOR_Y, z, "minecraft:dirt");
            }
        }
        let mut columns = HashMap::new();
        columns.insert((0, 0), column);
        Self {
            columns: Mutex::new(columns),
        }
    }

    fn put(&self, pos: BlockPos, state: &str) {
        self.set_block(pos.x, pos.y, pos.z, state);
    }

    fn at(&self, pos: BlockPos) -> String {
        self.block_state(pos.x, pos.y, pos.z)
    }
}

impl ChunkSource for RigWorld {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.columns
            .lock()
            .expect("rig world poisoned")
            .entry((cx, cz))
            .or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT))
            .clone()
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        self.columns
            .lock()
            .expect("rig world poisoned")
            .get(&(cx, cz))
            .map(|c| {
                c.block_state(x.rem_euclid(16), y, z.rem_euclid(16))
                    .to_string()
            })
            .unwrap_or_else(|| crate::chunk::AIR.to_string())
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        self.columns
            .lock()
            .expect("rig world poisoned")
            .get(&(cx, cz))
            .map(|c| {
                c.biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
                    .to_string()
            })
            .unwrap_or_else(|| crate::chunk::AIR.to_string())
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        self.columns
            .lock()
            .expect("rig world poisoned")
            .entry((cx, cz))
            .or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT))
            .set_block(x.rem_euclid(16), y, z.rem_euclid(16), name);
    }
}

/// The premise every gate below rests on: a write through [`ChunkSource`] is
/// readable back. A double whose `column()` ignores its own edit map makes every
/// arm here vacuous.
#[test]
fn the_rig_world_reflects_its_own_edits() {
    let world = RigWorld::new();
    let pos = BlockPos::new(3, FLOOR_Y + 1, 3);
    assert_eq!(world.at(pos), "minecraft:air");
    world.put(pos, "minecraft:torch");
    assert_eq!(world.at(pos), "minecraft:torch");
    assert_eq!(
        world.at(BlockPos::new(3, FLOOR_Y, 3)),
        "minecraft:dirt",
        "the floor must be there, or 'the support went away' is not a change"
    );
}

/// One arm per family shape. Every mismatch is collected and asserted on as a
/// set, so a broken arm cannot hide the other four.
#[test]
fn collapse_family_shapes() {
    struct Arm {
        label: &'static str,
        /// Places the subject; returns the cells that must be air afterwards.
        build: fn(&RigWorld) -> (BlockPos, Vec<BlockPos>),
    }

    let arms = [
        Arm {
            // Single-support plant, *and* its vertical cascade: three canes, and
            // breaking the dirt must take all three, not only the bottom one.
            label: "sugar cane column",
            build: |world| {
                let base = BlockPos::new(2, FLOOR_Y, 2);
                let cells: Vec<BlockPos> = (1..=3)
                    .map(|dy| BlockPos::new(base.x, base.y + dy, base.z))
                    .collect();
                for cell in &cells {
                    world.put(*cell, "minecraft:sugar_cane[age=0]");
                }
                (base, cells)
            },
        },
        Arm {
            // Wall-mounted: a torch on the east face of a pillar. Breaking the
            // pillar cell the torch is stuck to takes the torch; the floor is
            // untouched, so this arm cannot pass by accident on a below-rule.
            label: "wall torch",
            build: |world| {
                let wall = BlockPos::new(5, FLOOR_Y + 1, 5);
                world.put(wall, "minecraft:stone");
                let torch = BlockPos::new(6, FLOOR_Y + 1, 5);
                // `facing=east` means the torch points east and is stuck to the
                // block on its west — which is `wall`.
                world.put(torch, "minecraft:wall_torch[facing=east]");
                (wall, vec![torch])
            },
        },
        Arm {
            // Two-block: breaking the floor takes the lower half, and the lower
            // half's removal must then take the upper half.
            label: "oak door",
            build: |world| {
                let base = BlockPos::new(8, FLOOR_Y, 8);
                let lower = BlockPos::new(8, FLOOR_Y + 1, 8);
                let upper = BlockPos::new(8, FLOOR_Y + 2, 8);
                world.put(
                    lower,
                    "minecraft:oak_door[facing=north,half=lower,hinge=left,open=false,powered=false]",
                );
                world.put(
                    upper,
                    "minecraft:oak_door[facing=north,half=upper,hinge=left,open=false,powered=false]",
                );
                (base, vec![lower, upper])
            },
        },
        Arm {
            label: "rail",
            build: |world| {
                let base = BlockPos::new(11, FLOOR_Y, 11);
                let rail = BlockPos::new(11, FLOOR_Y + 1, 11);
                world.put(rail, "minecraft:rail[shape=north_south,waterlogged=false]");
                (base, vec![rail])
            },
        },
        Arm {
            // Lateral partner rather than a support below: breaking the FOOT must
            // take the HEAD, and the head's own support (the floor) is still there
            // — so a below-rule implementation fails this arm and only this arm.
            label: "bed partner",
            build: |world| {
                let foot = BlockPos::new(13, FLOOR_Y + 1, 13);
                let head = BlockPos::new(13, FLOOR_Y + 1, 14);
                world.put(
                    foot,
                    "minecraft:red_bed[facing=south,occupied=false,part=foot]",
                );
                world.put(
                    head,
                    "minecraft:red_bed[facing=south,occupied=false,part=head]",
                );
                // The player breaks the foot itself, which is what `destroy_block`
                // does before it calls the collapse.
                world.put(foot, crate::chunk::AIR);
                (foot, vec![head])
            },
        },
    ];

    let mut mismatches: Vec<String> = Vec::new();
    for arm in arms {
        let world = RigWorld::new();
        let (broken, expected) = (arm.build)(&world);
        // Exactly what `destroy_block` does: the broken cell goes to air first,
        // then the collapse runs from it.
        world.put(broken, crate::chunk::AIR);
        let removed = collapse_unsupported(&world, broken);
        for cell in &expected {
            if !removed.iter().any(|(pos, _)| pos == cell) {
                mismatches.push(format!(
                    "{}: {cell:?} was not collapsed (removed: {:?})",
                    arm.label,
                    removed.iter().map(|(p, _)| *p).collect::<Vec<_>>()
                ));
            }
            if world.at(*cell) != "minecraft:air" {
                mismatches.push(format!(
                    "{}: {cell:?} still holds {} in the world",
                    arm.label,
                    world.at(*cell)
                ));
            }
        }
        if removed.len() != expected.len() {
            mismatches.push(format!(
                "{}: collapsed {} cells, expected exactly {}: {:?}",
                arm.label,
                removed.len(),
                expected.len(),
                removed.iter().map(|(p, _)| *p).collect::<Vec<_>>()
            ));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

/// The negative control the arms above need: a block with **no** support rule
/// must survive its neighbour going away, and so must one whose support is still
/// there. Without this, a collapse that removed everything nearby would pass
/// every arm above.
#[test]
fn nothing_collapses_when_the_support_is_intact_or_unmodelled() {
    let world = RigWorld::new();
    // A torch two cells away from the break, on floor that stays.
    let torch = BlockPos::new(4, FLOOR_Y + 1, 4);
    world.put(torch, "minecraft:torch");
    // An ordinary block directly above the break — stone has no `canSurvive`.
    let stone = BlockPos::new(2, FLOOR_Y + 1, 2);
    world.put(stone, "minecraft:stone");
    let broken = BlockPos::new(2, FLOOR_Y, 2);
    world.put(broken, crate::chunk::AIR);
    let removed = collapse_unsupported(&world, broken);
    assert!(
        removed.is_empty(),
        "nothing here has a lost support, yet {removed:?} was collapsed"
    );
    assert_eq!(world.at(torch), "minecraft:torch");
    assert_eq!(world.at(stone), "minecraft:stone");
}

/// A collapsed plant must **drop**, not merely vanish — removal alone passes for
/// a block that silently disappears, which is a second bug wearing the first
/// one's clothes.
///
/// Rolled through the same `block_drops` entry point `destroy_block` uses, on the
/// states the collapse reported, so this is the production roll rather than a
/// re-implementation of it.
#[test]
fn a_collapsed_plant_rolls_its_loot() {
    let world = RigWorld::new();
    let base = BlockPos::new(6, FLOOR_Y, 6);
    let cane = BlockPos::new(6, FLOOR_Y + 1, 6);
    world.put(cane, "minecraft:sugar_cane[age=0]");
    world.put(base, crate::chunk::AIR);
    let removed = collapse_unsupported(&world, base);
    assert_eq!(removed.len(), 1, "expected the cane and nothing else");
    let mut rng = crate::mob_spawn::SpawnRng::new(0x5EED_C0DE);
    let popped = crate::block_drops::drop_block_loot(
        crate::block_drops::bundled_tables(),
        &removed[0].1,
        removed[0].0,
        None,
        &mut rng,
    );
    let items: Vec<String> = popped
        .iter()
        .map(|drop| drop.stack.item.to_string())
        .collect();
    assert!(
        items.contains(&"minecraft:sugar_cane".to_string()),
        "a collapsed cane must drop sugar cane, got {items:?}"
    );
}

/// The runaway guard, shown to be a ceiling rather than something that fires
/// unconditionally: a column taller than `MAX_SUPPORT_COLLAPSE` is truncated, and
/// the four-cell column in the arms above was not.
#[test]
fn the_collapse_bound_truncates_a_runaway_column_and_nothing_shorter() {
    let world = RigWorld::new();
    let base = BlockPos::new(9, FLOOR_Y, 9);
    // 200 canes, well past the 64-cell bound.
    for dy in 1..=200 {
        world.put(
            BlockPos::new(base.x, base.y + dy, base.z),
            "minecraft:sugar_cane[age=0]",
        );
    }
    world.put(base, crate::chunk::AIR);
    let removed = collapse_unsupported(&world, base);
    assert!(
        removed.len() <= 64,
        "the bound must cap the cascade, got {}",
        removed.len()
    );
    assert!(
        removed.len() >= 60,
        "and it must get most of the way there first, got {}",
        removed.len()
    );
}

/// **The discriminating pair for a support collapse's own write**: a dry rail's
/// cell becomes air, a waterlogged rail's keeps its water source. Either half
/// alone passes under both "always write air" and the correct rule — a rail
/// carries a real `waterlogged` property and a real `SupportKind::Below`
/// dependency, so one block family gives a clean pair with nothing else
/// changing between the two arms.
///
/// `Block.updateOrDestroy` reaches `Level.destroyBlock`, which — exactly like
/// `Level.removeBlock` behind a player's break — writes
/// `fluidState.createLegacyBlock()`, not `Blocks.AIR` unconditionally. Before
/// this fix `collapse_unsupported` wrote literal air for both arms below, so a
/// waterlogged sign or rail whose support vanished silently lost its water too.
#[test]
fn a_collapsed_waterlogged_block_keeps_its_water_source_while_a_dry_one_goes_to_air() {
    let world = RigWorld::new();

    let dry_base = BlockPos::new(2, FLOOR_Y, 2);
    let dry_rail = BlockPos::new(2, FLOOR_Y + 1, 2);
    world.put(dry_rail, "minecraft:rail[shape=north_south,waterlogged=false]");
    world.put(dry_base, crate::chunk::AIR);

    let wet_base = BlockPos::new(9, FLOOR_Y, 9);
    let wet_rail = BlockPos::new(9, FLOOR_Y + 1, 9);
    world.put(wet_rail, "minecraft:rail[shape=north_south,waterlogged=true]");
    world.put(wet_base, crate::chunk::AIR);

    let dry_removed = collapse_unsupported(&world, dry_base);
    let wet_removed = collapse_unsupported(&world, wet_base);

    assert_eq!(dry_removed.len(), 1, "the dry rail must still collapse");
    assert_eq!(wet_removed.len(), 1, "the waterlogged rail must still collapse");

    assert_eq!(
        world.at(dry_rail),
        "minecraft:air",
        "a dry block's cell must become air"
    );
    assert_eq!(
        world.at(wet_rail),
        "minecraft:water[level=0]",
        "a waterlogged block's cell must keep its water source, not go to air \
         — the level=0 legacy encoding is `FlowingFluid.getLegacyLevel`'s own \
         value for a source"
    );
}
