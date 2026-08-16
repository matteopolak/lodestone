//! Goal sets for the farm animals — cow, mooshroom, sheep, pig, chicken, rabbit
//! — plus the two tameable companions that are `Animal`s rather than neutral
//! mobs: cat and parrot. (The wolf, a neutral mob, lives in
//! [`super::neutral`] instead.)
//!
//! # What it is
//!
//! One [`Registration`] table per species, transcribed from
//! `.cache/mc/26.2/src/net/minecraft/world/entity/animal/`; extend it here
//! and nothing else in the tree changes.
//!
//! # Why this family is where the roster first becomes visible
//!
//! Before this module, `MobSim::spawn_species` installed `RandomStrollGoal` and
//! `RandomLookAroundGoal` on a cow and nothing else. `FloatGoal`, `PanicGoal`,
//! `BreedGoal`, `TemptGoal` and `FollowParentGoal` were **fully implemented, fully
//! unit-tested, fully fed with real perception by `MobSim::tick` — and installed
//! by nothing but tests.** Every call site outside `#[cfg(test)]` was zero. That
//! is the island shape one layer up from a previously-fixed perception island:
//! perception was no longer starved, but nothing put the goals that read it on a real mob.
//!
//! So the cow and pig tables below are the first production installation of five
//! goals, and `crates/lodestone-server/tests/mob_roster.rs` is the gate that says
//! so behaviourally rather than by counting goals.
//!
//! # Known gaps, all disclosed in the tables
//!
//! * **A pig's carrot-on-a-stick tempt** is [`Coverage::Missing`] separately from
//!   its food tempt, because it is a distinct vanilla registration with a
//!   distinct item and the server's `tempt_food` feed covers only the food tag.
//! * **A rabbit's `AvoidEntityGoal` is modelled but inert**, because the server's
//!   `avoided_species` feed has no rabbit arm — see [`rabbit_avoid_player`].
//! * **Two rabbit rows are unmodelled for block-perception-adjacent reasons**, next.
//! * **The cat and the parrot** ([`CAT`], [`PARROT`]) close a previously reported
//!   gap: both are tameable and ownable and had no roster entry at all before
//!   this, so a tamed one could never sit or follow. Each table's own doc
//!   comment carries its species-specific traps.
//!
//! # Block perception: the sheep's row is closed, two rabbit rows are not
//!
//! Sheep grazing (`Sheep.eatBlockGoal`) used to be
//! [`Coverage::Missing`] because [`MobController`](crate::ai::MobController)
//! could not read a block at all — a goal that eats grass could not ask whether
//! there was grass. That seam landed (`bdf7120`, `b50255a`):
//! `PathWorld::block_cues` answers block *identity* on the world seam,
//! `MobController::block_cues_at_feet`/`_below` are overridden on
//! [`NavigatingMob`](crate::ai::navigating_mob::NavigatingMob) from the
//! [`PathWorld`](crate::pathfinding::PathWorld) it already borrows, and the goal
//! reports each eat back as an `ate(EatenBlock)` intent for the host to apply.
//! [`eat_block`] is installed by the [`SHEEP`] table below.
//!
//! **What that achieves, and what it does not.** The goal is installed on a real
//! mob and reads the seam, and a sheep in a running game grazes end to end: the
//! host half landed as `ChunkWorld::block_cues` — the classification, which
//! `base_path_type` deliberately erases, since `grass_block`, `dirt` and `stone`
//! are one `Blocked` — plus a `pending_grazes` handoff drained in
//! `run_tick_loop`, the one place mutable chunk access lives (`MobSim` borrows
//! the world immutably, so the mutation takes the `pending_detonations` route
//! through the tick driver). What remains is wool regrowth — `Sheep.ate()`'s
//! `setSheared(false)` plus `ageUp(60)` (`Sheep.ate`),
//! which is entity metadata on the wire. `docs/mob-block-perception.md` is the
//! doc.
//!
//! **A generalisation not to inherit.** The fix's body grouped seven `Missing` rows
//! across two families as one seam capability. Measured against the jar it closes
//! **one**: a rabbit's `ClimbOnTopOfPowderSnowGoal` needs powder-snow physics
//! nothing here models, its `RaidGardenGoal` needs a host-computed candidate
//! block position (`MoveToBlockGoal`'s spiral) plus a block-state *property*, and
//! [`hostile_melee`](super::hostile_melee)'s `RestrictSunGoal` reads no block at
//! all. Anyone planning off the original table would expect the rest to be free.
//!
//! **A stale claim not to inherit either.** An earlier plan said grazing is blocked on
//! random ticks, and a later correction amends that to "unblocked, because
//! `random_tick.rs` exists and runs in the production tick loop". The correction
//! is true and it was *not sufficient*: `random_tick.rs` being real makes a
//! grass→dirt **world mutation** available, which was never the binding
//! constraint — the seam above was.
//!
//! # What consumes these tables — and the honest limit on it
//!
//! [`goals_for`](super::goals_for) is called by `MobSim::spawn_species`, so every
//! table here reaches a real `GoalSelector` on a real mob. **This paragraph used
//! to say `seed_demo_mobs`'s hardcoded zombie ring was the only production path
//! and none of these species reached a running game — that is now stale and the
//! correction matters more than the original claim did.** `crate::natural_spawn`
//! (`tick.rs`'s own tick loop, not a test) now drives a real per-species spawn
//! cycle, and every farm animal in this file — cow, mooshroom, sheep, pig,
//! chicken, rabbit — plus the wolf and the parrot below all have rows in its
//! table (`"cow" | "sheep" | "pig" | "chicken"` on `ANIMALS_ON`, `"rabbit"` on
//! its own ground set, `"wolf"`/`"parrot"` on theirs). So "a player can see a
//! cow" — and a wolf, and a parrot — is true today, through the ordinary spawn
//! cycle, with no special-casing anywhere in this file.
//!
//! **The cat is the one exception, and it is a vanilla fact rather than a gap
//! here.** `crate::natural_spawn`'s table has an `"ocelot"` row and no `"cat"`
//! row, matching vanilla: a real cat spawns near villages through a dedicated
//! `CatSpawner`, not the ordinary per-biome cycle, and that mechanism is not
//! modelled anywhere in this tree. [`CAT`] is therefore reachable from tests, a
//! caller that names `"cat"` directly, or an ocelot a future v-cat-conversion
//! feature turns into one — never from a spawn a player did not cause some
//! other way — until a village-spawner analogue exists. That is a real,
//! disclosed limit on this table, not something to route around here.
//!

use crate::ai::goal::Goal;
use crate::ai::goals::{
    AvoidEntityGoal, BreedGoal, CatLieOnBedGoal, CatSitOnBlockGoal, EatBlockGoal, FollowOwnerGoal,
    FollowParentGoal, LookAtPlayerGoal, PanicGoal, RandomStrollGoal, TemptGoal,
};

use super::{
    LOOK_PROBABILITY, Registration, Selector, SpeciesContext, breed_1_0, float_goal,
    look_at_player_6, look_at_player_8, random_look_around, sit_when_ordered, stroll,
};

/// Every species this family claims. Iterated by `roster`'s invariant gates.
///
/// `cat` and `parrot` joined this family: both are `Animal`s
/// with a `TamableAnimal`-style goal set (not neutral, not hostile), the same
/// shape as the farm animals above them — see [`CAT`] and [`PARROT`]'s own
/// doc comments for what each does and does not carry.
pub const SPECIES: &[&str] = &[
    "cow", "mooshroom", "sheep", "pig", "chicken", "rabbit", "cat", "parrot",
];

/// Resolves a species path to its table, or `None` if this family does not claim
/// it.
#[must_use]
pub fn lookup(species: &str) -> Option<&'static [Registration]> {
    match species {
        // `MushroomCow` declares no `registerGoals` of its own
        // (`animal/cow/MushroomCow.java`), so a mooshroom inherits
        // `AbstractCow`'s verbatim — the same reason they share `cow_food`.
        "cow" | "mooshroom" => Some(COW),
        "sheep" => Some(SHEEP),
        "pig" => Some(PIG),
        "chicken" => Some(CHICKEN),
        "rabbit" => Some(RABBIT),
        "cat" => Some(CAT),
        "parrot" => Some(PARROT),
        _ => None,
    }
}

/// `AbstractCow.registerGoals`.
///
/// The only table in the roster with **no** gaps: all eight of vanilla's cow
/// registrations have an equivalent here.
pub const COW: &[Registration] = &[
    Registration::goal(0, "FloatGoal", float_goal),
    Registration::goal(1, "PanicGoal", panic_2_0),
    Registration::goal(2, "BreedGoal", breed_1_0),
    Registration::goal(3, "TemptGoal(COW_FOOD)", tempt_1_25),
    Registration::goal(4, "FollowParentGoal", follow_parent_1_25),
    Registration::goal(5, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(6, "LookAtPlayerGoal(Player)", look_at_player_6),
    Registration::goal(7, "RandomLookAroundGoal", random_look_around),
];

/// `Sheep.registerGoals`. Note it assigns `this.eatBlockGoal` before
/// the first `addGoal`, so the `addGoal` calls themselves come after.
pub const SHEEP: &[Registration] = &[
    Registration::goal(0, "FloatGoal", float_goal),
    Registration::goal(1, "PanicGoal", panic_1_25),
    Registration::goal(2, "BreedGoal", breed_1_0),
    Registration::goal(3, "TemptGoal(SHEEP_FOOD)", tempt_1_1),
    Registration::goal(4, "FollowParentGoal", follow_parent_1_1),
    // The seam gap this row waited on is closed (`bdf7120`): the goal reads
    // the block below through `MobController::block_cues_below`. Grazing still
    // needs the host's drain of `take_new_eaten` to see grass turn to dirt — see
    // `docs/mob-block-perception.md`.
    Registration::goal(5, "EatBlockGoal", eat_block),
    Registration::goal(6, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(7, "LookAtPlayerGoal(Player)", look_at_player_6),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
];

/// `Pig.registerGoals`.
///
/// A pig is the one species here with **two** `TemptGoal` registrations at the
/// same priority, for carrot-on-a-stick and for `PIG_FOOD`.
pub const PIG: &[Registration] = &[
    Registration::goal(0, "FloatGoal", float_goal),
    Registration::goal(1, "PanicGoal", panic_1_25),
    // Vanilla puts a pig's `BreedGoal` at 3, not 2 — nothing occupies 2.
    Registration::goal(3, "BreedGoal", breed_1_0),
    // `TemptGoal(this, 1.2, i -> i.is(Items.CARROT_ON_A_STICK), false)`. A
    // separate registration from the food one below, and a separate item: the
    // server's `tempt_food` feed resolves the `pig_food` **tag** only, so
    // steering a pig with a carrot on a stick is not modelled. It is also the
    // riding-control item, so it belongs with saddles rather than with tempting.
    Registration::missing(Selector::Goal, 4, "TemptGoal(CARROT_ON_A_STICK)"),
    Registration::goal(4, "TemptGoal(PIG_FOOD)", tempt_1_2),
    Registration::goal(5, "FollowParentGoal", follow_parent_1_1),
    Registration::goal(6, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(7, "LookAtPlayerGoal(Player)", look_at_player_6),
    Registration::goal(8, "RandomLookAroundGoal", random_look_around),
];

/// `Chicken.registerGoals`.
pub const CHICKEN: &[Registration] = &[
    Registration::goal(0, "FloatGoal", float_goal),
    Registration::goal(1, "PanicGoal", panic_1_4),
    Registration::goal(2, "BreedGoal", breed_1_0),
    Registration::goal(3, "TemptGoal(CHICKEN_FOOD)", tempt_1_0),
    Registration::goal(4, "FollowParentGoal", follow_parent_1_1),
    Registration::goal(5, "WaterAvoidingRandomStrollGoal", stroll),
    Registration::goal(6, "LookAtPlayerGoal(Player)", look_at_player_6),
    Registration::goal(7, "RandomLookAroundGoal", random_look_around),
];

/// `Rabbit.registerGoals`.
///
/// The odd one out of this family in five ways, every one a jar fact rather than
/// a transcription choice. They are listed because four of the five are exactly
/// the shape of thing that gets "fixed" into symmetry by a later reader:
///
/// * **No `FollowParentGoal`.** Every other species here registers one; a rabbit
///   does not — `registerGoals` has no such line — so there is no row for it. Do not
///   add one for consistency with its siblings.
/// * **Three registrations share priority 1** (`FloatGoal`,
///   `ClimbOnTopOfPowderSnowGoal`, `RabbitPanicGoal`), where every other species
///   here has exactly one goal per priority.
/// * **Its look goal is at priority 11**, not 6 or 7, and at **`10.0F`** rather
///   than the `6.0F` every other farm animal uses.
/// * **It is the only species in this family that flees anything**, and it
///   registers three `AvoidEntityGoal`s to do it.
/// * **Its breed and stroll speeds are not the family's** — `0.8` and `0.6`
///   against everyone else's `1.0`, so neither shared builder applies.
///
/// The killer-bunny variant installs a `MeleeAttackGoal(1.4, true)` and two
/// target goals from `Rabbit.setVariant`, **not** from `registerGoals`.
/// They are deliberately absent here: this table is the `registerGoals`
/// transcription that the multiset gate cites, and a conditional runtime
/// installation is a different mechanism — the one
/// [`GoalSelector::remove`](crate::ai::goal::GoalSelector::remove) exists for.
/// Adding them as rows would make the cited line range a lie.
pub const RABBIT: &[Registration] = &[
    Registration::goal(1, "FloatGoal", float_goal),
    // `ClimbOnTopOfPowderSnowGoal(this, this.level())`. The *cue* half
    // is now answerable — `MobController::block_cues_*` could carry
    // "the block above is powder snow or has empty collision" — but the goal
    // also gates on `isInPowderSnow`/`wasInPowderSnow`, which no physics here
    // sets, and `#powder_snow` identity is not a `BlockCues` field. Blocked on
    // powder-snow physics, not on block access;
    // `docs/mob-block-perception.md`.
    Registration::missing(Selector::Goal, 1, "ClimbOnTopOfPowderSnowGoal"),
    Registration::goal(1, "Rabbit.RabbitPanicGoal", panic_2_2),
    Registration::goal(2, "BreedGoal", breed_0_8),
    Registration::goal(3, "TemptGoal(RABBIT_FOOD)", tempt_1_0),
    // The three `RabbitAvoidEntityGoal`s differ from the creeper's
    // pair in a way worth being explicit about: the creeper's Ocelot and Cat
    // registrations share one radius (`6.0F`), so one class-agnostic goal of
    // ours reproduces both exactly. A rabbit's three do **not** — Player
    // `8.0F`, Wolf `10.0F`, Monster `4.0F`. One instance therefore cannot carry
    // all three radii, and this row takes the Player figure.
    //
    // So the two `CoveredBy` rows below are coverage of the *behaviour* (the
    // rabbit flees what the server's feed reports as a threat) at the **wrong
    // radius**: a wolf is fled from 2 blocks later than vanilla, a monster 4
    // blocks earlier. A disclosed approximation, and the honest alternative to
    // three goals fighting over MOVE at equal priority.
    Registration::goal(
        4,
        "Rabbit.RabbitAvoidEntityGoal(Player)",
        rabbit_avoid_player,
    ),
    Registration::covered(
        Selector::Goal,
        4,
        "Rabbit.RabbitAvoidEntityGoal(Wolf)",
        "Rabbit.RabbitAvoidEntityGoal(Player)",
    ),
    Registration::covered(
        Selector::Goal,
        4,
        "Rabbit.RabbitAvoidEntityGoal(Monster)",
        "Rabbit.RabbitAvoidEntityGoal(Player)",
    ),
    // `Rabbit.RaidGardenGoal(this)` — a `MoveToBlockGoal` that hunts
    // carrot crops and eats them. Not a local-cue question: it needs a
    // host-computed candidate block position (the `MoveToBlockGoal` spiral over
    // 16 blocks), the `#supports_crops` tag, and `CarrotBlock.AGE` — a
    // block-state *property*, not a boolean cue. The mutation half is the
    // `ate`-style intent; `docs/mob-block-perception.md` has the full shape.
    Registration::missing(Selector::Goal, 5, "Rabbit.RaidGardenGoal"),
    Registration::goal(6, "WaterAvoidingRandomStrollGoal", stroll_0_6),
    Registration::goal(11, "LookAtPlayerGoal(Player)", look_at_player_10),
];

/// `Cat.registerGoals`.
///
/// Closes a previously reported gap: taming, ownership and breeding landed for the
/// cat, but with no roster entry it could be owned and never follow or sit —
/// see `docs/taming-and-breeding.md` §8. This table closes that.
///
/// Four things worth knowing before "fixing" this table into symmetry with the
/// rest of the family:
///
/// * **A cat's taming item is its whole food tag** (`#cat_food` = raw cod and
///   salmon), unlike the wolf, whose bone is in no wolf food tag at all. So an
///   untamed cat fed cod always attempts a tame and never reaches `BreedGoal`
///   however the roll lands — see `docs/taming-and-breeding.md`'s note on
///   `breeding_items_are_per_species_and_a_parrot_has_none`'s sibling case.
/// * **`SitWhenOrderedToGoal` and `FollowOwnerGoal` are shared with the wolf**
///   ([`sit_when_ordered`](super::sit_when_ordered)) and the parrot below, but
///   the follow distances are the cat's own — `(10, 5)`, not the wolf's
///   `(10, 2)` — so [`cat_follow_owner`] is a distinct builder.
/// * **A cat has no combat goal at all.** Unlike the wolf, vanilla registers no
///   `OwnerHurtByTargetGoal`/`OwnerHurtTargetGoal` for `Cat` — its two
///   `targetSelector` rows are both `NonTameRandomTargetGoal`, an *untamed*
///   cat's own rabbit/turtle hunting, unrelated to its owner. A cat does not
///   defend you.
/// * **Its `WaterAvoidingRandomStrollGoal` almost never fires.** Vanilla passes
///   an explicit `1.0000001E-5F` probability, the reciprocal of which
///   is is ~100,000 ticks between attempts — a cat parked near its owner or a
///   bed essentially does not wander on its own, unlike every other species in
///   this family which strolls constantly.
pub const CAT: &[Registration] = &[
    Registration::goal(1, "FloatGoal", float_goal),
    Registration::goal(1, "TamableAnimal.TamableAnimalPanicGoal", cat_panic_1_5),
    Registration::goal(2, "SitWhenOrderedToGoal", sit_when_ordered),
    // `Cat.CatRelaxOnOwnerGoal` — lies down near a seated owner and
    // purrs, occasionally gifting an item. No goal type here models it: it
    // needs "is my owner sitting/sleeping nearby" plus an item-gift side
    // effect, neither of which any existing goal carries.
    Registration::missing(Selector::Goal, 3, "Cat.CatRelaxOnOwnerGoal"),
    // `this.temptGoal = new Cat.CatTemptGoal(this, 0.6, i -> i.is(ItemTags.CAT_FOOD), true)`,
    // added at priority 4. The `canScare` third argument
    // (fleeing a sudden nearby sprinting player) is not modelled — our
    // `TemptGoal` has no scare state, same simplification as every other
    // `TemptGoal` row in this roster.
    Registration::goal(4, "Cat.CatTemptGoal(CAT_FOOD)", cat_tempt_0_6),
    // `CatLieOnBedGoal(this, 1.1, 8)` — a `MoveToBlockGoal` that hunts
    // beds in an 8-block radius. The candidate bed position is host-computed
    // (`MobController::cat_bed_target`, `docs/mob-block-perception.md`'s own
    // guidance for a goal that needs to search a neighbourhood) rather than
    // searched in-goal.
    Registration::goal(5, "CatLieOnBedGoal", cat_lie_on_bed_1_1),
    Registration::goal(6, "FollowOwnerGoal", cat_follow_owner),
    // `CatSitOnBlockGoal(this, 0.8)` — hunts chests and lit furnaces to
    // perch on, same host-computed-candidate shape as `CatLieOnBedGoal` above
    // (`MobController::cat_sit_target`).
    Registration::goal(7, "CatSitOnBlockGoal", cat_sit_on_block_0_8),
    // `LeapAtTargetGoal(this, 0.3F)` — pounces at its own attack
    // target. No goal type here models a leap.
    Registration::missing(Selector::Goal, 8, "LeapAtTargetGoal"),
    // `OcelotAttackGoal(this)` — an untamed cat's own chicken-stalking
    // hunt. It picks its target internally (`Level.getEntitiesOfClass`) rather
    // than through `targetSelector`, so there is no companion target row to
    // pin here either; no goal type models the stalk-then-pounce shape.
    Registration::missing(Selector::Goal, 9, "OcelotAttackGoal"),
    Registration::goal(10, "BreedGoal", breed_0_8),
    Registration::goal(11, "WaterAvoidingRandomStrollGoal", cat_stroll),
    Registration::goal(12, "LookAtPlayerGoal(Player)", look_at_player_10),
    // `NonTameRandomTargetGoal<>(this, Rabbit.class, false, null)` —
    // an untamed cat hunting a random nearby rabbit. Unrelated to ownership;
    // no goal type here models a random-same-class target search.
    Registration::missing(Selector::Target, 1, "NonTameRandomTargetGoal(Rabbit)"),
    // `NonTameRandomTargetGoal<>(this, Turtle.class, false, Turtle.BABY_ON_LAND_SELECTOR)`
    // — same gap as the row above, narrowed to baby turtles on land.
    Registration::missing(Selector::Target, 1, "NonTameRandomTargetGoal(Turtle)"),
];

/// `Parrot.registerGoals`.
///
/// Closes another previously reported gap. A parrot **does** register
/// `SitWhenOrderedToGoal` — do not drop that row for symmetry with
/// "the parrot doesn't sit" — but `Parrot.tryToTame` is the one taming success
/// of the three that omits the automatic `setOrderedToSit(true)`
/// (`docs/taming-and-breeding.md` §2, already correct in `mobs.rs`'s
/// `tame_mechanism`). The two are different mechanisms: this row is the
/// *goal* an owner's right-click toggle still needs to mean anything, and it
/// is present in the jar regardless of how taming leaves the flag.
///
/// A parrot registers **no targetSelector goal at all** — it cannot fight,
/// has no `OwnerHurtByTargetGoal`/`OwnerHurtTargetGoal`, and (unlike every
/// farm animal and the cat above) has no `BreedGoal` either:
/// `Parrot.canMate` returns `false` and `Parrot.isFood` returns a literal
/// `false`, so there is nothing to tempt it into breeding with — see
/// `breeding_food`'s own comment on the empty `"parrot"` row.
pub const PARROT: &[Registration] = &[
    Registration::goal(0, "TamableAnimal.TamableAnimalPanicGoal", parrot_panic_1_25),
    Registration::goal(0, "FloatGoal", float_goal),
    Registration::goal(1, "LookAtPlayerGoal(Player)", look_at_player_8),
    Registration::goal(2, "SitWhenOrderedToGoal", sit_when_ordered),
    Registration::goal(2, "FollowOwnerGoal", parrot_follow_owner),
    // `Parrot.ParrotWanderGoal(this, 1.0)` — a flying variant of
    // random-stroll. Our `RandomStrollGoal` drives ground A*; a parrot's
    // `FlyingPathNavigation` picks candidate points in the air, which this
    // seam has no equivalent search for (same class of gap `Bee.BeeWanderGoal`
    // is `Missing` for in the neutral family).
    Registration::missing(Selector::Goal, 2, "Parrot.ParrotWanderGoal"),
    // `LandOnOwnersShoulderGoal(this)` — shoulder riding. No component
    // here models a mob perching on a player, let alone the client-visible
    // pose that would require.
    Registration::missing(Selector::Goal, 3, "LandOnOwnersShoulderGoal"),
    // `FollowMobGoal(this, 1.0, 3.0F, 7.0F)` — a tame, non-sitting
    // parrot follows the nearest *other mob* it can imitate. No goal type here
    // models following an arbitrary nearby mob rather than the owner.
    Registration::missing(Selector::Goal, 3, "FollowMobGoal"),
];

// -- builders, one per distinct jar speed multiplier -------------------------
//
// Vanilla's speed arguments are multipliers on the mob's own MOVEMENT_SPEED, so
// each of these is `ctx.speed * <the jar's factor>` and the factor stays visible
// next to the citation. `Registration.build` must be a plain `fn` item, so a
// parameterised closure is not an option.

/// `PanicGoal(this, 2.0)` — cow (`AbstractCow.registerGoals`). The fastest
/// panic in this family.
fn panic_2_0(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 2.0))
}

/// `PanicGoal(this, 1.25)` — sheep (`Sheep.registerGoals`) and pig
/// (`Pig.registerGoals`).
fn panic_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 1.25))
}

/// `PanicGoal(this, 1.4)` — chicken (`Chicken.registerGoals`).
fn panic_1_4(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 1.4))
}

/// `TemptGoal(this, 1.25, …)` — cow (`AbstractCow.registerGoals`).
fn tempt_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TemptGoal::new(ctx.speed * 1.25))
}

/// `TemptGoal(this, 1.1, …)` — sheep (`Sheep.registerGoals`).
fn tempt_1_1(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TemptGoal::new(ctx.speed * 1.1))
}

/// `TemptGoal(this, 1.2, …)` — pig (`Pig.registerGoals`).
fn tempt_1_2(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TemptGoal::new(ctx.speed * 1.2))
}

/// `TemptGoal(this, 1.0, …)` — chicken (`Chicken.registerGoals`).
fn tempt_1_0(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TemptGoal::new(ctx.speed))
}

/// `Rabbit.RabbitPanicGoal(this, 2.2)` — rabbit
/// (`Rabbit.registerGoals`). The fastest panic in the family; a cow's
/// `2.0` is next.
///
/// `RabbitPanicGoal` is a `PanicGoal` subclass whose only addition is
/// setting the jump control while fleeing, so the speed argument is the whole of
/// what our `PanicGoal` models and the subclass is not a separate gap.
fn panic_2_2(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 2.2))
}

/// `BreedGoal(this, 0.8)` — rabbit (`Rabbit.registerGoals`) and cat
/// (`Cat.registerGoals`), the two species in this family whose breed
/// speed is not `1.0`, which is why neither can use the shared [`breed_1_0`].
fn breed_0_8(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(BreedGoal::new(ctx.speed * 0.8))
}

/// `WaterAvoidingRandomStrollGoal(this, 0.6)` — rabbit
/// (`Rabbit.registerGoals`), against the `1.0` every other farm animal
/// registers, so the shared [`stroll`] would be wrong by a factor of 1.67.
fn stroll_0_6(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RandomStrollGoal::new(ctx.speed * 0.6))
}

/// `LookAtPlayerGoal(this, Player.class, 10.0F)` — rabbit
/// (`Rabbit.registerGoals`), the only non-`6.0F` look distance in this
/// family, so [`look_at_player_6`] does not apply.
fn look_at_player_10(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(LookAtPlayerGoal::new(10.0, LOOK_PROBABILITY))
}

/// `Rabbit.RabbitAvoidEntityGoal<>(this, Player.class, 8.0F, 2.2, 2.2)` — rabbit
/// (`Rabbit.registerGoals`).
///
/// Vanilla's fourth and fifth arguments are the walk and sprint tiers, and a
/// rabbit is the one registration in the roster where **they are equal**
/// (`2.2, 2.2`), so the shared builder's "take the walk tier, sprint is not
/// modelled" caveat costs nothing here.
///
/// **This goal is inert for a rabbit in production today**, and that is not a
/// defect in this row. Our `AvoidEntityGoal` reads
/// [`MobController::avoid_threat`](crate::ai::MobController::avoid_threat),
/// which `MobSim` feeds from its own `avoided_species` table — and that table
/// has arms for creeper, the skeletons and the spiders only. Until it gains a
/// `"rabbit" => &["player", "wolf", "monster"]` arm the rabbit sees no threats
/// and this goal never starts. The roster deliberately does not carry perception
/// data (see the module header of [`super`]), so the fix belongs there, not here.
fn rabbit_avoid_player(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(AvoidEntityGoal::new(8.0, ctx.speed * 2.2))
}

/// `FollowParentGoal(this, 1.25)` — cow (`AbstractCow.registerGoals`).
fn follow_parent_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FollowParentGoal::new(ctx.speed * 1.25))
}

/// `FollowParentGoal(this, 1.1)` — sheep, pig
/// and chicken (each in their own `registerGoals`).
fn follow_parent_1_1(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FollowParentGoal::new(ctx.speed * 1.1))
}

/// `EatBlockGoal(this)` — sheep only (`animal/sheep/Sheep.java`), no arguments.
/// Its predicate reads the block at and below the mob through
/// `MobController::block_cues_*`; a host whose `PathWorld` does not
/// classify blocks leaves it inert rather than wrong.
fn eat_block(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(EatBlockGoal::new())
}

// -- cat and parrot builders --------------------------------------------------

/// `TamableAnimal.TamableAnimalPanicGoal(1.5)` — cat (`Cat.registerGoals`).
/// The same multiplier and the same vanilla class as the wolf's row in
/// `neutral::WOLF`, but no shared builder: a `Registration` table is a `const`,
/// so `build` must be a plain `fn` item, and the two live in different family
/// modules by construction (see this module's "How to change it").
fn cat_panic_1_5(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 1.5))
}

/// `Cat.CatTemptGoal(this, 0.6, …)` — cat (`Cat.registerGoals`).
fn cat_tempt_0_6(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TemptGoal::new(ctx.speed * 0.6))
}

/// `FollowOwnerGoal(this, 1.0, 10.0F, 5.0F)` — cat (`Cat.registerGoals`).
/// A cat stops five blocks out, against the wolf's two.
fn cat_follow_owner(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FollowOwnerGoal::new(ctx.speed, 10.0, 5.0))
}

/// `CatSitOnBlockGoal(this, 0.8)` — cat (`Cat.registerGoals`).
fn cat_sit_on_block_0_8(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(CatSitOnBlockGoal::new(ctx.speed * 0.8))
}

/// `CatLieOnBedGoal(this, 1.1, 8)` — cat (`Cat.registerGoals`).
fn cat_lie_on_bed_1_1(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(CatLieOnBedGoal::new(ctx.speed * 1.1))
}

/// `WaterAvoidingRandomStrollGoal(this, 0.8, 1.0000001E-5F)` — cat
/// (`Cat.registerGoals`). The probability argument is the reciprocal
/// of [`RandomStrollGoal::with_interval`]'s tick count: `1 / 1.0000001E-5 ≈
/// 100_000`, so a cat only picks a new wander target roughly once every
/// 100,000 ticks (~83 minutes) — a near-total absence of unprompted wandering,
/// unlike every other species in this family which uses the `120`-tick
/// default.
fn cat_stroll(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(RandomStrollGoal::new(ctx.speed * 0.8).with_interval(100_000))
}

/// `TamableAnimal.TamableAnimalPanicGoal(1.25)` — parrot
/// (`Parrot.registerGoals`).
fn parrot_panic_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 1.25))
}

/// `FollowOwnerGoal(this, 1.0, 5.0F, 1.0F)` — parrot
/// (`Parrot.registerGoals`). The tightest follow distances in the
/// tameable set — a parrot stays close.
fn parrot_follow_owner(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FollowOwnerGoal::new(ctx.speed, 5.0, 1.0))
}

#[cfg(test)]
mod tests {
    use lodestone_model::Vec3;

    use super::*;
    use crate::ai::goal::GoalSelector;
    use crate::ai::mob::MobController;
    use crate::ai::navigating_mob::NavigatingMob;
    use crate::ai::roster::Coverage;
    use crate::pathfinding::{Aabb, BlockCues, MobShape, PathType, PathWorld};

    /// A cow's table is the roster's completeness benchmark: vanilla registers
    /// eight goals and every one has an equivalent here. If a gap ever appears in
    /// it, that is a regression in this crate's goal coverage, not a
    /// transcription choice.
    #[test]
    fn a_cow_has_no_unmodelled_registrations() {
        for r in COW {
            assert!(
                matches!(r.coverage, Coverage::Modelled(_)),
                "cow's {} is no longer modelled: {:?}",
                r.vanilla,
                r.coverage
            );
        }
        assert_eq!(COW.len(), 8, "`AbstractCow.java:41-48` registers 8 goals");
    }

    /// The speed each goal actually asks the mob to move at, measured, against
    /// the value predicted from the jar's own multiplier.
    ///
    /// This is the gate a priority-multiset check cannot replace. A cow's
    /// `TemptGoal` built with sheep's `1.1` instead of cow's `1.25` sits at the
    /// right priority, under the right vanilla name, and still moves the cow
    /// toward the player — so every structural assertion and every
    /// direction-of-movement assertion passes. Only predicting `0.2 × 1.25 =
    /// 0.25` and requiring the measurement to land there separates them.
    ///
    /// `BASE` is deliberately not `1.0`: with a base of 1.0 a dropped
    /// multiplication is invisible for the `1.0` multipliers and a swapped
    /// multiplier still shows, so the test would be weaker in exactly the case it
    /// exists for.
    #[test]
    fn every_speed_matches_the_jars_multiplier() {
        use crate::ai::roster::probe::SpeedProbe;

        const BASE: f64 = 0.2;
        let ctx = SpeciesContext::new(BASE);

        // (species, vanilla name, jar multiplier, cited line). Transcribed from
        // the `.java` files, not from the tables above — the whole point is that
        // the expected value originates outside the code under test.
        let expected: &[(&str, &str, f64, &str)] = &[
            ("cow", "PanicGoal", 2.0, "AbstractCow.java:42"),
            ("cow", "BreedGoal", 1.0, "AbstractCow.java:43"),
            ("cow", "TemptGoal(COW_FOOD)", 1.25, "AbstractCow.java:44"),
            ("cow", "FollowParentGoal", 1.25, "AbstractCow.java:45"),
            ("cow", "WaterAvoidingRandomStrollGoal", 1.0, "AbstractCow.java:46"),
            ("sheep", "PanicGoal", 1.25, "Sheep.java:77"),
            ("sheep", "BreedGoal", 1.0, "Sheep.java:78"),
            ("sheep", "TemptGoal(SHEEP_FOOD)", 1.1, "Sheep.java:79"),
            ("sheep", "FollowParentGoal", 1.1, "Sheep.java:80"),
            ("sheep", "WaterAvoidingRandomStrollGoal", 1.0, "Sheep.java:82"),
            ("pig", "PanicGoal", 1.25, "Pig.java:82"),
            ("pig", "BreedGoal", 1.0, "Pig.java:83"),
            ("pig", "TemptGoal(PIG_FOOD)", 1.2, "Pig.java:85"),
            ("pig", "FollowParentGoal", 1.1, "Pig.java:86"),
            ("pig", "WaterAvoidingRandomStrollGoal", 1.0, "Pig.java:87"),
            ("chicken", "PanicGoal", 1.4, "Chicken.java:87"),
            ("chicken", "BreedGoal", 1.0, "Chicken.java:88"),
            ("chicken", "TemptGoal(CHICKEN_FOOD)", 1.0, "Chicken.java:89"),
            ("chicken", "FollowParentGoal", 1.1, "Chicken.java:90"),
            ("chicken", "WaterAvoidingRandomStrollGoal", 1.0, "Chicken.java:91"),
            // A rabbit shares not one multiplier with its siblings except the
            // tempt `1.0`, which makes it the strongest row in this table: four
            // of its five figures are unique in the family, so a builder copied
            // from a neighbour fails here rather than passing by coincidence.
            ("rabbit", "Rabbit.RabbitPanicGoal", 2.2, "Rabbit.java:122"),
            ("rabbit", "BreedGoal", 0.8, "Rabbit.java:123"),
            ("rabbit", "TemptGoal(RABBIT_FOOD)", 1.0, "Rabbit.java:124"),
            (
                "rabbit",
                "Rabbit.RabbitAvoidEntityGoal(Player)",
                2.2,
                "Rabbit.java:125",
            ),
            ("rabbit", "WaterAvoidingRandomStrollGoal", 0.6, "Rabbit.java:129"),
            // Cat and parrot share no multiplier with each other or with any
            // farm animal here except the cat's breed `0.8` (shared with the
            // rabbit), so a builder copied from the wrong species fails this
            // gate rather than passing by coincidence.
            (
                "cat",
                "TamableAnimal.TamableAnimalPanicGoal",
                1.5,
                "Cat.java:108",
            ),
            ("cat", "Cat.CatTemptGoal(CAT_FOOD)", 0.6, "Cat.java:106"),
            ("cat", "BreedGoal", 0.8, "Cat.java:117"),
            (
                "cat",
                "WaterAvoidingRandomStrollGoal",
                0.8,
                "Cat.java:118",
            ),
            (
                "parrot",
                "TamableAnimal.TamableAnimalPanicGoal",
                1.25,
                "Parrot.java:163",
            ),
        ];

        for &(species, vanilla, multiplier, cite) in expected {
            let table = super::super::registrations_for(species);
            let row = table
                .iter()
                .find(|r| r.vanilla == vanilla)
                .unwrap_or_else(|| panic!("{species} has no {vanilla} row"));
            let build = row
                .build()
                .unwrap_or_else(|| panic!("{species}'s {vanilla} builds nothing"));

            let mut probe = SpeedProbe::new();
            let mut goal = build(&ctx);
            assert!(
                goal.can_use(&mut probe),
                "{species}'s {vanilla} could not start against a permissive \
                 probe, so no speed can be read from it"
            );
            goal.start(&mut probe);
            goal.tick(&mut probe);

            let measured = probe.first_speed().unwrap_or_else(|| {
                panic!("{species}'s {vanilla} never called move_to, so its speed argument is unobservable")
            });
            let want = BASE * multiplier;
            assert!(
                (measured - want).abs() < 1e-9,
                "{species}'s {vanilla} moves at {measured}, but {cite} says \
                 {multiplier} × the mob's speed = {want}"
            );
        }
    }

    /// The control for the gate above: it must be *able* to fail. Feed it the
    /// wrong species' multiplier and the same assertion has to reject it —
    /// otherwise the tolerance is loose enough to accept anything.
    ///
    /// Cow panic is `2.0` and sheep panic is `1.25` (`AbstractCow.registerGoals` vs
    /// `Sheep.registerGoals`), so the two are 0.15 blocks/tick apart at this base and
    /// the 1e-9 tolerance cannot straddle them.
    #[test]
    fn the_speed_gate_rejects_a_wrong_multiplier() {
        use crate::ai::roster::probe::SpeedProbe;

        const BASE: f64 = 0.2;
        let ctx = SpeciesContext::new(BASE);
        let cow_panic = COW
            .iter()
            .find(|r| r.vanilla == "PanicGoal")
            .and_then(super::Registration::build)
            .expect("cow has a modelled PanicGoal");

        let mut probe = SpeedProbe::new();
        let mut goal = cow_panic(&ctx);
        assert!(goal.can_use(&mut probe));
        goal.start(&mut probe);
        let measured = probe.first_speed().expect("panic moves the mob");

        assert!(
            (measured - BASE * 2.0).abs() < 1e-9,
            "cow panic must be 2.0x"
        );
        assert!(
            (measured - BASE * 1.25).abs() >= 1e-9,
            "the gate above would accept sheep's 1.25 for a cow, so it is not \
             measuring the multiplier at all"
        );
    }

    /// A rabbit's table against the exact multiset of `addGoal` calls at
    /// `Rabbit.registerGoals`.
    ///
    /// The family's other four species are covered by the equivalent gate in
    /// [`super`]; this one lives here so that adding a
    /// species to this family never requires an edit outside it. The expectation
    /// is transcribed from the jar in jar order, **including the two
    /// registrations this repo does not implement** — an expectation listing only
    /// what we build would be satisfied by any subset, including a wrong one.
    #[test]
    fn a_rabbits_table_matches_the_jars_addgoal_block() {
        let want: Vec<(Selector, i32, &str)> = vec![
            (Selector::Goal, 1, "FloatGoal"),
            (Selector::Goal, 1, "ClimbOnTopOfPowderSnowGoal"),
            (Selector::Goal, 1, "Rabbit.RabbitPanicGoal"),
            (Selector::Goal, 2, "BreedGoal"),
            (Selector::Goal, 3, "TemptGoal(RABBIT_FOOD)"),
            (Selector::Goal, 4, "Rabbit.RabbitAvoidEntityGoal(Player)"),
            (Selector::Goal, 4, "Rabbit.RabbitAvoidEntityGoal(Wolf)"),
            (Selector::Goal, 4, "Rabbit.RabbitAvoidEntityGoal(Monster)"),
            (Selector::Goal, 5, "Rabbit.RaidGardenGoal"),
            (Selector::Goal, 6, "WaterAvoidingRandomStrollGoal"),
            (Selector::Goal, 11, "LookAtPlayerGoal(Player)"),
        ];
        let got: Vec<(Selector, i32, &str)> = super::super::registrations_for("rabbit")
            .iter()
            .map(|r| (r.selector, r.priority, r.vanilla))
            .collect();
        assert_eq!(
            got, want,
            "the rabbit table does not match `animal/rabbit/Rabbit.java:119-130` \
             — re-read the jar before editing either side of this"
        );

        // Three jar facts a later reader is most likely to "correct" into
        // symmetry with the rest of the family. Asserted rather than left to the
        // comment on the table, because a comment cannot fail.
        assert!(
            !RABBIT.iter().any(|r| r.vanilla == "FollowParentGoal"),
            "`Rabbit.java:119-130` registers no FollowParentGoal — every other \
             species in this family does, and adding one here for consistency is \
             exactly what this assertion exists to reject"
        );
        assert!(
            RABBIT.iter().any(|r| r.priority == 11),
            "a rabbit's LookAtPlayerGoal is at priority 11 (`Rabbit.java:130`), \
             not 6 or 7 like its siblings'"
        );
        assert_eq!(
            RABBIT.iter().filter(|r| r.priority == 1).count(),
            3,
            "`Rabbit.java:120-122` puts three registrations at priority 1"
        );
    }

    /// A cat's table against the exact multiset of `addGoal` calls at
    /// `Cat.registerGoals`, transcribed from the jar rather than
    /// from [`CAT`] — copying from the table under test would be satisfied by
    /// any subset, including a wrong one.
    #[test]
    fn a_cats_table_matches_the_jars_addgoal_block() {
        let want: Vec<(Selector, i32, &str)> = vec![
            (Selector::Goal, 1, "FloatGoal"),
            (Selector::Goal, 1, "TamableAnimal.TamableAnimalPanicGoal"),
            (Selector::Goal, 2, "SitWhenOrderedToGoal"),
            (Selector::Goal, 3, "Cat.CatRelaxOnOwnerGoal"),
            (Selector::Goal, 4, "Cat.CatTemptGoal(CAT_FOOD)"),
            (Selector::Goal, 5, "CatLieOnBedGoal"),
            (Selector::Goal, 6, "FollowOwnerGoal"),
            (Selector::Goal, 7, "CatSitOnBlockGoal"),
            (Selector::Goal, 8, "LeapAtTargetGoal"),
            (Selector::Goal, 9, "OcelotAttackGoal"),
            (Selector::Goal, 10, "BreedGoal"),
            (Selector::Goal, 11, "WaterAvoidingRandomStrollGoal"),
            (Selector::Goal, 12, "LookAtPlayerGoal(Player)"),
            (Selector::Target, 1, "NonTameRandomTargetGoal(Rabbit)"),
            (Selector::Target, 1, "NonTameRandomTargetGoal(Turtle)"),
        ];
        let got: Vec<(Selector, i32, &str)> = super::super::registrations_for("cat")
            .iter()
            .map(|r| (r.selector, r.priority, r.vanilla))
            .collect();
        assert_eq!(
            got, want,
            "the cat table does not match `animal/feline/Cat.java:105-121` — \
             re-read the jar before editing either side of this"
        );

        // The fact a later reader is most likely to "fix": a cat has no
        // owner-defence goal at all, unlike the wolf.
        assert!(
            !CAT.iter()
                .any(|r| r.vanilla.contains("OwnerHurt")),
            "`Cat.java`'s targetSelector registers no OwnerHurtByTargetGoal or \
             OwnerHurtTargetGoal — a cat does not defend its owner, and adding \
             one here for symmetry with the wolf is exactly what this assertion \
             exists to reject"
        );
    }

    /// A parrot's table against the exact multiset of `addGoal` calls at
    /// `Parrot.registerGoals`.
    #[test]
    fn a_parrots_table_matches_the_jars_addgoal_block() {
        let want: Vec<(Selector, i32, &str)> = vec![
            (Selector::Goal, 0, "TamableAnimal.TamableAnimalPanicGoal"),
            (Selector::Goal, 0, "FloatGoal"),
            (Selector::Goal, 1, "LookAtPlayerGoal(Player)"),
            (Selector::Goal, 2, "SitWhenOrderedToGoal"),
            (Selector::Goal, 2, "FollowOwnerGoal"),
            (Selector::Goal, 2, "Parrot.ParrotWanderGoal"),
            (Selector::Goal, 3, "LandOnOwnersShoulderGoal"),
            (Selector::Goal, 3, "FollowMobGoal"),
        ];
        let got: Vec<(Selector, i32, &str)> = super::super::registrations_for("parrot")
            .iter()
            .map(|r| (r.selector, r.priority, r.vanilla))
            .collect();
        assert_eq!(
            got, want,
            "the parrot table does not match `animal/parrot/Parrot.java:162-171` \
             — re-read the jar before editing either side of this"
        );

        // The fact a later reader is most likely to "fix" into symmetry with
        // this file's other omission: unlike the cat, a parrot's
        // `SitWhenOrderedToGoal` really is in the jar — only its
        // *taming* mechanism omits the automatic sit, which is a different
        // mechanism entirely (`mobs.rs::tame_mechanism`'s `sit_on_success`).
        assert!(
            PARROT.iter().any(|r| r.vanilla == "SitWhenOrderedToGoal"),
            "`Parrot.java:166` registers SitWhenOrderedToGoal — a parrot can \
             still be ordered to sit by right-click even though taming it does \
             not auto-sit it. Removing this row for 'the parrot doesn't sit' is \
             exactly what this assertion exists to reject"
        );
        assert!(
            !PARROT.iter().any(|r| r.vanilla == "BreedGoal"),
            "`Parrot.java:162-171` registers no BreedGoal — Parrot.canMate is a \
             literal false, so a parrot cannot be bred at all"
        );
        assert!(
            !PARROT.iter().any(|r| r.selector == Selector::Target),
            "a parrot registers no targetSelector goal at all — it cannot fight"
        );
    }

    /// A cat built from [`CAT`] and driven through the production
    /// `NavigatingMob` + `GoalSelector` path both follows its owner and stops
    /// dead once ordered to sit — the two behaviours previously reported
    /// missing, proven on a real mob rather than by the table's presence.
    ///
    /// The second half is the one a structural gate cannot see: `CAT` installs
    /// both `SitWhenOrderedToGoal` (priority 2) and `FollowOwnerGoal`
    /// (priority 6) claiming the same MOVE flag, so this also proves the
    /// priority ordering actually lets the sit order preempt an in-progress
    /// follow rather than the two fighting over motion forever.
    #[test]
    fn a_cat_follows_its_owner_and_then_stops_when_ordered_to_sit() {
        let world = Flat;
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.3, 0.35),
            Vec3::new(0.5, 0.0, 0.5),
            WALK,
            560,
            0,
        );
        let mut ai = GoalSelector::new();
        for (p, g) in super::super::goals_for("cat", &SpeciesContext::new(WALK)) {
            ai.add(p, g);
        }

        let owner = Vec3::new(12.5, 0.0, 0.5);
        let gap_to = |p: Vec3, o: Vec3| ((p.x - o.x).powi(2) + (p.z - o.z).powi(2)).sqrt();

        mob.set_tame(true);
        mob.set_owner(Some(owner));
        assert!(
            !mob.is_ordered_to_sit(),
            "precondition: a freshly tamed cat is not yet ordered to sit"
        );

        let before = gap_to(mob.position(), owner);
        assert!((before - 12.0).abs() < 1e-9, "precondition gap");

        for _ in 0..300 {
            mob.tick(&mut ai);
        }
        let followed_gap = gap_to(mob.position(), owner);
        // `FollowOwnerGoal`'s stop distance for a cat is 5.0 (`Cat.registerGoals`),
        // against the wolf's 2.0 — a value prediction, not a direction: a
        // cat that merely moved *closer* than 12 blocks could still be short
        // of actually reaching its own stop distance.
        assert!(
            followed_gap < 5.0 + WALK,
            "a tame cat with an owner 12 blocks away should have closed to \
             within its 5-block stop distance in 300 ticks, got {followed_gap}"
        );
        let settled_position = mob.position();

        // Order it to sit, then move the "owner" further away — if
        // `SitWhenOrderedToGoal` did not actually preempt `FollowOwnerGoal`,
        // the cat would resume closing the new gap.
        mob.set_ordered_to_sit(true);
        mob.set_owner(Some(Vec3::new(60.5, 0.0, 0.5)));
        for _ in 0..300 {
            mob.tick(&mut ai);
        }
        let after_sit = mob.position();
        let drift =
            ((after_sit.x - settled_position.x).powi(2) + (after_sit.z - settled_position.z).powi(2)).sqrt();
        assert!(
            drift < 1e-6,
            "a cat ordered to sit drifted {drift} blocks toward its owner's new \
             position over 300 ticks; SitWhenOrderedToGoal did not preempt \
             FollowOwnerGoal as the priority-2-vs-6 ordering requires"
        );
    }

    // -- the behavioural gate: a real `NavigatingMob`, not a fake -------------
    //
    // Everything above is structural or reads an argument back off a probe. Both
    // are necessary and neither can see the failure that matters: a table whose
    // goals are installed on a mob that cannot act on them. `SpeedProbe` and
    // `ScriptMob` override every perception method, so a goal's `can_use` is
    // true against them whatever production does — which is precisely how such
    // an island stayed green before. The gate below installs a table into a real
    // `GoalSelector` on a real `NavigatingMob` over a real `PathWorld`, feeds it
    // only what `MobSim::tick` feeds, and measures where the mob ends up.

    /// Flat ground at `y <= -1` with air above — the smallest world a real A\*
    /// search can cross.
    struct Flat;

    impl PathWorld for Flat {
        fn min_y(&self) -> i32 {
            -8
        }
        fn base_path_type(&self, _x: i32, y: i32, _z: i32) -> PathType {
            if y <= -1 {
                PathType::Blocked
            } else {
                PathType::Open
            }
        }
        fn collision_top(&self, _x: i32, y: i32, _z: i32) -> f64 {
            if y <= -1 { 1.0 } else { 0.0 }
        }
        fn collides(&self, aabb: Aabb) -> bool {
            aabb.min_y < 0.0
        }
    }

    /// The mob's `movement_speed`, and the one figure the two runs share.
    const WALK: f64 = 0.3;
    /// Where the tempting player stands, on flat ground 12 blocks along +X.
    const PLAYER: Vec3 = Vec3::new(12.5, 0.0, 0.5);

    /// Installs `species`' table — or nothing, when `species` is `None` — onto a
    /// real [`NavigatingMob`], ticks it `ticks` times with a player holding food
    /// standing [`PLAYER`] away, and returns the horizontal gap before and after.
    ///
    /// The only thing that varies between the measurement and its controls is
    /// **which registration table is installed**. Same world, same mob, same
    /// speed, same perception feed, same tick count — so a difference in outcome
    /// can only come from the roster.
    fn approach_gap(species: Option<&str>, ticks: usize) -> (f64, f64) {
        let world = Flat;
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.4, 0.5),
            Vec3::new(0.5, 0.0, 0.5),
            WALK,
            560,
            0,
        );
        let mut ai = GoalSelector::new();
        if let Some(s) = species {
            for (p, g) in super::super::goals_for(s, &SpeciesContext::new(WALK)) {
                ai.add(p, g);
            }
        }

        let gap = |p: Vec3| ((p.x - PLAYER.x).powi(2) + (p.z - PLAYER.z).powi(2)).sqrt();
        let before = gap(mob.position());
        for _ in 0..ticks {
            // Exactly the perception `MobSim::tick` feeds when a player holds an
            // item in this species' food tag, and the only input this mob gets.
            mob.set_temptation(Some(PLAYER));
            mob.tick(&mut ai);
        }
        (before, gap(mob.position()))
    }

    /// A rabbit built from [`RABBIT`] and driven through the production
    /// `NavigatingMob` + `GoalSelector` path walks to a player holding a carrot.
    ///
    /// Two controls run inside the test, so neither can be skipped or drift out
    /// of date, and both are *differences in the table alone*:
    ///
    /// * **An empty roster entry** — the shape of "this species' table was never
    ///   filled in". The gap must not change at all.
    /// * **The [`FALLBACK`](super::super::FALLBACK) table**, which any unclaimed
    ///   species gets: a stroll and a look, no `TemptGoal`. It receives the
    ///   *identical* temptation feed, so it separates "the roster's tempt row
    ///   moved the rabbit" from "any goal set moves a mob about and 200 ticks is
    ///   long enough to arrive by accident".
    #[test]
    fn a_rabbit_walks_to_a_player_holding_food_and_a_tableless_one_does_not() {
        const TICKS: usize = 200;

        let (before, after) = approach_gap(Some("rabbit"), TICKS);
        assert!(
            (before - 12.0).abs() < 1e-9,
            "precondition: the gap must start at 12 blocks, got {before}"
        );

        // `TemptGoal` stops navigating inside 2.5 blocks (vanilla's stop
        // distance), so a rabbit that genuinely followed ends just inside that,
        // and one walk-step of slack covers the tick it crosses the line on.
        // This is a predicted *value*, not a direction: "it got closer" would be
        // satisfied by a single accidental step.
        assert!(
            after < 2.5 + WALK,
            "a rabbit fed a temptation 12 blocks away ended {after} blocks from \
             it; `TemptGoal`'s stop distance is 2.5, so it never followed"
        );

        let (c_before, c_after) = approach_gap(None, TICKS);
        assert!(
            (c_before - c_after).abs() < 1e-9,
            "control: a mob with an empty roster entry moved from {c_before} to \
             {c_after}. Something other than an installed goal is moving it, so \
             the measurement above is not attributable to the table"
        );

        let (f_before, f_after) = approach_gap(Some("llama"), TICKS);
        assert!(
            (f_before - 12.0).abs() < 1e-9,
            "precondition: the fallback control starts at the same gap"
        );
        assert!(
            f_after > 2.5 + WALK,
            "control: the FALLBACK table — a stroll and a look, no TemptGoal — \
             also reached {f_after} blocks from the player. Then the rabbit's \
             approach is not evidence about its TemptGoal row, and this gate is \
             measuring nothing more than that mobs wander"
        );
    }

    /// [`Flat`], with the floor classified as `minecraft:grass_block`.
    ///
    /// A separate world rather than a cue arm on [`Flat`] on purpose: `Flat`
    /// answers [`BlockCues::NONE`], which is what keeps the tempt gate above free
    /// of a sheep that stops to graze mid-approach. Note the cue is the *only*
    /// difference — a host that classifies nothing leaves [`eat_block`] inert
    /// rather than wrong, which is exactly the state production is in until
    /// `ChunkWorld::block_cues` lands.
    struct Grass;

    impl PathWorld for Grass {
        fn min_y(&self) -> i32 {
            -8
        }
        fn base_path_type(&self, _x: i32, y: i32, _z: i32) -> PathType {
            if y <= -1 {
                PathType::Blocked
            } else {
                PathType::Open
            }
        }
        fn collision_top(&self, _x: i32, y: i32, _z: i32) -> f64 {
            if y <= -1 { 1.0 } else { 0.0 }
        }
        fn collides(&self, aabb: Aabb) -> bool {
            aabb.min_y < 0.0
        }
        fn block_cues(&self, _x: i32, y: i32, _z: i32) -> BlockCues {
            if y <= -1 {
                BlockCues { grass_block: true, ..BlockCues::NONE }
            } else {
                BlockCues::NONE
            }
        }
    }

    /// Ticks a baby `species` on [`Grass`] with **no `add` call of this test's
    /// own** and returns how many eat intents reached the host.
    ///
    /// A baby because [`EatBlockGoal::BABY_INTERVAL`] is 25 ticks against an
    /// adult's 500, so reachability is observable in a short run. The world never
    /// mutates, so nothing depletes the supply — the failure mode that made the
    /// seam's first interval measurement read grass scarcity instead of the eat
    /// interval.
    fn grazes(species: &str, ticks: usize) -> usize {
        let world = Grass;
        let mut mob = NavigatingMob::new(
            &world,
            MobShape::land(0.9, 1.3),
            Vec3::new(0.5, 0.0, 0.5),
            WALK,
            256,
            0,
        );
        mob.set_age(crate::ai::navigating_mob::BABY_START_AGE);

        let mut ai = GoalSelector::new();
        for (p, g) in super::super::goals_for(species, &SpeciesContext::new(WALK)) {
            ai.add(p, g);
        }

        let mut eaten = 0;
        for _ in 0..ticks {
            mob.tick(&mut ai);
            eaten += mob.take_new_eaten().len();
        }
        eaten
    }

    /// The [`SHEEP`] table installs an `EatBlockGoal` that a real
    /// [`NavigatingMob`] can actually reach.
    ///
    /// **What this asserts is installation and reachability, not grazing.** The
    /// cue feed here belongs to this test's [`Grass`] world; production's
    /// `ChunkWorld` does not classify blocks yet and the host does not drain
    /// `take_new_eaten`, so a sheep in a running game grazes nothing. An
    /// eat-*count* prediction would therefore be measuring the absent host half
    /// rather than this row — `crates/lodestone-entity/tests/block_perception.rs`
    /// is where the 444-vs-286 interval calibration lives.
    ///
    /// The control is a difference in the table alone: a cow stands on the same
    /// grass, gets the same feed and the same ticks, and its table has no grazing
    /// row — so a blanket install, or a goal reachable from any passive table,
    /// fails here rather than passing as a sheep.
    #[test]
    fn the_sheeps_table_installs_a_reachable_eat_block_goal() {
        const TICKS: usize = 2_000;

        let sheep = grazes("sheep", TICKS);
        assert!(
            sheep > 0,
            "a baby sheep built only from the roster ate nothing in {TICKS} ticks \
             on classified grass. Either SHEEP no longer carries the EatBlockGoal \
             row, or goals_for does not reach it: with BABY_INTERVAL = {} the \
             chance of a genuinely installed goal never firing is vanishing",
            EatBlockGoal::BABY_INTERVAL
        );

        let cow = grazes("cow", TICKS);
        assert_eq!(
            cow, 0,
            "control: a cow on the same grass, ticked the same {TICKS} times, ate \
             {cow} times. `AbstractCow.java:41-48` registers no EatBlockGoal, so \
             something installs grazing regardless of the table and the sheep \
             measurement above is not attributable to its row"
        );
    }
}
