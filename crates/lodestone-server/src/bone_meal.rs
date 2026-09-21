//! Bone meal — the instant-growth right-click, `BoneMealItem::useOn`.
//!
//! # What was missing
//!
//! The *growth* half of this family has been live since crop, sapling and leaf
//! random ticks landed: [`crate::growth_tick`] holds the probability rules and
//! [`crate::random_tick`] drives them every tick. What did not exist was bone
//! meal — the word appeared in this crate only in the composter's *output* paths,
//! so the one item whose entire purpose is to grow a plant did nothing when a
//! player right-clicked with it.
//!
//! So this is the rule layer for one item, not a growth engine: three vanilla
//! methods per block family (`isValidBonemealTarget`, `isBonemealSuccess`,
//! `performBonemeal`), plus `BoneMealItem::growCrop`'s own consume-and-report
//! contract.
//!
//! # The per-family variation is the substance
//!
//! | family | `isValidBonemealTarget` | `isBonemealSuccess` | `performBonemeal` |
//! |---|---|---|---|
//! | `CropBlock` (wheat, carrots, potatoes) | `!isMaxAge` | `true`, no draw | `age += Mth.nextInt(random, 2, 5)`, clamped to 7 |
//! | `BeetrootBlock` | same | same | the same draw **divided by 3**, so `+0` or `+1`, clamped to 3 |
//! | `SaplingBlock` | inside build height | `nextFloat() < 0.45` | stage 0 → 1, else grow a tree |
//! | `GrassBlock` | the cell above is air | `true`, no draw | place up to 128 vegetation features |
//!
//! **The item is consumed even when the success roll fails.**
//! `BoneMealItem::growCrop` shrinks the stack outside the `isBonemealSuccess`
//! branch, so a sapling eats bone meal 55% of the time for nothing. That is
//! [`BoneMealOutcome::ConsumedNoChange`], and getting it wrong would give players
//! free bone meal.
//!
//! # The RNG draws, which are the specification
//!
//! Exactly one draw per successful *use*, and which draw depends on the family:
//!
//! * a crop draws `nextInt(4)` once (`Mth::nextInt(random, 2, 5)` is
//!   `nextInt(max - min + 1) + min`), and **nothing else** — `isBonemealSuccess`
//!   is a constant `true` with no draw at all;
//! * a sapling draws `nextFloat()` once for the 0.45 gate and, on a hit, **no
//!   further draw** — the stage-0 advance is a plain `cycle(STAGE)`;
//! * a bone meal that finds no valid target draws nothing.
//!
//! Beetroot is the one that looks like it should differ and does not: its
//! `getBonemealAgeIncrease` is `super.getBonemealAgeIncrease(level) / 3`, so it is
//! the *same single draw*, divided. `(nextInt(4) + 2) / 3` is `0` for one of the
//! four outcomes and `1` for the other three — a 3-in-4 chance of a single stage,
//! never two. [`beetroot_advances_by_zero_or_one_from_one_draw`] pins that
//! distribution.
//!
//! # Two named gaps, both because the growth they need does not exist here
//!
//! * **`GrassBlock::performBonemeal`** places vegetation *features*
//!   (`VegetationPlacements.GRASS_BONEMEAL`, plus the biome's own bone-meal
//!   features) across 128 attempts, and each attempt's offset walk and each
//!   feature placement draw from the same RNG. `lodestone-worldgen` has no feature
//!   placer, and a partial version — say, dropping one `short_grass` where the
//!   feature would have gone — would consume a *different* number of draws and so
//!   corrupt every later attempt in the same call. That is the "plausible world
//!   that is not vanilla's" failure mode, so this reports
//!   [`BoneMealOutcome::NotModelled`] and consumes nothing rather than inventing a
//!   sequence.
//! * **A stage-1 sapling** needs `TreeGrower::growTree`, the same missing feature
//!   placer, and [`crate::growth_tick`] already documents it as an uncloseable gap
//!   for the random-tick path. Same treatment here.
//!
//! `growWaterPlant` (seagrass and coral from bone meal on water) is out of scope
//! for the same reason plus a second: it needs biome tags this crate does not
//! carry.
//!
//! # How to change it
//!
//! Adding a family means adding an arm to [`apply_bone_meal`] and its own
//! predicate; the caller passes the clicked state and the state directly above
//! it, and nothing here touches the world. If the new family's `performBonemeal`
//! draws, transcribe the draw *count* first and assert it — the surrounding
//! outcome is easy to eyeball and the draw count is not.

use lodestone_data::block::Block;
use lodestone_data::block_properties::{
    BuiltinPropertyValue, Properties, PropertyKey, PropertyValue,
};
use lodestone_data::block_states::StateId;

use crate::mob_spawn::SpawnRng;

/// `SaplingBlock::isBonemealSuccess`'s threshold.
pub const SAPLING_SUCCESS_CHANCE: f32 = 0.45;

/// `CropBlock::getBonemealAgeIncrease`'s `Mth.nextInt(random, 2, 5)` bounds.
pub const CROP_AGE_INCREASE_MIN: u32 = 2;
/// The inclusive upper bound of the same expression.
pub const CROP_AGE_INCREASE_MAX: u32 = 5;

/// `minecraft:bone_meal`.
pub const BONE_MEAL: &str = "minecraft:bone_meal";

/// What one bone-meal right-click did, before any world mutation — the same
/// decide-then-apply split [`crate::hand_use`] uses, so the whole rule is testable
/// with no `ChunkSource` in scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoneMealOutcome {
    /// Not a bone-mealable block, or already at max age — vanilla's
    /// `InteractionResult.PASS`. Nothing is consumed and the caller should fall
    /// through to whatever a right-click would otherwise do.
    NotBonemealable,
    /// A valid target whose `isBonemealSuccess` roll failed: **one bone meal is
    /// consumed** and the block is unchanged. Only saplings can produce this.
    ConsumedNoChange,
    /// One bone meal is consumed and the block becomes `state`.
    Grew {
        /// The new canonical block state for the clicked position.
        state: StateId,
    },
    /// A valid vanilla target this crate cannot grow — see the module doc's two
    /// named gaps. Treated as `PASS`: nothing is consumed, because consuming an
    /// item for an effect we did not produce is worse than doing nothing.
    NotModelled {
        /// Which gap, for a log line or a test message.
        reason: &'static str,
    },
}

/// `true` for any block [`apply_bone_meal`] recognises as a bone-meal target at
/// all, including the two it cannot grow.
///
/// Cheap and total, so a caller can ask before doing any work — the same contract
/// [`crate::hand_use::is_hand_usable`] has.
#[must_use]
pub fn is_bonemealable(state: StateId) -> bool {
    crop_max_age(state).is_some() || is_sapling(state) || state.block() == Block::GrassBlock
}

/// Resolves one bone-meal right-click on `state`.
///
/// `above_state` is the block directly above the clicked one — the caller reads
/// it, because this function has no world access. It is used only by the grass
/// arm (`GrassBlock::isValidBonemealTarget` requires air above); pass anything for
/// the other families.
///
/// Draws from `rng` exactly as vanilla does — see the module doc's draw table. A
/// [`BoneMealOutcome::NotBonemealable`] result draws nothing at all.
#[must_use]
pub fn apply_bone_meal(state: StateId, above_state: StateId, rng: &mut SpawnRng) -> BoneMealOutcome {
    if let Some(max_age) = crop_max_age(state) {
        let age = property_u32(state, PropertyKey::Age);
        // `CropBlock::isValidBonemealTarget` — `!isMaxAge`.
        if age >= max_age {
            return BoneMealOutcome::NotBonemealable;
        }
        // `isBonemealSuccess` is a constant `true`: no draw here.
        let increase = crop_age_increase(state, rng);
        let new_age = max_age.min(age + increase);
        return BoneMealOutcome::Grew { state: with_u32_property(state, PropertyKey::Age, new_age) };
    }

    if is_sapling(state) {
        // `SaplingBlock::isValidBonemealTarget` is a build-height check on
        // `pos.above(minimumHeight)`, which is true everywhere a sapling can
        // actually be standing, so it is not modelled as a rejection.
        if rng.next_f32() >= SAPLING_SUCCESS_CHANCE {
            return BoneMealOutcome::ConsumedNoChange;
        }
        // `advanceTree`: stage 0 cycles to 1 with no further draw; stage 1 grows a
        // real tree, which needs a feature placer this crate has none of.
        return match property_u32(state, PropertyKey::Stage) {
            0 => BoneMealOutcome::Grew { state: with_u32_property(state, PropertyKey::Stage, 1) },
            _ => BoneMealOutcome::NotModelled {
                reason: "a stage-1 sapling needs TreeGrower::growTree, and no tree feature exists here",
            },
        };
    }

    if state.block() == Block::GrassBlock {
        // `GrassBlock::isValidBonemealTarget` — the cell above must be air.
        if !matches!(above_state.block(), Block::Air | Block::CaveAir | Block::VoidAir) {
            return BoneMealOutcome::NotBonemealable;
        }
        return BoneMealOutcome::NotModelled {
            reason: "GrassBlock::performBonemeal places vegetation features, and no feature placer exists here",
        };
    }

    BoneMealOutcome::NotBonemealable
}

fn crop_max_age(state: StateId) -> Option<u32> {
    match state.block() {
        Block::Wheat | Block::Carrots | Block::Potatoes => Some(7),
        Block::Beetroots => Some(3),
        _ => None,
    }
}

fn is_sapling(state: StateId) -> bool {
    matches!(
        state.block(),
        Block::OakSapling
            | Block::SpruceSapling
            | Block::BirchSapling
            | Block::JungleSapling
            | Block::AcaciaSapling
            | Block::CherrySapling
            | Block::DarkOakSapling
            | Block::PaleOakSapling
    )
}

fn property_u32(state: StateId, key: PropertyKey) -> u32 {
    Properties::from_state_id(state)
        .get(key)
        .and_then(PropertyValue::name)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0)
}

fn with_u32_property(state: StateId, key: PropertyKey, value: u32) -> StateId {
    let value = BuiltinPropertyValue::from_name(
        [
            "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13",
            "14", "15", "16", "17", "18", "19", "20", "21", "22", "23", "24", "25",
        ]
        .get(value as usize)
        .copied()
        .unwrap_or("0"),
    )
    .expect("generated numeric property value");
    let properties = Properties::from_state_id(state);
    let mut pairs = Vec::with_capacity(properties.len());
    for property in properties.iter() {
        if property.key() != key {
            pairs.push((property.key(), property.value()));
        }
    }
    pairs.push((key, PropertyValue::builtin(value)));
    Properties::try_from_pairs(&pairs)
        .ok()
        .and_then(|properties| Properties::state_for_block(state.block(), &properties))
        .expect("generated block-state property transition")
}

/// `getBonemealAgeIncrease` — `Mth.nextInt(random, 2, 5)`, and for beetroot that
/// same value divided by three.
///
/// **One draw either way.** `Mth::nextInt(random, min, max)` is
/// `nextInt(max - min + 1) + min`, so the draw is `nextInt(4)`.
#[must_use]
pub fn crop_age_increase(state: StateId, rng: &mut SpawnRng) -> u32 {
    let bound = (CROP_AGE_INCREASE_MAX - CROP_AGE_INCREASE_MIN + 1) as i32;
    let raw = CROP_AGE_INCREASE_MIN + rng.next_int(bound) as u32;
    if state.block() == Block::Beetroots {
        raw / 3
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One seed, so a test can build two *independent* generators — `SpawnRng` is
    /// not `Copy`, and a determinism or draw-count gate that reused one instance
    /// would be measuring memoisation rather than the count.
    const SEED: u64 = 0xB0_5EED_1234_9ABC;

    fn rng() -> SpawnRng {
        SpawnRng::new(SEED)
    }

    fn id(state: &str) -> StateId {
        StateId::from_state_str(state).expect("built-in test state")
    }

    /// Every recognised family, and a control that is not one.
    #[test]
    fn recognises_exactly_the_three_families() {
        assert!(is_bonemealable(id("minecraft:wheat[age=0]")));
        assert!(is_bonemealable(id("minecraft:carrots[age=3]")));
        assert!(is_bonemealable(id("minecraft:potatoes[age=3]")));
        assert!(is_bonemealable(id("minecraft:beetroots[age=1]")));
        assert!(is_bonemealable(id("minecraft:oak_sapling[stage=0]")));
        assert!(is_bonemealable(id("minecraft:grass_block[snowy=false]")));
        assert!(!is_bonemealable(id("minecraft:stone")));
        assert!(!is_bonemealable(id("minecraft:dirt")));
        assert!(!is_bonemealable(id("minecraft:oak_leaves[distance=1,persistent=false]")));
    }

    /// A wheat crop advances by 2..=5 in one step and never past its max age of
    /// 7. The bound is predicted from vanilla's `Mth.nextInt(random, 2, 5)` rather
    /// than observed: over many uses every one of `{2, 3, 4, 5}` must appear and
    /// nothing else.
    #[test]
    fn wheat_advances_by_two_to_five() {
        let mut r = rng();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..400 {
            match apply_bone_meal(id("minecraft:wheat[age=0]"), id("minecraft:air"), &mut r) {
                BoneMealOutcome::Grew { state } => {
                    let age = property_u32(state, PropertyKey::Age);
                    assert!((2..=5).contains(&age), "age {age} outside 2..=5");
                    seen.insert(age);
                }
                other => panic!("wheat at age 0 must grow, got {other:?}"),
            }
        }
        assert_eq!(
            seen.into_iter().collect::<Vec<_>>(),
            vec![2, 3, 4, 5],
            "all four increments must be reachable and no others"
        );
    }

    /// One use of bone meal on a crop draws **exactly one** value — vanilla's
    /// `isBonemealSuccess` is a constant `true` with no draw, so a port that
    /// "rolled for success" would draw two and shift every later stream.
    ///
    /// Measured against an independently constructed reference generator advanced
    /// by hand; the control below shows the equality really depends on the count.
    #[test]
    fn one_crop_use_draws_exactly_one_value() {
        let mut used = rng();
        for _ in 0..100 {
            let _ = apply_bone_meal(id("minecraft:wheat[age=0]"), id("minecraft:air"), &mut used);
        }
        let mut reference = rng();
        for _ in 0..100 {
            reference.next_int(4);
        }
        assert_eq!(
            reference.next_int(1_000_000),
            used.next_int(1_000_000),
            "100 crop uses must consume exactly 100 draws"
        );
    }

    /// Negative control for the count above.
    #[test]
    fn the_crop_draw_count_control_fails_at_one_draw_fewer() {
        let mut a = rng();
        let mut b = rng();
        for _ in 0..99 {
            a.next_int(4);
        }
        for _ in 0..100 {
            b.next_int(4);
        }
        assert_ne!(a.next_int(1_000_000), b.next_int(1_000_000));
    }

    /// The clamp: wheat at age 5 lands on 7 for every draw, and wheat at age 7 is
    /// not a target at all.
    #[test]
    fn wheat_clamps_at_max_age_and_refuses_when_already_there() {
        let mut r = rng();
        for _ in 0..50 {
            match apply_bone_meal(id("minecraft:wheat[age=5]"), id("minecraft:air"), &mut r) {
                BoneMealOutcome::Grew { state } => {
                    assert_eq!(property_u32(state, PropertyKey::Age), 7, "must clamp to the max age");
                }
                other => panic!("expected growth, got {other:?}"),
            }
        }
        assert_eq!(
            apply_bone_meal(id("minecraft:wheat[age=7]"), id("minecraft:air"), &mut r),
            BoneMealOutcome::NotBonemealable,
            "a fully grown crop is not a bone-meal target"
        );
    }

    /// Beetroot's override: the *same single draw*, divided by three, so the
    /// increase is `0` for one of the four outcomes and `1` for the other three —
    /// and never `2`. Asserted as a distribution over the exact draw space rather
    /// than as a direction.
    #[test]
    fn beetroot_advances_by_zero_or_one_from_one_draw() {
        let mut r = rng();
        let mut zero = 0usize;
        let mut one = 0usize;
        for _ in 0..4000 {
            match apply_bone_meal(id("minecraft:beetroots[age=0]"), id("minecraft:air"), &mut r) {
                BoneMealOutcome::Grew { state } => match property_u32(state, PropertyKey::Age) {
                    0 => zero += 1,
                    1 => one += 1,
                    other => panic!("beetroot cannot advance to age {other} from one use"),
                },
                other => panic!("expected growth, got {other:?}"),
            }
        }
        // (nextInt(4) + 2) / 3 is 0 only for the draw 0, so a quarter of uses.
        let ratio = zero as f64 / (zero + one) as f64;
        assert!(
            (ratio - 0.25).abs() < 0.05,
            "one in four uses must advance by zero; got {ratio} ({zero} of {})",
            zero + one
        );
        assert_eq!(zero + one, 4000);
    }

    /// Beetroot's max age is 3, not 7 — the clamp differs per family.
    #[test]
    fn beetroot_clamps_at_three() {
        let mut r = rng();
        for _ in 0..50 {
            match apply_bone_meal(id("minecraft:beetroots[age=3]"), id("minecraft:air"), &mut r) {
                BoneMealOutcome::NotBonemealable => {}
                other => panic!("a max-age beetroot is not a target, got {other:?}"),
            }
        }
        for _ in 0..50 {
            if let BoneMealOutcome::Grew { state } =
                apply_bone_meal(id("minecraft:beetroots[age=2]"), id("minecraft:air"), &mut r)
            {
                assert!(property_u32(state, PropertyKey::Age) <= 3);
            }
        }
    }

    /// The sapling gate: 45% of uses advance stage 0 to 1, the other 55% consume
    /// the bone meal for nothing. A magnitude assertion on the *rate*, since 0.45
    /// is the whole content of `isBonemealSuccess`.
    #[test]
    fn a_sapling_succeeds_forty_five_percent_of_the_time_and_eats_the_rest() {
        let mut r = rng();
        let mut grew = 0usize;
        let mut wasted = 0usize;
        for _ in 0..4000 {
            match apply_bone_meal(id("minecraft:oak_sapling[stage=0]"), id("minecraft:air"), &mut r) {
                BoneMealOutcome::Grew { state } => {
                    assert_eq!(property_u32(state, PropertyKey::Stage), 1);
                    grew += 1;
                }
                BoneMealOutcome::ConsumedNoChange => wasted += 1,
                other => panic!("unexpected {other:?}"),
            }
        }
        let rate = grew as f64 / (grew + wasted) as f64;
        assert!(
            (rate - 0.45).abs() < 0.03,
            "sapling success rate must be 0.45, got {rate} ({grew} of {})",
            grew + wasted
        );
    }

    /// A stage-1 sapling is a valid vanilla target we cannot grow, and it must
    /// **not** consume the item — the honest half of the named gap.
    #[test]
    fn a_stage_one_sapling_reports_the_tree_gap_without_consuming() {
        let mut r = rng();
        let mut not_modelled = 0usize;
        let mut wasted = 0usize;
        for _ in 0..2000 {
            match apply_bone_meal(id("minecraft:oak_sapling[stage=1]"), id("minecraft:air"), &mut r) {
                BoneMealOutcome::NotModelled { reason } => {
                    assert!(reason.contains("TreeGrower"));
                    not_modelled += 1;
                }
                // The 0.45 gate still runs first, so 55% of uses are consumed for
                // nothing exactly as vanilla's would be.
                BoneMealOutcome::ConsumedNoChange => wasted += 1,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(not_modelled > 0 && wasted > 0, "both paths must be reachable");
    }

    /// Grass block: air above is the target test, and the gap is reported without
    /// consuming. A non-air cell above is not a target at all — the control that
    /// proves the predicate is read.
    #[test]
    fn grass_block_reports_the_feature_gap_only_when_air_is_above() {
        let mut r = rng();
        match apply_bone_meal(id("minecraft:grass_block[snowy=false]"), id("minecraft:air"), &mut r) {
            BoneMealOutcome::NotModelled { reason } => assert!(reason.contains("feature")),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            apply_bone_meal(id("minecraft:grass_block[snowy=false]"), id("minecraft:stone"), &mut r),
            BoneMealOutcome::NotBonemealable,
            "grass with a block above it is not a bone-meal target"
        );
    }

    /// A non-target draws **nothing** — so a failed click cannot shift the stream
    /// for the next one.
    #[test]
    fn a_non_target_draws_no_rng() {
        let mut r = rng();
        assert_eq!(
            apply_bone_meal(id("minecraft:stone"), id("minecraft:air"), &mut r),
            BoneMealOutcome::NotBonemealable
        );
        assert_eq!(
            apply_bone_meal(id("minecraft:wheat[age=7]"), id("minecraft:air"), &mut r),
            BoneMealOutcome::NotBonemealable
        );
        let mut reference = rng();
        assert_eq!(reference.next_int(1_000_000), r.next_int(1_000_000));
    }

    /// Determinism: two independently seeded runs of the same script agree.
    #[test]
    fn two_independent_runs_from_one_seed_agree() {
        let build = || {
            let mut r = rng();
            let mut out = Vec::new();
            for _ in 0..100 {
                out.push(apply_bone_meal(id("minecraft:wheat[age=0]"), id("minecraft:air"), &mut r));
                out.push(apply_bone_meal(id("minecraft:oak_sapling[stage=0]"), id("minecraft:air"), &mut r));
            }
            out
        };
        assert_eq!(build(), build());
    }
}
