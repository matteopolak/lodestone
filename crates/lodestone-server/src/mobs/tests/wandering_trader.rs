use super::*;

/// A real floor — see `leash_tests::flat_world`'s own doc comment for
/// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=16 {
        for z in -8..=16 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

#[test]
fn spawning_a_wandering_trader_leashes_two_llamas_to_it() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);

    let (trader_id, llamas) = sim.spawn_wandering_trader(Vec3::new(10.0, 5.0, 10.0));

    assert_eq!(llamas.len(), 2, "vanilla attempts exactly two escorts");
    let trader = sim.get(trader_id).expect("trader spawned");
    assert_eq!(trader.entity_type().path(), "wandering_trader");

    for llama_id in llamas {
        let llama = sim.get(llama_id).expect("llama spawned");
        assert_eq!(llama.entity_type().path(), "trader_llama");
        assert_eq!(
            llama.leash_holder(),
            Some(LeashHolder::Mob(trader_id)),
            "each llama must be leashed to the trader, not merely placed near it"
        );
    }
}

/// **The leash is real, not cosmetic** — moving the trader and ticking
/// leashes must pull an escort that has drifted past the elastic
/// distance, exactly as it would for a player-held leash. This is the
/// control that separates "the llama has a `leash_holder` field set" from
/// "the llama is actually tethered".
#[test]
fn the_escort_leash_actually_pulls_when_the_trader_moves_away() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let (trader_id, llamas) = sim.spawn_wandering_trader(Vec3::new(0.0, 0.0, 0.0));
    let llama_id = llamas[0];

    // Drag the trader far enough that the escort (2 blocks from spawn,
    // at x=2) is past LEASH_ELASTIC_DIST (6) from it but still short of
    // LEASH_TOO_FAR_DIST (12), so this exercises the *pull* branch —
    // distance 8, not the snap branch a farther drag would hit instead.
    // There is no teleport API, so drive it through a knockback impulse
    // large enough to land at the target position deterministically.
    let trader_pos = sim.get(trader_id).expect("spawned").position();
    let target = Vec3::new(10.0, 0.0, 0.0);
    sim.get_mut(trader_id).expect("spawned").apply_knockback(Vec3::new(
        target.x - trader_pos.x,
        target.y - trader_pos.y,
        target.z - trader_pos.z,
    ));
    assert_eq!(sim.get(trader_id).expect("spawned").position(), target);

    let llama_before = sim.get(llama_id).expect("spawned").position();
    sim.tick_leashes();
    let llama_after = sim.get(llama_id).expect("still spawned").position();
    assert_ne!(
        llama_before, llama_after,
        "the llama must move toward its holder once the trader is far enough away"
    );
}
