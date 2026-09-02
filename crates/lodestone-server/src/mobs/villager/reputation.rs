//! Villager reputation, including Hero of the Village (issue #246) — built
//! on [`super::gossip::GossipContainer`] (issue #244).
//!
//! # What it is
//!
//! Two pure ports over [`super::gossip::GossipContainer`] and
//! [`crate::villager_trade::OfferState`]:
//!
//! - [`ReputationEventType`] plus [`apply_reputation_event`] — vanilla
//!   `Villager.onReputationEventFrom`, the *only* place gossip is actually
//!   written from an event rather than transferred between villagers. Every
//!   arm is transcribed from vanilla's own villager entity's four-branch `if`/`else if`
//!   chain for it.
//! - [`update_special_prices`] — vanilla `Villager.updateSpecialPrices`, the
//!   formula that turns a reputation score (and an optional Hero of the
//!   Village amplifier) into calls against
//!   [`crate::villager_trade::OfferState::add_special_price_diff`] — the hook
//!   that module's own doc names as built-but-uncalled for exactly this
//!   issue.
//!
//! # How it works
//!
//! [`ReputationEventType`] mirrors vanilla's own reputation-event-type registry
//! entries as a plain enum — this
//! crate has no runtime registry of open-ended event types, and the five
//! vanilla constants are the total set any caller here can produce.
//! [`apply_reputation_event`] applies **only** the four
//! [`GossipType`](super::gossip::GossipType) mutations `Villager`'s own
//! method performs; [`ReputationEventType::GolemKilled`] deliberately has no
//! arm — vanilla's own `onReputationEventFrom` has none either (golem death
//! reputation, if it exists at all in 26.2, is not this method's job), so an
//! arm here would be invented behaviour, not a port.
//!
//! [`update_special_prices`] takes reputation and the Hero of the Village
//! amplifier as **plain inputs** rather than reading a live player or a
//! `SimMob` — this module has no dependency on `SimMob`/`MobSim` or on
//! `super::gossip` (whichever wraps it as a percentage-through-`
//! GossipContainer::reputation` call is `crate::mobs::MobSim`'s job, not
//! this pure module's) matching `lodestone_server::villager_trade`'s own
//! "pure logic" boundary.
//!
//! # How to change it, and the gotchas
//!
//! - **The reputation discount and the Hero of the Village discount are
//!   independent and additive**, not a choice between the two — vanilla
//!   applies both `if` blocks unconditionally when their own guard is met
//!   (`reputation != 0`, `hasEffect(HERO_OF_THE_VILLAGE)`), so a player who
//!   is both reputable *and* carrying the effect gets both discounts summed
//!   into `special_price_diff`.
//! - **The reputation discount multiplies by each offer's own
//!   `price_multiplier`** (`OfferState::modified_cost_a_count`'s demand
//!   term), so two offers with different `price_multiplier` values (`0.05`
//!   vs `0.2` — see `lodestone_data::villager_trades`'s own doc) get
//!   different discounts from the *same* reputation score. A caller that
//!   hoists one flat discount out of the loop has silently ignored this.
//! - **The Hero of the Village discount is a `floor`, not a `round`, and is
//!   floored at `1`** (`Math.max(costReduction, 1)`) — a very cheap trade
//!   (`wants_count` small enough that `0.3 * wants_count < 1.0`) still gets
//!   at least `1` off, never `0`.
//! - **Both discounts are *negative* deltas** (`addToSpecialPriceDiff` is
//!   called with a negated magnitude in both branches) — a positive
//!   reputation makes prices cheaper, matching
//!   [`crate::villager_trade::OfferState::modified_cost_a_count`]'s formula
//!   adding `special_price_diff` directly to the base cost.
//! - **`special_price_diff` must be reset when a trading session ends**
//!   (vanilla's `resetSpecialPrices`, called from `stopTrading`) — not this
//!   module's job (`OfferState::reset_special_price_diff` already exists and
//!   is tested in `crate::villager_trade`); a caller that calls
//!   [`update_special_prices`] on every screen-open without ever resetting
//!   would accumulate discounts across sessions.
//!
//! # What is not built, named rather than silent
//!
//! - **No live per-villager `OfferState` list exists to call this against**
//!   yet — `crate::villager_trade`'s own doc already discloses that
//!   `SELECT_TRADE` is decoded and discarded and nothing calls
//!   `VillagerTrades::maybe_restock`. This module is ready the moment that
//!   lands; it does not perform the wiring itself (`crate::server`, off
//!   limits for this change).
//! - **Iron-golem aggression toward a low-reputation player** (named in
//!   issue #246's own body) has no evidenced vanilla mechanism in
//!   vanilla's own iron-golem entity
//!   tying golem targeting to `GossipContainer`/reputation at all — nothing
//!   there reads gossip. Inventing a golem-targeting rule with no jar
//!   citation would be exactly the kind of un-evidenced port this repo's own
//!   standards forbid; not built, and named here rather than silently
//!   dropped.

use crate::villager_trade::OfferState;

use super::gossip::{GossipContainer, GossipType};

/// `ReputationEventType` — vanilla's five registered event kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReputationEventType {
    ZombieVillagerCured,
    GolemKilled,
    VillagerHurt,
    VillagerKilled,
    Trade,
}

/// `Villager.onReputationEventFrom`: the four real gossip mutations a
/// reputation event causes. `source` is the entity the event is *about*
/// (vanilla's `Entity source` parameter — the player who cured/traded/hurt/
/// killed, not the villager whose ledger this is).
///
/// [`ReputationEventType::GolemKilled`] falls through with no mutation — see
/// this module's own doc for why that is a port of vanilla's own omission,
/// not a gap.
pub fn apply_reputation_event(
    gossip: &mut GossipContainer,
    event: ReputationEventType,
    source: uuid::Uuid,
) {
    match event {
        ReputationEventType::ZombieVillagerCured => {
            gossip.add(source, GossipType::MajorPositive, 20);
            gossip.add(source, GossipType::MinorPositive, 25);
        }
        ReputationEventType::Trade => {
            gossip.add(source, GossipType::Trading, 2);
        }
        ReputationEventType::VillagerHurt => {
            gossip.add(source, GossipType::MinorNegative, 25);
        }
        ReputationEventType::VillagerKilled => {
            gossip.add(source, GossipType::MajorNegative, 25);
        }
        ReputationEventType::GolemKilled => {}
    }
}

/// `Villager.updateSpecialPrices`: applies both the ordinary-reputation
/// discount and, when present, the Hero of the Village discount to every
/// offer in `offers`. See this module's own doc for why the two are
/// independent and additive, and why both deltas are negative (a discount).
///
/// `hero_of_the_village_amplifier` is `player.getEffect(HERO_OF_THE_VILLAGE).
/// getAmplifier()` when the player carries the effect, `None` otherwise —
/// this module reads no live effect state itself.
pub fn update_special_prices(
    offers: &mut [OfferState],
    reputation: i32,
    hero_of_the_village_amplifier: Option<u32>,
) {
    if reputation != 0 {
        for offer in offers.iter_mut() {
            let discount = (reputation as f32 * offer.record.price_multiplier).floor() as i32;
            offer.add_special_price_diff(-discount);
        }
    }
    if let Some(amplifier) = hero_of_the_village_amplifier {
        let modifier = 0.3 + 0.0625 * f64::from(amplifier);
        for offer in offers.iter_mut() {
            let cost_reduction = (modifier * f64::from(offer.record.wants_count)).floor() as i32;
            offer.add_special_price_diff(-cost_reduction.max(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_data::villager_trades::TradeRecord;

    fn uuid(byte: u8) -> uuid::Uuid {
        uuid::Uuid::from_bytes([byte; 16])
    }

    fn offer(wants_count: i32, price_multiplier: f32) -> OfferState {
        OfferState::new(TradeRecord {
            wants_item: "minecraft:emerald",
            wants_count,
            wants_b: None,
            gives_item: "minecraft:bread",
            gives_count: 1,
            max_uses: 12,
            xp: 1,
            price_multiplier,
        })
    }

    /// `ZombieVillagerCured` writes **two** gossip entries, not one — a
    /// literal transcription of `Villager.onReputationEventFrom`'s cured
    /// branch. Predicted exactly: `20 * major_positive.weight()(5) + 25 *
    /// minor_positive.weight()(1) = 100 + 25 = 125`.
    #[test]
    fn curing_a_zombie_villager_grants_both_the_major_and_minor_positive_entries() {
        let mut gossip = GossipContainer::new();
        apply_reputation_event(
            &mut gossip,
            ReputationEventType::ZombieVillagerCured,
            uuid(1),
        );
        assert_eq!(gossip.reputation(uuid(1)), 125);
    }

    /// `Trade` grants `2` `trading` gossip (weight `1`) — the ordinary
    /// demand-independent reputation gain repeated trading builds.
    #[test]
    fn trading_grants_two_trading_gossip() {
        let mut gossip = GossipContainer::new();
        apply_reputation_event(&mut gossip, ReputationEventType::Trade, uuid(1));
        assert_eq!(gossip.reputation(uuid(1)), 2);
    }

    /// `VillagerHurt`/`VillagerKilled` must move reputation in the
    /// **negative** direction, at their real predicted magnitudes: `25 *
    /// minor_negative.weight()(-1) = -25`, `25 * major_negative.weight()(-5)
    /// = -125`.
    #[test]
    fn hurting_and_killing_a_villager_lower_reputation_by_the_predicted_amounts() {
        let mut hurt = GossipContainer::new();
        apply_reputation_event(&mut hurt, ReputationEventType::VillagerHurt, uuid(1));
        assert_eq!(hurt.reputation(uuid(1)), -25);

        let mut killed = GossipContainer::new();
        apply_reputation_event(&mut killed, ReputationEventType::VillagerKilled, uuid(1));
        assert_eq!(killed.reputation(uuid(1)), -125);
    }

    /// Control: `GolemKilled` is a real, distinct variant but must mutate
    /// nothing — vanilla's own method has no arm for it.
    #[test]
    fn golem_killed_writes_no_gossip() {
        let mut gossip = GossipContainer::new();
        apply_reputation_event(&mut gossip, ReputationEventType::GolemKilled, uuid(1));
        assert!(gossip.is_empty());
    }

    /// `update_special_prices`'s reputation branch, predicted exactly at two
    /// different `price_multiplier` values so one offer's arithmetic cannot
    /// be copied onto the other and still pass: reputation `40`, multiplier
    /// `0.05` -> `floor(40 * 0.05) = 2`; multiplier `0.2` -> `floor(40 *
    /// 0.2) = 8`. Both deltas are negative (a discount).
    #[test]
    fn reputation_discount_scales_by_each_offers_own_price_multiplier() {
        let mut offers = [offer(10, 0.05), offer(10, 0.2)];
        update_special_prices(&mut offers, 40, None);
        assert_eq!(offers[0].special_price_diff, -2);
        assert_eq!(offers[1].special_price_diff, -8);
    }

    /// Zero reputation must add nothing at all — not a zero-magnitude call,
    /// literally no mutation (matching vanilla's own `if (reputation != 0)`
    /// guard, which this test would not distinguish from "adds exactly
    /// zero" if `special_price_diff` were not already asserted to start at
    /// zero either way — the discriminating check is that a *negative*
    /// reputation is not silently treated the same as zero).
    #[test]
    fn zero_reputation_adds_no_discount() {
        let mut offers = [offer(10, 0.2)];
        update_special_prices(&mut offers, 0, None);
        assert_eq!(offers[0].special_price_diff, 0);
    }

    /// A negative reputation must **raise** the price (a positive
    /// `special_price_diff`) — the mirror case a test that only ever tries
    /// positive reputation cannot see. `floor(-30 * 0.2) = floor(-6.0) =
    /// -6`, negated `-(-6) = 6`.
    #[test]
    fn negative_reputation_raises_the_price() {
        let mut offers = [offer(10, 0.2)];
        update_special_prices(&mut offers, -30, None);
        assert_eq!(offers[0].special_price_diff, 6);
    }

    /// Hero of the Village at amplifier `0` (`modifier = 0.3`): `wants_count
    /// = 10` -> `floor(0.3 * 10) = floor(3.0) = 3`, at least the `1` floor
    /// (not binding here). At amplifier `1` (`modifier = 0.3625`) on the
    /// same offer: `floor(3.625) = 3` — same integer result at a different
    /// amplifier is exactly why the next test picks a `wants_count` where
    /// the two amplifiers disagree.
    #[test]
    fn hero_of_the_village_discounts_by_the_predicted_floor() {
        let mut offers = [offer(10, 0.2)];
        update_special_prices(&mut offers, 0, Some(0));
        assert_eq!(offers[0].special_price_diff, -3);
    }

    /// A higher amplifier must produce a **larger** discount at a
    /// `wants_count` where the two floors actually differ: `wants_count =
    /// 20`. Amplifier `0` (`modifier 0.3`): `floor(6.0) = 6`. Amplifier `2`
    /// (`modifier 0.425`): `floor(8.5) = 8`. `8 != 6`, so this is a real
    /// discriminating pair, not two amplifiers that coincidentally floor to
    /// the same integer.
    #[test]
    fn a_higher_hero_amplifier_yields_a_larger_discount() {
        let mut low = [offer(20, 0.2)];
        update_special_prices(&mut low, 0, Some(0));
        assert_eq!(low[0].special_price_diff, -6);

        let mut high = [offer(20, 0.2)];
        update_special_prices(&mut high, 0, Some(2));
        assert_eq!(high[0].special_price_diff, -8);
    }

    /// The `max(cost_reduction, 1)` floor: a cheap trade (`wants_count =
    /// 1`) at amplifier `0` computes `floor(0.3 * 1) = 0`, which must still
    /// discount by `1`, not `0`.
    #[test]
    fn hero_of_the_village_never_discounts_by_less_than_one() {
        let mut offers = [offer(1, 0.2)];
        update_special_prices(&mut offers, 0, Some(0));
        assert_eq!(
            offers[0].special_price_diff, -1,
            "floor(0.3 * 1) = 0, but the discount must floor at 1, not 0"
        );
    }

    /// Reputation and Hero of the Village are additive, not exclusive — a
    /// player with both must get both deltas summed onto the same offer.
    #[test]
    fn reputation_and_hero_of_the_village_discounts_are_additive() {
        let mut offers = [offer(10, 0.2)];
        update_special_prices(&mut offers, 40, Some(0));
        // Reputation: floor(40 * 0.2) = 8, delta -8.
        // Hero: floor(0.3 * 10) = 3, delta -3.
        assert_eq!(offers[0].special_price_diff, -11);
    }
}
