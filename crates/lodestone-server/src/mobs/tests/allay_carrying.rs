use super::*;

fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

fn alice() -> PlayerIdentity {
    PlayerIdentity {
        uuid: Uuid::from_u128(0xA11CE),
        entity_id: 4242,
    }
}

/// The empty-handed allay "carrying" interaction path: an allay given an
/// item must take it
/// into its main hand, consume the item, and report `ItemGiven`.
#[test]
fn giving_an_empty_handed_allay_an_item_makes_it_hold_that_item() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(
            "minecraft:allay".parse().expect("valid key"),
            Vec3::new(0.0, 0.0, 0.0),
        )
        .id();
    assert!(
        sim.get(id).expect("spawned").mob.main_hand_item().is_none(),
        "a freshly spawned allay must start empty-handed"
    );

    let outcome = sim.interact(
        id,
        alice(),
        Some(&"minecraft:emerald".parse().expect("valid key")),
    );

    assert_eq!(outcome, InteractOutcome::ItemGiven);
    assert!(
        outcome.consumes_item(),
        "the given item must be consumed, matching itemStack.consume(1, player)"
    );
    assert_eq!(
        sim.get(id).expect("still alive").mob.main_hand_item(),
        Some("emerald"),
        "the allay must now be carrying exactly the item it was given"
    );
}

/// The negative control: an allay **already** carrying an item must
/// refuse a second one — vanilla's own allay interaction override's gate is
/// specifically "empty main hand", not "any interaction with an item".
/// Without this, the positive gate above could be passing because every
/// interaction overwrites the held item unconditionally rather than
/// because the empty-hand gate is real.
#[test]
fn an_already_carrying_allay_refuses_a_second_item() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(
            "minecraft:allay".parse().expect("valid key"),
            Vec3::new(0.0, 0.0, 0.0),
        )
        .id();
    let first = sim.interact(
        id,
        alice(),
        Some(&"minecraft:emerald".parse().expect("valid key")),
    );
    assert_eq!(first, InteractOutcome::ItemGiven);

    let second = sim.interact(
        id,
        alice(),
        Some(&"minecraft:diamond".parse().expect("valid key")),
    );
    assert_eq!(
        second,
        InteractOutcome::Pass,
        "an allay already carrying an item must refuse a second one"
    );
    assert_eq!(
        sim.get(id).expect("still alive").mob.main_hand_item(),
        Some("emerald"),
        "the original item must still be held after the refused second gift"
    );
}

/// A second negative control: an empty-handed interaction (no item held
/// by the actor) must never clear or otherwise touch an allay's hands.
#[test]
fn an_empty_hand_interaction_does_nothing_to_an_allay() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(
            "minecraft:allay".parse().expect("valid key"),
            Vec3::new(0.0, 0.0, 0.0),
        )
        .id();

    let outcome = sim.interact(id, alice(), None);
    assert_eq!(outcome, InteractOutcome::Pass);
    assert!(sim.get(id).expect("still alive").mob.main_hand_item().is_none());
}

/// An allay-specific pickup check: a carrying allay
/// with a matching item dropped right next to it absorbs the whole
/// stack into [`SimMob::allay_inventory_count`] and the ground item is
/// gone, driven through the real production path (`MobSim::tick` →
/// `allay_pick_up_items`).
#[test]
fn an_allay_picks_up_a_matching_dropped_item_nearby() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:allay".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.interact(id, alice(), Some(&"minecraft:stick".parse().expect("valid key")));
    let stick_id = sim.spawn_item(
        "minecraft:stick".parse().expect("valid key"),
        Vec3::new(0.5, 0.0, 0.0),
        Vec3::new(0.0, 0.0, 0.0),
        ItemLifecycle::newly_dropped(3, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
    );

    sim.tick();

    assert_eq!(
        sim.get(id).expect("alive").allay_inventory_count(),
        3,
        "the whole 3-stack must be absorbed"
    );
    assert!(
        sim.item_lifecycle(stick_id).is_none(),
        "the fully-absorbed ground stack must be removed, not left at count 0"
    );
}

/// **Control**: an emerald dropped next to a stick-carrying allay must
/// never be picked up — `allayConsidersItemEqual`'s own item-identity
/// gate, without which the positive test above could be passing because
/// every nearby item is absorbed regardless of type.
#[test]
fn an_allay_ignores_a_dropped_item_of_a_different_type() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:allay".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.interact(id, alice(), Some(&"minecraft:stick".parse().expect("valid key")));
    sim.spawn_item(
        "minecraft:emerald".parse().expect("valid key"),
        Vec3::new(0.5, 0.0, 0.0),
        Vec3::new(0.0, 0.0, 0.0),
        ItemLifecycle::newly_dropped(1, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
    );

    sim.tick();

    assert_eq!(
        sim.get(id).expect("alive").allay_inventory_count(),
        0,
        "a mismatched item type must never be picked up"
    );
    assert_eq!(sim.item_count(), 1, "the mismatched item must still be on the ground");
}

/// `GoAndGiveItemsToTarget`: a carrying allay standing at its own liked
/// note block's `.above()` cell throws exactly one item there per tick
/// — a real dropped [`crate::item_entity::ItemEntity`] a player could
/// walk over, not a state flag. Drives `MobSim::tick` →
/// `allay_deliver_items` directly against host state set the way
/// `resolve_vibrations`' own `hearNoteblock` arm would have left it,
/// isolating the *delivery* half from the *hearing* half already proven
/// end-to-end in `crate::tick`'s own note-block gates.
#[test]
fn a_carrying_allay_at_its_liked_noteblock_delivers_one_item_per_tick() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(
            "minecraft:allay".parse().expect("valid key"),
            Vec3::new(0.0, 1.0, 0.0),
        )
        .id();
    sim.interact(id, alice(), Some(&"minecraft:stick".parse().expect("valid key")));
    {
        let mob = sim.get_mut(id).expect("alive");
        mob.allay_inventory_count = 2;
        mob.allay_liked_noteblock = Some((Vec3::new(0.0, 0.0, 0.0), 100));
    }

    sim.tick();

    assert_eq!(
        sim.get(id).expect("alive").allay_inventory_count(),
        1,
        "exactly one item must be thrown this tick"
    );
    assert_eq!(sim.item_count(), 1, "the thrown item must be a real ground entity");
}

/// **Control**: the identical fixture but far from any liked note block
/// (`allay_liked_noteblock` left `None`) must never deliver — proving
/// the arrival check above is a real gate, not unconditional draining.
#[test]
fn a_carrying_allay_with_no_liked_noteblock_never_delivers() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(
            "minecraft:allay".parse().expect("valid key"),
            Vec3::new(0.0, 1.0, 0.0),
        )
        .id();
    sim.interact(id, alice(), Some(&"minecraft:stick".parse().expect("valid key")));
    sim.get_mut(id).expect("alive").allay_inventory_count = 2;

    sim.tick();

    assert_eq!(
        sim.get(id).expect("alive").allay_inventory_count(),
        2,
        "with nothing liked, nothing must be thrown"
    );
    assert_eq!(sim.item_count(), 0);
}

/// **The allay duplication arm, through the production path** — driven by
/// `allay_liked_noteblock` as the dance signal. An amethyst shard on such an allay
/// must spawn a second, real allay and consume the shard.
#[test]
fn an_amethyst_shard_duplicates_an_allay_that_recently_heard_a_noteblock() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:allay".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.get_mut(id).expect("alive").allay_liked_noteblock = Some((Vec3::new(3.0, 0.0, 0.0), 100));

    let before = sim.snapshots().iter().filter(|s| s.entity_type.path() == "allay").count();
    let outcome = sim.interact(
        id,
        alice(),
        Some(&"minecraft:amethyst_shard".parse().expect("valid key")),
    );

    assert_eq!(outcome, InteractOutcome::AllayDuplicated);
    assert!(outcome.consumes_item(), "the shard must be consumed");
    let after = sim.snapshots().iter().filter(|s| s.entity_type.path() == "allay").count();
    assert_eq!(after, before + 1, "duplication must spawn exactly one real new allay");
    assert!(
        sim.get(id).expect("alive").allay_duplication_cooldown > 0,
        "the original allay must be put on cooldown too"
    );
    assert!(
        sim.take_vocalisations().iter().any(|effect| matches!(
            effect,
            crate::effects::WorldEffect::Particles { particle, .. } if particle == "minecraft:heart"
        )),
        "vanilla's own allay entity-event handler's status-18 heart burst \
         must reach the production queue too, not just the outcome's own \
         particle() classification"
    );
}

/// **Control**: the identical shard interaction against an allay that
/// has never heard a note block must do nothing — proving the
/// `isDancing()` substitute is a real gate, not one that always fires
/// on an amethyst shard.
#[test]
fn an_amethyst_shard_does_nothing_to_an_allay_that_never_heard_a_noteblock() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:allay".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();

    let before = sim.snapshots().iter().filter(|s| s.entity_type.path() == "allay").count();
    let outcome = sim.interact(
        id,
        alice(),
        Some(&"minecraft:amethyst_shard".parse().expect("valid key")),
    );

    assert_ne!(
        outcome,
        InteractOutcome::AllayDuplicated,
        "an allay that never heard a note block must never duplicate"
    );
    let after = sim.snapshots().iter().filter(|s| s.entity_type.path() == "allay").count();
    assert_eq!(after, before, "no new allay must have spawned");
}
