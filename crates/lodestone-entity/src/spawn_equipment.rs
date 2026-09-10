//! Mob spawn equipment — vanilla's `Mob.populateDefaultEquipmentSlots` and the
//! per-species overrides that replace or extend it.
//!
//! # What it is
//!
//! Nothing in this crate modelled what a mob is *holding* or *wearing* at
//! spawn before this module existed: [`crate::equipment`] already turns an
//! `(EquipmentSlot, item id)` pair into real attribute modifiers, but nothing
//! ever produced that pair for a **mob** (only [`crate::equipment::player_combat_stats`]
//! had a caller, and only for a player's own inventory). That is the confirmed
//! blocker this module exists to remove: a drowned's `RangedAttackGoal` trident
//! builder ([`crate::ai::roster::ranged::trident_attack`]) has existed for a
//! while with zero producers of "is this drowned holding a trident", and
//! vanilla's generic armour-upgrade roll (`Mob.populateDefaultEquipmentSlots`)
//! had no caller for [`crate::equipment`]'s numbers to feed at all.
//!
//! # How it works
//!
//! [`populate_default_equipment_slots`] is one function per species, table-
//! shaped like [`crate::ai::roster`]'s per-species goal tables, transcribed
//! from each class's own `populateDefaultEquipmentSlots(RandomSource,
//! DifficultyInstance)` override:
//!
//! * **`Mob.populateDefaultEquipmentSlots`** (the fallback every unlisted
//!   species takes) — [`base_armor_roll`]. A `0.15 * specialMultiplier` chance
//!   of any armour at all, then an `armorType` in `0..=5` (`nextInt(3)` plus up
//!   to three `+1` bumps at 10.87% each), then walks
//!   `[Head, Chest, Legs, Feet]` in that order, stopping early after the first
//!   slot with a per-difficulty chance (`10%` Hard, `25%` otherwise) and never
//!   overwriting a slot that already holds something.
//! * **`Zombie.populateDefaultEquipmentSlots`** calls `super` (the roll above)
//!   then rolls a `1%`/`5%` (Hard) chance of an iron weapon: sword, spear or
//!   shovel at `1/6, 1/6, 4/6`. `Husk` and `ZombieVillager` declare no override
//!   of their own, so they share this arm.
//! * **`Drowned.populateDefaultEquipmentSlots`** does **not** call `super` — no
//!   armour roll at all, just a `10%` chance of a main-hand weapon, `10/16` of
//!   that a trident and the rest a fishing rod. This is the one this module
//!   exists for: `10% * 10/16 = 6.25%` of drowned spawns get a trident, which
//!   is what [`crate::ai::roster::ranged::trident_attack`]'s own `canUse` gate
//!   (via [`crate::ai::MobController::main_hand_item`]) now reads.
//! * **`AbstractSkeleton.populateDefaultEquipmentSlots`** calls `super` then
//!   sets a bow **unconditionally** — no roll. `Skeleton`, `Stray`, `Bogged`
//!   and `Parched` declare no override, so they share this arm.
//! * **`WitherSkeleton.populateDefaultEquipmentSlots`** does **not** call
//!   `super` — no armour roll, just an unconditional stone sword.
//! * **`Pillager.populateDefaultEquipmentSlots`** does **not** call `super`
//!   either — just an unconditional crossbow.
//!
//! # How to change it
//!
//! Add a species by reading its own `populateDefaultEquipmentSlots` override in
//! the decompiled 26.2 source: note whether it calls
//! `super` (meaning it gets [`base_armor_roll`] first) and transcribe whatever
//! it does after that in the same call order the roll functions read RNG in,
//! since a reordered pair of `next_f32`/`next_int` calls changes what a fixed
//! seed produces even though nothing here promises byte-identical RNG streams
//! with a real vanilla server (`SpawnRng` is a different generator).
//!
//! An unmodelled RNG-derived rarity is a known gap for the general roll:
//! `populateDefaultEquipmentEnchantments` (enchanted spawn gear) is not
//! transcribed at all — there is no enchantment model in this workspace, the
//! same disclosed gap [`crate::equipment`]'s own module doc names.
//!
//! # Configuration
//!
//! `special_multiplier` is [`DifficultyInstance::special_multiplier`]-shaped —
//! `0.0` below effective difficulty `2.0`, `1.0` above `4.0`, linear between —
//! and `hard` is `getDifficulty() == Difficulty.HARD` (**not** the same
//! predicate as `special_multiplier == 1.0`; a saturated Normal-difficulty
//! world can reach `special_multiplier` `1.0` while `hard` stays `false`,
//! which is exactly why [`base_armor_roll`] takes both rather than deriving one
//! from the other).
//!
//! [`DifficultyInstance::special_multiplier`]: ../../lodestone_server/regional_difficulty/struct.DifficultyInstance.html

use crate::equipment::EquipmentSlot;
use lodestone_data::entity_type::EntityType;
use lodestone_data::item::Item;

/// The two `RandomSource` operations this module needs. A trait rather than a
/// concrete type so this crate stays free of any particular RNG algorithm —
/// see [`crate::ai::MobController`]'s identical `next_f32`/`next_i32` shape,
/// which this deliberately mirrors so a caller already threading one RNG
/// through goal code has no second generator to invent.
pub trait EquipRandom {
    /// A uniform random `f32` in `[0, 1)` (vanilla's `random.nextFloat`).
    fn next_f32(&mut self) -> f32;
    /// A uniform random `i32` in `[0, bound)` (vanilla's `random.nextInt`).
    fn next_int(&mut self, bound: i32) -> i32;
}

/// What a mob spawns holding and wearing: one built-in item (or none) per
/// [`EquipmentSlot`].
///
/// Default equipment is a closed, generated table, so carrying a `String`
/// here would let an arbitrary item name masquerade as a result of the spawn
/// roll. Dynamic item names still enter at the player-combat boundary; this
/// producer has no such extension point. [`iter`](Self::iter) therefore feeds
/// validated [`Item`] values directly to [`crate::equipment::apply_equipment`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EquipmentSlots {
    pub main_hand: Option<Item>,
    pub off_hand: Option<Item>,
    pub head: Option<Item>,
    pub chest: Option<Item>,
    pub legs: Option<Item>,
    pub feet: Option<Item>,
}

impl EquipmentSlots {
    /// The item id in `slot`, if any.
    #[must_use]
    pub fn get(&self, slot: EquipmentSlot) -> Option<Item> {
        match slot {
            EquipmentSlot::MainHand => self.main_hand,
            EquipmentSlot::OffHand => self.off_hand,
            EquipmentSlot::Head => self.head,
            EquipmentSlot::Chest => self.chest,
            EquipmentSlot::Legs => self.legs,
            EquipmentSlot::Feet => self.feet,
        }
    }

    /// Sets `slot`, replacing whatever was there. Vanilla's own roll never
    /// calls this on an occupied slot (`itemStack.isEmpty()` guards it), but
    /// this setter does not itself enforce that — callers that want "only if
    /// empty" check [`get`](Self::get) first, as [`base_armor_roll`] does.
    pub fn set(&mut self, slot: EquipmentSlot, item: Item) {
        let item = Some(item);
        match slot {
            EquipmentSlot::MainHand => self.main_hand = item,
            EquipmentSlot::OffHand => self.off_hand = item,
            EquipmentSlot::Head => self.head = item,
            EquipmentSlot::Chest => self.chest = item,
            EquipmentSlot::Legs => self.legs = item,
            EquipmentSlot::Feet => self.feet = item,
        }
    }

    /// Every occupied slot, as `(slot, built-in item)` pairs — the exact shape
    /// [`crate::equipment::apply_equipment`] takes.
    pub fn iter(&self) -> impl Iterator<Item = (EquipmentSlot, Item)> + '_ {
        [
            (EquipmentSlot::MainHand, self.main_hand),
            (EquipmentSlot::OffHand, self.off_hand),
            (EquipmentSlot::Head, self.head),
            (EquipmentSlot::Chest, self.chest),
            (EquipmentSlot::Legs, self.legs),
            (EquipmentSlot::Feet, self.feet),
        ]
        .into_iter()
        .filter_map(|(slot, item)| item.map(|item| (slot, item)))
    }
}

/// Vanilla's own equipment population order.
const EQUIPMENT_POPULATION_ORDER: [EquipmentSlot; 4] = [
    EquipmentSlot::Head,
    EquipmentSlot::Chest,
    EquipmentSlot::Legs,
    EquipmentSlot::Feet,
];

/// `Mob.getEquipmentForSlot` — the six-tier armour ladder in vanilla's own
/// (non-monotonic-in-defence) order: leather, copper, golden, chainmail, iron,
/// diamond. `None` outside `0..=5`, which [`base_armor_roll`]'s own `armor_type`
/// never produces (`nextInt(3)` plus at most three `+1`s tops out at `5`), and
/// for the two non-armour slots.
#[must_use]
fn equipment_for_slot(slot: EquipmentSlot, armor_type: i32) -> Option<Item> {
    Some(match (armor_type, slot) {
        (0, EquipmentSlot::Head) => Item::LeatherHelmet,
        (0, EquipmentSlot::Chest) => Item::LeatherChestplate,
        (0, EquipmentSlot::Legs) => Item::LeatherLeggings,
        (0, EquipmentSlot::Feet) => Item::LeatherBoots,
        (1, EquipmentSlot::Head) => Item::CopperHelmet,
        (1, EquipmentSlot::Chest) => Item::CopperChestplate,
        (1, EquipmentSlot::Legs) => Item::CopperLeggings,
        (1, EquipmentSlot::Feet) => Item::CopperBoots,
        (2, EquipmentSlot::Head) => Item::GoldenHelmet,
        (2, EquipmentSlot::Chest) => Item::GoldenChestplate,
        (2, EquipmentSlot::Legs) => Item::GoldenLeggings,
        (2, EquipmentSlot::Feet) => Item::GoldenBoots,
        (3, EquipmentSlot::Head) => Item::ChainmailHelmet,
        (3, EquipmentSlot::Chest) => Item::ChainmailChestplate,
        (3, EquipmentSlot::Legs) => Item::ChainmailLeggings,
        (3, EquipmentSlot::Feet) => Item::ChainmailBoots,
        (4, EquipmentSlot::Head) => Item::IronHelmet,
        (4, EquipmentSlot::Chest) => Item::IronChestplate,
        (4, EquipmentSlot::Legs) => Item::IronLeggings,
        (4, EquipmentSlot::Feet) => Item::IronBoots,
        (5, EquipmentSlot::Head) => Item::DiamondHelmet,
        (5, EquipmentSlot::Chest) => Item::DiamondChestplate,
        (5, EquipmentSlot::Legs) => Item::DiamondLeggings,
        (5, EquipmentSlot::Feet) => Item::DiamondBoots,
        (_, EquipmentSlot::MainHand | EquipmentSlot::OffHand) | (..=-1 | 6.., _) => return None,
    })
}

/// `Mob.populateDefaultEquipmentSlots` — the generic armour-upgrade roll every
/// species takes unless its own override skips calling `super` (`Drowned`,
/// `WitherSkeleton`, `Pillager`, all transcribed as *not* calling this).
///
/// Never overwrites a slot [`EquipmentSlots::get`] already reports occupied,
/// matching `itemStack.isEmpty()`'s guard — so calling this before a
/// species-specific weapon roll (as [`populate_default_equipment_slots`] does
/// for the zombie and skeleton families, exactly mirroring `super` running
/// first) cannot clobber a main-hand item a species-specific arm sets *before*
/// calling this, though nothing here does that today.
pub fn base_armor_roll(
    rng: &mut impl EquipRandom,
    special_multiplier: f32,
    hard: bool,
    slots: &mut EquipmentSlots,
) {
    if rng.next_f32() >= 0.15 * special_multiplier {
        return;
    }
    let mut armor_type = rng.next_int(3);
    for _ in 0..3 {
        if rng.next_f32() < 0.1087 {
            armor_type += 1;
        }
    }
    let partial_chance = if hard { 0.1 } else { 0.25 };
    let mut first = true;
    for slot in EQUIPMENT_POPULATION_ORDER {
        if !first && rng.next_f32() < partial_chance {
            break;
        }
        first = false;
        if slots.get(slot).is_none()
            && let Some(item) = equipment_for_slot(slot, armor_type)
        {
            slots.set(slot, item);
        }
    }
}

/// Vanilla's `populateDefaultEquipmentSlots(RandomSource, DifficultyInstance)`
/// for one generated entity type. An unlisted species takes
/// [`base_armor_roll`] alone, which is the honest default: every species this
/// module does not name individually still extends `Mob` and inherits its
/// generic roll, exactly as the jar does.
///
/// See the module doc's per-species table for which classes call `super`
/// (and so take [`base_armor_roll`] first) and which fully override it.
#[must_use]
pub fn populate_default_equipment_slots(
    species: EntityType,
    rng: &mut impl EquipRandom,
    special_multiplier: f32,
    hard: bool,
) -> EquipmentSlots {
    let mut slots = EquipmentSlots::default();
    match species {
        // `Zombie.populateDefaultEquipmentSlots`: `super` first, then a
        // 1%/5%(Hard) chance of an iron weapon at 1/6 sword, 1/6 spear, 4/6
        // shovel. `Husk` and `ZombieVillager` declare no override, so they
        // share this arm; `Drowned` overrides fully and gets its own arm.
        EntityType::Zombie | EntityType::Husk | EntityType::ZombieVillager => {
            base_armor_roll(rng, special_multiplier, hard, &mut slots);
            let weapon_chance = if hard { 0.05 } else { 0.01 };
            if rng.next_f32() < weapon_chance {
                let item = match rng.next_int(6) {
                    0 => Item::IronSword,
                    1 => Item::IronSpear,
                    _ => Item::IronShovel,
                };
                slots.set(EquipmentSlot::MainHand, item);
            }
        }
        // `Drowned.populateDefaultEquipmentSlots` does not call `super` — no
        // armour, just the trident/fishing-rod roll: 10% * 10/16 = 6.25% of
        // spawns get a trident, 10% * 6/16 = 3.75% a fishing rod, 90% neither.
        EntityType::Drowned => {
            if rng.next_f32() > 0.9 {
                let item = if rng.next_int(16) < 10 {
                    Item::Trident
                } else {
                    Item::FishingRod
                };
                slots.set(EquipmentSlot::MainHand, item);
            }
        }
        // `AbstractSkeleton.populateDefaultEquipmentSlots`: `super` then an
        // unconditional bow. `Stray`, `Bogged` and `Parched` share this arm.
        EntityType::Skeleton | EntityType::Stray | EntityType::Bogged | EntityType::Parched => {
            base_armor_roll(rng, special_multiplier, hard, &mut slots);
            slots.set(EquipmentSlot::MainHand, Item::Bow);
        }
        // `WitherSkeleton.populateDefaultEquipmentSlots`: no `super`, just an
        // unconditional stone sword.
        EntityType::WitherSkeleton => {
            slots.set(EquipmentSlot::MainHand, Item::StoneSword);
        }
        // `Pillager.populateDefaultEquipmentSlots`: no `super`, just an
        // unconditional crossbow.
        EntityType::Pillager => {
            slots.set(EquipmentSlot::MainHand, Item::Crossbow);
        }
        // Every other species: the generic roll alone, the honest default for
        // a class that declares no override at all.
        _ => base_armor_roll(rng, special_multiplier, hard, &mut slots),
    }
    slots
}

/// Populates the inherited equipment for an entity type supplied by an
/// extension rather than the generated built-in registry.
///
/// A custom entity has no generated variant on which to select one of the
/// built-in species overrides above, but it still enters the same generic
/// mob path as an unlisted built-in type. Keeping that fallback as a separate,
/// string-free function preserves the extension seam without pretending a
/// custom key is a built-in [`EntityType`].
#[must_use]
pub fn populate_default_equipment_slots_for_extension(
    rng: &mut impl EquipRandom,
    special_multiplier: f32,
    hard: bool,
) -> EquipmentSlots {
    let mut slots = EquipmentSlots::default();
    base_armor_roll(rng, special_multiplier, hard, &mut slots);
    slots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str) -> Item {
        Item::from_name(path).expect("test item is in the built-in registry")
    }

    /// A scripted [`EquipRandom`]: a fixed sequence of floats and ints, so a
    /// test can drive a specific branch rather than hoping a real RNG lands on
    /// one. Panics on exhaustion rather than wrapping, so a test that
    /// over-consumes fails loudly instead of silently repeating.
    struct Script {
        floats: std::vec::IntoIter<f32>,
        ints: std::vec::IntoIter<i32>,
    }
    impl Script {
        fn new(floats: Vec<f32>, ints: Vec<i32>) -> Self {
            Self {
                floats: floats.into_iter(),
                ints: ints.into_iter(),
            }
        }
    }
    impl EquipRandom for Script {
        fn next_f32(&mut self) -> f32 {
            self.floats.next().expect("script exhausted (f32)")
        }
        fn next_int(&mut self, _bound: i32) -> i32 {
            self.ints.next().expect("script exhausted (int)")
        }
    }

    /// Below effective difficulty 2.0, `special_multiplier` is `0.0`, so
    /// `0.15 * 0.0 == 0.0` and the roll's own first draw (anything `>= 0.0`)
    /// always fails the `< 0.0` gate — armour must never appear regardless of
    /// what the rest of the script would produce, and a script with only one
    /// float value proves nothing after it was consumed.
    #[test]
    fn zero_special_multiplier_never_rolls_armor() {
        let mut rng = Script::new(vec![0.0], Vec::new());
        let mut slots = EquipmentSlots::default();
        base_armor_roll(&mut rng, 0.0, false, &mut slots);
        assert_eq!(slots, EquipmentSlots::default());
    }

    /// At `special_multiplier == 1.0` (`0.15` gate) with a `0.0` first draw the
    /// roll proceeds; `armor_type` starts at `nextInt(3) == 2` and gets zero
    /// bumps (three `0.99` draws, each `>= 0.1087`), landing on tier 2 —
    /// golden. All four slots fill because every `partial_chance` draw is
    /// `0.99`, past both the `0.25` (not-hard) and `0.1` (hard) thresholds —
    /// so `break` never fires and every empty slot gets golden gear.
    #[test]
    fn full_roll_fills_all_four_slots_with_the_predicted_tier() {
        let mut rng = Script::new(
            vec![0.0, 0.99, 0.99, 0.99, 0.99, 0.99, 0.99],
            vec![2],
        );
        let mut slots = EquipmentSlots::default();
        base_armor_roll(&mut rng, 1.0, false, &mut slots);
        assert_eq!(slots.head, Some(item("golden_helmet")));
        assert_eq!(slots.chest, Some(item("golden_chestplate")));
        assert_eq!(slots.legs, Some(item("golden_leggings")));
        assert_eq!(slots.feet, Some(item("golden_boots")));
    }

    /// The three `0.1087` bumps each fire, carrying `armor_type` from the
    /// `nextInt(3)` starting value up by three tiers — `0 -> 3`, chainmail —
    /// and the discriminating control against "the bumps do nothing": a
    /// script whose bumps all fail would instead land on tier `0`, leather,
    /// which every assertion below excludes.
    ///
    /// Draw order: gate, three bumps, then one `partial_chance` check per
    /// slot after the first (`Chest`, `Legs`, `Feet` — three more, all `0.99`
    /// so the walk never breaks early and every slot fills).
    #[test]
    fn three_upgrade_bumps_land_on_chainmail_not_leather() {
        let mut rng = Script::new(
            vec![0.0, 0.01, 0.01, 0.01, 0.99, 0.99, 0.99],
            vec![0],
        );
        let mut slots = EquipmentSlots::default();
        base_armor_roll(&mut rng, 1.0, false, &mut slots);
        assert_eq!(
            slots.head,
            Some(item("chainmail_helmet")),
            "three bumps from tier 0 must reach tier 3 (chainmail), not stay at 0 (leather)"
        );
    }

    /// The `partial_chance` break, discriminated at a draw **between** the two
    /// thresholds (Hard `0.1`, Normal `0.25`): `0.15` fails Hard's `< 0.1` (so
    /// the walk continues past `Chest`) but satisfies Normal's `< 0.25` (so it
    /// breaks there, and `Chest` is never set — the break happens before the
    /// slot is filled).
    ///
    /// Draw order for both scripts: gate, three bumps, then the `Chest` check
    /// at `0.15`; the Hard script carries two more `0.99`s to complete `Legs`
    /// and `Feet` without exhausting.
    #[test]
    fn hard_partial_chance_is_less_likely_to_stop_early_than_normals() {
        let mut hard_slots = EquipmentSlots::default();
        base_armor_roll(
            &mut Script::new(vec![0.0, 0.99, 0.99, 0.99, 0.15, 0.99, 0.99], vec![2]),
            1.0,
            true,
            &mut hard_slots,
        );
        assert!(hard_slots.head.is_some());
        assert!(
            hard_slots.chest.is_some(),
            "Hard's 0.1 partial chance must NOT stop at a 0.15 draw (0.15 is not < 0.1)"
        );

        let mut normal_slots = EquipmentSlots::default();
        base_armor_roll(
            &mut Script::new(vec![0.0, 0.99, 0.99, 0.99, 0.15], vec![2]),
            1.0,
            false,
            &mut normal_slots,
        );
        assert!(normal_slots.head.is_some());
        assert!(
            normal_slots.chest.is_none(),
            "Normal's 0.25 partial chance must stop at the identical 0.15 draw (0.15 < 0.25)"
        );
    }

    /// An already-occupied slot is never overwritten — the `itemStack.isEmpty()`
    /// guard, isolated from the rest of the roll. Draw order: gate, three
    /// bumps, three partial-chance checks (all `0.99`, so every slot is
    /// visited).
    #[test]
    fn an_occupied_slot_is_never_overwritten() {
        let mut rng = Script::new(
            vec![0.0, 0.99, 0.99, 0.99, 0.99, 0.99, 0.99],
            vec![5],
        );
        let mut slots = EquipmentSlots::default();
        slots.set(EquipmentSlot::Head, item("diamond_helmet"));
        base_armor_roll(&mut rng, 1.0, false, &mut slots);
        assert_eq!(slots.head, Some(item("diamond_helmet")));
    }

    /// The headline consumer: a drowned's trident roll. `> 0.9` then
    /// `< 10` of `16` — both hypotheses (weapon-at-all, and which weapon) are
    /// exercised, matching the module doc's `6.25%` figure.
    #[test]
    fn drowned_rolls_a_trident_on_the_documented_branch() {
        let mut rng = Script::new(vec![0.95], vec![5]);
        let slots =
            populate_default_equipment_slots(EntityType::Drowned, &mut rng, 1.0, false);
        assert_eq!(slots.main_hand, Some(item("trident")));
        assert!(slots.head.is_none(), "drowned's override must not call super");
    }

    /// The fishing-rod branch: still within the 10% weapon chance, but the
    /// `nextInt(16)` roll lands at or above 10.
    #[test]
    fn drowned_rolls_a_fishing_rod_on_the_sibling_branch() {
        let mut rng = Script::new(vec![0.95], vec![12]);
        let slots =
            populate_default_equipment_slots(EntityType::Drowned, &mut rng, 1.0, false);
        assert_eq!(slots.main_hand, Some(item("fishing_rod")));
    }

    /// The 90% common case: no weapon at all, and — the discriminating half —
    /// no armour either, because `Drowned` does not call `super`. A species
    /// that *did* call `super` (the zombie family) is the control.
    #[test]
    fn drowned_usually_holds_nothing_and_never_wears_armor() {
        let mut rng = Script::new(vec![0.5], Vec::new());
        let slots =
            populate_default_equipment_slots(EntityType::Drowned, &mut rng, 1.0, false);
        assert_eq!(slots, EquipmentSlots::default());
    }

    /// A skeleton always holds a bow, unconditionally — no roll consumed for
    /// the weapon itself, only for the inherited armour roll ahead of it.
    #[test]
    fn a_skeleton_always_holds_a_bow() {
        let mut rng = Script::new(vec![0.99], Vec::new());
        let slots =
            populate_default_equipment_slots(EntityType::Skeleton, &mut rng, 0.0, false);
        assert_eq!(slots.main_hand, Some(item("bow")));
    }

    /// A wither skeleton always holds a stone sword and never rolls armour —
    /// no `super` call, so a script with zero entries must still succeed.
    #[test]
    fn a_wither_skeleton_always_holds_a_stone_sword_with_no_armor_roll() {
        let mut rng = Script::new(Vec::new(), Vec::new());
        let slots = populate_default_equipment_slots(
            EntityType::WitherSkeleton,
            &mut rng,
            1.0,
            true,
        );
        assert_eq!(slots.main_hand, Some(item("stone_sword")));
        assert_eq!(slots.head, None);
    }

    /// A pillager always holds a crossbow, the same no-`super`, no-roll shape.
    #[test]
    fn a_pillager_always_holds_a_crossbow() {
        let mut rng = Script::new(Vec::new(), Vec::new());
        let slots = populate_default_equipment_slots(EntityType::Pillager, &mut rng, 1.0, false);
        assert_eq!(slots.main_hand, Some(item("crossbow")));
    }

    /// The zombie weapon roll's three-way split, each branch driven
    /// explicitly rather than inferred from one sample.
    #[test]
    fn zombie_weapon_roll_splits_one_sixth_one_sixth_four_sixths() {
        for (roll, expect) in [(0, "iron_sword"), (1, "iron_spear"), (2, "iron_shovel"), (5, "iron_shovel")] {
            let mut rng = Script::new(vec![0.0, 0.005], vec![roll]);
            let slots =
                populate_default_equipment_slots(EntityType::Zombie, &mut rng, 0.0, true);
            assert_eq!(
                slots.main_hand,
                Some(item(expect)),
                "roll {roll} should give {expect}"
            );
        }
    }

    /// A custom entity has no generated variant for a species-specific arm,
    /// but it still receives the inherited generic armour roll that an
    /// unlisted built-in type receives.
    #[test]
    fn extension_equipment_keeps_the_generic_armor_fallback() {
        let mut rng = Script::new(
            vec![0.0, 0.99, 0.99, 0.99, 0.99, 0.99, 0.99],
            vec![2],
        );
        let slots = populate_default_equipment_slots_for_extension(&mut rng, 1.0, false);
        assert_eq!(slots.head, Some(item("golden_helmet")));
        assert_eq!(slots.chest, Some(item("golden_chestplate")));
        assert_eq!(slots.legs, Some(item("golden_leggings")));
        assert_eq!(slots.feet, Some(item("golden_boots")));
    }

    /// `iter()` round-trips through [`crate::equipment::apply_equipment`],
    /// the actual consumer this module exists to feed — a diamond helmet
    /// resolved from the roll folds into real armour attribute value.
    #[test]
    fn iter_feeds_apply_equipment_and_produces_real_armor() {
        let mut slots = EquipmentSlots::default();
        slots.set(EquipmentSlot::Head, item("diamond_helmet"));
        slots.set(EquipmentSlot::MainHand, item("iron_sword"));
        let mut attrs = crate::attribute::AttributeMap::new();
        crate::equipment::apply_equipment(&mut attrs, slots.iter());
        let defenses = crate::equipment::defenses_from_attributes(&attrs);
        assert!((defenses.armor - 3.0).abs() < 1e-6, "diamond helmet is armor 3.0");
    }
}
