//! Issue #107's end-to-end proof: two toy plugins in separate crates exchange a
//! custom message, and the subscriber has **no compile-time dependency on the
//! publisher**.
//!
//! This test is the only place the two halves meet, which is why
//! `lodestone-shop` is a `[dev-dependencies]` entry here and not a
//! `[dependencies]` one — the test harness needs both, the library needs one.
//! `tests/dependency_direction.rs` asserts that split as a fact about the
//! manifest.

use lodestone_ecs::app::App;
use lodestone_ecs::ecs::prelude::Messages;
use lodestone_ecs::GameTick;
use lodestone_shop::{PendingPurchases, ShopPlugin};
use lodestone_shop_api::ShopPurchase;
use lodestone_shop_stats::{ShopStats, ShopStatsPlugin};

/// Build the `App` the way a server owner would: add both plugins, in an order
/// neither plugin controls.
fn app() -> App {
    let mut app = App::new();
    app.add_plugins(ShopPlugin);
    app.add_plugins(ShopStatsPlugin);
    app
}

/// The headline: a message written by `lodestone-shop` is read by
/// `lodestone-shop-stats`, which has never heard of it.
///
/// Asserts *which* messages arrived and their summed cost, not merely a count —
/// a subscriber that dropped one and double-counted another would pass a
/// count-only check.
#[test]
fn a_message_published_by_one_plugin_reaches_a_subscriber_in_another_crate() {
    let mut app = app();
    app.world_mut()
        .resource_mut::<PendingPurchases>()
        .0
        .extend([
            ShopPurchase { item: 7, coins: 10 },
            ShopPurchase { item: 9, coins: 32 },
        ]);

    app.world_mut().run_schedule(GameTick);

    let stats = app.world().resource::<ShopStats>();
    assert_eq!(stats.purchases, 2);
    assert_eq!(stats.coins_spent, 42);
    assert_eq!(stats.items, vec![7, 9], "both messages, in publish order");
}

/// **The control.** With only the subscriber installed, nothing publishes, so
/// the stats stay empty — and, critically, the `App` still **builds and runs**.
///
/// Both halves matter. The empty stats prove the test above is measuring a real
/// delivery rather than a subscriber that invents data. The clean run proves a
/// subscriber installed *without* its publisher does not crash, which is the
/// normal configuration for a message bus and the reason `ShopApiPlugin` has to
/// be non-unique and the registration idempotent — a `MessageReader<T>` with no
/// `Messages<T>` resource would panic.
#[test]
fn the_subscriber_alone_builds_runs_and_observes_nothing() {
    let mut app = App::new();
    app.add_plugins(ShopStatsPlugin);
    assert!(
        app.world().get_resource::<Messages<ShopPurchase>>().is_some(),
        "the subscriber must register the message type itself, or its reader panics"
    );

    for _ in 0..5 {
        app.world_mut().run_schedule(GameTick);
    }

    assert_eq!(*app.world().resource::<ShopStats>(), ShopStats::default());
}

/// The publisher alone also builds and runs — the mirror of the control above.
/// A publisher with no subscribers must not be a special case, or "publish and
/// forget" is not actually what the pattern offers.
#[test]
fn the_publisher_alone_builds_and_runs_with_nobody_listening() {
    let mut app = App::new();
    app.add_plugins(ShopPlugin);
    app.world_mut()
        .resource_mut::<PendingPurchases>()
        .0
        .push(ShopPurchase { item: 1, coins: 1 });
    app.world_mut().run_schedule(GameTick);
    assert!(
        app.world().resource::<PendingPurchases>().0.is_empty(),
        "the publisher must still have drained its queue"
    );
}

/// **Both plugins register the same message type.** This is the case that
/// panics without `ShopApiPlugin::is_unique() == false` plus
/// `add_plugin_message`'s `is_plugin_added` check — measured, because an earlier
/// draft of the async-task work hit bevy's "plugin was already added in
/// application" on exactly this shape.
///
/// Checked in both registration orders, since neither plugin can know which one
/// a server owner adds first.
#[test]
fn both_plugins_registering_the_message_type_is_fine_in_either_order() {
    let mut a = App::new();
    a.add_plugins(ShopPlugin);
    a.add_plugins(ShopStatsPlugin);

    let mut b = App::new();
    b.add_plugins(ShopStatsPlugin);
    b.add_plugins(ShopPlugin);

    for app in [&mut a, &mut b] {
        app.world_mut()
            .resource_mut::<PendingPurchases>()
            .0
            .push(ShopPurchase { item: 3, coins: 4 });
        app.world_mut().run_schedule(GameTick);
        assert_eq!(app.world().resource::<ShopStats>().items, vec![3]);
    }
}

/// A second subscriber would be a third crate; standing in for it here with a
/// locally-defined system proves the bus fans out rather than delivering to one
/// reader. `MessageReader` is per-reader-cursor, so this is a real property and
/// not a tautology — a single shared cursor would let the first reader consume
/// the batch.
#[test]
fn a_second_independent_reader_also_sees_every_message() {
    use lodestone_ecs::ecs::message::MessageReader;
    use lodestone_ecs::ecs::prelude::{IntoScheduleConfigs, Resource};
    use lodestone_ecs::ecs::system::ResMut;
    use lodestone_ecs::EventPriority;

    #[derive(Resource, Default)]
    struct SecondReader(Vec<u32>);

    fn second(mut inbox: MessageReader<ShopPurchase>, mut out: ResMut<SecondReader>) {
        for msg in inbox.read() {
            out.0.push(msg.item);
        }
    }

    let mut app = app();
    app.init_resource::<SecondReader>();
    app.add_systems(GameTick, second.in_set(EventPriority::Highest));
    app.world_mut()
        .resource_mut::<PendingPurchases>()
        .0
        .push(ShopPurchase { item: 5, coins: 6 });

    app.world_mut().run_schedule(GameTick);

    assert_eq!(app.world().resource::<ShopStats>().items, vec![5]);
    assert_eq!(
        app.world().resource::<SecondReader>().0,
        vec![5],
        "each reader has its own cursor; one must not consume the other's batch"
    );
}
