//! `minecraft:food`'s hunger gate, client side: whether the current hunger
//! level lets an item start being eaten at all.
//!
//! # What it is
//!
//! `lodestone_server::item_use::can_eat` implements vanilla's own can-eat
//! check verbatim
//! and is the server's real gate — nutrition, saturation and the rest of
//! `minecraft:food` stay server-authoritative in `lodestone_server::item_use`'s
//! `FOODS` table, which this crate cannot see (`pub(crate)`, a different
//! crate). What this module carries is the **one flag** a client-side
//! prediction needs to agree with that gate before it *looks* like eating
//! started: vanilla's own always-eat flag (its own `alwaysEdible` field),
//! per item.
//!
//! Without this, [`crate::consumable::consumable_for_item`] alone treats every
//! food as always eatable — it only answers "is this item consumable at
//! all", the question the animation *shape* needs, not "would the server
//! actually accept this use". A full-hunger player right-clicking a steak
//! gets `FAIL` server-side (nothing happens) while the client played the
//! whole bite animation and threw crumbs for it — the gap this module closes.
//!
//! # Why a second table rather than reading `lodestone_server`'s
//!
//! `lodestone_server::item_use::FOODS` is `pub(crate)`: a version-free crate
//! cannot depend on the server crate to begin with, and even if it could, the
//! table is not exported. `minecraft:food` is a *prototype* component (never
//! on the wire — see that module's own doc), so the record definition is the
//! only source, exactly as `lodestone_server::item_use::FOODS` and
//! `lodestone_game::consumable::CONSUMABLES` already transcribe it twice each
//! for their own disjoint fields. This is a third, disjoint slice of the same
//! 40-row source: only the `alwaysEdible` column,
//! because nutrition/saturation stay server-side and the consume duration is
//! `minecraft:consumable`'s column, already carried in `consumable.rs`.
//!
//! # How to change it
//!
//! Adding a food is one row in [`ALWAYS_EAT`], kept sorted for the binary
//! search — check vanilla's own `alwaysEdible` argument for the new item
//! (`false` unless it is a golden apple, honey bottle, chorus fruit or
//! suspicious stew; vanilla's own food-properties table has had exactly five `true` rows since food
//! components were introduced, matching [`tests::exactly_five_items_are_always_edible`]).
//! A drink (`milk_bucket`, `potion`, `ominous_bottle`) does not go here at
//! all — [`always_eat_for_food`] returning `None` for it is what tells the
//! shell's `ConsumeState::resolve` (`lodestone-shell/src/consume.rs`) no
//! hunger gate applies, and adding a row for one would incorrectly start
//! gating it on hunger.

/// Vanilla's own food-needed ceiling, and its own can-eat check's — the same number
/// `lodestone_server::food::MAX_FOOD` names server-side.
pub const MAX_FOOD: i32 = 20;

/// Whether `item` carries `minecraft:food` at all, and if so its
/// vanilla's own always-eat flag.
///
/// `None` means `item` is not `minecraft:food` — a drink, a tool, or
/// anything else — which the caller must read as "no hunger gate applies",
/// never as "cannot be eaten"; [`crate::consumable::consumable_for_item`] is
/// the question of whether an item can be used at all.
#[must_use]
pub fn always_eat_for_food(item: &str) -> Option<bool> {
    ALWAYS_EAT
        .binary_search_by_key(&item, |&(name, _)| name)
        .ok()
        .map(|index| ALWAYS_EAT[index].1)
}

/// Vanilla's own can-eat check — `abilities.invulnerable || canAlwaysEat ||
/// foodData.needsFood()`, where `needsFood()` is `foodLevel < 20`. Mirrors
/// `lodestone_server::item_use::can_eat` so a client prediction of "am I
/// about to eat" agrees with what the server will actually accept.
#[must_use]
pub fn can_eat(always_eat: bool, food_level: i32, invulnerable: bool) -> bool {
    invulnerable || always_eat || food_level < MAX_FOOD
}

/// Every `minecraft:food` item in 26.2 and its `alwaysEdible` flag,
/// sorted by id for [`always_eat_for_food`]'s
/// binary search. Cross-checked against `lodestone_server::item_use::FOODS`'s
/// 40 rows and its own five `can_always_eat: true` entries — the two tables
/// must have identical `true` sets or the client's prediction and the
/// server's real gate disagree about which foods bypass a full bar.
const ALWAYS_EAT: &[(&str, bool)] = &[
    ("minecraft:apple", false),
    ("minecraft:baked_potato", false),
    ("minecraft:beef", false),
    ("minecraft:beetroot", false),
    ("minecraft:beetroot_soup", false),
    ("minecraft:bread", false),
    ("minecraft:carrot", false),
    ("minecraft:chicken", false),
    ("minecraft:chorus_fruit", true),
    ("minecraft:cod", false),
    ("minecraft:cooked_beef", false),
    ("minecraft:cooked_chicken", false),
    ("minecraft:cooked_cod", false),
    ("minecraft:cooked_mutton", false),
    ("minecraft:cooked_porkchop", false),
    ("minecraft:cooked_rabbit", false),
    ("minecraft:cooked_salmon", false),
    ("minecraft:cookie", false),
    ("minecraft:dried_kelp", false),
    ("minecraft:enchanted_golden_apple", true),
    ("minecraft:glow_berries", false),
    ("minecraft:golden_apple", true),
    ("minecraft:golden_carrot", false),
    ("minecraft:honey_bottle", true),
    ("minecraft:melon_slice", false),
    ("minecraft:mushroom_stew", false),
    ("minecraft:mutton", false),
    ("minecraft:poisonous_potato", false),
    ("minecraft:porkchop", false),
    ("minecraft:potato", false),
    ("minecraft:pufferfish", false),
    ("minecraft:pumpkin_pie", false),
    ("minecraft:rabbit", false),
    ("minecraft:rabbit_stew", false),
    ("minecraft:rotten_flesh", false),
    ("minecraft:salmon", false),
    ("minecraft:spider_eye", false),
    ("minecraft:suspicious_stew", true),
    ("minecraft:sweet_berries", false),
    ("minecraft:tropical_fish", false),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_complete() {
        assert!(
            ALWAYS_EAT.windows(2).all(|w| w[0].0 < w[1].0),
            "ALWAYS_EAT must be sorted by id for the binary search"
        );
        assert_eq!(ALWAYS_EAT.len(), 40, "26.2 has 40 minecraft:food items");
    }

    #[test]
    fn exactly_five_items_are_always_edible() {
        let always: Vec<&str> = ALWAYS_EAT
            .iter()
            .filter(|(_, always)| *always)
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            always,
            vec![
                "minecraft:chorus_fruit",
                "minecraft:enchanted_golden_apple",
                "minecraft:golden_apple",
                "minecraft:honey_bottle",
                "minecraft:suspicious_stew",
            ]
        );
    }

    /// A drink (`minecraft:consumable` but not `minecraft:food`) must answer
    /// `None`, not `Some(false)` — the distinction [`can_eat`]'s caller
    /// depends on to know no hunger gate applies at all.
    #[test]
    fn a_drink_is_not_food() {
        assert_eq!(always_eat_for_food("minecraft:potion"), None);
        assert_eq!(always_eat_for_food("minecraft:milk_bucket"), None);
        assert_eq!(always_eat_for_food("minecraft:diamond_pickaxe"), None);
    }

    /// The discriminating pair named in this issue: a plain apple is refused
    /// at a full bar, a golden apple is not. Two plain foods would coincide
    /// on both hypotheses and prove nothing about the `can_always_eat` half.
    #[test]
    fn a_golden_apple_bypasses_a_full_bar_a_plain_apple_does_not() {
        let apple = always_eat_for_food("minecraft:apple").expect("apple is food");
        let golden = always_eat_for_food("minecraft:golden_apple").expect("golden apple is food");
        assert!(!apple, "a plain apple is not always-edible");
        assert!(golden, "a golden apple is always-edible");

        assert!(can_eat(apple, 19, false), "a hungry player may eat a plain apple");
        assert!(
            !can_eat(apple, MAX_FOOD, false),
            "a full, non-invulnerable player may not eat a plain apple"
        );
        assert!(
            can_eat(golden, MAX_FOOD, false),
            "a golden apple bypasses a full bar"
        );
        assert!(
            can_eat(apple, MAX_FOOD, true),
            "an invulnerable (creative/spectator) player may always eat"
        );
    }
}
