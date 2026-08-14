//! Species-keyed tables with no `MobSim` dependency: hostility, leashability,
//! avoid/tempt/breeding food, taming mechanism and its horse-temper/food
//! variants, and the death-experience reward roll. Moved out of `mobs/mod.rs`
//! verbatim as part of the `mobs.rs` file split (see
//! `docs/plans/crate-and-file-splits.md`).

use lodestone_model::ResourceKey;

use crate::mob_spawn::SpawnRng;

/// Whether `entity_type` is one of the hostile "monster" species, for the
/// purpose of picking its [`MobCategory`] and whether it resists natural
/// despawn.
///
/// **This no longer decides anything about goals.** It used to be the
/// hostile-versus-passive switch that gave a monster a `MeleeAttackGoal` and a
/// farm animal nothing, which is why it was a literal 8-name string match; that
/// job now belongs to [`lodestone_entity::ai::roster`], keyed per species and
/// cited against the jar. What is left here is spawn-category data, and it stays
/// here on purpose: species-to-category is version/registry knowledge, kept
/// separate from [`crate::mob_spawn::MobCategory`] itself. That type used to be
/// a second, independent 8-variant `MobCategory` (this crate's own, next to
/// [`lodestone_entity::spawn::MobCategory`], with its own `check_despawn`) —
/// issue #518 unified them: `crate::mob_spawn::MobCategory` is now a re-export
/// of the `lodestone-entity` one, so this module and its callers see one type,
/// not two.
///
/// # Where these names come from, and the heuristic that was wrong (issue #457)
///
/// Every path below was read from that species' own registration in
/// `EntityTypes.java` (`.cache/mc/26.2/src/net/minecraft/world/entity/`), which
/// is where vanilla's `MobCategory` actually lives — `EntityType.Builder.of(X::new,
/// MobCategory.MONSTER)`. The list previously held the original **eight** and its
/// doc claimed it "covers exactly the families
/// [`lodestone_entity::attribute::default_attributes`] templates as `Monster`".
/// That heuristic is **not equivalent to vanilla's category**, and reading the
/// registrations is what showed it:
///
/// * A **ghast** is `MobCategory.MONSTER` (`EntityTypes.java:473-474`) while its
///   attribute builder is a bare `Mob.createMobAttributes()` with no
///   `attack_damage` at all (`monster/Ghast.java:116-122`). Deriving the category
///   from the attribute template would have made it a persistent `Creature`.
/// * A **snow golem** is `MobCategory.MISC` (`EntityTypes.java:886`) — neither
///   `Monster` nor `Creature`. This function is a boolean, so it lands as
///   `Creature`; that is *not* vanilla's category, merely the safe direction
///   (`Misc` also never natural-despawns). Recorded here rather than papered
///   over, because it is the one species in the roster this predicate cannot
///   represent, and it is the argument for #221's category unification.
///
/// A species outside this list is still treated as a persistent `Creature`,
/// which is the safe direction: it will not be despawned out from under a
/// player. The failure mode this list has is therefore under-listing (a monster
/// that never despawns), not over-listing.
///
/// This is still a name list, and a name list still ages — see #455 for why the
/// *goal* half took the structural route instead. What keeps this one honest is
/// `every_rostered_monster_is_categorised_hostile` below, which drives the
/// roster's own species set through it rather than restating the names.
pub(super) fn is_hostile_species(entity_type: &ResourceKey) -> bool {
    matches!(
        entity_type.path(),
        // Zombie family — `EntityTypes.java:1090, 534, 345, 1116, 1126`.
        "zombie"
            | "husk"
            | "drowned"
            | "zombie_villager"
            | "zombified_piglin"
            // Skeleton family — `:844, 931, 1058, 238, 736`.
            | "skeleton"
            | "stray"
            | "wither_skeleton"
            | "bogged"
            | "parched"
            // `:315`, `:903`, `:265`.
            | "creeper"
            | "spider"
            | "cave_spider"
            // `:513`, `:359` — both `MONSTER` despite being water-bound.
            | "guardian"
            | "elder_guardian"
            // `:473` (bare-`Mob` attributes, `MONSTER` category), `:231`, `:368`.
            | "ghast"
            | "blaze"
            | "enderman"
            // `EntityTypes.java:1039` and `:775` — both
            // `EntityType.Builder.of(…, MobCategory.MONSTER)`. Raiders, so they are
            // *also* spawned by a raid, but their registration category is what this
            // function answers and it is `MONSTER` like any other hostile.
            | "witch"
            | "pillager"
    )
}

/// Whether `entity_type` can be leashed at all — vanilla `Mob.canBeLeashed()`'s
/// real default, `!(this instanceof Enemy)`
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/Mob.java:1292-1294`):
/// every species tagged `Enemy` (`Monster`'s whole hierarchy, plus `Ghast`,
/// `Phantom` and `Shulker`, which implement it directly) refuses a lead, and
/// every other `Mob` accepts one — **not a curated allowlist**, which is
/// what an earlier draft of this issue assumed ("its own small vanilla
/// table"). [`is_hostile_species`] already tracks exactly the `Enemy` set
/// for every species this sim spawns today — checked per species against
/// its own class hierarchy, not assumed from the name overlap — so this is
/// a thin wrapper rather than a second table that could drift from it.
///
/// Vanilla layers per-species exceptions on both sides of that default:
/// `TamableAnimal` forces it back to `true` (redundant here, since none of
/// wolf/cat/parrot/the horse family are `Enemy` anyway), several water
/// creatures force it to `false`, and a few `Enemy`-hierarchy species
/// (hoglin, zoglin, the undead horse/camel variants) force it back to
/// `true`. **None of those exceptions apply to any species this sim spawns
/// today** — no water creature, hoglin, zoglin or undead mount is modelled
/// yet. Add an exception table here, not a rewrite of this function, the
/// day one is.
pub(super) fn is_leashable_species(entity_type: &ResourceKey) -> bool {
    !is_hostile_species(entity_type)
}

/// The species a given species flees, i.e. the `avoidClass` of each vanilla
/// `AvoidEntityGoal` registration. This is **perception data, not a goal set** —
/// it answers "is that thing a threat to me", which is what
/// [`MobController::avoid_threat`] needs; assembling the goals themselves is
/// the roster's job (plan units B1/B4), not this feed's.
///
/// Deliberately only the registrations that exist in 26.2 for species this sim
/// can currently spawn. An unknown species yields an empty slice, so
/// `AvoidEntityGoal` stays correctly inert for it rather than silently fleeing
/// everything.
pub(super) fn avoided_species(species: &str) -> &'static [&'static str] {
    match species {
        // `monster/Creeper.java:67-68` — two separate goals, one per class.
        "creeper" => &["ocelot", "cat"],
        // `monster/skeleton/AbstractSkeleton.java:79`, inherited by every
        // skeleton variant.
        "skeleton" | "stray" | "wither_skeleton" | "bogged" => &["wolf"],
        // `monster/spider/Spider.java:59`. Vanilla additionally requires
        // `!armadillo.isScared()`; nothing here models an armadillo's scared
        // state, so that filter is a disclosed omission rather than a silent
        // one — it can only make a spider flee slightly more often.
        "spider" | "cave_spider" => &["armadillo"],
        _ => &[],
    }
}

/// The item paths in each species' vanilla food tag — what `TemptGoal` follows
/// a player for.
///
/// **Every entry is transcribed from the jar's own tag JSON**, not from memory,
/// which matters more than it sounds: older Minecraft versions used a *single*
/// item per species, and a from-memory list ("carrot for pig, seeds for
/// chicken") is wrong for 26.2 in two places — `pig_food` is three items and
/// `chicken_food` is six. Files, all under
/// `.cache/mc/26.2/src/data/minecraft/tags/item/`:
///
/// | tag | file | values |
/// |---|---|---|
/// | cow | `cow_food.json` | `wheat` |
/// | sheep | `sheep_food.json` | `wheat` |
/// | pig | `pig_food.json` | `carrot`, `potato`, `beetroot` |
/// | chicken | `chicken_food.json` | `wheat_seeds`, `melon_seeds`, `pumpkin_seeds`, `beetroot_seeds`, `torchflower_seeds`, `pitcher_pod` |
/// | rabbit | `rabbit_food.json` | `carrot`, `golden_carrot`, `dandelion` |
/// | cat | `cat_food.json` | `cod`, `salmon` |
///
/// The cat is the one row here that is not a farm animal: `Cat.CatTemptGoal`
/// (`animal/feline/Cat.java:106`) is constructed with the very same
/// `#cat_food` tag `tame_mechanism`/`breeding_food` already use for it, so
/// this row and those two agree by construction rather than by luck. Contrast
/// [`breeding_food`]'s own doc comment, which names the wolf as the species
/// where tempt and tame/breed *diverge* — the cat is the ordinary case, the
/// wolf is the exception.
///
/// **This is an interim table and should be replaced, not extended.** Roster
/// unit B2 owns a *generated* item-tag table following the
/// `collision_shapes`/`hardness` generate-or-assert + `LODESTONE_REGEN=1`
/// pattern; the `damage_types` extraction is the closest existing precedent for
/// pulling tags out of datapack JSON. When that lands, this function's body
/// becomes a lookup into it and nothing else changes — the plumbing above and
/// below it is already in terms of a real held item.
///
/// Matched on the resource-key *path*, so a namespace other than `minecraft:`
/// would also match. Harmless today (nothing loads datapacks) and the generated
/// table will carry full keys.
pub(super) fn tempt_food(species: &str) -> &'static [&'static str] {
    match species {
        // `AbstractCow` covers both, and they share `cow_food`.
        "cow" | "mooshroom" => &["wheat"],
        "sheep" => &["wheat"],
        "pig" => &["carrot", "potato", "beetroot"],
        "chicken" => &[
            "wheat_seeds",
            "melon_seeds",
            "pumpkin_seeds",
            "beetroot_seeds",
            "torchflower_seeds",
            "pitcher_pod",
        ],
        "rabbit" => &["carrot", "golden_carrot", "dandelion"],
        // `#cat_food` — the same two items `tame_mechanism("cat")` and
        // `breeding_food("cat")` already use. `lodestone_entity`'s
        // `roster::passive::CAT` installs the goal this feeds
        // (`Cat.CatTemptGoal(CAT_FOOD)`); without this arm the row was
        // installed on a real mob but never reached by real perception.
        "cat" => &["cod", "salmon"],
        // Not a mistake: most species have no food tag, and an empty slice
        // keeps `TemptGoal` correctly inert for them rather than tempting them
        // with anything.
        _ => &[],
    }
}

/// What feeding this species does — vanilla's `Animal.isFood` tag, read out of
/// `.cache/mc/26.2/src/data/minecraft/tags/item/*_food.json`.
///
/// # This is not [`tempt_food`], and the two must not be merged
///
/// They coincide for the five species [`tempt_food`] covers, because those
/// species' `TemptGoal` is constructed with the very same tag. They diverge
/// wherever vanilla constructs the tempt goal with a *different* predicate, and
/// the wolf is the case that matters: `Wolf.isFood` is `#wolf_food` (meat and
/// fish), while a **bone** is what tames it — and a bone is in neither tag.
/// Merging the tables would make a bone a breeding item and meat a taming item,
/// both wrong.
///
/// # `hay_block` is deliberately absent from the horse's row
///
/// The horse family has no `isFood`-driven love at all — see
/// [`horse_breeding_items`] — so its row here is empty rather than
/// `#horse_food`. Filling it in from the tag would make wheat a breeding item
/// for a horse, which it is not.
pub(super) fn breeding_food(species: &str) -> &'static [&'static str] {
    match species {
        // `AbstractCow` covers both, and they share `#cow_food`.
        "cow" | "mooshroom" => &["wheat"],
        "sheep" => &["wheat"],
        "pig" => &["carrot", "potato", "beetroot"],
        "chicken" => &[
            "wheat_seeds",
            "melon_seeds",
            "pumpkin_seeds",
            "beetroot_seeds",
            "torchflower_seeds",
            "pitcher_pod",
        ],
        "rabbit" => &["carrot", "golden_carrot", "dandelion"],
        // `#wolf_food` = `#meat` + the five fish + rabbit stew.
        "wolf" => &[
            "beef",
            "chicken",
            "cooked_beef",
            "cooked_chicken",
            "cooked_mutton",
            "cooked_porkchop",
            "cooked_rabbit",
            "mutton",
            "porkchop",
            "rabbit",
            "rotten_flesh",
            "cod",
            "cooked_cod",
            "salmon",
            "cooked_salmon",
            "tropical_fish",
            "pufferfish",
            "rabbit_stew",
        ],
        // `#cat_food`. Note it is *raw* fish only, unlike `#wolf_food`.
        "cat" => &["cod", "salmon"],
        // `Parrot.isFood` returns a literal `false`, and `Parrot.canMate`
        // returns `false` with `getBreedOffspring` returning `null`. A parrot
        // cannot be bred at all — an empty row, not an unfinished one.
        "parrot" => &[],
        _ => &[],
    }
}

/// How this species is tamed, or `None` if it is not tameable.
///
/// # Four species, four mechanisms — not one with different constants
///
/// | species | trigger | roll |
/// |---|---|---|
/// | wolf | a **bone** (`Items.BONE`), and only while not angry | `random.nextInt(3) == 0` |
/// | cat | `#cat_food` (raw cod or salmon) | `random.nextInt(3) == 0` |
/// | parrot | `#parrot_food` (six seeds) | `random.nextInt(10) == 0` |
/// | horse family | **being ridden**, not fed | `random.nextInt(getMaxTemper()) < getTemper()` |
///
/// The wolf's trigger item is in none of its own food tags, the parrot's odds
/// differ by a factor of three, and the horse's roll is not a constant chance at
/// all — it is a function of a persisted `Temper` counter that *feeding* raises
/// (see [`horse_temper_gain`]). A single "tame chance per species" table would be
/// wrong about three of the four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TameMechanism {
    /// `tryToTame`: consume the item, then tame on `random.nextInt(one_in) == 0`.
    /// Both the wolf's and the cat's shape, differing only in the item set.
    FoodRoll {
        /// Item paths that trigger an attempt. Anything else is `Pass`.
        items: &'static [&'static str],
        /// The bound of vanilla's `nextInt`; success is a draw of exactly `0`.
        one_in: i32,
        /// Whether a successful tame also orders the animal to sit — vanilla's
        /// `setOrderedToSit(true)` inside `tryToTame`. **The parrot's does
        /// not**, and it is the only one of the three that omits it.
        sit_on_success: bool,
    },
    /// The horse family: feeding raises `Temper` and never rolls; the roll
    /// happens in `RunAroundLikeCrazyGoal` while a player is riding.
    Temper {
        /// `AbstractHorse.getMaxTemper()`.
        max_temper: i32,
    },
}

/// The taming mechanism for a species path, or `None` for a species that cannot
/// be tamed.
///
/// The `donkey`/`mule`/`skeleton_horse`/`zombie_horse` rows are `AbstractHorse`
/// subclasses and inherit its temper mechanism unchanged; `getMaxTemper` is
/// overridden by none of them.
pub(super) fn tame_mechanism(species: &str) -> Option<TameMechanism> {
    match species {
        "wolf" => Some(TameMechanism::FoodRoll {
            // `Wolf.mobInteract`: `itemStack.is(Items.BONE)`, a single item and
            // not a tag.
            items: &["bone"],
            one_in: 3,
            sit_on_success: true,
        }),
        "cat" => Some(TameMechanism::FoodRoll {
            items: &["cod", "salmon"],
            one_in: 3,
            sit_on_success: true,
        }),
        "parrot" => Some(TameMechanism::FoodRoll {
            items: &[
                "wheat_seeds",
                "melon_seeds",
                "pumpkin_seeds",
                "beetroot_seeds",
                "torchflower_seeds",
                "pitcher_pod",
            ],
            one_in: 10,
            sit_on_success: false,
        }),
        "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" => {
            Some(TameMechanism::Temper { max_temper: 100 })
        }
        _ => None,
    }
}

/// `AbstractHorse.handleEating`'s temper gain for one item, or `0` for anything
/// that is not horse food.
///
/// # Read the table, not the tag
///
/// `#horse_food` and this function disagree twice, in both directions, and both
/// disagreements are vanilla's:
///
/// * **`hay_block` is horse food and grants no temper at all** (`heal = 20.0F`,
///   `ageUp = 180`, and `temper` left at its `0` initialiser). Deriving temper
///   from the tag would let a stack of hay bales tame a horse.
/// * **`red_mushroom` grants 3 temper and is *not* in `#horse_food`**, so
///   `AbstractHorse.isFood` is false for it while `handleEating` still accepts
///   it. Deriving the accepted set from `isFood` would drop it.
pub(super) fn horse_temper_gain(item: &str) -> i32 {
    match item {
        "wheat" | "sugar" | "apple" | "carrot" | "red_mushroom" => 3,
        "golden_carrot" => 5,
        "golden_apple" | "enchanted_golden_apple" => 10,
        // Including `hay_block`, which heals 20 and tempers nothing.
        _ => 0,
    }
}

/// The items that put a **tamed** horse in love — the horse family's breeding
/// trigger, which is not its food tag.
///
/// `AbstractHorse.handleEating` calls `setInLove` in exactly two of its arms,
/// `Items.GOLDEN_CARROT` and `Items.GOLDEN_APPLE`/`ENCHANTED_GOLDEN_APPLE`, and
/// each is additionally gated on `isTamed() && getAge() == 0 && !isInLove()`.
/// Wheat, sugar, apples, carrots and hay all feed a horse and none of them breeds
/// it, so the ordinary [`breeding_food`] route cannot express this species and its
/// row there is empty.
pub(super) fn horse_breeding_items(item: &str) -> bool {
    matches!(
        item,
        "golden_carrot" | "golden_apple" | "enchanted_golden_apple"
    )
}

/// `Wolf.feed`/`Cat.feed`'s heal amount for a tame pet fed its food item.
///
/// `Wolf.mobInteract` passes `2.0F, 2.0F` and `Cat.mobInteract` passes
/// `1.0F, 1.0F` — the first argument is the heal. Not one constant: a wolf
/// recovers twice as fast per fish as a cat does.
pub(super) fn tame_feed_heal(species: &str) -> f32 {
    match species {
        "wolf" => 2.0,
        "cat" => 1.0,
        _ => 0.0,
    }
}

/// `LivingEntity.getExperienceReward`'s base value for one species —
/// `Mob.getBaseExperienceReward`, which returns the per-class `xpReward` field.
///
/// # Where each number comes from
///
/// The `xpReward` assignment in the species' own class, read out of the 26.2
/// decompile. Class defaults do the rest, and there are only two that matter:
/// `Monster`'s constructor sets `5`, and `Animal.getBaseExperienceReward` overrides the
/// field entirely with `1 + random.nextInt(3)` — so **an animal's reward is a roll of
/// 1..=3, not a constant**, and a table of flat numbers would be wrong for every
/// passive mob.
///
/// | value | species | source class |
/// |---|---|---|
/// | `0` | `creaking`, `snow_golem`, `iron_golem` | never assign `xpReward`, so it keeps `Mob`'s `0` |
/// | `1..=3` | every `Animal` | `Animal.getBaseExperienceReward` |
/// | `3` | `vex`, `endermite` | own assignment |
/// | `5` | every other `Monster` | `Monster`'s constructor |
/// | `10` | `blaze`, `guardian`, `elder_guardian`, `evoker`, `breeze` | own assignment (`elder_guardian` inherits `Guardian`'s) |
/// | `20` | `ravager`, `piglin_brute` | own assignment |
/// | `50` | `wither` | own assignment |
///
/// # What is deliberately not modelled
///
/// * **The equipment bonus.** `Mob.getBaseExperienceReward` adds `1 + nextInt(3)` per
///   droppable equipped item. Nothing in this sim equips a mob, so the sum is always
///   over an empty set.
/// * **`Zombie`'s baby ×2.5.** It is real and it is unreachable: `dropExperience`
///   requires `shouldDropExperience()`, which is `!isBaby()`, so no baby ever reaches
///   the multiplier on death. Modelling it would be modelling dead code.
/// * **`Slime`/`MagmaCube`, whose reward is their size.** This sim has no slime size,
///   and neither species is in the roster.
///
/// # The fallback, stated rather than silent
///
/// A species in neither table gets `0`. For an unlisted *monster* that is
/// conservative-but-wrong (it should be 5) and for an unlisted *animal* it is likewise
/// low; both under-award rather than over-award, and both are fixed by adding the name
/// here. The lists cover every species `lodestone_entity::ai::roster` registers, which
/// is what this sim can spawn.
pub(super) fn mob_experience_reward(entity_type: &ResourceKey, rng: &mut SpawnRng) -> i32 {
    match entity_type.path() {
        // `Mob`'s untouched `xpReward` — these classes never assign it.
        "creaking" | "snow_golem" | "iron_golem" => 0,
        "vex" | "endermite" => 3,
        "blaze" | "guardian" | "elder_guardian" | "evoker" | "breeze" => 10,
        "ravager" | "piglin_brute" => 20,
        "wither" => 50,
        // `Animal.getBaseExperienceReward` and `AgeableWaterCreature`'s identical
        // override: a roll, not a constant.
        "bee" | "camel" | "cat" | "chicken" | "cow" | "donkey" | "fox" | "goat" | "horse"
        | "llama" | "mooshroom" | "mule" | "ocelot" | "panda" | "pig" | "polar_bear"
        | "rabbit" | "sheep" | "sniffer" | "squid" | "strider" | "turtle" | "wolf" => {
            1 + rng.next_int(3)
        }
        // `Monster`'s constructor. Asked *after* the overrides above so a species with
        // its own assignment is not flattened to the class default.
        path if is_hostile_species(&super::hostile_probe(path)) => 5,
        _ => 0,
    }
}

/// Gates on [`is_hostile_species`] (issue #457), which stood at the original
/// eight names long after the roster grew to twenty-seven species.
#[cfg(test)]
mod hostility_category_tests {
    use super::*;
    use lodestone_entity::ai::roster;
    use super::super::{ChunkWorld, DEMO_SPECIES, MobSim, seed_demo_mobs};
    use crate::mob_spawn::MobCategory;
    use lodestone_model::Vec3;
    use std::str::FromStr;

    /// Every species any roster family claims, paired with the `MobCategory`
    /// vanilla registers it under in `EntityTypes.java`.
    ///
    /// **This is an independent statement of the answer, not a restatement of
    /// the code under test**: the values were read from the jar's
    /// `EntityType.Builder.of(X::new, MobCategory.…)` registrations, which is a
    /// different file and a different mechanism from the `matches!` under test.
    /// `true` here means `MONSTER`.
    ///
    /// Note the two rows a "derive it from the attribute template" heuristic
    /// gets wrong, and which are the reason this table exists rather than a
    /// clever predicate: **`ghast` is `MONSTER`** despite a bare-`Mob`
    /// attribute builder with no `attack_damage`, and **`snow_golem` is
    /// `MISC`** — neither `Monster` nor `Creature`, so it is `false` here for
    /// want of a third state (see [`is_hostile_species`]'s own doc).
    const JAR_CATEGORY: &[(&str, bool)] = &[
        // hostile_melee
        ("zombie", true),
        ("husk", true),
        ("zombie_villager", true),
        ("drowned", true),
        ("creeper", true),
        ("spider", true),
        ("cave_spider", true),
        ("skeleton", true),
        ("stray", true),
        ("bogged", true),
        ("parched", true),
        ("wither_skeleton", true),
        // ranged
        ("blaze", true),
        ("snow_golem", false), // MobCategory.MISC — see above
        // passive
        ("cow", false),
        ("mooshroom", false),
        ("sheep", false),
        ("pig", false),
        ("chicken", false),
        ("rabbit", false),
        // `EntityType.Builder.of(Cat::new, MobCategory.CREATURE)` and
        // `EntityType.Builder.of(Parrot::new, MobCategory.CREATURE)`
        // (`EntityTypes.java`) — issue #229's cat and parrot, neither ever
        // `MONSTER` however their taming interaction goes.
        ("cat", false),
        ("parrot", false),
        // neutral — all four are non-`MONSTER` *or* conditionally hostile;
        // enderman and zombified_piglin are registered `MONSTER`, bee and wolf
        // `CREATURE`. Hostility-on-sight is a separate axis the roster owns.
        ("enderman", true),
        ("zombified_piglin", true),
        ("bee", false),
        ("wolf", false),
        // specialist
        ("guardian", true),
        ("elder_guardian", true),
        ("ghast", true), // MobCategory.MONSTER, bare-`Mob` attributes
        // Both `EntityType.Builder.of(…, MobCategory.MONSTER)` — a raider is still a
        // monster by registration, whatever a raid does with it.
        ("witch", true),
        ("pillager", true),
    ];

    fn key(path: &str) -> ResourceKey {
        ResourceKey::from_str(&format!("minecraft:{path}")).expect("valid key")
    }

    /// The coverage half: **every species the roster claims must appear in
    /// [`JAR_CATEGORY`]**, so adding a species to a family without deciding its
    /// spawn category fails here instead of silently defaulting to `Creature`.
    ///
    /// This is the assertion the old eight-name list could never have had, and
    /// it is driven from `roster::*::SPECIES` — the same lists `goals_for`
    /// dispatches on — rather than from a copy of them.
    #[test]
    fn every_rostered_species_has_a_decided_category() {
        let all: Vec<&str> = roster::hostile_melee::SPECIES
            .iter()
            .chain(roster::ranged::SPECIES)
            .chain(roster::passive::SPECIES)
            .chain(roster::neutral::SPECIES)
            .chain(roster::specialist::SPECIES)
            .copied()
            .collect();
        assert!(
            !all.is_empty(),
            "the roster exported no species, so this gate measured nothing"
        );

        let undecided: Vec<&str> = all
            .iter()
            .copied()
            .filter(|s| !JAR_CATEGORY.iter().any(|(name, _)| name == s))
            .collect();
        assert!(
            undecided.is_empty(),
            "these rostered species have no jar-cited spawn category, so they \
             silently fall through to persistent Creature (#457): {undecided:?}"
        );
    }

    /// [`DEMO_SPECIES`]'s two invariants (issue #457): every entry is claimed
    /// by a roster family, and the first six span all five families.
    ///
    /// The first half is what stops a typo or a plausible-but-unrostered name
    /// (`"villager"`, `"bat"`) from spawning a mob that renders fine and
    /// exercises nothing — `roster::registrations_for` answers `FALLBACK` for
    /// an unclaimed species rather than failing, so nothing else would notice.
    ///
    /// The second half is the one that matters for the issue: seeding six
    /// mobs of six *different monsters* would still leave four families at zero
    /// pixels, which is the defect, not the fix.
    #[test]
    fn demo_species_are_all_rostered_and_span_every_family() {
        use lodestone_entity::ai::roster;

        assert!(
            !DEMO_SPECIES.is_empty(),
            "an empty list would make both checks below vacuous"
        );

        let unclaimed: Vec<&str> = DEMO_SPECIES
            .iter()
            .copied()
            .filter(|s| roster::is_fallback(roster::registrations_for(s)))
            .collect();
        assert!(
            unclaimed.is_empty(),
            "these DEMO_SPECIES entries are claimed by no roster family, so they \
             spawn with FALLBACK goals and demonstrate nothing: {unclaimed:?}"
        );

        // `mob_count` in `lodestone-shell/src/net.rs`. Stated here as the
        // expectation this list is ordered against; if production changes it,
        // the ordering argument in `DEMO_SPECIES`' doc needs revisiting.
        const PRODUCTION_COUNT: usize = 6;
        let families: [(&str, &[&str]); 5] = [
            ("hostile_melee", roster::hostile_melee::SPECIES),
            ("ranged", roster::ranged::SPECIES),
            ("passive", roster::passive::SPECIES),
            ("neutral", roster::neutral::SPECIES),
            ("specialist", roster::specialist::SPECIES),
        ];
        let first_six = &DEMO_SPECIES[..PRODUCTION_COUNT.min(DEMO_SPECIES.len())];
        let unreached: Vec<&str> = families
            .iter()
            .filter(|(_, members)| !first_six.iter().any(|s| members.contains(s)))
            .map(|(name, _)| *name)
            .collect();
        assert!(
            unreached.is_empty(),
            "a default singleplayer world seeds {PRODUCTION_COUNT} mobs, and these \
             roster families are not among them — so their goal tables still reach \
             zero pixels, which is exactly the #457 defect: {unreached:?}"
        );
    }

    /// The seeder really produces those species — not merely that the constant
    /// lists them.
    ///
    /// Drives [`seed_demo_mobs`] itself (the function `MobHandle::reseed` calls
    /// in production) rather than restating the loop, and reads the entity types
    /// back off the resulting sim's snapshots. The assertion that matters is
    /// **`> 1` distinct types**: a seeder that still hardcoded one species would
    /// produce exactly one and pass any "mobs exist" check.
    #[test]
    fn the_seeder_spawns_more_than_one_species() {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -12..=12 {
            for z in -12..=12 {
                world.set_solid(x, -1, z, true);
            }
        }
        let world: &'static ChunkWorld = Box::leak(Box::new(world));
        let mut sim = MobSim::new(world);
        seed_demo_mobs(&mut sim, 0, 0, 6);

        let types: Vec<String> = sim
            .snapshots()
            .iter()
            .map(|s| s.entity_type.path().to_string())
            .collect();
        assert_eq!(types.len(), 6, "six requested mobs must all reach the sim");

        let mut distinct: Vec<&str> = types.iter().map(String::as_str).collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(
            distinct.len() > 1,
            "the seeder produced only {distinct:?} — a single-species ring is the \
             #457 defect, and 'mobs were spawned' passes for it"
        );
        assert_eq!(
            types[0], "zombie",
            "the first demo mob must stay a zombie: entity id 1000 is \
             deterministic and live_mob_sim.rs depends on it"
        );
        for want in ["cow", "wolf", "blaze", "guardian", "creeper"] {
            assert!(
                types.iter().any(|t| t == want),
                "a default world must contain a {want}; got {types:?}"
            );
        }
    }

    /// The value half: the predicate must agree with the jar for every row.
    #[test]
    fn hostility_matches_the_jar_registration_for_every_rostered_species() {
        let mut wrong = Vec::new();
        for &(path, want) in JAR_CATEGORY {
            let got = is_hostile_species(&key(path));
            if got != want {
                wrong.push(format!("{path}: want {want}, got {got}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "is_hostile_species disagrees with EntityTypes.java: {wrong:?}"
        );

        // Control: the predicate is capable of answering `false`, so the
        // agreement above is not "everything is hostile". A species with no
        // roster entry and no reason to be a monster must still be `false`.
        assert!(
            !is_hostile_species(&key("armadillo")),
            "an unlisted species must fall through to Creature — if this is \
             true, the predicate has stopped discriminating and the whole \
             table above passes vacuously"
        );
    }

    /// The category the predicate feeds must actually reach the spawned mob:
    /// [`MobSim::spawn_species`] is the production path, and a predicate whose
    /// answer never lands on a `SimMob` is the island shape this repo keeps
    /// paying for.
    #[test]
    fn the_decided_category_reaches_a_spawned_mob() {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -4..=4 {
            for z in -4..=4 {
                world.set_solid(x, -1, z, true);
            }
        }
        let world: &'static ChunkWorld = Box::leak(Box::new(world));
        let mut sim = MobSim::new(world);

        let pos = Vec3::new(0.5, 0.0, 0.5);
        let ghast = sim.spawn_species(key("ghast"), pos).category();
        let wolf = sim.spawn_species(key("wolf"), pos).category();

        assert_eq!(
            ghast,
            MobCategory::Monster,
            "a ghast is MobCategory.MONSTER (EntityTypes.java:473); if this is \
             Creature the widened list is not reaching spawn_species"
        );
        assert_eq!(
            wolf,
            MobCategory::Creature,
            "a wolf is MobCategory.CREATURE (EntityTypes.java:1073)"
        );
    }
}
