//! The toy **subscriber** in issue #107's cross-plugin messaging example.
//!
//! It reads [`lodestone_shop_api::ShopPurchase`] and has **no compile-time
//! dependency on the publisher** — `Cargo.toml`'s `[dependencies]` contains
//! `lodestone-shop-api` and not `lodestone-shop`. That is the Bukkit property
//! issue #107 asks for, and `tests/dependency_direction.rs` asserts it as a fact
//! about the manifest rather than trusting this paragraph.
//!
//! See `docs/cross-plugin-messages.md` for the pattern.

use lodestone_ecs::GameTick;
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::message::MessageReader;
use lodestone_ecs::ecs::prelude::{IntoScheduleConfigs, Resource};
use lodestone_ecs::ecs::system::ResMut;
use lodestone_ecs::EventPriority;
use lodestone_shop_api::{ShopApiPlugin, ShopPurchase};

/// What this subscriber has observed. A real stats plugin would persist it.
#[derive(Resource, Debug, Default, PartialEq, Eq)]
pub struct ShopStats {
    /// How many purchases were seen.
    pub purchases: u32,
    /// Their total cost.
    pub coins_spent: u32,
    /// Every item id seen, in arrival order — so a test can assert *which*
    /// messages arrived, not merely how many.
    pub items: Vec<u32>,
}

/// `EventPriority::High`: fold every [`ShopPurchase`] this tick into
/// [`ShopStats`].
///
/// Anchored above the publisher's `Low` so the two get a defined order without
/// either crate naming a system in the other — the whole reason
/// `EventPriority` is published from `lodestone-ecs` rather than from either
/// plugin.
pub fn collect_purchases(mut inbox: MessageReader<ShopPurchase>, mut stats: ResMut<ShopStats>) {
    for purchase in inbox.read() {
        stats.purchases += 1;
        stats.coins_spent += purchase.coins;
        stats.items.push(purchase.item);
    }
}

/// The subscriber plugin.
///
/// Adds [`ShopApiPlugin`] itself — it must, because a subscriber installed
/// *without* the publisher still needs `Messages<ShopPurchase>` to exist, or its
/// `MessageReader` panics on a missing resource. That is exactly why the
/// registration has to be idempotent.
#[derive(Debug, Default)]
pub struct ShopStatsPlugin;

impl Plugin for ShopStatsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ShopApiPlugin);
        app.init_resource::<ShopStats>();
        app.add_systems(GameTick, collect_purchases.in_set(EventPriority::High));
    }
}
