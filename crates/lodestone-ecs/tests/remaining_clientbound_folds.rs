//! The anti-island gate: each of the twenty-four new `ClientEvent`s must
//! reach a real fold through the **production** `SessionPlugin`, not through a
//! hand-called `apply`.
//!
//! # Why this is not a unit test on each store
//!
//! `lodestone_game::{debug_feeds, serverinfo, waypoints}` and
//! `progress::Statistics` all have unit tests, and every one of them constructs
//! the store and calls `apply` directly. That is a closed loop: the whole suite
//! stays green if `SessionPlugin` never registers the fold, if
//! `insert_session_components` never inserts the component, or if
//! `lodestone_model::event::route` claims the event for `ingest` instead of
//! `session` — which is the exact mistake `route`'s own doc says has cost work
//! twice, because `SharedState::apply` consults **both** predicates and an arm in
//! the wrong one compiles, unit-tests green, and never runs.
//!
//! So each test here pushes a real event onto [`IngestQueue`], runs the real
//! `NetIngest` schedule, and asserts the *component* changed.
//!
//! # The two controls
//!
//! * [`route_claims_every_new_event_for_session_and_not_ingest`] checks the fork
//!   directly, for all of them, so a misrouted arm fails here with a clear
//!   message rather than as a silently unchanged store.
//! * [`an_event_no_fold_claims_leaves_every_store_untouched`] pushes an event that
//!   belongs to none of these stores and requires nothing to change — without it,
//!   a fold that indiscriminately mutated on every event would pass every
//!   assertion below.

use bevy_app::App;
use lodestone_ecs::ingest::IngestQueue;
use lodestone_ecs::{
    NetIngest, SessionDebugFeeds, SessionPlugin, SessionRecipeBook, SessionServerInfo,
    SessionStatistics, SessionTrades, SessionWaypoints,
};
use lodestone_model::event::{
    ChatCompletionsAction, ClientEvent, DebugSampleKind, MerchantOffer, RecipeBookEntry,
    ServerLink, ServerLinkKind, StatAward, TrackedWaypoint, WaypointId, WaypointOperation,
    WaypointPosition,
};
use lodestone_model::{BlockPos, ChunkPos};

fn key(name: &str) -> lodestone_model::Identifier {
    name.parse().expect("test key parses")
}

/// Builds the production session `World` and returns `(app, session entity)`.
fn session_app() -> (App, bevy_ecs::entity::Entity) {
    let mut app = App::new();
    app.add_plugins(lodestone_ecs::CorePlugin);
    app.add_plugins(SessionPlugin);
    let entity = lodestone_ecs::spawn_session(app.world_mut());
    (app, entity)
}

/// Pushes `events` and runs the real ingest schedule once.
fn ingest(app: &mut App, events: Vec<ClientEvent>) {
    {
        let mut queue = app
            .world_mut()
            .get_resource_mut::<IngestQueue>()
            .expect("SessionPlugin installs IngestQueuePlugin, which owns IngestQueue");
        for event in events {
            queue.push(event);
        }
    }
    app.world_mut().run_schedule(NetIngest);
}

/// Every one of them, listed once so a new variant cannot be added to the decode
/// side and forgotten here.
fn every_new_event() -> Vec<ClientEvent> {
    vec![
        ClientEvent::StatisticsAwarded {
            stats: vec![StatAward {
                stat_type: key("minecraft:custom"),
                value: Some(key("minecraft:jump")),
                count: 11,
            }],
        },
        ClientEvent::ChatCompletionsChanged {
            action: ChatCompletionsAction::Add,
            entries: vec!["alice".to_owned()],
        },
        ClientEvent::DebugBlockValue {
            pos: BlockPos { x: 1, y: 2, z: 3 },
            subscription: key("minecraft:neighbor_updates"),
            value: Some(vec![1]),
        },
        ClientEvent::DebugChunkValue {
            chunk: ChunkPos { x: 1, z: 2 },
            subscription: key("minecraft:structures"),
            value: Some(vec![2]),
        },
        ClientEvent::DebugEntityValue {
            entity_id: 7,
            subscription: key("minecraft:brains"),
            value: Some(vec![3]),
        },
        ClientEvent::DebugEvent {
            subscription: key("minecraft:game_events"),
            value: vec![4],
        },
        ClientEvent::DebugSample {
            sample: vec![1_000],
            kind: DebugSampleKind::TickTime,
        },
        ClientEvent::GameTestHighlightPos {
            absolute: BlockPos { x: 0, y: 0, z: 0 },
            relative: BlockPos { x: 0, y: 0, z: 0 },
        },
        ClientEvent::LowDiskSpaceWarning,
        ClientEvent::CustomReportDetails {
            details: vec![("t".to_owned(), "d".to_owned())],
        },
        ClientEvent::ServerLinksReceived {
            links: vec![ServerLink {
                kind: ServerLinkKind::Known(0),
                url: "https://example.invalid".to_owned(),
            }],
        },
        ClientEvent::WaypointUpdated {
            operation: WaypointOperation::Track,
            waypoint: TrackedWaypoint {
                id: WaypointId::Named("w".to_owned()),
                style: key("minecraft:default"),
                color: None,
                position: WaypointPosition::Exact(BlockPos { x: 4, y: 5, z: 6 }),
            },
        },
        ClientEvent::TagQueryResponse {
            transaction_id: 3,
            tag: Some(vec![0x0A, 0x00]),
        },
        ClientEvent::TickingStateChanged {
            tick_rate: 7.5,
            frozen: true,
        },
        ClientEvent::TickingStepped { tick_steps: 2 },
        ClientEvent::TestInstanceBlockStatus {
            status: lodestone_model::Text::literal("ok"),
            size: Some((1, 2, 3)),
        },
        ClientEvent::DialogShown {
            registry_id: Some(3),
            inline: None,
        },
        // Deliberately last, and deliberately after `DialogShown`: it clears the
        // slot, so a fold that ignored ordering would leave a dialog open.
        ClientEvent::DialogCleared,
        // ---- the recipe/trade tranche ----
        ClientEvent::RecipeBookAdded {
            entries: vec![RecipeBookEntry {
                display_id: 4,
                result_items: vec![12],
                notification: true,
                highlight: false,
            }],
            replace: true,
        },
        ClientEvent::GhostRecipeShown {
            window_id: 2,
            result_items: vec![12],
        },
        ClientEvent::RecipePropertySetsUpdated {
            item_sets: vec![(key("minecraft:furnace_input"), vec![1, 2])],
            stonecutter_results: vec![vec![3]],
        },
        ClientEvent::MerchantOffersReceived {
            window_id: 5,
            offers: vec![MerchantOffer {
                cost_a: (1, 2),
                cost_b: None,
                result: None,
                out_of_stock: false,
                uses: 0,
                max_uses: 12,
                xp: 1,
                special_price_diff: 0,
                price_multiplier: 0.05,
                demand: 0,
            }],
            villager_level: 3,
            villager_xp: 70,
            show_progress: true,
            can_restock: true,
        },
        // `RecipeBookRemoved` is exercised on its own in
        // `a_removed_recipe_leaves_has_data_set`, not here: putting it in this
        // batch would empty the set the positive gate below asserts is non-empty,
        // which would make that assertion pass for a fold that did nothing.
    ]
}

/// The control for the fork. `SharedState::apply` consults *both* predicates, so
/// an arm in the wrong one is invisible to every other assertion in this file.
#[test]
fn route_claims_every_new_event_for_session_and_not_ingest() {
    for event in every_new_event() {
        let route = lodestone_model::event::route(&event);
        assert!(
            route.session,
            "{event:?} is not claimed by session -- its fold will never run"
        );
        assert!(
            lodestone_ecs::session::handles_event(&event),
            "session::handles_event disagrees with route for {event:?}"
        );
        assert!(
            !lodestone_ecs::ingest::handles_event(&event),
            "{event:?} is claimed by ingest too -- that is a double fold, not redundancy"
        );
        assert!(
            !route.is_island(),
            "{event:?} reports as an island despite a session fold"
        );
    }
}

/// The positive gate: push all eighteen through the real schedule and require
/// every store to have moved.
#[test]
fn every_new_event_reaches_its_session_component() {
    let (mut app, entity) = session_app();
    ingest(&mut app, every_new_event());

    let world = app.world();

    let stats = &world
        .get::<SessionStatistics>(entity)
        .expect("SessionStatistics must be inserted by insert_session_components")
        .0;
    assert_eq!(
        stats.len(),
        1,
        "award_stats reached no fold -- this is the packet that leaves the \
         statistics screen empty"
    );

    let feeds = &world
        .get::<SessionDebugFeeds>(entity)
        .expect("SessionDebugFeeds must be inserted")
        .0;
    assert_eq!(feeds.value_count(), 3, "one block, one chunk, one entity value");
    assert_eq!(feeds.events().count(), 1);
    assert_eq!(feeds.samples().count(), 1);
    assert_eq!(feeds.highlights().len(), 1);
    assert!(feeds.test_instance_status().is_some());
    assert_eq!(
        feeds.nbt_reply(3).map(Option::is_some),
        Some(true),
        "the tag_query reply must land keyed on its transaction id"
    );

    let info = &world
        .get::<SessionServerInfo>(entity)
        .expect("SessionServerInfo must be inserted")
        .0;
    assert_eq!(info.links().len(), 1);
    assert_eq!(info.report_details().len(), 1);
    assert_eq!(info.chat_completions().count(), 1);
    assert_eq!(info.low_disk_space_warnings(), 1);
    assert_eq!(info.ticking().tick_rate, 7.5);
    assert!(info.ticking().frozen);
    assert_eq!(info.ticking().pending_steps, 2);
    assert!(
        info.dialog().is_none(),
        "DialogCleared came after DialogShown, so the slot must be empty -- \
         ordering inside one batch is real"
    );

    let waypoints = &world
        .get::<SessionWaypoints>(entity)
        .expect("SessionWaypoints must be inserted")
        .0;
    assert_eq!(waypoints.len(), 1);
    assert_eq!(waypoints.positioned().count(), 1);

    let book = &world
        .get::<SessionRecipeBook>(entity)
        .expect("SessionRecipeBook must be inserted")
        .0;
    assert!(book.has_data(), "recipe_book_add reached no fold");
    assert!(book.is_unlocked(4));
    assert_eq!(book.ghost().map(|ghost| ghost.window_id), Some(2));
    assert_eq!(book.property_set_count(), 1);
    assert_eq!(book.stonecutter_results().len(), 1);
    // The join a panel needs, since a RecipeDisplayId carries no recipe name.
    assert_eq!(book.unlocked_producing(12).count(), 1);

    let trades = &world
        .get::<SessionTrades>(entity)
        .expect("SessionTrades must be inserted")
        .0;
    assert_eq!(trades.window_id(), Some(5));
    assert_eq!(trades.offers().len(), 1);
    assert!(trades.is_available(0));
    assert_eq!(trades.villager_level(), 3);
}

/// `RecipeBookRemoved` gets its own run, because including it in the batch above
/// would empty the set that gate asserts is populated.
#[test]
fn a_removed_recipe_leaves_has_data_set() {
    let (mut app, entity) = session_app();
    ingest(
        &mut app,
        vec![
            ClientEvent::RecipeBookAdded {
                entries: vec![RecipeBookEntry {
                    display_id: 7,
                    result_items: vec![1],
                    notification: false,
                    highlight: false,
                }],
                replace: true,
            },
            ClientEvent::RecipeBookRemoved {
                display_ids: vec![7],
            },
        ],
    );
    let book = &app
        .world()
        .get::<SessionRecipeBook>(entity)
        .expect("SessionRecipeBook must be inserted")
        .0;
    assert!(!book.is_unlocked(7));
    assert!(
        book.has_data(),
        "an emptied set must still report has_data, or a consumer falls back to          showing every recipe unlocked"
    );
}

/// The control for the gate above: an event none of these stores claims must
/// leave all four untouched. Without this, a fold that mutated on every event
/// would satisfy every assertion in `every_new_event_reaches_its_session_component`.
#[test]
fn an_event_no_fold_claims_leaves_every_store_untouched() {
    let (mut app, entity) = session_app();
    ingest(&mut app, vec![ClientEvent::KeepAlive { id: 42 }]);

    let world = app.world();
    assert_eq!(world.get::<SessionStatistics>(entity).unwrap().0.len(), 0);
    assert_eq!(
        world.get::<SessionDebugFeeds>(entity).unwrap().0.value_count(),
        0
    );
    assert_eq!(
        world
            .get::<SessionServerInfo>(entity)
            .unwrap()
            .0
            .low_disk_space_warnings(),
        0
    );
    assert!(world.get::<SessionWaypoints>(entity).unwrap().0.is_empty());
    assert!(!world.get::<SessionRecipeBook>(entity).unwrap().0.has_data());
    assert_eq!(
        world.get::<SessionTrades>(entity).unwrap().0.window_id(),
        None
    );
}

/// A cleared debug key must actually disappear from the store *through the
/// schedule*, not only through a direct `apply` — the `Optional` on the wire is
/// what carries "stop drawing this", and a fold that stored `Some(vec![])` would
/// leave the overlay up.
#[test]
fn a_cleared_debug_value_is_removed_through_the_real_fold() {
    let (mut app, entity) = session_app();
    let pos = BlockPos { x: 9, y: 9, z: 9 };
    ingest(
        &mut app,
        vec![ClientEvent::DebugBlockValue {
            pos,
            subscription: key("minecraft:neighbor_updates"),
            value: Some(vec![1, 2]),
        }],
    );
    assert_eq!(
        app.world()
            .get::<SessionDebugFeeds>(entity)
            .unwrap()
            .0
            .value_count(),
        1
    );

    ingest(
        &mut app,
        vec![ClientEvent::DebugBlockValue {
            pos,
            subscription: key("minecraft:neighbor_updates"),
            value: None,
        }],
    );
    assert_eq!(
        app.world()
            .get::<SessionDebugFeeds>(entity)
            .unwrap()
            .0
            .value_count(),
        0,
        "a clear must remove the key across ingest runs, not just within one"
    );
}
