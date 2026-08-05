//! The toy **publisher** in issue #107's cross-plugin messaging example.
//!
//! It writes [`lodestone_shop_api::ShopPurchase`] and knows nothing about who
//! reads it — this crate has no dependency on any subscriber, which is half of
//! the property issue #107 is about. See `docs/cross-plugin-messages.md`.

use lodestone_ecs::GameTick;
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::message::MessageWriter;
use lodestone_ecs::ecs::prelude::{IntoScheduleConfigs, Resource};
use lodestone_ecs::ecs::system::ResMut;
use lodestone_ecs::EventPriority;
use lodestone_shop_api::{ShopApiPlugin, ShopPurchase};

/// Purchases this plugin has been asked to announce. A real shop plugin would
/// fill this from a container click or a command; the toy fills it from a test,
/// so the *messaging* is what is under test rather than a shop.
#[derive(Resource, Debug, Default)]
pub struct PendingPurchases(pub Vec<ShopPurchase>);

/// `EventPriority::Low`: drain [`PendingPurchases`] onto the message bus.
///
/// Anchored low so a subscriber at `Normal` or above sees this tick's batch in
/// the same tick — `EventPriority` is the cross-plugin anchor two plugins that
/// have never heard of each other can both name (`lodestone_ecs::sets`).
pub fn announce_purchases(
    mut pending: ResMut<PendingPurchases>,
    mut out: MessageWriter<ShopPurchase>,
) {
    for purchase in pending.0.drain(..) {
        out.write(purchase);
    }
}

/// The publisher plugin.
///
/// Adds [`ShopApiPlugin`] itself. So does every subscriber, and that is fine —
/// see [`ShopApiPlugin`]'s doc on why it is not unique.
#[derive(Debug, Default)]
pub struct ShopPlugin;

impl Plugin for ShopPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ShopApiPlugin);
        app.init_resource::<PendingPurchases>();
        app.add_systems(GameTick, announce_purchases.in_set(EventPriority::Low));
    }
}
