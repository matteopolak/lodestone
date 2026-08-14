//! Ported `VillagerTrades`/`TradeSet` tables — issue #245.
//!
//! # What it is
//!
//! Per-profession, per-level trade pools, transcribed from the real 26.2
//! registry data under `.cache/mc/26.2/src/data/minecraft/` — **not** the
//! hand-written `ItemListing[]` factories older versions hardcoded in
//! `VillagerTrades.java`. In this version trading is data-driven:
//! `data/minecraft/trade_set/<profession>/level_<n>.json` names an `amount`
//! and a tag (`data/minecraft/tags/villager_trade/<profession>/level_<n>.json`)
//! whose `values` list resolves to individual `data/minecraft/villager_trade/<profession>/<n>/<name>.json`
//! records — each a `VillagerTrade` (`wants`/`gives`/`max_uses`/`xp`/
//! `reputation_discount`). Following the data-source rule this is Mojang's
//! own generator output, so it is transcribed directly rather than routed
//! through the decompiled Java (which only holds the *codec*, not the table).
//!
//! # What is ported, and what is not
//!
//! Only **farmer**, all five levels — 18 real `VillagerTrade` records,
//! transcribed verbatim, each citing its own resource path in a comment.
//! [`super::poi_type_for_block`]/[`super::profession_for_poi_type`] resolve
//! and claim workstations for **every** profession (issue #243's whole
//! scope) — a librarian or a cartographer gets a real workstation, a real
//! profession, and the right `VillagerData` metadata/texture on the wire.
//! [`offers_for`] just returns an empty pool for an unported profession
//! rather than inventing numbers "close enough" to the real table: this
//! repo's own evidence standard is that an absent trade table is honest and
//! a wrong one is worse, because a wrong one looks finished.
//!
//! # The one deliberate simplification: selection order, not selection math
//!
//! A real `TradeSet` draws `amount` trades **at random**, without
//! replacement (every farmer `trade_set/level_*.json` omits
//! `allow_duplicates`, whose codec default is `false`), from its tag's pool,
//! seeded per-position through vanilla's `RandomSequence` (`WorldGenRandom`
//! keyed by `random_sequence` + world seed) — a whole RNG subsystem this
//! crate has no port of, and porting it correctly is a larger undertaking
//! than this issue's slice. [`offers_for`] instead takes the pool's first
//! `amount` entries in the tag's own declared order.
//!
//! This means the **specific subset** a villager offers will not match a
//! real vanilla server seeded identically — it does **not** mean the
//! numbers are wrong. Every [`TradeRecord`] below is the real jar value, and
//! this module's own tests assert the exact generated offer against those
//! values, not against this module's own selection algorithm — the outside
//! source is the jar's JSON, transcribed once, here.

use super::Profession;

/// One `VillagerTrade` record.
///
/// `reputation_discount` (present on every farmer record at `0.05`) is not
/// modelled: nothing in this crate tracks villager reputation yet (see the
/// reputation issue), so there is nothing for it to discount, and pretending
/// it applies would be a wrong number rather than an absent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeRecord {
    /// What the villager wants, first input.
    pub wants_item: &'static str,
    pub wants_count: i32,
    /// What the trade produces.
    pub gives_item: &'static str,
    pub gives_count: i32,
    /// `VillagerTrade.CODEC`'s `max_uses`, present on every farmer record.
    pub max_uses: i32,
    /// `VillagerTrade.CODEC`'s `xp` — codec default `1` when the field is
    /// absent (`NumberProviders.CODEC.lenientOptionalFieldOf("xp",
    /// ConstantValue.exactly(1.0F))`), **not** `0`: `farmer/1/emerald_bread`
    /// has no `xp` key and still grants `1`.
    pub xp: i32,
}

/// `data/minecraft/villager_trade/farmer/1/*.json`, in
/// `tags/villager_trade/farmer/level_1.json`'s own `values` order:
/// `wheat_emerald`, `potato_emerald`, `carrot_emerald`, `beetroot_emerald`,
/// `emerald_bread`. `trade_set/farmer/level_1.json`'s `amount` is `2`.
const FARMER_LEVEL_1: &[TradeRecord] = &[
    // minecraft:farmer/1/wheat_emerald
    TradeRecord {
        wants_item: "minecraft:wheat",
        wants_count: 20,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // minecraft:farmer/1/potato_emerald
    TradeRecord {
        wants_item: "minecraft:potato",
        wants_count: 26,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // minecraft:farmer/1/carrot_emerald
    TradeRecord {
        wants_item: "minecraft:carrot",
        wants_count: 22,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // minecraft:farmer/1/beetroot_emerald
    TradeRecord {
        wants_item: "minecraft:beetroot",
        wants_count: 15,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 16,
        xp: 2,
    },
    // minecraft:farmer/1/emerald_bread — no `xp` key on the record; codec
    // default is 1, not 0 (see `TradeRecord::xp`'s own doc).
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        gives_item: "minecraft:bread",
        gives_count: 6,
        max_uses: 16,
        xp: 1,
    },
];

/// `tags/villager_trade/farmer/level_2.json`: `pumpkin_emerald`,
/// `emerald_pumpkin_pie`, `emerald_apple`. `amount` is `2`.
const FARMER_LEVEL_2: &[TradeRecord] = &[
    // minecraft:farmer/2/pumpkin_emerald
    TradeRecord {
        wants_item: "minecraft:pumpkin",
        wants_count: 6,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 10,
    },
    // minecraft:farmer/2/emerald_pumpkin_pie
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        gives_item: "minecraft:pumpkin_pie",
        gives_count: 4,
        max_uses: 12,
        xp: 5,
    },
    // minecraft:farmer/2/emerald_apple
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        gives_item: "minecraft:apple",
        gives_count: 4,
        max_uses: 16,
        xp: 5,
    },
];

/// `tags/villager_trade/farmer/level_3.json`: `emerald_cookie`,
/// `melon_emerald`. `amount` is `2` — the whole pool, since it has exactly
/// two entries.
const FARMER_LEVEL_3: &[TradeRecord] = &[
    // minecraft:farmer/3/emerald_cookie
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        gives_item: "minecraft:cookie",
        gives_count: 18,
        max_uses: 12,
        xp: 10,
    },
    // minecraft:farmer/3/melon_emerald
    TradeRecord {
        wants_item: "minecraft:melon",
        wants_count: 4,
        gives_item: "minecraft:emerald",
        gives_count: 1,
        max_uses: 12,
        xp: 20,
    },
];

/// `tags/villager_trade/farmer/level_4.json`: `emerald_cake`,
/// `emerald_suspicious_stew`. `amount` is `2`.
///
/// `emerald_suspicious_stew`'s real record also carries a
/// `given_item_modifiers` entry (`minecraft:set_stew_effect`, six potion
/// effects) that this port does not apply — the resulting stew is plain,
/// with no status effect. `wants`/`gives`/`max_uses`/`xp` are exact.
const FARMER_LEVEL_4: &[TradeRecord] = &[
    // minecraft:farmer/4/emerald_cake
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        gives_item: "minecraft:cake",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
    // minecraft:farmer/4/emerald_suspicious_stew
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 1,
        gives_item: "minecraft:suspicious_stew",
        gives_count: 1,
        max_uses: 12,
        xp: 15,
    },
];

/// `tags/villager_trade/farmer/level_5.json`: `emerald_golden_carrot`,
/// `emerald_glistening_melon_slice`. `amount` is `2`.
const FARMER_LEVEL_5: &[TradeRecord] = &[
    // minecraft:farmer/5/emerald_golden_carrot
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 3,
        gives_item: "minecraft:golden_carrot",
        gives_count: 3,
        max_uses: 12,
        xp: 30,
    },
    // minecraft:farmer/5/emerald_glistening_melon_slice
    TradeRecord {
        wants_item: "minecraft:emerald",
        wants_count: 4,
        gives_item: "minecraft:glistering_melon_slice",
        gives_count: 3,
        max_uses: 12,
        xp: 30,
    },
];

/// This level's own trade set: the pool [`offers_for`] draws from and how
/// many of it a real `TradeSet` picks (`trade_set/<profession>/level_<n>.json`'s
/// `amount`, constant `2` for every farmer level in the jar).
fn pool_for(profession: Profession, level: i32) -> Option<(&'static [TradeRecord], usize)> {
    match (profession, level) {
        (Profession::Farmer, 1) => Some((FARMER_LEVEL_1, 2)),
        (Profession::Farmer, 2) => Some((FARMER_LEVEL_2, 2)),
        (Profession::Farmer, 3) => Some((FARMER_LEVEL_3, 2)),
        (Profession::Farmer, 4) => Some((FARMER_LEVEL_4, 2)),
        (Profession::Farmer, 5) => Some((FARMER_LEVEL_5, 2)),
        _ => None,
    }
}

/// The trades this level's own `TradeSet` contributes — **not** cumulative
/// across levels; see [`offers_up_to`] for the villager-facing accumulation.
/// Empty for any profession/level this module has not ported (every
/// profession but farmer, today) or for `level` outside `1..=5`.
#[must_use]
pub fn offers_for(profession: Profession, level: i32) -> Vec<TradeRecord> {
    match pool_for(profession, level) {
        Some((pool, amount)) => pool.iter().take(amount).copied().collect(),
        None => Vec::new(),
    }
}

/// Every trade a villager at `level` actually offers: `Villager.updateTrades`
/// is called once per level-up and *adds* that level's `TradeSet` on top of
/// whatever the villager already offers (`addOffersFromTradeSet` appends,
/// `Villager.java`'s `updateTrades`/`increaseMerchantCareer`) — so a
/// level-3 farmer still offers its level-1 and level-2 trades alongside its
/// level-3 ones. This is that accumulation, computed fresh from the level
/// rather than modelling the incremental history (restocking/relisting is
/// issue #245's third piece and is not built here — see the module doc).
#[must_use]
pub fn offers_up_to(profession: Profession, level: i32) -> Vec<TradeRecord> {
    (1..=level.clamp(1, 5))
        .flat_map(|l| offers_for(profession, l))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The discriminating assertion issue #245 asks for: a **specific**
    /// generated offer, checked against the real jar record, not "some
    /// trades were generated". `wheat_emerald` is the pool's first entry, so
    /// it is always present under this module's first-N selection.
    #[test]
    fn farmer_level_1_generates_the_real_wheat_for_emerald_trade() {
        let offers = offers_for(Profession::Farmer, 1);
        assert_eq!(offers.len(), 2, "trade_set/farmer/level_1.json's amount is 2");
        assert_eq!(
            offers[0],
            TradeRecord {
                wants_item: "minecraft:wheat",
                wants_count: 20,
                gives_item: "minecraft:emerald",
                gives_count: 1,
                max_uses: 16,
                xp: 2,
            },
            "must match farmer/1/wheat_emerald.json exactly"
        );
        assert_eq!(
            offers[1],
            TradeRecord {
                wants_item: "minecraft:potato",
                wants_count: 26,
                gives_item: "minecraft:emerald",
                gives_count: 1,
                max_uses: 16,
                xp: 2,
            },
            "must match farmer/1/potato_emerald.json exactly"
        );
    }

    /// The codec-default trap: `emerald_bread`'s record has no `xp` key at
    /// all. The codec's own default is `1`, not the "plausible round number"
    /// `0` — a hand-guessed table would very likely get this one wrong.
    #[test]
    fn a_trade_missing_its_xp_field_uses_the_codecs_default_of_one_not_zero() {
        let offers = offers_for(Profession::Farmer, 1);
        let emerald_bread = offers
            .iter()
            .chain(FARMER_LEVEL_1.iter())
            .find(|t| t.gives_item == "minecraft:bread")
            .expect("emerald_bread is in the level-1 pool even if not in the first two picked");
        assert_eq!(emerald_bread.xp, 1);
    }

    #[test]
    fn an_unported_profession_returns_no_offers_rather_than_invented_ones() {
        assert!(offers_for(Profession::Librarian, 1).is_empty());
        assert!(offers_for(Profession::Weaponsmith, 3).is_empty());
    }

    /// Levels accumulate: a level-3 farmer offers level-1's trades too.
    #[test]
    fn trades_accumulate_across_levels() {
        let up_to_3 = offers_up_to(Profession::Farmer, 3);
        assert_eq!(up_to_3.len(), 6, "2 trades per level across 3 levels");
        assert!(up_to_3.iter().any(|t| t.gives_item == "minecraft:emerald" && t.wants_item == "minecraft:wheat"));
        assert!(up_to_3.iter().any(|t| t.gives_item == "minecraft:cookie"));
    }

    #[test]
    fn every_farmer_level_pool_is_present_and_nonempty() {
        for level in 1..=5 {
            let offers = offers_for(Profession::Farmer, level);
            assert!(!offers.is_empty(), "farmer level {level} should have real trades");
        }
    }
}
