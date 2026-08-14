//! Can a player tame a mob to *themselves*, and does the ownership go anywhere?
//!
//! # What these gates are for
//!
//! Before this, `SimMob::owner_id` existed with **no producer** — an island in
//! the "nothing calls this" direction — because `PlayerPerception` carried only a
//! position and a held item, so "tame this wolf to *me*" was not expressible.
//! Every gate here therefore starts from a [`PlayerIdentity`] and ends at
//! something observable: a resolved owner position, a sitting pose the goal
//! actually entered, a movement toward the owner, a particle on the effect queue,
//! or an experience orb.
//!
//! Two habits are load-bearing:
//!
//! * **The RNG is driven to both sides of each threshold.** A tame *chance*
//!   asserted as "sometimes tames" measures that the code runs. Each gate finds a
//!   seed whose first draw is known, then requires the exact outcome. Because the
//!   draw *count* is part of the specification, a seed chosen for `next_int(3)`
//!   is also checked against `next_int(10)` where the two mechanisms must differ.
//! * **Mismatches are collected, not asserted inside the loop.** An `assert!`
//!   inside a `for` aborts on the first failure, so a four-species gate would
//!   prove one species and leave the other three as arguments rather than
//!   observations.

use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{
    ChunkWorld, InteractOutcome, MobOwner, MobSim, PerceivedPlayer, PlayerIdentity,
    PlayerPerception, SpawnRng,
};
use std::str::FromStr;
use uuid::Uuid;

fn rk(name: &str) -> ResourceKey {
    ResourceKey::from_str(name).expect("valid key")
}

/// A flat floor at y = -1, wide enough for a pet to walk a dozen blocks on.
fn pen() -> ChunkWorld {
    let mut world = ChunkWorld::new(-4, 24);
    for x in -32..=32 {
        for z in -32..=32 {
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

fn bob() -> PlayerIdentity {
    PlayerIdentity {
        uuid: Uuid::from_u128(0xB0B),
        entity_id: 4343,
    }
}

fn seen(identity: PlayerIdentity, at: Vec3) -> PerceivedPlayer {
    PerceivedPlayer {
        identity: Some(identity),
        perception: PlayerPerception {
            position: at,
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }
}

/// The first `next_int(bound)` a stream seeded with `seed` yields. Used to
/// *choose* seeds rather than to predict outcomes from them, so the gates below
/// stay readable when `SpawnRng`'s internals change.
fn first_draw(seed: u64, bound: i32) -> i32 {
    SpawnRng::new(seed).next_int(bound)
}

/// A seed whose first `next_int(bound)` satisfies `want`, searched over a small
/// range so a gate can say "drive the roll to this side of the threshold" without
/// hardcoding a magic number that a change to `SpawnRng` would silently
/// invalidate into the *passing* direction.
fn seed_where(bound: i32, want: impl Fn(i32) -> bool) -> u64 {
    (1u64..100_000)
        .find(|&seed| want(first_draw(seed, bound)))
        .expect("a draw satisfying the predicate exists within the search range")
}

// ---------------------------------------------------------------------------
// Taming: the roll
// ---------------------------------------------------------------------------

/// The wolf's `tryToTame` is one `random.nextInt(3)` draw and success is a draw
/// of exactly `0`. Both sides are driven and the **exact** outcome is required on
/// each, together with the state each one leaves behind — a gate that only
/// checked `Tamed` would pass for an implementation that tamed without recording
/// an owner.
#[test]
fn a_bone_tames_a_wolf_on_a_zero_draw_and_fails_on_any_other() {
    let world = pen();

    for (label, seed, want_tamed) in [
        ("success", seed_where(3, |d| d == 0), true),
        ("failure", seed_where(3, |d| d != 0), false),
    ] {
        let mut sim = MobSim::new(&world);
        sim.set_tame_rng(SpawnRng::new(seed));
        let wolf = sim
            .spawn_species(rk("minecraft:wolf"), Vec3::new(0.0, 0.0, 0.0))
            .id();

        let outcome = sim.interact(wolf, alice(), Some(&rk("minecraft:bone")));
        let mob = sim.get(wolf).expect("alive");

        if want_tamed {
            assert_eq!(outcome, InteractOutcome::Tamed, "{label}");
            assert!(mob.is_tame(), "{label}: the 0x04 tame flag must be set");
            assert_eq!(
                mob.owner(),
                Some(MobOwner::Player(alice().uuid)),
                "{label}: ownership is keyed on the account uuid"
            );
            assert!(
                mob.is_ordered_to_sit(),
                "{label}: `Wolf.tryToTame` ends with `setOrderedToSit(true)`"
            );
        } else {
            assert_eq!(outcome, InteractOutcome::TameFailed, "{label}");
            assert!(!mob.is_tame(), "{label}");
            assert_eq!(mob.owner(), None, "{label}");
            assert!(
                !mob.is_ordered_to_sit(),
                "{label}: a failed tame must not sit the wolf down"
            );
        }
        // Both outcomes consume the bone — that is what makes taming cost bones
        // rather than patience.
        assert!(outcome.consumes_item(), "{label}");
    }
}

/// A parrot's odds are `nextInt(10)`, not the wolf's and cat's `nextInt(3)`, and
/// this gate is built on a draw where the two **disagree**: a stream whose first
/// `next_int(3)` is `0` while its first `next_int(10)` is not. Under that one
/// seed a wolf must tame and a parrot must not.
///
/// Without a discriminating seed this would pass for a single shared constant,
/// which is exactly the "one mechanism with different constants" reading the
/// species tables exist to refute.
#[test]
fn the_parrots_odds_are_ten_and_the_wolfs_are_three() {
    let world = pen();
    let seed = seed_where(3, |d| d == 0);
    // The premise, asserted rather than assumed: this seed has to separate the
    // two bounds or the gate below is measuring nothing.
    assert_eq!(first_draw(seed, 3), 0, "chosen for the 1-in-3 mechanism");
    assert_ne!(
        first_draw(seed, 10),
        0,
        "the same seed must miss the 1-in-10 mechanism, or this gate cannot \
         tell the two odds apart"
    );

    let mut wolf_sim = MobSim::new(&world);
    wolf_sim.set_tame_rng(SpawnRng::new(seed));
    let wolf = wolf_sim
        .spawn_species(rk("minecraft:wolf"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    assert_eq!(
        wolf_sim.interact(wolf, alice(), Some(&rk("minecraft:bone"))),
        InteractOutcome::Tamed
    );

    let mut parrot_sim = MobSim::new(&world);
    parrot_sim.set_tame_rng(SpawnRng::new(seed));
    let parrot = parrot_sim
        .spawn_species(rk("minecraft:parrot"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    assert_eq!(
        parrot_sim.interact(parrot, alice(), Some(&rk("minecraft:wheat_seeds"))),
        InteractOutcome::TameFailed,
        "a parrot's 1-in-10 roll must reject a draw the wolf's 1-in-3 accepts"
    );
}

/// A parrot that *is* tamed must **not** be ordered to sit — `Parrot.mobInteract`
/// is the one of the three `tryToTame` bodies that omits `setOrderedToSit(true)`.
/// Asserting only "it tamed" would miss this, and a pet that sits itself down the
/// moment you tame it is visibly wrong.
#[test]
fn taming_a_parrot_does_not_sit_it_down_but_taming_a_cat_does() {
    let world = pen();
    let mut mismatches: Vec<String> = Vec::new();

    // (species, taming item, does vanilla's `tryToTame` also sit it?)
    for &(species, item, sits) in &[
        ("wolf", "bone", true),
        ("cat", "cod", true),
        ("parrot", "wheat_seeds", false),
    ] {
        let bound = if species == "parrot" { 10 } else { 3 };
        let mut sim = MobSim::new(&world);
        sim.set_tame_rng(SpawnRng::new(seed_where(bound, |d| d == 0)));
        let id = sim
            .spawn_species(rk(&format!("minecraft:{species}")), Vec3::new(0.0, 0.0, 0.0))
            .id();
        let outcome = sim.interact(id, alice(), Some(&rk(&format!("minecraft:{item}"))));
        let mob = sim.get(id).expect("alive");
        if outcome != InteractOutcome::Tamed {
            mismatches.push(format!("{species}: expected Tamed, got {outcome:?}"));
            continue;
        }
        if mob.is_ordered_to_sit() != sits {
            mismatches.push(format!(
                "{species}: sit-on-tame is {}, vanilla's is {sits}",
                mob.is_ordered_to_sit()
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "collected so every species reports rather than only the first: {mismatches:?}"
    );
}

/// The two discriminating negatives, without which "it tamed" is satisfied by a
/// function that tames everything: the **wrong item** on a tameable species, and
/// the **right-looking item** on a species that is not tameable at all.
///
/// The draw is forced to the success side throughout, so any tame here is the
/// dispatch being wrong rather than the roll being lucky.
#[test]
fn the_wrong_item_and_an_untameable_species_are_both_left_alone() {
    let world = pen();
    let lucky = seed_where(3, |d| d == 0);
    let mut mismatches: Vec<String> = Vec::new();

    // (species, item, why this must not tame)
    let cases: &[(&str, &str, &str)] = &[
        ("wolf", "wheat", "wheat is in no wolf tag at all"),
        (
            "wolf",
            "beef",
            "#wolf_food breeds a wolf and never tames it — only a bone tames",
        ),
        ("cat", "bone", "a bone is the wolf's item, not the cat's"),
        (
            "parrot",
            "cookie",
            "#parrot_poisonous_food is not #parrot_food",
        ),
        ("cow", "bone", "a cow is not tameable by anything"),
        ("cow", "wheat", "wheat breeds a cow, it does not tame it"),
        ("creeper", "bone", "a creeper is not tameable"),
    ];

    for &(species, item, why) in cases {
        let mut sim = MobSim::new(&world);
        sim.set_tame_rng(SpawnRng::new(lucky));
        let id = sim
            .spawn_species(rk(&format!("minecraft:{species}")), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.interact(id, alice(), Some(&rk(&format!("minecraft:{item}"))));
        let mob = sim.get(id).expect("alive");
        if mob.is_tame() || mob.owner().is_some() {
            mismatches.push(format!("{species} + {item} became tame — {why}"));
        }
    }

    assert!(mismatches.is_empty(), "{mismatches:?}");
}

// ---------------------------------------------------------------------------
// Ownership: the identity at the seam
// ---------------------------------------------------------------------------

/// Ownership is keyed on the account **uuid**, and this is the gate that says so
/// rather than the doc comment.
///
/// Three arms, and the middle one is the point: the same player reconnecting with
/// a **different runtime entity id** still owns their pet, because vanilla's
/// `TamableAnimal.DATA_OWNERUUID_ID` is a uuid on the wire and in NBT alike. An
/// implementation keyed on the entity id passes the first arm and fails the
/// second, which is precisely the bug the two-field
/// [`PlayerIdentity`] exists to avoid.
#[test]
fn a_pets_owner_resolves_by_uuid_and_survives_a_new_entity_id() {
    let world = pen();
    let mut sim = MobSim::new(&world);
    sim.set_tame_rng(SpawnRng::new(seed_where(3, |d| d == 0)));
    let wolf = sim
        .spawn_species(rk("minecraft:wolf"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    assert_eq!(
        sim.interact(wolf, alice(), Some(&rk("minecraft:bone"))),
        InteractOutcome::Tamed
    );

    // Arm 1: the owner is in the list, so the seam carries their position.
    sim.set_players(vec![seen(alice(), Vec3::new(6.0, 0.0, 0.0))]);
    sim.tick();
    assert_eq!(
        sim.get(wolf).expect("alive").owner_position(),
        Some(Vec3::new(6.0, 0.0, 0.0)),
        "a tamed pet must resolve its owner's live position"
    );

    // Arm 2: same account, brand-new entity id — a reconnect.
    let reconnected = PlayerIdentity {
        uuid: alice().uuid,
        entity_id: alice().entity_id + 9_999,
    };
    sim.set_players(vec![seen(reconnected, Vec3::new(-3.0, 0.0, 2.0))]);
    sim.tick();
    assert_eq!(
        sim.get(wolf).expect("alive").owner_position(),
        Some(Vec3::new(-3.0, 0.0, 2.0)),
        "ownership keyed on the runtime entity id would have lost the pet here"
    );

    // Arm 3: a different account standing in exactly the same place resolves to
    // nothing. Without this the first two arms are satisfied by "resolve the
    // nearest player".
    sim.set_players(vec![seen(bob(), Vec3::new(-3.0, 0.0, 2.0))]);
    sim.tick();
    assert_eq!(
        sim.get(wolf).expect("alive").owner_position(),
        None,
        "a stranger standing where the owner stood is not the owner"
    );
    assert!(
        sim.get(wolf).expect("alive").is_tame(),
        "and the pet is still tame — tameness is not derived from a resolved owner"
    );
}

/// A producer that supplies no identity can still be looked at and tempted, and
/// still owns nothing. This is the arm that keeps `From<PlayerPerception>`'s
/// identity-less conversion honest: an unidentified player must not inherit
/// somebody else's pet.
#[test]
fn an_unidentified_player_is_never_resolved_as_an_owner() {
    let world = pen();
    let mut sim = MobSim::new(&world);
    sim.set_tame_rng(SpawnRng::new(seed_where(3, |d| d == 0)));
    let wolf = sim
        .spawn_species(rk("minecraft:wolf"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.interact(wolf, alice(), Some(&rk("minecraft:bone")));

    // The bare-`PlayerPerception` shape, which is what a producer with no
    // identity supplies.
    sim.set_players(vec![PlayerPerception {
        position: Vec3::new(6.0, 0.0, 0.0),
        held_item: None,
        view_direction: Vec3::new(0.0, 0.0, 1.0),
    }]);
    sim.tick();
    let mob = sim.get(wolf).expect("alive");
    assert_eq!(
        mob.owner_position(),
        None,
        "an identity-less player must not resolve as anyone's owner"
    );
    assert_eq!(
        mob.nearest_player(),
        Some(Vec3::new(6.0, 0.0, 0.0)),
        "but they are still perceived — the control that says the feed ran at all"
    );
}

/// Only the owner may command a pet. A stranger's right-click on somebody else's
/// tamed wolf is `Pass`, leaving the sitting order untouched.
#[test]
fn a_stranger_cannot_sit_someone_elses_pet() {
    let world = pen();
    let mut sim = MobSim::new(&world);
    sim.set_tame_rng(SpawnRng::new(seed_where(3, |d| d == 0)));
    let wolf = sim
        .spawn_species(rk("minecraft:wolf"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.interact(wolf, alice(), Some(&rk("minecraft:bone")));
    // `tryToTame` left it sitting.
    assert!(sim.get(wolf).expect("alive").is_ordered_to_sit());

    assert_eq!(
        sim.interact(wolf, bob(), None),
        InteractOutcome::Pass,
        "a tame animal ignores everyone but its owner"
    );
    assert!(
        sim.get(wolf).expect("alive").is_ordered_to_sit(),
        "and the order is unchanged"
    );

    // The owner's own click toggles it, twice, so the gate distinguishes a
    // toggle from a set.
    assert_eq!(
        sim.interact(wolf, alice(), None),
        InteractOutcome::SitToggled { sitting: false }
    );
    assert_eq!(
        sim.interact(wolf, alice(), None),
        InteractOutcome::SitToggled { sitting: true }
    );
    assert!(
        !InteractOutcome::SitToggled { sitting: true }.consumes_item(),
        "`InteractionResult.SUCCESS.withoutItem()` — sitting a pet eats nothing"
    );
}

/// A tame pet's owner feeding it its food item **heals** it and does not breed
/// it, and only once it is at full health does the same item put it in love.
/// Those are two different arms of `Wolf.mobInteract`, in that order, and a port
/// that reordered them would look correct in any test that only fed a healthy
/// wolf.
#[test]
fn feeding_a_hurt_pet_heals_it_and_feeding_a_healthy_one_breeds_it() {
    let world = pen();
    let mut sim = MobSim::new(&world);
    sim.set_tame_rng(SpawnRng::new(seed_where(3, |d| d == 0)));
    let wolf = sim
        .spawn_species(rk("minecraft:wolf"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.interact(wolf, alice(), Some(&rk("minecraft:bone")));

    let max = sim.get(wolf).expect("alive").max_health();
    sim.get_mut(wolf).expect("alive").set_health(max - 4.0);

    assert_eq!(
        sim.interact(wolf, alice(), Some(&rk("minecraft:beef"))),
        InteractOutcome::Fed,
        "a hurt pet is healed, not bred"
    );
    // `Wolf.feed(player, hand, stack, 2.0F, 2.0F)` — 2.0, not the cat's 1.0 and
    // not "some healing".
    assert!(
        (sim.get(wolf).expect("alive").health() - (max - 2.0)).abs() < 1e-6,
        "the wolf's own heal is 2.0; measured {}",
        sim.get(wolf).expect("alive").health()
    );
    assert!(!sim.get(wolf).expect("alive").is_in_love());

    sim.get_mut(wolf).expect("alive").set_health(max);
    assert_eq!(
        sim.interact(wolf, alice(), Some(&rk("minecraft:beef"))),
        InteractOutcome::InLove,
        "at full health the same item reaches `Animal.mobInteract`'s love arm"
    );
    assert!(sim.get(wolf).expect("alive").is_in_love());
}

// ---------------------------------------------------------------------------
// Ownership reaching behaviour
// ---------------------------------------------------------------------------

/// `SitWhenOrderedToGoal` and `FollowOwnerGoal` were `Missing` rows in the wolf's
/// roster table because no owner could be a player. This gate drives the **real**
/// spawn path (never `add_goal`) and asserts both directions of the one switch
/// that separates them, because either alone is satisfied by a pet that never
/// moves or by one that never stops.
#[test]
fn a_tamed_wolf_sits_when_ordered_and_walks_to_its_owner_when_not() {
    let world = pen();
    let mut sim = MobSim::new(&world);
    sim.set_tame_rng(SpawnRng::new(seed_where(3, |d| d == 0)));
    let wolf = sim
        .spawn_species(rk("minecraft:wolf"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.interact(wolf, alice(), Some(&rk("minecraft:bone")));
    assert!(sim.get(wolf).expect("alive").is_ordered_to_sit());

    // Owner well beyond the wolf's 10-block start distance.
    let owner_at = Vec3::new(16.0, 0.0, 0.0);
    sim.set_players(vec![seen(alice(), owner_at)]);

    sim.tick_for(40);
    let sat_at = sim.position(wolf).expect("alive");
    assert!(
        sim.get(wolf).expect("alive").is_in_sitting_pose(),
        "the sit goal must have actually run, not merely been installed"
    );
    assert!(
        sat_at.x < 2.0,
        "a sitting wolf must not walk to its owner; it reached x = {}",
        sat_at.x
    );

    // Stand it up. Nothing else changes.
    sim.get_mut(wolf).expect("alive").set_ordered_to_sit(false);
    sim.tick_for(120);
    let followed_to = sim.position(wolf).expect("alive");
    assert!(
        !sim.get(wolf).expect("alive").is_in_sitting_pose(),
        "the pose must be released when the order is"
    );
    assert!(
        followed_to.x > sat_at.x + 2.0,
        "a standing wolf must close on its owner: {} -> {}",
        sat_at.x,
        followed_to.x
    );
}

/// Same shape as the wolf's gate above, but for the **cat** — issue #229's
/// reported gap: cat and parrot were tameable and ownable with no roster
/// entry at all, so a tamed one could be owned and would never sit or follow.
///
/// `FollowOwnerGoal(1.0, 10.0F, 5.0F)` (`animal/feline/Cat.java:113`): a cat
/// stops **five** blocks out, not the wolf's two, so the final-position band
/// below is what actually discriminates "the cat's own row" from "a copy of
/// the wolf's row" — a `> sat_at.x + 2.0` check alone would pass either way.
#[test]
fn a_tamed_cat_sits_when_ordered_and_walks_to_its_owner_when_not() {
    let world = pen();
    let mut sim = MobSim::new(&world);
    sim.set_tame_rng(SpawnRng::new(seed_where(3, |d| d == 0)));
    let cat = sim
        .spawn_species(rk("minecraft:cat"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    // `#cat_food` is raw cod or salmon — and, per `docs/taming-and-breeding.md`,
    // also the cat's *whole* food tag, so this interaction cannot fall through
    // to a breeding arm the way the wolf's bone (in no wolf food tag) does.
    sim.interact(cat, alice(), Some(&rk("minecraft:cod")));
    assert!(
        sim.get(cat).expect("alive").is_ordered_to_sit(),
        "Cat.tryToTame also auto-sits on success, same as the wolf \
         (`mobs.rs`'s `tame_mechanism`'s `sit_on_success: true` for cat)"
    );

    // Beyond the cat's 10-block start distance.
    let owner_at = Vec3::new(16.0, 0.0, 0.0);
    sim.set_players(vec![seen(alice(), owner_at)]);

    sim.tick_for(40);
    let sat_at = sim.position(cat).expect("alive");
    assert!(
        sim.get(cat).expect("alive").is_in_sitting_pose(),
        "the sit goal must have actually run, not merely been installed"
    );
    assert!(
        sat_at.x < 2.0,
        "a sitting cat must not walk to its owner; it reached x = {}",
        sat_at.x
    );

    sim.get_mut(cat).expect("alive").set_ordered_to_sit(false);
    sim.tick_for(200);
    let followed_to = sim.position(cat).expect("alive");
    assert!(
        !sim.get(cat).expect("alive").is_in_sitting_pose(),
        "the pose must be released when the order is"
    );
    let final_gap = owner_at.x - followed_to.x;
    assert!(
        (3.5..7.5).contains(&final_gap),
        "a standing cat must close toward its owner and stop near its own \
         5-block stop distance (`Cat.java:113`) — not the wolf's 2, and not \
         zero (no stop at all); it ended {final_gap} blocks out (moved {} -> \
         {})",
        sat_at.x,
        followed_to.x
    );
}

/// Same shape again, for the **parrot** — the species whose taming mechanism
/// deliberately does *not* auto-sit (`docs/taming-and-breeding.md` §2), which
/// this gate asserts as its own explicit step rather than assuming it from
/// the cat/wolf pattern. `FollowOwnerGoal(1.0, 5.0F, 1.0F)`
/// (`animal/parrot/Parrot.java:167`) is the tightest follow distance in the
/// tameable set, which the final-position band below is chosen to separate
/// from both the wolf's and the cat's.
#[test]
fn a_tamed_parrot_does_not_auto_sit_but_can_still_be_ordered_to_and_follows_tightly() {
    let world = pen();
    let mut sim = MobSim::new(&world);
    sim.set_tame_rng(SpawnRng::new(seed_where(10, |d| d == 0)));
    let parrot = sim
        .spawn_species(rk("minecraft:parrot"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.interact(parrot, alice(), Some(&rk("minecraft:wheat_seeds")));
    assert!(
        sim.get(parrot).expect("alive").is_tame(),
        "the tame roll must have succeeded at the driven seed"
    );
    assert!(
        !sim.get(parrot).expect("alive").is_ordered_to_sit(),
        "`Parrot.tryToTame` is the one of the three taming mechanisms that \
         omits the automatic `setOrderedToSit(true)` — a parrot that sits \
         itself down on being tamed is visibly wrong, per `mobs.rs`'s \
         `tame_mechanism`'s `sit_on_success: false` for parrot"
    );

    // Right-click again, empty-handed: `interact_tamable`'s last arm, the
    // sit toggle — present because `Parrot.java:166` really does register
    // `SitWhenOrderedToGoal`, only the auto-sit-on-tame side effect is
    // parrot-specific. Removing the roster row for "the parrot doesn't sit"
    // would make this assertion fail.
    sim.interact(parrot, alice(), None);
    assert!(
        sim.get(parrot).expect("alive").is_ordered_to_sit(),
        "a tame parrot must still accept an explicit sit order from its owner"
    );

    // Beyond the parrot's 5-block start distance.
    let owner_at = Vec3::new(10.0, 0.0, 0.0);
    sim.set_players(vec![seen(alice(), owner_at)]);

    sim.tick_for(40);
    let sat_at = sim.position(parrot).expect("alive");
    assert!(
        sim.get(parrot).expect("alive").is_in_sitting_pose(),
        "the sit goal must have actually run, not merely been installed"
    );
    assert!(
        sat_at.x < 2.0,
        "a sitting parrot must not walk to its owner; it reached x = {}",
        sat_at.x
    );

    sim.get_mut(parrot).expect("alive").set_ordered_to_sit(false);
    sim.tick_for(200);
    let followed_to = sim.position(parrot).expect("alive");
    let final_gap = owner_at.x - followed_to.x;
    assert!(
        (0.25..3.5).contains(&final_gap),
        "a standing parrot must close to near its own 1-block stop distance \
         (`Parrot.java:167`) — clearly tighter than the cat's 5 or the \
         wolf's 2; it ended {final_gap} blocks out (moved {} -> {})",
        sat_at.x,
        followed_to.x
    );
}

// ---------------------------------------------------------------------------
// The horse family: temper, not a flat roll
// ---------------------------------------------------------------------------

/// The horse's mechanism is a persisted counter, and this gate predicts its
/// values rather than its direction.
///
/// `handleEating`'s temper column disagrees with `#horse_food` in both
/// directions, so both disagreements are arms here: `hay_block` is horse food and
/// grants **nothing**, `red_mushroom` grants 3 and is **not** horse food. A
/// version derived from the tag fails on exactly those two rows.
#[test]
fn feeding_a_horse_raises_temper_by_the_jars_amounts() {
    let world = pen();
    let mut mismatches: Vec<String> = Vec::new();

    // (item, temper after one feed)
    let cases: &[(&str, i32)] = &[
        ("wheat", 3),
        ("sugar", 3),
        ("apple", 3),
        ("carrot", 3),
        // In `handleEating`, absent from `#horse_food`.
        ("red_mushroom", 3),
        ("golden_carrot", 5),
        ("golden_apple", 10),
        ("enchanted_golden_apple", 10),
        // In `#horse_food`, `temper` left at its `0` initialiser.
        ("hay_block", 0),
        // Not horse food in any sense.
        ("bone", 0),
    ];

    for &(item, want) in cases {
        let mut sim = MobSim::new(&world);
        let horse = sim
            .spawn_species(rk("minecraft:horse"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        let outcome = sim.interact(horse, alice(), Some(&rk(&format!("minecraft:{item}"))));
        let got = sim.get(horse).expect("alive").temper();
        if got != want {
            mismatches.push(format!("{item}: temper {got}, jar says {want}"));
        }
        let want_outcome = if want > 0 {
            InteractOutcome::TemperRaised { temper: want }
        } else {
            InteractOutcome::Pass
        };
        if outcome != want_outcome {
            mismatches.push(format!("{item}: outcome {outcome:?}, expected {want_outcome:?}"));
        }
        if sim.get(horse).expect("alive").is_tame() {
            mismatches.push(format!("{item}: feeding must never tame a horse"));
        }
    }

    assert!(mismatches.is_empty(), "{mismatches:?}");
}

/// `RunAroundLikeCrazyGoal`'s roll is `nextInt(maxTemper) < temper`, so it is
/// **certain to fail at temper 0** and certain to succeed at temper 100 — two
/// predictions that need no seed at all, and that a flat per-species chance
/// cannot reproduce.
///
/// The failure arm additionally adds 5 temper (`modifyTemper(5)`), which is the
/// whole reason a horse eventually yields to a patient rider; asserting only "it
/// did not tame" would pass for a horse that never becomes tameable.
#[test]
fn the_horses_tame_roll_is_a_function_of_temper_and_failure_raises_it() {
    let world = pen();

    // Temper 0: `nextInt(100) < 0` is false for every draw in the range, so no
    // seed can make this succeed.
    let mut sim = MobSim::new(&world);
    let horse = sim
        .spawn_species(rk("minecraft:horse"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    assert_eq!(sim.get(horse).expect("alive").temper(), 0);
    assert_eq!(
        sim.attempt_horse_tame(horse, alice(), 100),
        InteractOutcome::TameFailed,
        "temper 0 cannot be tamed by any draw"
    );
    assert_eq!(
        sim.get(horse).expect("alive").temper(),
        5,
        "`modifyTemper(5)` per failed mount is what makes a horse eventually yield"
    );

    // Temper 100 = maxTemper: `nextInt(100) < 100` is true for every draw.
    let mut sim = MobSim::new(&world);
    let horse = sim
        .spawn_species(rk("minecraft:horse"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.get_mut(horse).expect("alive").set_temper(100);
    assert_eq!(
        sim.attempt_horse_tame(horse, alice(), 100),
        InteractOutcome::Tamed,
        "temper at maxTemper cannot fail"
    );
    let mob = sim.get(horse).expect("alive");
    assert_eq!(mob.owner(), Some(MobOwner::Player(alice().uuid)));
    assert!(
        !mob.is_ordered_to_sit(),
        "`tameWithName` does not sit a horse — horses have no sitting pose"
    );
}

/// A horse's breeding items are **not** its food tag: only a golden carrot or a
/// golden apple puts one in love, and only once it is tamed. Wheat feeds a horse
/// and never breeds it, which is the arm a `breeding_food`-shaped port gets wrong.
#[test]
fn only_gold_breeds_a_horse_and_only_once_it_is_tame() {
    let world = pen();
    let mut mismatches: Vec<String> = Vec::new();

    // (item, tamed first?, expect love?)
    let cases: &[(&str, bool, bool)] = &[
        ("golden_carrot", true, true),
        ("golden_apple", true, true),
        ("golden_carrot", false, false),
        ("wheat", true, false),
        ("carrot", true, false),
        ("hay_block", true, false),
    ];

    for &(item, tamed, want_love) in cases {
        let mut sim = MobSim::new(&world);
        let horse = sim
            .spawn_species(rk("minecraft:horse"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        if tamed {
            sim.get_mut(horse)
                .expect("alive")
                .tame(MobOwner::Player(alice().uuid));
        }
        sim.interact(horse, alice(), Some(&rk(&format!("minecraft:{item}"))));
        let got = sim.get(horse).expect("alive").is_in_love();
        if got != want_love {
            mismatches.push(format!(
                "{item} (tamed={tamed}): in love = {got}, expected {want_love}"
            ));
        }
    }

    assert!(mismatches.is_empty(), "{mismatches:?}");
}

// ---------------------------------------------------------------------------
// Breeding: the trigger, the cooldown, the child, the orb
// ---------------------------------------------------------------------------

/// Feeding two adjacent adults their breeding item puts both in love, produces a
/// baby, applies vanilla's 6000-tick parent cooldown to **both**, and pops one
/// experience orb worth 1..=7.
///
/// The cooldown is the assertion that keeps this from being a one-mating gate: a
/// pair whose age was not reset breeds again 60 ticks later and the population
/// doubles.
#[test]
fn feeding_two_cows_wheat_breeds_them_once_and_pops_an_orb() {
    let world = pen();
    let mut sim = MobSim::new(&world);
    let a = sim
        .spawn_species(rk("minecraft:cow"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    let b = sim
        .spawn_species(rk("minecraft:cow"), Vec3::new(1.0, 0.0, 0.0))
        .id();

    assert_eq!(
        sim.interact(a, alice(), Some(&rk("minecraft:wheat"))),
        InteractOutcome::InLove
    );
    assert_eq!(
        sim.interact(b, alice(), Some(&rk("minecraft:wheat"))),
        InteractOutcome::InLove
    );
    assert_eq!(sim.len(), 2, "no child before the goal has run");
    assert_eq!(sim.orb_count(), 0);

    // `BreedGoal.BREED_TIME` is 60 ticks together within 3 blocks.
    sim.tick_for(120);

    assert_eq!(sim.len(), 3, "exactly one child, not two");
    let babies = sim.iter().filter(|m| m.is_baby()).count();
    assert_eq!(babies, 1);

    let mut cooldowns: Vec<i32> = Vec::new();
    for id in [a, b] {
        let mob = sim.get(id).expect("parent alive");
        cooldowns.push(mob.age());
        assert!(!mob.is_in_love(), "love is reset on both parents");
    }
    // Collected rather than asserted in the loop so both parents report.
    assert!(
        cooldowns.iter().all(|&age| age > 0),
        "`PARENT_AGE_AFTER_BREEDING` is a positive countdown on both parents; got \
         {cooldowns:?}"
    );

    assert_eq!(sim.orb_count(), 1, "`random.nextInt(7) + 1`, one orb");
    let value = sim
        .orbs_within_pickup_range(Vec3::new(0.0, 0.0, 0.0))
        .first()
        .map(|&(_, v)| v)
        .or_else(|| {
            sim.orbs_within_pickup_range(Vec3::new(1.0, 0.0, 0.0))
                .first()
                .map(|&(_, v)| v)
        });
    assert!(
        matches!(value, Some(v) if (1..=7).contains(&v)),
        "the breeding orb's value is `nextInt(7) + 1`; got {value:?}"
    );
}

/// A mob already in its post-breeding cooldown cannot be re-triggered, because
/// `Animal.mobInteract`'s gate is `getAge() == 0` and **not** `!isBaby()`.
///
/// This is the discriminating input for that distinction: a cooling-down parent
/// is not a baby, so the wrong reading accepts it and the pair breeds forever.
#[test]
fn a_cooling_down_parent_and_a_baby_both_refuse_the_breeding_item() {
    let world = pen();
    let mut mismatches: Vec<String> = Vec::new();

    // (label, age to stage, must the feed take?)
    for &(label, age, want) in &[
        ("adult", 0, true),
        ("post-breeding cooldown", 6000, false),
        ("baby", -24_000, false),
    ] {
        let mut sim = MobSim::new(&world);
        let cow = sim
            .spawn_species(rk("minecraft:cow"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(cow).expect("alive").set_age(age);
        let outcome = sim.interact(cow, alice(), Some(&rk("minecraft:wheat")));
        let got = outcome == InteractOutcome::InLove;
        if got != want {
            mismatches.push(format!("{label} (age {age}): in love = {got}, expected {want}"));
        }
    }

    assert!(mismatches.is_empty(), "{mismatches:?}");
}

/// The breeding-item tables are per species, and this gate is built on the
/// crossings rather than on the matches: wheat breeds a cow and not a pig, a
/// carrot breeds a pig and not a cow, and a **parrot cannot be bred at all**
/// (`Parrot.canMate` returns `false` and `getBreedOffspring` returns `null`).
///
/// # The wolf and the cat are not the same case, and the difference is measured
///
/// Whether a mob has to be tamed *first* to be bred depends on whether its taming
/// item overlaps its food tag, and the two tameable rows here fall on opposite
/// sides of that:
///
/// * A **wolf** is tamed by `Items.BONE`, which is in no wolf food tag. So an
///   untamed wolf fed meat misses the bone arm, reaches `super.mobInteract`, and
///   really does fall in love.
/// * A **cat** is tamed by `#cat_food` — the very tag that is also its `isFood`.
///   So an untamed cat fed cod always attempts a *tame* and never reaches the love
///   arm at all, however the roll lands. It must be tamed first.
///
/// The first draft of this gate asserted an untamed cat fed cod falls in love, and
/// it failed. The code was right.
#[test]
fn breeding_items_are_per_species_and_a_parrot_has_none() {
    let world = pen();
    let mut mismatches: Vec<String> = Vec::new();

    // (species, item, tame it first?, expect love?)
    let cases: &[(&str, &str, bool, bool)] = &[
        ("cow", "wheat", false, true),
        ("cow", "carrot", false, false),
        ("pig", "carrot", false, true),
        ("pig", "wheat", false, false),
        ("sheep", "wheat", false, true),
        ("chicken", "wheat_seeds", false, true),
        ("chicken", "wheat", false, false),
        ("rabbit", "dandelion", false, true),
        // A bone tames a wolf, so meat and fish are free to breed one whether or
        // not it is tame.
        ("wolf", "beef", false, true),
        ("wolf", "cod", false, true),
        ("wolf", "beef", true, true),
        // A cat's taming item *is* its food, so only a tame one can be bred.
        ("cat", "cod", true, true),
        ("cat", "cod", false, false),
        // Raw only: `#cat_food` is cod and salmon, not their cooked forms — so a
        // cooked cod is neither a taming item nor a breeding one.
        ("cat", "cooked_cod", true, false),
        ("parrot", "wheat_seeds", false, false),
        ("parrot", "wheat_seeds", true, false),
        ("parrot", "cookie", false, false),
    ];

    for &(species, item, tame_first, want) in cases {
        let mut sim = MobSim::new(&world);
        // Force every tame roll to fail so a wolf fed meat cannot be confused
        // with a wolf that got tamed instead.
        sim.set_tame_rng(SpawnRng::new(seed_where(3, |d| d != 0)));
        let id = sim
            .spawn_species(rk(&format!("minecraft:{species}")), Vec3::new(0.0, 0.0, 0.0))
            .id();
        if tame_first {
            sim.get_mut(id)
                .expect("alive")
                .tame(MobOwner::Player(alice().uuid));
        }
        sim.interact(id, alice(), Some(&rk(&format!("minecraft:{item}"))));
        let got = sim.get(id).expect("alive").is_in_love();
        if got != want {
            mismatches.push(format!(
                "{species} + {item} (tamed={tame_first}): in love = {got}, expected {want}"
            ));
        }
    }

    assert!(mismatches.is_empty(), "{mismatches:?}");
}

// ---------------------------------------------------------------------------
// What the player actually sees
// ---------------------------------------------------------------------------

/// Taming and breeding put a particle burst on the same queue
/// `crate::tick::run_mob_tick_loop` already drains into real `LEVEL_PARTICLES`
/// packets — so the outcome is visible without an `ENTITY_EVENT` encoder.
///
/// The three arms are chosen so a single hardcoded particle cannot pass: success
/// and love are HEART, failure is SMOKE, and a sit toggle emits **nothing**.
#[test]
fn taming_publishes_hearts_on_success_and_smoke_on_failure() {
    let world = pen();

    let particles = |seed: u64, item: &str, then_sit: bool| -> Vec<String> {
        let mut sim = MobSim::new(&world);
        sim.set_tame_rng(SpawnRng::new(seed));
        let wolf = sim
            .spawn_species(rk("minecraft:wolf"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        let _ = sim.take_vocalisations();
        sim.interact(wolf, alice(), Some(&rk(&format!("minecraft:{item}"))));
        if then_sit {
            sim.interact(wolf, alice(), None);
        }
        sim.take_vocalisations()
            .into_iter()
            .filter_map(|effect| match effect {
                lodestone_server::effects::WorldEffect::Particles { particle, count, .. } => {
                    Some(format!("{particle} x{count}"))
                }
                _ => None,
            })
            .collect()
    };

    assert_eq!(
        particles(seed_where(3, |d| d == 0), "bone", false),
        vec!["minecraft:heart x7".to_owned()],
        "`spawnTamingParticles(true)` is seven hearts"
    );
    assert_eq!(
        particles(seed_where(3, |d| d != 0), "bone", false),
        vec!["minecraft:smoke x7".to_owned()],
        "`spawnTamingParticles(false)` is seven smoke"
    );
    // A successful tame followed by an owner's sit toggle must add nothing: the
    // sit arm has no `broadcastEntityEvent` at all.
    assert_eq!(
        particles(seed_where(3, |d| d == 0), "bone", true),
        vec!["minecraft:heart x7".to_owned()],
        "sitting a pet emits no particle, so the burst count must not grow"
    );
}

/// **The tame flag reaches the wire, under the right variant per species.**
///
/// The tame *state* was reachable and the *packet* was not: `SimMob::snapshot`
/// produced metadata for the creeper alone, so a wolf streamed an empty metadata
/// list and `EntityStreamer::sync` sent no `SET_ENTITY_DATA` at all. Everything
/// below the sim was correct and the client was never told.
///
/// # Why the "not the other one" arm is the point
///
/// Index 18 is a `BYTE` for `TamableAnimal.DATA_FLAGS_ID`, `AbstractHorse.DATA_ID_FLAGS`,
/// `Sheep.DATA_WOOL_ID` and `Shulker.DATA_COLOR_ID` (checked against the committed
/// jar dump by `lodestone-v770`'s own `index_eighteen_tests`), and the tame bit is
/// `0x04` on a tamable against `FLAG_TAME = 2` on a horse. One shared "tamed"
/// variant would compile, encode, and put an **unnamed** bit on whichever species it
/// was not written for — so that animal reads as untamed with a well-formed packet on
/// the wire. Asserting only that the right variant is present would pass for a
/// producer that emits both.
///
/// Mismatches are collected rather than asserted in the loop, so every species
/// reports.
#[test]
fn a_tamed_mob_streams_the_flag_variant_its_own_class_uses() {
    use lodestone_server::MetadataField;

    let world = pen();
    let mut mismatches: Vec<String> = Vec::new();

    // (species, taming item, the `next_int` bound its tame roll uses)
    let tamables = [("wolf", "bone", 3), ("cat", "cod", 3), ("parrot", "wheat_seeds", 10)];
    for &(species, item, bound) in &tamables {
        let mut sim = MobSim::new(&world);
        sim.set_tame_rng(SpawnRng::new(seed_where(bound, |d| d == 0)));
        let id = sim
            .spawn_species(rk(&format!("minecraft:{species}")), Vec3::new(0.0, 0.0, 0.0))
            .id();

        // The control: a wild mob's byte is all-zero, which is the client's own
        // default, so nothing must be streamed. Without this arm the gate below
        // would pass for a producer that emits the field unconditionally.
        let wild = sim.get(id).expect("alive").snapshot();
        if wild
            .metadata
            .iter()
            .any(|f| matches!(f, MetadataField::TamableFlags { .. } | MetadataField::HorseFlags { .. }))
        {
            mismatches.push(format!("{species}: a wild mob must stream no flag field"));
        }

        if sim.interact(id, alice(), Some(&rk(&format!("minecraft:{item}")))) != InteractOutcome::Tamed
        {
            mismatches.push(format!("{species}: setup failed — the tame roll did not succeed"));
            continue;
        }
        let tamed = sim.get(id).expect("alive").snapshot();
        let has_tamable = tamed
            .metadata
            .iter()
            .any(|f| matches!(f, MetadataField::TamableFlags { tame: true, .. }));
        let has_horse = tamed
            .metadata
            .iter()
            .any(|f| matches!(f, MetadataField::HorseFlags { .. }));
        if !has_tamable {
            mismatches.push(format!(
                "{species}: expected TamableFlags {{ tame: true }}, got {:?}",
                tamed.metadata
            ));
        }
        if has_horse {
            mismatches.push(format!(
                "{species}: must NOT carry HorseFlags — its tame bit is 0x04, the \
                 horse's is 0x02, so the wrong variant reads as untamed"
            ));
        }
    }

    // The horse family, from the other side of the same collision. Tamed directly
    // rather than through `interact`, because the horse temper mechanism is a
    // different (and separately gated) path — what this arm is about is which
    // variant `snapshot` picks once `tame` is set, not how it got set.
    for species in ["horse", "donkey", "mule", "skeleton_horse", "zombie_horse"] {
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(rk(&format!("minecraft:{species}")), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id).expect("alive").tame(MobOwner::Player(alice().uuid));
        let tamed = sim.get(id).expect("alive").snapshot();
        if !tamed
            .metadata
            .iter()
            .any(|f| matches!(f, MetadataField::HorseFlags { tame: true }))
        {
            mismatches.push(format!(
                "{species}: expected HorseFlags {{ tame: true }}, got {:?}",
                tamed.metadata
            ));
        }
        if tamed
            .metadata
            .iter()
            .any(|f| matches!(f, MetadataField::TamableFlags { .. }))
        {
            mismatches.push(format!(
                "{species}: must NOT carry TamableFlags — 0x04 is not in \
                 AbstractHorse's flag set at all (FLAG_BRED is 8), so the horse \
                 would read as untamed"
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "collected so every species reports rather than only the first: {mismatches:?}"
    );
}
