//! Villager merchant-offer purchase mechanics and restock cadence — issue
//! #245's "refresh" half.
//!
//! # What it is
//!
//! `lodestone_data::villager_trades` (issue #243/#245's data half) is a
//! static table: which trades a profession/level *can* offer. This module is
//! the missing dynamic half — how many uses a specific villager's specific
//! offer has left, how its price moves with demand, and when a villager's
//! stock resets — a faithful port of vanilla's `MerchantOffer`'s mutable
//! state and `Villager`'s restock cadence
//! (`shouldRestock`/`allowedToRestock`/`needsToRestock`/`restock`).
//!
//! # Wire order, not constructor order
//!
//! `MerchantOffer.writeToStream` emits, in order: cost A, **result**, cost B,
//! `isOutOfStock` (bool), uses, max uses, xp, special-price diff, price
//! multiplier, demand — not the constructor's parameter order (`buy, buyB,
//! result, uses, maxUses, ...`). This module doesn't encode a packet itself
//! (`crate::protocol::MerchantOfferOut`/`encode_merchant_offers` already do,
//! from `crate::mobs::villager::trades`'s output), but every mutable-field
//! method below is named and grouped to match `writeToStream`'s own order,
//! per this repo's own port-from-`write` rule, so a future encoder built
//! against [`OfferState`] cannot silently transpose two same-typed `i32`
//! fields the way `writeToStream`'s own layout does not.
//!
//! # What this module does not do
//!
//! It has no connection to the network or to `SimMob`/`MobSim` — nothing
//! here reaches a wire or a player's inventory. `crate::mobs::villager`'s
//! `OpenTrade` interact outcome and `crate::server::open_merchant_screen`
//! already produce the *static* offer list a client sees when a screen
//! opens. Wiring `SELECT_TRADE` (currently decoded and discarded in
//! `crates/protocol/v770/src/server_protocol.rs`) to actually call
//! [`VillagerTrades::try_trade`] needs three more things, none built here:
//!
//! 1. A `ServerBound::SelectTrade { index }` variant (currently the packet is
//!    decoded and the value thrown away) plus a real dispatch arm.
//! 2. A window-id → villager-entity mapping threaded through
//!    `crate::server::dispatch_play_packet`, which already carries roughly
//!    thirty per-connection parameters (`next_window_id`/`open_container`
//!    are the existing precedent for the shape it would take) — plus the
//!    same tracking on the per-villager side (`crate::mobs::villager`,
//!    off limits for this change).
//! 3. A player-inventory item exchange: [`TradeTake`] already names exactly
//!    what to remove and what to give, but performing that removal/grant
//!    against `PlayerInventory` and syncing the changed slots back to the
//!    client is `crate::server`'s job, not this module's.
//!
//! Named rather than silent, the same shape as this repo's own precedent for
//! the Brain-driven half of #243: build what the file-ownership boundary
//! allows, and report the remainder rather than rushing a hunk into this
//! repo's single largest choke-point function while other agents are live in
//! adjacent files.
//!
//! # The hook for gossip (#244) and reputation (#246)
//!
//! [`OfferState::special_price_diff`] and
//! [`OfferState::add_special_price_diff`]/[`OfferState::reset_special_price_diff`]
//! are vanilla's own mechanism for both systems: a player under
//! `Hero of the Village` gets a temporary trade discount by a call to
//! `addToSpecialPriceDiff` with a negative delta scaled by the effect's
//! amplifier (`Villager.updateSpecialPrices`), and ordinary reputation reads
//! the gossip-derived score through the same call. Neither this module nor
//! any caller here computes that score — #244/#246's job — but the field and
//! the two mutators they need already exist and are already tested (see
//! [`tests::a_special_price_diff_reduces_the_next_purchases_cost`]).
//! `OfferState::modified_cost_a_count`'s own demand term
//! (`price_multiplier`, from each `TradeRecord`'s real jar
//! `reputation_discount` value — see `lodestone_data::villager_trades`'s own
//! doc for why the field is named that despite having nothing to do with
//! this section) is the *other* half of "price fluctuation": ordinary
//! repeated buying, with no gossip system involved at all.

use lodestone_data::villager_trades::{TradeRecord, pool_for};

use crate::mobs::villager::Profession;

/// One villager's live state for one offer — vanilla's `MerchantOffer`'s
/// mutable/derived fields (`uses`, `demand`, `specialPriceDiff`) layered over
/// the static [`TradeRecord`] issue #243/#245's data table already carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OfferState {
    pub record: TradeRecord,
    pub uses: i32,
    pub demand: i32,
    pub special_price_diff: i32,
}

/// What a successful [`OfferState::take`]/[`VillagerTrades::try_trade`]
/// removes from the buyer and grants in return — the caller's contract with
/// `PlayerInventory`, not performed here. Every count is independently
/// meaningful (a real trade's `take_a`/`take_b`/`give` counts are rarely
/// equal), which is exactly the shape this repo's own evidence standard
/// asks fixtures to exercise: see this module's tests for a case where all
/// three differ.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TradeTake {
    pub take_a_item: &'static str,
    pub take_a_count: i32,
    pub take_b: Option<(&'static str, i32)>,
    pub give_item: &'static str,
    pub give_count: i32,
    pub xp: i32,
}

impl OfferState {
    #[must_use]
    pub fn new(record: TradeRecord) -> Self {
        Self {
            record,
            uses: 0,
            demand: 0,
            special_price_diff: 0,
        }
    }

    /// `MerchantOffer.getModifiedCostCount`, applied to the primary cost:
    /// `basePrice + max(0, floor(basePrice * demand * priceMultiplier)) +
    /// specialPriceDiff`, clamped to `1..=` the cost item's own max stack
    /// size (vanilla's `cost.itemStack().getMaxStackSize()`; `64` when this
    /// crate has no prototype for the item at all, matching every ordinary
    /// trade-currency item's real cap).
    #[must_use]
    pub fn modified_cost_a_count(&self) -> i32 {
        modified_cost_count(
            self.record.wants_count,
            self.demand,
            self.record.price_multiplier,
            self.special_price_diff,
            self.record.wants_item,
        )
    }

    #[must_use]
    pub fn is_out_of_stock(&self) -> bool {
        self.uses >= self.record.max_uses
    }

    pub fn set_out_of_stock(&mut self) {
        self.uses = self.record.max_uses;
    }

    /// `MerchantOffer.needsRestock` — any use at all means restocking would
    /// do something, not just "is this offer exhausted".
    #[must_use]
    pub fn needs_restock(&self) -> bool {
        self.uses > 0
    }

    pub fn reset_uses(&mut self) {
        self.uses = 0;
    }

    pub fn increase_uses(&mut self) {
        self.uses += 1;
    }

    /// `MerchantOffer.updateDemand`. Must run on the *pre-reset* uses count
    /// — `Villager.restock` calls this before `resetUses`, not after; doing
    /// it the other way would always compute `demand - maxUses`, never
    /// reflecting how much was actually bought since the last restock.
    pub fn update_demand(&mut self) {
        self.demand += self.uses - (self.record.max_uses - self.uses);
    }

    pub fn add_special_price_diff(&mut self, add: i32) {
        self.special_price_diff += add;
    }

    pub fn reset_special_price_diff(&mut self) {
        self.special_price_diff = 0;
    }

    /// `MerchantOffer.satisfiedBy`: does `offered_a`/`offered_b` cover this
    /// offer's live cost? `offered_b` is compared against `0` when the
    /// record has no second cost — vanilla's `buyB.isEmpty()` — so a caller
    /// holding an unrelated item in the "B" slot must pass `0`, not that
    /// item's count.
    #[must_use]
    pub fn satisfied_by(&self, offered_a: i32, offered_b: i32) -> bool {
        if offered_a < self.modified_cost_a_count() {
            return false;
        }
        match self.record.wants_b {
            Some((_, count)) => offered_b >= count,
            None => offered_b == 0,
        }
    }

    /// `MerchantOffer.take`, fused with `increaseUses`: validates via
    /// [`satisfied_by`](Self::satisfied_by), and on success returns exactly
    /// what to remove/grant and increments `uses`. Returns `None` without
    /// mutating anything on an unsatisfied offer or one already out of
    /// stock — vanilla's own silent refusal, not a distinct error.
    #[must_use]
    pub fn take(&mut self, offered_a: i32, offered_b: i32) -> Option<TradeTake> {
        if self.is_out_of_stock() || !self.satisfied_by(offered_a, offered_b) {
            return None;
        }
        let take_a_count = self.modified_cost_a_count();
        let take_b = self.record.wants_b;
        self.increase_uses();
        Some(TradeTake {
            take_a_item: self.record.wants_item,
            take_a_count,
            take_b,
            give_item: self.record.gives_item,
            give_count: self.record.gives_count,
            xp: self.record.xp,
        })
    }
}

fn modified_cost_count(
    base: i32,
    demand: i32,
    price_multiplier: f32,
    special_price_diff: i32,
    cost_item: &str,
) -> i32 {
    let demand_diff = (base as f32 * demand as f32 * price_multiplier)
        .floor()
        .max(0.0) as i32;
    let max_stack = lodestone_data::item_prototypes::prototype(cost_item)
        .map(|proto| i32::from(proto.max_stack_size))
        .unwrap_or(64);
    (base + demand_diff + special_price_diff).clamp(1, max_stack.max(1))
}

/// A villager's own restock cadence — vanilla's three private `Villager`
/// fields (`lastRestockGameTime`, `numberOfRestocksToday`,
/// `lastRestockCheckDay`) plus the two gates that read them
/// (`allowedToRestock`/`shouldRestock`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RestockState {
    pub last_restock_game_time: i64,
    pub number_of_restocks_today: i32,
    pub last_restock_check_day: i64,
}

/// Vanilla's own gap between restocks: `2400` ticks (two minutes), the
/// `allowedToRestock` cooldown between a villager's first and second restock
/// of the same day.
const RESTOCK_COOLDOWN_TICKS: i64 = 2400;
/// A restock is also allowed once a half day (`12000` ticks) has passed with
/// no restock at all, independent of the daily counter — `shouldRestock`'s
/// own `halfDayPassedTime` gate.
const HALF_DAY_TICKS: i64 = 12000;
/// At most two restocks credited per real day before `allowedToRestock`
/// requires waiting for the next day rollover.
const MAX_RESTOCKS_PER_DAY: i32 = 2;

impl RestockState {
    /// `Villager.allowedToRestock`: the first restock of a day is always
    /// allowed; the second needs the cooldown to have elapsed; a third (or
    /// later) is refused until `should_restock` sees a new day and resets
    /// the counter.
    #[must_use]
    fn allowed_to_restock(&self, game_time: i64) -> bool {
        self.number_of_restocks_today == 0
            || (self.number_of_restocks_today < MAX_RESTOCKS_PER_DAY
                && game_time > self.last_restock_game_time + RESTOCK_COOLDOWN_TICKS)
    }

    /// `Villager.shouldRestock`: rolls the daily counter over on a day
    /// change (either a half-day of real elapsed time with no restock, or
    /// the world's own day counter advancing), then answers
    /// `allowed_to_restock(...) && any_offer_needs_restock`.
    ///
    /// `current_day` is the world's day-period count (vanilla reads it
    /// through `Timelines.OVERWORLD_DAY`; the caller's equivalent — this
    /// crate's own day-time source — is `crate::world_state`, not consulted
    /// here since this module has no world handle at all).
    pub fn should_restock(
        &mut self,
        game_time: i64,
        current_day: i64,
        any_offer_needs_restock: bool,
    ) -> bool {
        let half_day_passed_time = self.last_restock_game_time + HALF_DAY_TICKS;
        let mut is_new_day = game_time > half_day_passed_time;
        is_new_day |= self.last_restock_check_day > 0 && current_day > self.last_restock_check_day;
        self.last_restock_check_day = current_day;
        if is_new_day {
            self.last_restock_game_time = game_time;
            self.number_of_restocks_today = 0;
        }
        self.allowed_to_restock(game_time) && any_offer_needs_restock
    }

    /// `Villager.restock`'s own cadence bookkeeping (`lastRestockGameTime`/
    /// `numberOfRestocksToday`) — the per-offer `updateDemand`/`resetUses`
    /// loop lives on the caller ([`VillagerTrades::maybe_restock`]), since
    /// this struct alone has no offer list to iterate.
    pub fn mark_restocked(&mut self, game_time: i64) {
        self.last_restock_game_time = game_time;
        self.number_of_restocks_today += 1;
    }
}

/// One villager's complete live trade state: every offer its level entitles
/// it to, plus its own restock cadence.
#[derive(Debug, Clone, Default)]
pub struct VillagerTrades {
    pub offers: Vec<OfferState>,
    pub restock: RestockState,
}

impl VillagerTrades {
    /// Builds a fresh (zero uses, zero demand) offer list for `profession`
    /// at `level`, accumulating every level from `1` up to `level` — the
    /// same "trades accumulate across levels" rule
    /// `crate::mobs::villager::trades::offers_up_to` already documents,
    /// applied here against the complete `lodestone_data::villager_trades`
    /// table instead of that module's farmer-only one.
    #[must_use]
    pub fn for_profession(profession: Profession, level: i32) -> Self {
        let offers = (1..=level.clamp(1, 5))
            .flat_map(|l| {
                pool_for(profession.path(), l)
                    .into_iter()
                    .flat_map(|(pool, amount)| pool.iter().take(amount).copied())
            })
            .map(OfferState::new)
            .collect();
        Self {
            offers,
            restock: RestockState::default(),
        }
    }

    /// Attempts to buy offer `index` with `offered_a`/`offered_b` held by the
    /// buyer. `None` for an out-of-range index, an out-of-stock offer, or an
    /// unsatisfied cost — see [`OfferState::take`].
    pub fn try_trade(&mut self, index: usize, offered_a: i32, offered_b: i32) -> Option<TradeTake> {
        self.offers.get_mut(index)?.take(offered_a, offered_b)
    }

    /// Runs one `WorkAtPoi`-equivalent restock check
    /// (`body.shouldRestock(level)` / `body.restock()`, vanilla's own two
    /// call sites, both inside the Brain `WorkAtPoi` behavior — not built in
    /// this crate; see this module's own doc for why). Returns whether a
    /// restock actually happened.
    pub fn maybe_restock(&mut self, game_time: i64, current_day: i64) -> bool {
        let any_needs_restock = self.offers.iter().any(OfferState::needs_restock);
        if !self
            .restock
            .should_restock(game_time, current_day, any_needs_restock)
        {
            return false;
        }
        // `Villager.restock`'s own order: demand is updated from the
        // *pre-reset* uses, then uses reset — see `update_demand`'s doc for
        // why the order is load-bearing.
        for offer in &mut self.offers {
            offer.update_demand();
            offer.reset_uses();
        }
        self.restock.mark_restocked(game_time);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wheat_for_emerald() -> TradeRecord {
        pool_for("farmer", 1).expect("farmer level 1 is ported").0[0]
    }

    fn cod_for_cooked_cod() -> TradeRecord {
        pool_for("fisherman", 1)
            .expect("fisherman level 1 is ported")
            .0
            .iter()
            .copied()
            .find(|t| t.gives_item == "minecraft:cooked_cod")
            .expect("raw_cod_and_emerald_cooked_cod is in fisherman level 1's pool")
    }

    /// A fresh offer's live price is exactly the jar's base price — no
    /// demand, no special discount yet.
    #[test]
    fn a_fresh_offer_costs_exactly_the_base_price() {
        let offer = OfferState::new(wheat_for_emerald());
        assert_eq!(offer.modified_cost_a_count(), 20);
    }

    /// The discriminating purchase-mechanics assertion: pairwise-distinct
    /// take/give amounts (20 wheat in, 1 emerald out, 2 xp) so a
    /// transposition between `take_a_count`/`give_count`/`xp` cannot survive.
    /// Insufficient funds is the control — it must refuse and change
    /// nothing, checked before the successful purchase that follows.
    #[test]
    fn a_satisfied_trade_takes_and_gives_exactly_the_jar_amounts() {
        let mut offer = OfferState::new(wheat_for_emerald());

        // Control: 19 wheat is one short of the real cost of 20.
        assert_eq!(offer.take(19, 0), None, "one short of cost must refuse");
        assert_eq!(offer.uses, 0, "a refused trade must not mutate uses");

        let take = offer.take(20, 0).expect("20 wheat satisfies the cost");
        assert_eq!(take.take_a_item, "minecraft:wheat");
        assert_eq!(take.take_a_count, 20);
        assert_eq!(take.take_b, None);
        assert_eq!(take.give_item, "minecraft:emerald");
        assert_eq!(take.give_count, 1);
        assert_eq!(take.xp, 2);
        assert_eq!(offer.uses, 1, "a successful trade must increase uses");
    }

    /// An out-of-stock offer refuses even with more than enough currency.
    #[test]
    fn an_out_of_stock_offer_refuses_a_trade() {
        let mut offer = OfferState::new(wheat_for_emerald());
        offer.set_out_of_stock();
        assert_eq!(offer.take(1000, 0), None);
    }

    /// A two-cost trade: both A and B must independently satisfy, checked
    /// with two separate single-short controls before the real purchase —
    /// an assert-inside-a-loop shape would only ever prove one of the two
    /// arms.
    #[test]
    fn a_two_cost_trade_requires_both_costs() {
        let mut mismatches = Vec::new();
        for (offered_a, offered_b, should_succeed) in
            [(5, 1, false), (6, 0, false), (6, 1, true)]
        {
            let mut offer = OfferState::new(cod_for_cooked_cod());
            let result = offer.take(offered_a, offered_b);
            if result.is_some() != should_succeed {
                mismatches.push((offered_a, offered_b, should_succeed, result));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:?}");

        let mut offer = OfferState::new(cod_for_cooked_cod());
        let take = offer.take(6, 1).unwrap();
        assert_eq!(take.take_a_item, "minecraft:cod");
        assert_eq!(take.take_a_count, 6);
        assert_eq!(take.take_b, Some(("minecraft:emerald", 1)));
        assert_eq!(take.give_item, "minecraft:cooked_cod");
        assert_eq!(take.give_count, 6);
    }

    /// Demand magnitude, predicted from the outside formula, not merely
    /// "the price went up": after one restock following four uses out of a
    /// max of sixteen, `demand = 0 + 4 - (16 - 4) = -8`, which *lowers* the
    /// price (negative demand is a real vanilla state — an
    /// under-purchased offer gets cheaper, not just "purchased offers get
    /// pricier"). `price_multiplier` is `0.05`, so the modified cost is
    /// `20 + max(0, floor(20 * -8 * 0.05)) + 0 = 20 + max(0, -8) = 20`
    /// (clamped by `max(0, ...)`, not by the outer clamp) — demand can only
    /// ever raise the price, never lower it below the special-price-adjusted
    /// base, exactly like vanilla's own `Math.max(0, ...)` term.
    #[test]
    fn demand_after_underuse_does_not_lower_the_price_below_base() {
        let mut offer = OfferState::new(wheat_for_emerald());
        for _ in 0..4 {
            offer.take(offer.modified_cost_a_count(), 0).unwrap();
        }
        assert_eq!(offer.uses, 4);
        offer.update_demand();
        assert_eq!(offer.demand, 4 - (16 - 4), "demand = uses - (maxUses - uses)");
        assert_eq!(offer.demand, -8);
        offer.reset_uses();
        assert_eq!(
            offer.modified_cost_a_count(),
            20,
            "negative demand must not discount below the base price"
        );
    }

    /// The inverse: heavy use (near `maxUses`) raises demand and therefore
    /// the price, by the exact predicted amount — not merely "some
    /// increase". `armorer/1/emerald_iron_leggings` (`price_multiplier`
    /// `0.2`, base cost `7`, `max_uses` `12`) chosen because its multiplier
    /// differs from the farmer table's `0.05`, so this test cannot pass by
    /// accident from copy-pasting the other one's arithmetic.
    #[test]
    fn heavy_use_raises_the_price_by_the_predicted_amount() {
        let leggings = pool_for("armorer", 1)
            .unwrap()
            .0
            .iter()
            .copied()
            .find(|t| t.gives_item == "minecraft:iron_leggings")
            .unwrap();
        assert_eq!((leggings.wants_count, leggings.max_uses, leggings.price_multiplier), (7, 12, 0.2));

        let mut offer = OfferState::new(leggings);
        for _ in 0..10 {
            offer.take(offer.modified_cost_a_count(), 0).unwrap();
        }
        offer.update_demand();
        // demand = 10 - (12 - 10) = 8
        assert_eq!(offer.demand, 8);
        offer.reset_uses();
        // demand_diff = floor(7 * 8 * 0.2) = floor(11.2) = 11
        // modified cost = 7 + 11 = 18, within emerald's real max stack (64)
        assert_eq!(offer.modified_cost_a_count(), 18);
    }

    /// The gossip/reputation hook: a negative special-price delta (vanilla's
    /// Hero of the Village discount) reduces the next purchase's cost, and
    /// the floor is `1`, never below — matched against a *positive* control
    /// so the sign itself is exercised, not just "the number changed".
    #[test]
    fn a_special_price_diff_reduces_the_next_purchases_cost() {
        let mut discounted = OfferState::new(wheat_for_emerald());
        discounted.add_special_price_diff(-15);
        assert_eq!(discounted.modified_cost_a_count(), 5);

        let mut surcharged = OfferState::new(wheat_for_emerald());
        surcharged.add_special_price_diff(3);
        assert_eq!(surcharged.modified_cost_a_count(), 23);

        let mut floored = OfferState::new(wheat_for_emerald());
        floored.add_special_price_diff(-1000);
        assert_eq!(floored.modified_cost_a_count(), 1, "price must never clamp below 1");

        let mut reset = OfferState::new(wheat_for_emerald());
        reset.add_special_price_diff(-15);
        reset.reset_special_price_diff();
        assert_eq!(reset.modified_cost_a_count(), 20);
    }

    /// `VillagerTrades::for_profession` accumulates every level up to the
    /// requested one, sourced from the complete table rather than the
    /// farmer-only one `crate::mobs::villager::trades` still carries.
    #[test]
    fn for_profession_accumulates_every_level_up_to_the_requested_one() {
        let trades = VillagerTrades::for_profession(Profession::Farmer, 3);
        // 2 offers/level (farmer's trade_set amount) across 3 levels.
        assert_eq!(trades.offers.len(), 6);
        assert!(
            trades
                .offers
                .iter()
                .any(|o| o.record.gives_item == "minecraft:cookie"),
            "a level-3 farmer must offer its level-3 trades"
        );
        assert!(
            trades
                .offers
                .iter()
                .any(|o| o.record.wants_item == "minecraft:wheat"),
            "a level-3 farmer must still offer its level-1 trades"
        );
    }

    /// A profession this table has no data for at all (`none`/`nitwit`
    /// resolve to no POI and are never claimed, so they never reach this
    /// constructor with a positive level in production) still returns an
    /// empty, not a panicking, list.
    #[test]
    fn an_unported_profession_yields_no_offers_rather_than_panicking() {
        let trades = VillagerTrades::for_profession(Profession::Nitwit, 5);
        assert!(trades.offers.is_empty());
    }

    /// Restock cadence, predicted from the exact tick thresholds rather than
    /// a round number: the first restock is always allowed regardless of
    /// elapsed time; a second within the 2400-tick cooldown is refused; the
    /// same second restock exactly one tick past the cooldown is allowed.
    #[test]
    fn restock_cadence_matches_the_exact_tick_thresholds() {
        let mut state = RestockState::default();
        // First restock: always allowed once something needs it.
        assert!(state.should_restock(0, 1, true));
        state.mark_restocked(0);
        assert_eq!(state.number_of_restocks_today, 1);

        // One tick short of the 2400-tick cooldown: refused.
        assert!(
            !state.should_restock(2400, 1, true),
            "game_time > last + 2400 is required; == 2400 is not > "
        );

        // Exactly at the threshold, still same day: allowed.
        assert!(state.should_restock(2401, 1, true));
        state.mark_restocked(2401);
        assert_eq!(state.number_of_restocks_today, 2);

        // A third same-day restock is refused even with no time constraint,
        // until a new day rolls the counter over.
        assert!(!state.should_restock(100_000, 1, true));

        // A day rollover resets the counter, and the first restock of the
        // new day is allowed again.
        assert!(state.should_restock(100_000, 2, true));
    }

    /// No offer needing restock refuses even when the cadence would
    /// otherwise allow it — `needs_restock`, not just time, gates a restock.
    #[test]
    fn no_offer_needing_restock_refuses_even_when_cadence_allows_it() {
        let mut state = RestockState::default();
        assert!(!state.should_restock(0, 1, false));
    }

    /// End-to-end restock: a used offer's uses reset and its demand updates,
    /// in that order (demand computed from the pre-reset uses).
    #[test]
    fn maybe_restock_resets_uses_and_updates_demand_from_the_pre_reset_count() {
        let mut trades = VillagerTrades::for_profession(Profession::Farmer, 1);
        let cost = trades.offers[0].modified_cost_a_count();
        trades.offers[0].take(cost, 0).unwrap();
        assert_eq!(trades.offers[0].uses, 1);

        let restocked = trades.maybe_restock(0, 1);
        assert!(restocked);
        assert_eq!(trades.offers[0].uses, 0, "restock must reset uses");
        assert_eq!(
            trades.offers[0].demand,
            1 - (trades.offers[0].record.max_uses - 1),
            "demand must be computed from the pre-reset uses count"
        );
    }
}
