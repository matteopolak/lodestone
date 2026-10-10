//! Goal sets for the farm animals — cow, mooshroom, sheep, pig, chicken, rabbit
//! — plus the two tameable companions that are `Animal`s rather than neutral
//! mobs: cat and parrot. (The wolf, a neutral mob, lives in
//! [`super::neutral`] instead.)
//!
//! # What it is
//!
//! One [`Registration`] table per species, transcribed from
//! the decompiled tree's passive-animal package; extend it here
//! and nothing else in the tree changes.
//!
//! # Why this family is where the roster first becomes visible
//!
//! Before this module, `MobSim::spawn_species` installed `WanderGoal` and
//! `IdleGlanceGoal` on a cow and nothing else. `StayAfloatGoal`, `FleeInPanicGoal`,
//! `MateGoal`, `LureGoal` and `TrailParentGoal` were **fully implemented, fully
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
//! * **A rabbit's `FleeEntityGoal` is modelled but inert**, because the server's
//!   `avoided_species` feed has no rabbit arm — see [`rabbit_avoid_player`].
//! * **Two rabbit rows are unmodelled for block-perception-adjacent reasons**, next.
//! * **The cat and the parrot** ([`CAT`], [`PARROT`]) close a previously reported
//!   gap: both are tameable and ownable and had no roster entry at all before
//!   this, so a tamed one could never sit or follow. Each table's own doc
//!   comment carries its species-specific traps.
//!
//! # Block perception: the sheep's row is closed, two rabbit rows are not
//!
//! Sheep grazing (vanilla's own sheep eat-block-goal field) used to be
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
//! through the tick driver). What remains is wool regrowth — vanilla's own
//! eaten-block hook's
//! `setSheared(false)` plus `ageUp(60)`,
//! which is entity metadata on the wire. `docs/mob-block-perception.md` is the
//! doc.
//!
//! **A generalisation not to inherit.** The fix's body grouped seven `Missing` rows
//! across two families as one seam capability. Measured against the jar it closes
//! **one**: a rabbit's powder-snow climb needs powder-snow physics
//! nothing here models, its `CropRaidGoal` needs a host-computed candidate
//! block position (`MoveToBlockGoal`'s spiral) plus a block-state *property*, and
//! [`hostile_melee`](super::hostile_melee)'s `AvoidSunlightGoal` reads no block at
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
//! table here reaches a real `GoalSelector` on a real mob. `crate::natural_spawn`
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
    FleeEntityGoal, MateGoal, CatLieOnBedGoal, CatSettleOnOwnerGoal, CatPerchGoal,
    GrazeGoal, AccompanyOwnerGoal, TrailParentGoal, PerchOnOwnerGoal, WatchPlayerGoal,
    FleeInPanicGoal, WanderGoal, LureGoal,
};

use super::{
    LOOK_PROBABILITY, Registration, Selector, SpeciesContext, breed_1_0, float_goal, leap_0_3,
    look_at_player_6, look_at_player_8, random_look_around, sit_when_ordered, stroll,
    untamed_target_baby_turtle, untamed_target_rabbit,
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
        // `MushroomCow` declares no goal registration of its own
        // in vanilla, so a mooshroom inherits
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

/// Vanilla's own cow goal registration.
///
/// The only table in the roster with **no** gaps: all eight of vanilla's cow
/// registrations have an equivalent here.
pub static COW: &[Registration] = &[
    Registration::goal(0, "stay_afloat", float_goal),
    Registration::goal(1, "flee_in_panic", panic_2_0),
    Registration::goal(2, "mate", breed_1_0),
    Registration::goal(3, "lure(cow_food)", tempt_1_25),
    Registration::goal(4, "trail_parent", follow_parent_1_25),
    Registration::goal(5, "wander_dry", stroll),
    Registration::goal(6, "watch_player(player)", look_at_player_6),
    Registration::goal(7, "idle_glance", random_look_around),
];

/// Vanilla's own sheep goal registration. Its grass-eating goal is constructed before
/// the first registration, so the registrations themselves come after.
pub static SHEEP: &[Registration] = &[
    Registration::goal(0, "stay_afloat", float_goal),
    Registration::goal(1, "flee_in_panic", panic_1_25),
    Registration::goal(2, "mate", breed_1_0),
    Registration::goal(3, "lure(sheep_food)", tempt_1_1),
    Registration::goal(4, "trail_parent", follow_parent_1_1),
    // The seam gap this row waited on is closed (`bdf7120`): the goal reads
    // the block below through `MobController::block_cues_below`. Grazing still
    // needs the host's drain of `take_new_eaten` to see grass turn to dirt — see
    // `docs/mob-block-perception.md`.
    Registration::goal(5, "graze", eat_block),
    Registration::goal(6, "wander_dry", stroll),
    Registration::goal(7, "watch_player(player)", look_at_player_6),
    Registration::goal(8, "idle_glance", random_look_around),
];

/// Vanilla's own pig goal registration.
///
/// A pig is the one species here with **two** `LureGoal` registrations at the
/// same priority, for carrot-on-a-stick and for `PIG_FOOD`.
pub static PIG: &[Registration] = &[
    Registration::goal(0, "stay_afloat", float_goal),
    Registration::goal(1, "flee_in_panic", panic_1_25),
    // Vanilla puts a pig's `MateGoal` at 3, not 2 — nothing occupies 2.
    Registration::goal(3, "mate", breed_1_0),
    // The carrot-on-a-stick goal differs from the food goal only in its item,
    // and the server's tempt feed lists that item beside the foods.
    Registration::covered(Selector::Goal, 4, "lure(carrot_on_a_stick)", "lure(pig_food)"),
    Registration::goal(4, "lure(pig_food)", tempt_1_2),
    Registration::goal(5, "trail_parent", follow_parent_1_1),
    Registration::goal(6, "wander_dry", stroll),
    Registration::goal(7, "watch_player(player)", look_at_player_6),
    Registration::goal(8, "idle_glance", random_look_around),
];

/// Vanilla's own chicken goal registration.
pub static CHICKEN: &[Registration] = &[
    Registration::goal(0, "stay_afloat", float_goal),
    Registration::goal(1, "flee_in_panic", panic_1_4),
    Registration::goal(2, "mate", breed_1_0),
    Registration::goal(3, "lure(chicken_food)", tempt_1_0),
    Registration::goal(4, "trail_parent", follow_parent_1_1),
    Registration::goal(5, "wander_dry", stroll),
    Registration::goal(6, "watch_player(player)", look_at_player_6),
    Registration::goal(7, "idle_glance", random_look_around),
];

/// Vanilla's own rabbit goal registration.
///
/// The odd one out of this family in five ways, every one a jar fact rather than
/// a transcription choice. They are listed because four of the five are exactly
/// the shape of thing that gets "fixed" into symmetry by a later reader:
///
/// * **No `TrailParentGoal`.** Every other species here registers one; a rabbit
///   does not — vanilla's own registration has no such line — so there is no row for it. Do not
///   add one for consistency with its siblings.
/// * **Three registrations share priority 1** (stay afloat,
///   powder-snow climb, panic), where every other species
///   here has exactly one goal per priority.
/// * **Its look goal is at priority 11**, not 6 or 7, and at **`10.0F`** rather
///   than the `6.0F` every other farm animal uses.
/// * **It is the only species in this family that flees anything**, and it
///   registers three `FleeEntityGoal`s to do it.
/// * **Its breed and stroll speeds are not the family's** — `0.8` and `0.6`
///   against everyone else's `1.0`, so neither shared builder applies.
///
/// The killer-bunny variant installs a `MeleeStrikeGoal(1.4, true)` and two
/// target goals from vanilla's own variant setter, **not** from vanilla's own main
/// registration.
/// They are deliberately absent here: this table is the main registration's
/// transcription that the multiset gate cites, and a conditional runtime
/// installation is a different mechanism — the one
/// [`GoalSelector::remove`](crate::ai::goal::GoalSelector::remove) exists for.
/// Adding them as rows would make the cited line range a lie.
pub static RABBIT: &[Registration] = &[
    Registration::goal(1, "stay_afloat", float_goal),
    Registration::goal(1, "walk_on_powder_snow", climb_out_of_powder_snow),
    Registration::goal(1, "rabbit.panic", panic_2_2),
    Registration::goal(2, "mate", breed_0_8),
    Registration::goal(3, "lure(rabbit_food)", tempt_1_0),
    // The rabbit's three flee goals differ from the creeper's
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
        "rabbit.flee_entity(player)",
        rabbit_avoid_player,
    ),
    Registration::covered(
        Selector::Goal,
        4,
        "rabbit.flee_entity(wolf)",
        "rabbit.flee_entity(player)",
    ),
    Registration::covered(
        Selector::Goal,
        4,
        "rabbit.flee_entity(monster)",
        "rabbit.flee_entity(player)",
    ),
    Registration::goal(5, "rabbit.crop_raid", raid_garden),
    Registration::goal(6, "wander_dry", stroll_0_6),
    Registration::goal(11, "watch_player(player)", look_at_player_10),
];

/// Jumps back out of powder snow the mob has sunk into.
fn climb_out_of_powder_snow(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(crate::ai::goals::ClimbOutOfPowderSnowGoal)
}

fn raid_garden(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(crate::ai::raid_garden::CropRaidGoal::new(ctx.speed))
}

/// Vanilla's own cat goal registration.
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
///   untamed cat fed cod always attempts a tame and never reaches `MateGoal`
///   however the roll lands — see `docs/taming-and-breeding.md`'s note on
///   `breeding_items_are_per_species_and_a_parrot_has_none`'s sibling case.
/// * **`SitOnCommandGoal` and `AccompanyOwnerGoal` are shared with the wolf**
///   ([`sit_when_ordered`](super::sit_when_ordered)) and the parrot below, but
///   the follow distances are the cat's own — `(10, 5)`, not the wolf's
///   `(10, 2)` — so [`cat_follow_owner`] is a distinct builder.
/// * **A cat has no combat goal at all.** Unlike the wolf, vanilla registers no
///   `DefendOwnerGoal`/`AssistOwnerGoal` for `Cat` — its two
///   `targetSelector` rows are both the untamed-only random target, an *untamed*
///   cat's own rabbit/turtle hunting, unrelated to its owner. A cat does not
///   defend you.
/// * **Its water-avoiding stroll almost never fires.** Vanilla passes
///   an explicit `1.0000001E-5F` probability, the reciprocal of which
///   is is ~100,000 ticks between attempts — a cat parked near its owner or a
///   bed essentially does not wander on its own, unlike every other species in
///   this family which strolls constantly.
pub static CAT: &[Registration] = &[
    Registration::goal(1, "stay_afloat", float_goal),
    Registration::goal(1, "tamable.panic", cat_panic_1_5),
    Registration::goal(2, "sit_on_command", sit_when_ordered),
    // Vanilla's own cat relax-on-owner goal — lies down near a sleeping owner and may
    // leave a morning gift on waking. `Coverage::Modelled` now —
    // see [`CatSettleOnOwnerGoal`]'s own doc for the disclosed simplifications
    // (bed-foot position, same-species exclusion, and the host-side gift
    // roll).
    Registration::goal(3, "cat.settle_on_owner", cat_relax_on_owner),
    // Vanilla's own cat tempt goal,
    // added at priority 4. Vanilla's own scare argument
    // (fleeing a sudden nearby sprinting player) is not modelled — our
    // `LureGoal` has no scare state, same simplification as every other
    // `LureGoal` row in this roster.
    Registration::goal(4, "cat.lure(cat_food)", cat_tempt_0_6),
    // Vanilla's own cat bed-hunt goal — a `MoveToBlockGoal` that hunts
    // beds in an 8-block radius. The candidate bed position is host-computed
    // (`MobController::cat_bed_target`, `docs/mob-block-perception.md`'s own
    // guidance for a goal that needs to search a neighbourhood) rather than
    // searched in-goal.
    Registration::goal(5, "cat.lie_on_bed", cat_lie_on_bed_1_1),
    Registration::goal(6, "accompany_owner", cat_follow_owner),
    // Vanilla's own cat perch-hunt goal — hunts chests and lit furnaces to
    // perch on, same host-computed-candidate shape as `CatLieOnBedGoal` above
    // (`MobController::cat_sit_target`).
    Registration::goal(7, "cat.perch", cat_sit_on_block_0_8),
    Registration::goal(8, "pounce", leap_0_3),
    Registration::goal(9, "cat.stalk_attack", stalk_attack),
    Registration::goal(10, "mate", breed_0_8),
    Registration::goal(11, "wander_dry", cat_stroll),
    Registration::goal(12, "watch_player(player)", look_at_player_10),
    // An untamed cat hunting a random nearby rabbit.
    Registration::target(1, "untamed_prey_target(rabbit)", untamed_target_rabbit),
    // The same hunt, narrowed to baby turtles on land.
    Registration::target(1, "untamed_prey_target(turtle)", untamed_target_baby_turtle),
];

/// Vanilla's own parrot goal registration.
///
/// Closes another previously reported gap. A parrot **does** register
/// `SitOnCommandGoal` — do not drop that row for symmetry with
/// "the parrot doesn't sit" — but vanilla's own try-to-tame step is the one taming success
/// of the three that omits the automatic `setOrderedToSit(true)`
/// (`docs/taming-and-breeding.md` §2, already correct in `mobs.rs`'s
/// `tame_mechanism`). The two are different mechanisms: this row is the
/// *goal* an owner's right-click toggle still needs to mean anything, and it
/// is present in the jar regardless of how taming leaves the flag.
///
/// A parrot registers **no targetSelector goal at all** — it cannot fight,
/// has no `DefendOwnerGoal`/`AssistOwnerGoal`, and (unlike every
/// farm animal and the cat above) has no `MateGoal` either:
/// vanilla's own can-mate check returns `false` and its own is-food check returns a literal
/// `false`, so there is nothing to tempt it into breeding with — see
/// `breeding_food`'s own comment on the empty `"parrot"` row.
pub static PARROT: &[Registration] = &[
    Registration::goal(0, "tamable.panic", parrot_panic_1_25),
    Registration::goal(0, "stay_afloat", float_goal),
    Registration::goal(1, "watch_player(player)", look_at_player_8),
    Registration::goal(2, "sit_on_command", sit_when_ordered),
    Registration::goal(2, "accompany_owner", parrot_follow_owner),
    Registration::goal(2, "parrot.perch_wander", parrot_wander),
    // Vanilla's own land-on-shoulder goal — shoulder riding. `Coverage::Modelled`
    // now: see [`PerchOnOwnerGoal`]'s own doc for the
    // disclosed owner-physical-state simplifications. Landing despawns the
    // parrot mob entity on the host side (vanilla discards it too — see
    // vanilla's own shoulder-mount setter); no client-visible perched
    // pose is rendered, since that is a player-model render layer this
    // crate's seam has no way to reach.
    Registration::goal(3, "perch_on_owner", parrot_land_on_shoulder),
    Registration::goal(3, "trail_mob", parrot_follow_mob),
];

// -- builders, one per distinct jar speed multiplier -------------------------
//
// Vanilla's speed arguments are multipliers on the mob's own MOVEMENT_SPEED, so
// each of these is `ctx.speed * <the jar's factor>` and the factor stays visible
// next to the citation. `Registration.build` must be a plain `fn` item, so a
// parameterised closure is not an option.

/// The cow's panic speed factor, `2.0`, from vanilla's own cow registration. The fastest
/// panic in this family.
fn panic_2_0(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FleeInPanicGoal::new(ctx.speed * 2.0))
}

/// The panic speed factor `1.25` — sheep (vanilla's own sheep registration) and pig
/// (vanilla's own pig registration).
fn panic_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FleeInPanicGoal::new(ctx.speed * 1.25))
}

/// The chicken's panic speed factor, `1.4`, from vanilla's own chicken registration.
fn panic_1_4(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FleeInPanicGoal::new(ctx.speed * 1.4))
}

/// The cow's tempt speed factor, `1.25`, from vanilla's own cow registration.
fn tempt_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(LureGoal::new(ctx.speed * 1.25))
}

/// The sheep's tempt speed factor, `1.1`, from vanilla's own sheep registration.
fn tempt_1_1(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(LureGoal::new(ctx.speed * 1.1))
}

/// The pig's tempt speed factor, `1.2`, from vanilla's own pig registration.
fn tempt_1_2(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(LureGoal::new(ctx.speed * 1.2))
}

/// The chicken's tempt speed factor, `1.0`, from vanilla's own chicken registration.
fn tempt_1_0(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(LureGoal::new(ctx.speed))
}

/// The rabbit's own panic goal, speed factor `2.2`
/// (vanilla's own rabbit registration). The fastest panic in the family; a cow's
/// `2.0` is next.
///
/// Vanilla's rabbit-specific panic goal is a `FleeInPanicGoal` subclass whose only addition is
/// setting the jump control while fleeing, so the speed argument is the whole of
/// what our `FleeInPanicGoal` models and the subclass is not a separate gap.
fn panic_2_2(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FleeInPanicGoal::new(ctx.speed * 2.2))
}

/// The breed speed factor `0.8` — rabbit (vanilla's own rabbit registration) and cat
/// (vanilla's own cat registration), the two species in this family whose breed
/// speed is not `1.0`, which is why neither can use the shared [`breed_1_0`].
fn breed_0_8(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(MateGoal::new(ctx.speed * 0.8))
}

/// The rabbit's stroll speed factor, `0.6`
/// (vanilla's own rabbit registration), against the `1.0` every other farm animal
/// registers, so the shared [`stroll`] would be wrong by a factor of 1.67.
fn stroll_0_6(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(WanderGoal::new(ctx.speed * 0.6))
}

/// The rabbit's look-at-player distance, `10.0`
/// (vanilla's own rabbit registration), the only non-`6.0F` look distance in this
/// family, so [`look_at_player_6`] does not apply.
fn look_at_player_10(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(WatchPlayerGoal::new(10.0, LOOK_PROBABILITY))
}

/// The rabbit's own avoid-player goal, radius `8.0`, walk and sprint speed factor `2.2`
/// (vanilla's own rabbit registration).
///
/// Vanilla's fourth and fifth arguments are the walk and sprint tiers, and a
/// rabbit is the one registration in the roster where **they are equal**
/// (`2.2, 2.2`), so the shared builder's "take the walk tier, sprint is not
/// modelled" caveat costs nothing here.
///
/// **This goal is inert for a rabbit in production today**, and that is not a
/// defect in this row. Our `FleeEntityGoal` reads
/// [`MobController::avoid_threat`](crate::ai::MobController::avoid_threat),
/// which `MobSim` feeds from its own `avoided_species` table — and that table
/// has arms for creeper, the skeletons and the spiders only. Until it gains a
/// `"rabbit" => &["player", "wolf", "monster"]` arm the rabbit sees no threats
/// and this goal never starts. The roster deliberately does not carry perception
/// data (see the module header of [`super`]), so the fix belongs there, not here.
fn rabbit_avoid_player(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FleeEntityGoal::new(8.0, ctx.speed * 2.2))
}

/// The cow's follow-parent speed factor, `1.25`, from vanilla's own cow registration.
fn follow_parent_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TrailParentGoal::new(ctx.speed * 1.25))
}

/// The follow-parent speed factor `1.1` — sheep, pig
/// and chicken (each in their own goal registration).
fn follow_parent_1_1(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TrailParentGoal::new(ctx.speed * 1.1))
}

/// Vanilla's own eat-block goal — sheep only, no arguments.
/// Its predicate reads the block at and below the mob through
/// `MobController::block_cues_*`; a host whose `PathWorld` does not
/// classify blocks leaves it inert rather than wrong.
fn eat_block(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(GrazeGoal::new())
}

// -- cat and parrot builders --------------------------------------------------

/// The cat's panic speed factor, `1.5`, from vanilla's own cat registration.
/// The same multiplier and the same vanilla goal type as the wolf's row in
/// `neutral::WOLF`, but no shared builder: a `Registration` table is a `const`,
/// so `build` must be a plain `fn` item, and the two live in different family
/// modules by construction (see this module's "How to change it").
fn cat_panic_1_5(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FleeInPanicGoal::new(ctx.speed * 1.5))
}

/// The cat's tempt speed factor, `0.6`, from vanilla's own cat registration.
fn cat_tempt_0_6(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(LureGoal::new(ctx.speed * 0.6))
}

/// The cat's own follow-owner distances, `(10.0, 5.0)`, from vanilla's own cat registration.
/// A cat stops five blocks out, against the wolf's two.
fn cat_follow_owner(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(AccompanyOwnerGoal::new(ctx.speed, 10.0, 5.0))
}

/// The cat's perch-hunt speed factor, `0.8`, from vanilla's own cat registration.
fn cat_sit_on_block_0_8(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(CatPerchGoal::new(ctx.speed * 0.8))
}

/// The cat's bed-hunt speed factor, `1.1`, and search radius, `8`, from vanilla's own cat registration.
fn cat_lie_on_bed_1_1(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(CatLieOnBedGoal::new(ctx.speed * 1.1))
}

/// The cat's own relax-on-owner goal, from vanilla's own cat registration. No
/// constructor speed argument in the jar (the goal's own `moveTo` calls
/// hardcode `1.1F`); `ctx.speed` still scales it, matching every other
/// builder here, since `1.1F` is itself a `speedModifier` multiplier on the
/// mob's own movement-speed attribute, not a literal blocks/tick figure.
fn cat_relax_on_owner(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(CatSettleOnOwnerGoal::new(ctx.speed * 1.1))
}

/// The cat's stroll speed factor, `0.8`, and its near-zero wander probability
/// (vanilla's own cat registration). The probability argument is the reciprocal
/// of [`WanderGoal::with_interval`]'s tick count: `1 / 1.0000001E-5 ≈
/// 100_000`, so a cat only picks a new wander target roughly once every
/// 100,000 ticks (~83 minutes) — a near-total absence of unprompted wandering,
/// unlike every other species in this family which uses the `120`-tick
/// default.
fn parrot_wander(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(crate::ai::parrot::PerchWanderGoal::new(ctx.speed))
}

fn parrot_follow_mob(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(crate::ai::parrot::TrailMobGoal::new(ctx.speed))
}

fn stalk_attack(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(crate::ai::stalk_attack::StalkAttackGoal::new(0.6, ctx.speed))
}

fn cat_stroll(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(WanderGoal::new(ctx.speed * 0.8).with_interval(100_000))
}

/// The parrot's panic speed factor, `1.25`
/// (vanilla's own parrot registration).
fn parrot_panic_1_25(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FleeInPanicGoal::new(ctx.speed * 1.25))
}

/// The parrot's own follow-owner distances, `(5.0, 1.0)`
/// (vanilla's own parrot registration). The tightest follow distances in the
/// tameable set — a parrot stays close.
fn parrot_follow_owner(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(AccompanyOwnerGoal::new(ctx.speed, 5.0, 1.0))
}

/// The parrot's own land-on-shoulder goal (vanilla's own parrot registration). No
/// constructor arguments in the jar at all.
fn parrot_land_on_shoulder(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PerchOnOwnerGoal::new())
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
                r.name,
                r.coverage
            );
        }
        assert_eq!(COW.len(), 8, "vanilla's own cow registration registers 8 goals");
    }

    /// The speed each goal actually asks the mob to move at, measured, against
    /// the value predicted from the jar's own multiplier.
    ///
    /// This is the gate a priority-multiset check cannot replace. A cow's
    /// `LureGoal` built with sheep's `1.1` instead of cow's `1.25` sits at the
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

        // (species, vanilla name, jar multiplier). Transcribed from
        // the decompiled sources, not from the tables above — the whole point is that
        // the expected value originates outside the code under test.
        let expected: &[(&str, &str, f64)] = &[
            ("cow", "flee_in_panic", 2.0),
            ("cow", "mate", 1.0),
            ("cow", "lure(cow_food)", 1.25),
            ("cow", "trail_parent", 1.25),
            ("cow", "wander_dry", 1.0),
            ("sheep", "flee_in_panic", 1.25),
            ("sheep", "mate", 1.0),
            ("sheep", "lure(sheep_food)", 1.1),
            ("sheep", "trail_parent", 1.1),
            ("sheep", "wander_dry", 1.0),
            ("pig", "flee_in_panic", 1.25),
            ("pig", "mate", 1.0),
            ("pig", "lure(pig_food)", 1.2),
            ("pig", "trail_parent", 1.1),
            ("pig", "wander_dry", 1.0),
            ("chicken", "flee_in_panic", 1.4),
            ("chicken", "mate", 1.0),
            ("chicken", "lure(chicken_food)", 1.0),
            ("chicken", "trail_parent", 1.1),
            ("chicken", "wander_dry", 1.0),
            // A rabbit shares not one multiplier with its siblings except the
            // tempt `1.0`, which makes it the strongest row in this table: four
            // of its five figures are unique in the family, so a builder copied
            // from a neighbour fails here rather than passing by coincidence.
            ("rabbit", "rabbit.panic", 2.2),
            ("rabbit", "mate", 0.8),
            ("rabbit", "lure(rabbit_food)", 1.0),
            ("rabbit", "rabbit.flee_entity(player)", 2.2),
            ("rabbit", "wander_dry", 0.6),
            // Cat and parrot share no multiplier with each other or with any
            // farm animal here except the cat's breed `0.8` (shared with the
            // rabbit), so a builder copied from the wrong species fails this
            // gate rather than passing by coincidence.
            ("cat", "tamable.panic", 1.5),
            ("cat", "cat.lure(cat_food)", 0.6),
            ("cat", "mate", 0.8),
            ("cat", "wander_dry", 0.8),
            ("parrot", "tamable.panic", 1.25),
        ];

        for &(species, vanilla, multiplier) in expected {
            let table = super::super::registrations_for(species);
            let row = table
                .iter()
                .find(|r| r.name == vanilla)
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
                "{species}'s {vanilla} moves at {measured}, but vanilla's own \
                 goal registration says \
                 {multiplier} × the mob's speed = {want}"
            );
        }
    }

    /// The control for the gate above: it must be *able* to fail. Feed it the
    /// wrong species' multiplier and the same assertion has to reject it —
    /// otherwise the tolerance is loose enough to accept anything.
    ///
    /// Cow panic is `2.0` and sheep panic is `1.25` (vanilla's own cow registration vs
    /// its own sheep registration), so the two are 0.15 blocks/tick apart at this base and
    /// the 1e-9 tolerance cannot straddle them.
    #[test]
    fn the_speed_gate_rejects_a_wrong_multiplier() {
        use crate::ai::roster::probe::SpeedProbe;

        const BASE: f64 = 0.2;
        let ctx = SpeciesContext::new(BASE);
        let cow_panic = COW
            .iter()
            .find(|r| r.name == "flee_in_panic")
            .and_then(super::Registration::build)
            .expect("cow has a modelled FleeInPanicGoal");

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
    /// vanilla's own rabbit goal registration.
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
            (Selector::Goal, 1, "stay_afloat"),
            (Selector::Goal, 1, "walk_on_powder_snow"),
            (Selector::Goal, 1, "rabbit.panic"),
            (Selector::Goal, 2, "mate"),
            (Selector::Goal, 3, "lure(rabbit_food)"),
            (Selector::Goal, 4, "rabbit.flee_entity(player)"),
            (Selector::Goal, 4, "rabbit.flee_entity(wolf)"),
            (Selector::Goal, 4, "rabbit.flee_entity(monster)"),
            (Selector::Goal, 5, "rabbit.crop_raid"),
            (Selector::Goal, 6, "wander_dry"),
            (Selector::Goal, 11, "watch_player(player)"),
        ];
        let got: Vec<(Selector, i32, &str)> = super::super::registrations_for("rabbit")
            .iter()
            .map(|r| (r.selector, r.priority, r.name))
            .collect();
        assert_eq!(
            got, want,
            "the rabbit table does not match vanilla's own rabbit registration \
             — re-read the jar before editing either side of this"
        );

        // Three jar facts a later reader is most likely to "correct" into
        // symmetry with the rest of the family. Asserted rather than left to the
        // comment on the table, because a comment cannot fail.
        assert!(
            !RABBIT.iter().any(|r| r.name == "trail_parent"),
            "vanilla's own rabbit registration registers no TrailParentGoal — every other \
             species in this family does, and adding one here for consistency is \
             exactly what this assertion exists to reject"
        );
        assert!(
            RABBIT.iter().any(|r| r.priority == 11),
            "a rabbit's WatchPlayerGoal is at priority 11 (vanilla's own \
             rabbit registration), \
             not 6 or 7 like its siblings'"
        );
        assert_eq!(
            RABBIT.iter().filter(|r| r.priority == 1).count(),
            3,
            "vanilla's own rabbit registration puts three registrations at priority 1"
        );
    }

    /// A cat's table against the exact multiset of `addGoal` calls at
    /// Vanilla's own cat goal registration, transcribed from the jar rather than
    /// from [`CAT`] — copying from the table under test would be satisfied by
    /// any subset, including a wrong one.
    #[test]
    fn a_cats_table_matches_the_jars_addgoal_block() {
        let want: Vec<(Selector, i32, &str)> = vec![
            (Selector::Goal, 1, "stay_afloat"),
            (Selector::Goal, 1, "tamable.panic"),
            (Selector::Goal, 2, "sit_on_command"),
            (Selector::Goal, 3, "cat.settle_on_owner"),
            (Selector::Goal, 4, "cat.lure(cat_food)"),
            (Selector::Goal, 5, "cat.lie_on_bed"),
            (Selector::Goal, 6, "accompany_owner"),
            (Selector::Goal, 7, "cat.perch"),
            (Selector::Goal, 8, "pounce"),
            (Selector::Goal, 9, "cat.stalk_attack"),
            (Selector::Goal, 10, "mate"),
            (Selector::Goal, 11, "wander_dry"),
            (Selector::Goal, 12, "watch_player(player)"),
            (Selector::Target, 1, "untamed_prey_target(rabbit)"),
            (Selector::Target, 1, "untamed_prey_target(turtle)"),
        ];
        let got: Vec<(Selector, i32, &str)> = super::super::registrations_for("cat")
            .iter()
            .map(|r| (r.selector, r.priority, r.name))
            .collect();
        assert_eq!(
            got, want,
            "the cat table does not match vanilla's own cat registration — \
             re-read the jar before editing either side of this"
        );

        // The fact a later reader is most likely to "fix": a cat has no
        // owner-defence goal at all, unlike the wolf.
        assert!(
            !CAT.iter()
                .any(|r| r.name.contains("defend_owner") || r.name.contains("assist_owner")),
            "vanilla's own cat registration targetSelector registers no DefendOwnerGoal or \
             AssistOwnerGoal — a cat does not defend its owner, and adding \
             one here for symmetry with the wolf is exactly what this assertion \
             exists to reject"
        );
    }

    /// A parrot's table against the exact multiset of `addGoal` calls at
    /// vanilla's own parrot goal registration.
    #[test]
    fn a_parrots_table_matches_the_jars_addgoal_block() {
        let want: Vec<(Selector, i32, &str)> = vec![
            (Selector::Goal, 0, "tamable.panic"),
            (Selector::Goal, 0, "stay_afloat"),
            (Selector::Goal, 1, "watch_player(player)"),
            (Selector::Goal, 2, "sit_on_command"),
            (Selector::Goal, 2, "accompany_owner"),
            (Selector::Goal, 2, "parrot.perch_wander"),
            (Selector::Goal, 3, "perch_on_owner"),
            (Selector::Goal, 3, "trail_mob"),
        ];
        let got: Vec<(Selector, i32, &str)> = super::super::registrations_for("parrot")
            .iter()
            .map(|r| (r.selector, r.priority, r.name))
            .collect();
        assert_eq!(
            got, want,
            "the parrot table does not match vanilla's own parrot registration \
             — re-read the jar before editing either side of this"
        );

        // The fact a later reader is most likely to "fix" into symmetry with
        // this file's other omission: unlike the cat, a parrot's
        // `SitOnCommandGoal` really is in the jar — only its
        // *taming* mechanism omits the automatic sit, which is a different
        // mechanism entirely (`mobs.rs::tame_mechanism`'s `sit_on_success`).
        assert!(
            PARROT.iter().any(|r| r.name == "sit_on_command"),
            "vanilla's own parrot registration registers SitOnCommandGoal — a parrot can \
             still be ordered to sit by right-click even though taming it does \
             not auto-sit it. Removing this row for 'the parrot doesn't sit' is \
             exactly what this assertion exists to reject"
        );
        assert!(
            !PARROT.iter().any(|r| r.name == "mate"),
            "vanilla's own parrot registration registers no MateGoal — its own can-mate check is a \
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
    /// both `SitOnCommandGoal` (priority 2) and `AccompanyOwnerGoal`
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
            63,
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
        // `AccompanyOwnerGoal`'s stop distance for a cat is 5.0 (vanilla's own cat registration),
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
        // `SitOnCommandGoal` did not actually preempt `AccompanyOwnerGoal`,
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
             position over 300 ticks; SitOnCommandGoal did not preempt \
             AccompanyOwnerGoal as the priority-2-vs-6 ordering requires"
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
            63,
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
    ///   species gets: a stroll and a look, no `LureGoal`. It receives the
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

        // `LureGoal` stops navigating inside 2.5 blocks (vanilla's stop
        // distance), so a rabbit that genuinely followed ends just inside that,
        // and one walk-step of slack covers the tick it crosses the line on.
        // This is a predicted *value*, not a direction: "it got closer" would be
        // satisfied by a single accidental step.
        assert!(
            after < 2.5 + WALK,
            "a rabbit fed a temptation 12 blocks away ended {after} blocks from \
             it; `LureGoal`'s stop distance is 2.5, so it never followed"
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
            "control: the FALLBACK table — a stroll and a look, no LureGoal — \
             also reached {f_after} blocks from the player. Then the rabbit's \
             approach is not evidence about its LureGoal row, and this gate is \
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
    /// A baby because [`GrazeGoal::BABY_INTERVAL`] is 25 ticks against an
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
            63,
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

    /// The [`SHEEP`] table installs an `GrazeGoal` that a real
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
             on classified grass. Either SHEEP no longer carries the GrazeGoal \
             row, or goals_for does not reach it: with BABY_INTERVAL = {} the \
             chance of a genuinely installed goal never firing is vanishing",
            GrazeGoal::BABY_INTERVAL
        );

        let cow = grazes("cow", TICKS);
        assert_eq!(
            cow, 0,
            "control: a cow on the same grass, ticked the same {TICKS} times, ate \
             {cow} times. vanilla's own cow registration registers no GrazeGoal, so \
             something installs grazing regardless of the table and the sheep \
             measurement above is not attributable to its row"
        );
    }
}
