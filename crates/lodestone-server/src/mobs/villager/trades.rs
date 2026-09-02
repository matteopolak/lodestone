//! Villager trade generation — the generation-facing seam of villager trades.
//!
//! # What it is
//!
//! The static per-level trade table `MobSim::interact`'s `OpenTrade` gate
//! reads to decide whether a villager has any offers at all (`has_offers`),
//! and what `crate::villager_trade::VillagerTrades::for_profession` builds
//! the *persistent*, per-villager offer list from. Thin delegation onto
//! [`lodestone_data::villager_trades`], which carries
//! the actual per-profession, per-level table (all thirteen workstation
//! professions, transcribed from the real 26.2 registry data — see that
//! module's own doc for what is and is not ported, and
//! `docs/villager-trade-generation.md` for the full derivation).
//!
//! # How it works
//!
//! [`Profession::path`] gives the bare registry path
//! (`"farmer"`, `"librarian"`, ...) [`lodestone_data::villager_trades::pool_for`]
//! is keyed on — that crate sits below `lodestone-server` and cannot name
//! this crate's [`Profession`] type, so the path string is the seam.
//! [`offers_for`]/[`offers_up_to`] just forward to it; no trade data lives in
//! this file.
//!
//! This used to be a second, farmer-only copy of the trade table
//! (`crate::mobs::villager::trades`'s own eighteen hand-transcribed
//! records) — `lodestone_data::villager_trades` landed as a complete
//! superset and this module switched over, closing the gap
//! `docs/villager-trade-generation.md` named ("this supersedes
//! `crate::mobs::villager::trades`'s farmer-only table in scope, but does
//! not replace it in code" / "switching it to call
//! `lodestone_data::villager_trades::pool_for` instead is the brokered hunk
//! this doc names rather than performs"). `crate::villager_trade::VillagerTrades`
//! already made the identical switch independently; this module now agrees
//! with it rather than shadowing it with a thinner table.
//!
//! # How to change it
//!
//! Add or correct trade data in `lodestone_data::villager_trades`, not here
//! — this file has no table of its own to edit. If `open_merchant_screen`
//! ever needs the second-cost (`wants_b`) or `price_multiplier` fields
//! [`TradeRecord`] now carries, it can read them directly; today it still
//! only reads the five fields the old farmer-only shape had.

use super::Profession;

pub use lodestone_data::villager_trades::TradeRecord;

/// This level's own trade set: the pool [`offers_for`] draws from and how
/// many of it a real `TradeSet` picks. Forwards to
/// [`lodestone_data::villager_trades::pool_for`] keyed by
/// [`Profession::path`].
#[must_use]
pub fn pool_for(profession: Profession, level: i32) -> Option<(&'static [TradeRecord], usize)> {
    lodestone_data::villager_trades::pool_for(profession.path(), level)
}

/// The trades this level's own `TradeSet` contributes — **not** cumulative
/// across levels; see [`offers_up_to`] for the villager-facing accumulation.
/// Empty for any profession/level [`lodestone_data::villager_trades`] has not
/// ported, or for `level` outside `1..=5`.
#[must_use]
pub fn offers_for(profession: Profession, level: i32) -> Vec<TradeRecord> {
    match pool_for(profession, level) {
        Some((pool, amount)) => pool.iter().take(amount).copied().collect(),
        None => Vec::new(),
    }
}

/// Every trade a villager at `level` actually offers: the real update-trades
/// rule is called once per level-up and *adds* that level's trade set on top of
/// whatever the villager already offers (vanilla's own append rule adds new
/// offers without clearing old ones) — so a
/// level-3 farmer still offers its level-1 and level-2 trades alongside its
/// level-3 ones. This is that accumulation, computed fresh from the level
/// rather than modelling the incremental history (restocking/relisting is
/// a third piece not built here).
#[must_use]
pub fn offers_up_to(profession: Profession, level: i32) -> Vec<TradeRecord> {
    (1..=level.clamp(1, 5))
        .flat_map(|l| offers_for(profession, l))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The production-reader count that matters.** Before this module
    /// delegated, `crate::mobs::villager::trades` (what
    /// `crate::server::open_merchant_screen` actually reads) had zero
    /// production readers of `lodestone_data::villager_trades` — the new
    /// 13-profession table existed and nothing live ever reached it. A
    /// librarian is the discriminating species: the old farmer-only table
    /// here returned nothing for any non-farmer profession, so this must
    /// come back non-empty now, and it exercises a profession
    /// `lodestone_data::villager_trades` actually ported (unlike `None`/
    /// `Nitwit`, which are `None` by design on both sides).
    #[test]
    fn a_librarian_now_offers_real_trades_through_this_seam() {
        let offers = offers_for(Profession::Librarian, 1);
        assert!(
            !offers.is_empty(),
            "librarian level 1 must have real trades now that this module \
             delegates to lodestone_data::villager_trades — an empty result \
             here means the delegation did not take"
        );
    }

    /// Farmer level 1's first two trades are pairwise-distinct from the
    /// librarian check above and match the real jar record exactly, proving
    /// the delegation carries the *right* data, not just non-empty data.
    #[test]
    fn farmer_level_1_still_generates_the_real_wheat_for_emerald_trade() {
        let offers = offers_for(Profession::Farmer, 1);
        assert_eq!(offers.len(), 2, "trade_set/farmer/level_1.json's amount is 2");
        assert_eq!(offers[0].wants_item, "minecraft:wheat");
        assert_eq!(offers[0].wants_count, 20);
        assert_eq!(offers[0].gives_item, "minecraft:emerald");
        assert_eq!(offers[0].gives_count, 1);
        assert_eq!(offers[0].max_uses, 16);
        assert_eq!(offers[0].xp, 2);
        assert_eq!(offers[1].wants_item, "minecraft:potato");
    }

    /// A profession neither side ported (`None`/`Nitwit` have no job site
    /// and no trades by design) still returns nothing — the honest-gap
    /// behaviour must survive the delegation, not just the non-empty case.
    #[test]
    fn an_unported_profession_still_returns_no_offers_rather_than_invented_ones() {
        assert!(offers_for(Profession::Nitwit, 1).is_empty());
        assert!(offers_for(Profession::None, 1).is_empty());
    }

    /// Levels accumulate: a level-3 farmer offers level-1's trades too —
    /// unchanged behaviour, checked again post-delegation since
    /// `offers_up_to`'s own loop is still local code.
    #[test]
    fn trades_accumulate_across_levels() {
        let up_to_3 = offers_up_to(Profession::Farmer, 3);
        assert_eq!(up_to_3.len(), 6, "2 trades per level across 3 levels");
        assert!(up_to_3.iter().any(|t| t.gives_item == "minecraft:emerald" && t.wants_item == "minecraft:wheat"));
        assert!(up_to_3.iter().any(|t| t.gives_item == "minecraft:cookie"));
    }

    /// Every profession `lodestone_data::villager_trades` ported reaches a
    /// non-empty pool through this seam for at least one level — the
    /// broadest form of the production-reader-count claim: not just
    /// librarian, but the whole thirteen.
    #[test]
    fn every_ported_profession_is_reachable_through_this_seam() {
        let professions = [
            Profession::Armorer,
            Profession::Butcher,
            Profession::Cartographer,
            Profession::Cleric,
            Profession::Farmer,
            Profession::Fisherman,
            Profession::Fletcher,
            Profession::Leatherworker,
            Profession::Librarian,
            Profession::Mason,
            Profession::Shepherd,
            Profession::Toolsmith,
            Profession::Weaponsmith,
        ];
        for profession in professions {
            let any_level_has_offers = (1..=5).any(|level| !offers_for(profession, level).is_empty());
            assert!(
                any_level_has_offers,
                "{profession:?} must have real trades at some level through \
                 this seam"
            );
        }
    }
}
