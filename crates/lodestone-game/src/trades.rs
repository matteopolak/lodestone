//! Villager and wandering-trader offers.
//!
//! ## What it is
//!
//! The trade list the server sends when a merchant screen opens, from
//! `ClientboundMerchantOffersPacket`. One slot: a merchant screen is modal, so a
//! new packet replaces the previous list entirely.
//!
//! ## How it works
//!
//! [`TradeOffers::apply`] stores the offers plus the merchant's level, xp and
//! restock flag. Costs are `(item registry id, count)` pairs rather than
//! `ItemStack`s: an `ItemCost` on the wire is an id, a count and a component
//! *predicate*, not a stack, and inventing a stack from it would imply component
//! data the packet does not carry.
//!
//! Whether a trade is usable is [`MerchantOffer::out_of_stock`] plus
//! `uses < max_uses` — vanilla greys out on the flag and locks on the counter, and
//! they can disagree for one tick after a purchase.
//!
//! ## How to change it
//!
//! `price_multiplier` and `demand` are the demand-pricing inputs; the adjusted
//! price is `cost_a.count + special_price_diff`, floored at 1. That arithmetic is
//! vanilla's and belongs next to the screen that shows a price, not here — this
//! store deliberately does no pricing.
//!
//! ## Dependencies
//!
//! [`lodestone_model::event::ClientEvent`] only.

use lodestone_model::event::{ClientEvent, MerchantOffer};

/// The open merchant's trade list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TradeOffers {
    window_id: Option<i32>,
    offers: Vec<MerchantOffer>,
    villager_level: i32,
    villager_xp: i32,
    show_progress: bool,
    can_restock: bool,
}

impl TradeOffers {
    /// An empty store — no merchant screen has opened.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The container id these offers belong to, or `None` if none has arrived.
    #[must_use]
    pub fn window_id(&self) -> Option<i32> {
        self.window_id
    }

    /// The offers, in the order shown.
    #[must_use]
    pub fn offers(&self) -> &[MerchantOffer] {
        &self.offers
    }

    /// The merchant's level, 1–5.
    #[must_use]
    pub fn villager_level(&self) -> i32 {
        self.villager_level
    }

    /// Experience toward the merchant's next level.
    #[must_use]
    pub fn villager_xp(&self) -> i32 {
        self.villager_xp
    }

    /// Whether the level/xp bar should be shown.
    #[must_use]
    pub fn show_progress(&self) -> bool {
        self.show_progress
    }

    /// Whether this merchant restocks. `false` for a wandering trader.
    #[must_use]
    pub fn can_restock(&self) -> bool {
        self.can_restock
    }

    /// Whether offer `index` can be traded right now.
    ///
    /// Both conditions, deliberately: vanilla greys out on `out_of_stock` and
    /// locks on the use counter, and the two can disagree for a tick after a
    /// purchase.
    #[must_use]
    pub fn is_available(&self, index: usize) -> bool {
        self.offers
            .get(index)
            .is_some_and(|offer| !offer.out_of_stock && offer.uses < offer.max_uses)
    }

    /// Folds one event, returning whether it belonged to this store.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        let ClientEvent::MerchantOffersReceived {
            window_id,
            offers,
            villager_level,
            villager_xp,
            show_progress,
            can_restock,
        } = event
        else {
            return false;
        };
        // A whole replace: a merchant screen is modal, so there is no merging to do.
        self.window_id = Some(*window_id);
        self.offers = offers.clone();
        self.villager_level = *villager_level;
        self.villager_xp = *villager_xp;
        self.show_progress = *show_progress;
        self.can_restock = *can_restock;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::TradeOffers;
    use lodestone_model::event::{ClientEvent, MerchantOffer};

    fn offer(out_of_stock: bool, uses: i32, max_uses: i32) -> MerchantOffer {
        MerchantOffer {
            cost_a: (1, 2),
            cost_b: None,
            result: None,
            out_of_stock,
            uses,
            max_uses,
            xp: 1,
            special_price_diff: 0,
            price_multiplier: 0.05,
            demand: 0,
        }
    }

    /// Availability needs **both** conditions; a test asserting only one would
    /// pass for an implementation that checked only the other.
    #[test]
    fn availability_needs_both_the_flag_and_the_counter() {
        let mut store = TradeOffers::new();
        store.apply(&ClientEvent::MerchantOffersReceived {
            window_id: 1,
            offers: vec![
                offer(false, 0, 12),  // usable
                offer(true, 0, 12),   // flagged out of stock
                offer(false, 12, 12), // counter exhausted
            ],
            villager_level: 2,
            villager_xp: 30,
            show_progress: true,
            can_restock: true,
        });
        assert!(store.is_available(0));
        assert!(!store.is_available(1), "the out_of_stock flag alone blocks it");
        assert!(!store.is_available(2), "the use counter alone blocks it");
        assert!(!store.is_available(99), "an absent index is not available");
    }

    #[test]
    fn a_new_packet_replaces_the_list() {
        let mut store = TradeOffers::new();
        assert_eq!(store.window_id(), None);
        store.apply(&ClientEvent::MerchantOffersReceived {
            window_id: 1,
            offers: vec![offer(false, 0, 1), offer(false, 0, 1)],
            villager_level: 1,
            villager_xp: 0,
            show_progress: false,
            can_restock: false,
        });
        assert_eq!(store.offers().len(), 2);
        store.apply(&ClientEvent::MerchantOffersReceived {
            window_id: 2,
            offers: vec![offer(false, 0, 1)],
            villager_level: 5,
            villager_xp: 250,
            show_progress: true,
            can_restock: true,
        });
        assert_eq!(store.offers().len(), 1);
        assert_eq!(store.window_id(), Some(2));
        assert_eq!(store.villager_level(), 5);
        assert!(store.can_restock());
    }

    #[test]
    fn an_unrelated_event_is_rejected() {
        let mut store = TradeOffers::new();
        assert!(!store.apply(&ClientEvent::KeepAlive { id: 1 }));
    }
}
