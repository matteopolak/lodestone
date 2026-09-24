use super::*;
use lodestone_entity::pathfinding::PathWorld;

/// The jar's real `#minecraft:edible_for_sheep` membership
/// (`data/minecraft/tags/block/edible_for_sheep.json`), transcribed here
/// **only as the expectation**. The implementation does not contain this
/// list — it resolves the tag through `lodestone_data::tool`, which is
/// generated from the jar — so this is an independent statement of the answer
/// rather than a restatement of the code under test.
const JAR_EDIBLE: &[&str] = &[
    "minecraft:short_grass",
    "minecraft:short_dry_grass",
    "minecraft:tall_dry_grass",
    "minecraft:fern",
];

/// A single cell of `block` with air around it, at a fixed position.
fn world_of(block: &str) -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    world.set_block(0, 0, 0, block);
    world
}

/// **The gate that a hand-written tag list fails.**
///
/// Every member of the jar's tag must classify as edible. Three of the four
/// would have been missed by the obvious `short_grass | tall_grass` guess:
/// `short_dry_grass`, `tall_dry_grass` and `fern`. A sheep would have refused
/// to graze a fern, and no test in the tree would have said so.
#[test]
fn every_jar_tag_member_classifies_as_edible_for_sheep() {
    for block in JAR_EDIBLE {
        let world = world_of(block);
        assert!(
            world.block_cues(0, 0, 0).edible_for_sheep,
            "{block} is in #minecraft:edible_for_sheep and must classify as edible — \
             a hand-written list missing it is exactly how this stays silently wrong"
        );
    }
}

/// **The other half of the same mistake: the guess's false positive.**
///
/// `tall_grass` is *not* in `#minecraft:edible_for_sheep` — the jar tag has
/// four entries and that is not one of them. It is the block most likely to be
/// added by anyone writing the list from memory, and asserting only the
/// positives above would let it through.
#[test]
fn tall_grass_is_not_edible_for_sheep_despite_looking_like_it_should_be() {
    let world = world_of("minecraft:tall_grass");
    assert!(
        !world.block_cues(0, 0, 0).edible_for_sheep,
        "minecraft:tall_grass is absent from the jar's edible_for_sheep tag; \
         classifying it as edible means the tag is being guessed, not read"
    );
}

/// `grass_block` is the *equality* cue, not a tag member — vanilla's own
/// "eat block" goal tests it
/// with block equality. So it must set
/// `grass_block` and must **not** set `edible_for_sheep`: a sheep standing on
/// grass eats the block below, a sheep standing in short grass eats the block
/// at its feet, and conflating the two would make either mechanism fire in the
/// wrong place.
#[test]
fn grass_block_is_the_equality_cue_and_not_a_tag_member() {
    let cues = world_of("minecraft:grass_block").block_cues(0, 0, 0);
    assert!(cues.grass_block, "grass_block must set its own cue");
    assert!(
        !cues.edible_for_sheep,
        "grass_block is not in the edible tag — the two cues are independent"
    );
}

/// The negative control. Ordinary blocks and air must set neither cue,
/// otherwise the positives above are satisfied by a classifier that says yes
/// to everything.
#[test]
fn control_ordinary_blocks_set_no_cue_at_all() {
    for block in ["minecraft:stone", "minecraft:dirt", "minecraft:oak_log"] {
        let cues = world_of(block).block_cues(0, 0, 0);
        assert!(
            !cues.edible_for_sheep && !cues.grass_block,
            "{block} must set no cue; a classifier that says yes to everything \
             passes every positive assertion above"
        );
    }
}

/// Property strings must not defeat the lookup: `block_state` yields a full
/// state string, so a cue keyed on the raw string would miss any block with
/// properties. `tall_dry_grass` is a real tag member *and* carries a
/// `half`/`facing`-style property list in some states, which is why this is a
/// distinct case rather than a restatement of the first test.
#[test]
fn a_state_with_properties_still_classifies() {
    let mut world = ChunkWorld::new(-64, 384);
    world.set_block(0, 0, 0, "minecraft:short_grass");
    assert!(world.block_cues(0, 0, 0).edible_for_sheep);
    // The `grass_block` arm goes through the same property strip.
    world.set_block(0, 1, 0, "minecraft:grass_block[snowy=false]");
    assert!(
        world.block_cues(0, 1, 0).grass_block,
        "a state with a property list must still match the equality cue — \
         `block_state` returns the full string, properties included"
    );
}

/// **The handoff gate.** A grazing mob's eat must survive `MobSim::tick` and
/// emerge from [`MobSim::take_grazes`].
///
/// The test supplies the goal directly, so the assertion covers only the
/// `take_new_eaten` → `pending_grazes` → `take_grazes` handoff. The
/// production roster intentionally has no sheep-eating goal.
///
/// It is deliberately **not** an assertion about the eat interval. That is
/// `lodestone-entity`'s `block_perception.rs` gate, which distinguishes 444
/// predicted eats from 286 — and which also recorded that a rate measured in a
/// mutating world measures grass scarcity instead. Nothing drains the world
/// here, so supply is infinite and the tick budget only has to make "at least
/// one eat" overwhelmingly likely: at the halved 1-in-500 adult interval,
/// 20,000 ticks puts the probability of zero at about e^-40.
#[test]
fn a_grazing_mob_hands_its_eat_to_the_driver() {
    let mut world = ChunkWorld::new(-64, 384);
    // Grass to stand on, short grass to stand in — so both cues are live and
    // whichever arm fires, the handoff is exercised.
    //
    // Wide enough that idle wandering cannot walk the sheep off it in
    // 20,000 ticks. That is not padding: at 5×5 the sheep reached the edge and
    // grazed at (-2, 0, -2), and outside the patch there is no floor at all,
    // so a narrower world tests falling rather than grazing.
    for x in -24..=24 {
        for z in -24..=24 {
            world.set_block(x, -1, z, "minecraft:grass_block");
            world.set_block(x, 0, z, "minecraft:short_grass");
        }
    }

    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(
            ResourceKey::from_str("minecraft:sheep").expect("valid key"),
            Vec3::new(0.5, 0.0, 0.5),
        )
        .id();
    sim.get_mut(id).expect("just spawned").add_goal(
        5,
        Box::new(lodestone_entity::ai::goals::EatBlockGoal::new()),
    );

    assert!(
        sim.take_grazes().is_empty(),
        "precondition: nothing is pending before any tick, so the assertion \
         below cannot be satisfied by a stale entry"
    );

    let mut grazes = Vec::new();
    for _ in 0..20_000 {
        sim.tick();
        grazes.extend(sim.take_grazes());
        if !grazes.is_empty() {
            break;
        }
    }

    assert!(
        !grazes.is_empty(),
        "a sheep standing in short grass on a grass block must record an eat \
         that reaches take_grazes; empty means the handoff is broken and #238 \
         can never mutate the world"
    );
    // The recorded position must be the *mob's* cell, not the eaten block's —
    // the consumer resolves `AtFeet` as that cell and `Below` as one down, so
    // reporting the eaten cell would make the `Below` arm write dirt a block
    // too low.
    //
    // **`y` is the whole assertion.** `x`/`z` are identical for both
    // candidates, so they carry no information about which one this is; only
    // the height distinguishes the mob's feet (`0`) from the grass block it
    // stands on (`-1`). An earlier draft of this pinned the full triple to
    // `(0, 0, 0)` and failed at `(-2, 0, -2)` — idle wandering had walked
    // the sheep two blocks before it grazed, so that assertion was testing a
    // false premise (that the mob holds still) rather than the handoff.
    let (pos, _what) = grazes[0];
    assert_eq!(
        pos.y, 0,
        "the handoff must carry the mob's own feet cell (y=0), not the grass \
         block below it (y=-1) — the EatenBlock variants are relative to the mob"
    );
    assert!(
        (-24..=24).contains(&pos.x) && (-24..=24).contains(&pos.z),
        "the graze must be recorded somewhere on the prepared patch, got \
         ({}, {}) — off-patch means the sheep grazed a cell with no grass",
        pos.x,
        pos.z
    );

    // Draining really drains: a second read must not re-report the same eat,
    // or a slow consumer would apply it twice.
    assert!(
        sim.take_grazes().is_empty(),
        "take_grazes must drain, not merely read"
    );
}
