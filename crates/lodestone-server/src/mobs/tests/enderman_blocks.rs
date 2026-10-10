//! An enderman picking up and putting down blocks through acknowledged edits.

use lodestone_entity::ai::{BlockEdit, BlockExpect, turtle_egg};

use super::*;

fn field() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -20..20 {
        for z in -20..20 {
            world.set_block(x, 0, z, "minecraft:stone");
        }
    }
    world
}

fn key(species: &str) -> ResourceKey {
    format!("minecraft:{species}").parse().expect("key")
}

fn far_player() -> PerceivedPlayer {
    PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(60.5, 1.0, 0.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }
}

/// Runs the sim applying edits to `world` the way the tick loop does, answering
/// acknowledged ones with `landed(edit)`.
fn run(
    sim: &mut MobSim<'_>,
    world: &mut ChunkWorld,
    ticks: u32,
    landed_if: &dyn Fn(&BlockEdit) -> bool,
) -> Vec<BlockEdit> {
    let mut seen = Vec::new();
    for _ in 0..ticks {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
        for edit in sim.take_block_edits() {
            let (x, y, z) = edit.cell;
            let old = world.block_state_id(x, y, z);
            let holds = match edit.expect {
                BlockExpect::Air => turtle_egg::is_air(old),
                BlockExpect::State(s) => old == s,
                BlockExpect::Block(b) => old.block() == b,
            };
            let landed = holds && landed_if(&edit);
            if landed {
                world.set_block_id(x, y, z, edit.set.unwrap_or_else(crate::chunk::air_state));
                seen.push(edit);
            }
            if edit.ack
                && let Some(mob) = edit.requester
            {
                sim.report_edit_result(mob, landed);
            }
        }
    }
    seen
}

fn setup<'w>(base: &'w ChunkWorld, griefing: bool) -> (MobSim<'w>, i32) {
    let mut sim = MobSim::new(base);
    sim.set_players(vec![far_player()]);
    sim.set_mob_griefing(griefing);
    let id = sim.spawn_species(key("enderman"), Vec3::new(0.5, 1.0, 0.5)).id();
    (sim, id)
}

/// An enderman among holdable blocks lifts one, and only when the host says the
/// removal landed.
#[test]
fn an_enderman_picks_up_a_block_only_when_the_edit_lands() {
    let carried_after = |accept: bool, griefing: bool| {
        let mut base = field();
        for x in -3..=3_i32 {
            for z in -3..=3_i32 {
                if (x, z) != (0, 0) {
                    base.set_block(x, 1, z, "minecraft:sand");
                    base.set_block(x, 2, z, "minecraft:sand");
                    base.set_block(x, 3, z, "minecraft:sand");
                }
            }
        }
        let mut world = base.clone();
        let (mut sim, id) = setup(&base, griefing);
        let seen = run(&mut sim, &mut world, 400, &|_| accept);
        let removed = seen.iter().filter(|e| e.set.is_none()).count();
        (sim.get(id).expect("alive").mob.carried_block(), removed)
    };
    let (carried, removed) = carried_after(true, true);
    assert_eq!(carried.map(|s| s.block()), Some(lodestone_data::block::Block::Sand));
    assert_eq!(removed, 1, "exactly one block comes out");
    assert_eq!(carried_after(false, true).0, None, "control: a rejected edit leaves the hands empty");
    assert_eq!(carried_after(true, false).0, None, "control: griefing off");
}

/// A carrying enderman sets the block down on a cell with a floor, and only
/// when mobs may grief.
#[test]
fn a_carrying_enderman_puts_its_block_down() {
    let placed = |griefing: bool| {
        let base = field();
        let mut world = field();
        let (mut sim, id) = setup(&base, griefing);
        sim.get_mut(id)
            .expect("enderman")
            .mob
            .restore_carried_block(Some(lodestone_data::block::Block::Sand.default_state()));
        let seen = run(&mut sim, &mut world, 20_000, &|_| true);
        (seen.iter().filter(|e| e.set.is_some()).count(), sim.get(id).expect("alive").mob.carried_block())
    };
    let (count, _) = placed(true);
    assert!(count >= 1, "it never set the block down");
    assert_eq!(placed(false).0, 0, "control: griefing off");
}

#[test]
fn a_carried_block_persists() {
    let base = field();
    let (mut sim, id) = setup(&base, true);
    let block = lodestone_data::block::Block::GrassBlock.default_state();
    sim.get_mut(id).expect("enderman").mob.restore_carried_block(Some(block));
    let saved = sim.saved_mob(sim.get(id).expect("enderman"));
    let mut other = MobSim::new(&base);
    other.restore_saved(&[saved]);
    let restored = other.mobs.iter().find(|m| m.entity_type().path() == "enderman").expect("restored");
    assert_eq!(restored.mob.carried_block(), Some(block));
}

