//! Starts a dimension's tick task when its terrain source is first resolved.
//!
//! The task uses the world's memoized entity runtime, dimension-owned block
//! registries and scheduled ticks, and canonical player anchors. Its clock
//! role is `Follower`: the primary loop alone advances the shared clock and
//! consumes weather requests. Runtime slots contain no source or task handles.

use std::sync::Arc;

use crate::block_entities::BlockEntityHandle;
use crate::chunk::ChunkSource;
use crate::dimension::Dimension;
// The shared context must not depend on filesystem-backed terrain modules.
use crate::scheduled_tick::ScheduledTickHandle;
use crate::sleep::{SleepFeed, SleepVote};
use crate::tick::{BlockTickFeed, ExplosionFeed, TickClock};
use crate::tick_area::TickFollow;
use crate::weather::{WeatherFeed, WeatherState};
use crate::world_state::WorldStateHandle;

/// Everything [`spawn_for_dimension`] needs that is shared with the primary
/// (overworld) tick loop, bundled the same way [`TickFollow`] bundles what a
/// single loop needs — see that type's own doc comment for why a struct
/// beats another handful of positional arguments here.
#[derive(Clone)]
pub(crate) struct DimensionTickContext {
    /// The world's shared scalars **and** its anchor set
    /// ([`WorldStateHandle::tick_anchors`]) — the same handle the primary
    /// loop and every connection already share. Player presence in the
    /// canonical registry supplies this loop's dimension-filtered anchors.
    pub world_state: WorldStateHandle,
    /// Races every spawned loop against the same shutdown signal every other
    /// background task in [`crate::integrated`] uses, so a dimension's tick
    /// loop cannot outlive the server that started it.
    pub shutdown: Arc<crate::integrated::ShutdownSignal>,
}

/// Starts a background tick loop for `dimension`'s own `source`, following
/// the shared anchor set in `ctx.world_state` and falling back to a small
/// fixed area around the origin while nobody is there — the same fallback
/// shape [`crate::tick_area`]'s own module doc documents for the primary
/// loop's pre-follow-area tests.
///
/// `block_entities`/`scheduled` are the dimension's **own** handles (from its
/// `RegionChunkSource` when the world is persistent, or a fresh empty pair
/// when it is not) — never the primary loop's, or a furnace restored in the
/// Nether would be ticked twice against two different registries and a save
/// would see neither one reliably. `block_tick_feed` is likewise the
/// dimension's own — the **same** instance
/// [`crate::dimension::DimensionalSource::alone_with_dimension_handles`]
/// stores on the sibling source this loop was handed, so a connection that
/// later travels here and calls `BlockTickFeed::request_scheduled_ticks`
/// reaches exactly the queue this loop drains every tick, not a disposable
/// one nothing else can ever see. Passed in rather than built with
/// `BlockTickFeed::default()` here, unlike the loop's other disposable
/// per-dimension feeds (`ExplosionFeed`, `WeatherFeed`). Entity simulation and
/// publication handles are resolved from the shared world's dimension runtime.
///
/// A no-op — logged and otherwise silent — when called outside a Tokio
/// runtime, which is deliberate rather than a panic: `with_nether`'s factory
/// closure runs from ordinary sync code (including inside a handful of
/// non-async unit tests that build a [`crate::dimension::DimensionalSource`]
/// directly and never touch a portal), and `tokio::spawn` would panic there
/// with no runtime to hand it to. See [`crate::tick::run_tick_loop`]'s own
/// "native only" note for why this whole module is `cfg`-gated the same way.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spawn_for_dimension(
    dimension: Dimension,
    source: Arc<dyn ChunkSource>,
    block_entities: BlockEntityHandle,
    scheduled: ScheduledTickHandle,
    block_tick_feed: BlockTickFeed,
    ctx: &DimensionTickContext,
) {
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::warn!(
            "no Tokio runtime available, {dimension:?}'s tick loop was not started \
             (expected only outside a running server, e.g. a sync unit test)"
        );
        return;
    }

    let world_state = ctx.world_state.clone();
    let runtime = world_state.ensure_dimension_runtime(dimension);
    let shutdown = Arc::clone(&ctx.shutdown);
    // The pointer wrapper supplies the tick loop's sized source type.
    let world: Arc<Arc<dyn ChunkSource>> = Arc::new(source);
    // `FALLBACK_TICK_RADIUS`-about-the-origin, exactly the square the
    // primary loop used before `crate::tick_area::FollowArea` existed —
    // `FollowArea`'s own module doc: "the fallback is load-bearing"; here it
    // is what keeps a Nether spawn-adjacent furnace ticking between visits
    // rather than the loop simulating nothing at all whenever the anchor set
    // is empty for this dimension.
    let radius = crate::chunk_store::FALLBACK_TICK_RADIUS;
    let tick_area = (-radius..=radius, -radius..=radius);
    let follow = TickFollow {
        dimension,
        radius: world_state.simulation_distance(),
        anchors: world_state.tick_anchors().clone(),
    };

    // `spawn_world_tick_task` races the future against `shutdown` on the same
    // isolated native runtime the primary loop uses. The `Handle` check above
    // is what makes its task hand-off safe to call unconditionally from here,
    // matching every other background task `crate::integrated` starts. The
    // returned `Task` is intentionally dropped: nothing outside
    // `crate::integrated`'s own fields is joined today (see this function's
    // own doc comment on the shutdown race being the only thing bounding
    // this loop's lifetime), which is a disclosed gap, not an oversight.
    let _ = crate::integrated::spawn_world_tick_task(&shutdown, async move {
        // Fresh, disposable sleep machinery: sleeping only skips the night in
        // the dimension a bed vote is counted in today (the overworld — see
        // `crate::sleep`'s own module doc), so a second, unconnected vote here
        // is the same "no producer reaches it" shape
        // `run_tick_loop`'s own wrapper already uses for its non-weather
        // callers, not a missing feature.
        let sleep_vote = SleepVote::new();
        let sleep_feed = SleepFeed::default();
        crate::tick::run_dimension_tick_loop_with_weather(
            runtime.mobs().clone(),
            runtime.entities().clone(),
            block_entities,
            Arc::new(TickClock::new()),
            world,
            block_tick_feed,
            tick_area,
            ExplosionFeed::default(),
            WeatherFeed::default(),
            WeatherState::default(),
            &sleep_vote,
            &sleep_feed,
            scheduled,
            world_state,
            follow,
            // Same "fresh, disposable, no producer reaches it"
            // shape as `sleep_vote`/`sleep_feed` above — the overworld is
            // the one dimension a `/worldborder` command (and a real player)
            // reaches today, so a per-dimension border here would be an
            // island by construction.
            crate::border::BorderFeed::default(),
            crate::tick::WorldClockRole::Follower,
        )
        .await;
    });
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::chunk::ChunkColumn;
    use crate::scheduled_tick::TickPriority;

    const MIN_Y: i32 = 0;
    const HEIGHT: i32 = 256;

    /// A single flat Nether-shaped column of netherrack, edited to carry a
    /// fire block at a known cell so a random tick has something to burn out
    /// — the real fire-block random-tick rule's no-neighbouring-flammable-block case removes
    /// fire outright, which is the predicted, non-round-number outcome this
    /// test checks for rather than merely "the block changed".
    #[derive(Debug, Default)]
    struct StubSource {
        edits: std::sync::Mutex<std::collections::HashMap<(i32, i32, i32), lodestone_data::block_states::StateId>>,
    }

    impl ChunkSource for StubSource {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
            for z in 0..16 {
                for x in 0..16 {
                    column.set_block_id(
                        x,
                        60,
                        z,
                        lodestone_data::block_states::StateId::from_state_str(
                            "minecraft:netherrack",
                        )
                        .expect("netherrack is canonical"),
                    );
                }
            }
            for (&(x, y, z), state) in self.edits.lock().expect("edits lock poisoned").iter() {
                let bcx = x.div_euclid(16);
                let bcz = z.div_euclid(16);
                if bcx == cx && bcz == cz {
                    column.set_block_id(x.rem_euclid(16), y, z.rem_euclid(16), *state);
                }
            }
            column
        }

        fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            self.column(cx, cz)
                .block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
        }

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            self.column(cx, cz)
                .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
                .to_string()
        }

        fn set_block(&self, x: i32, y: i32, z: i32, state: lodestone_data::block_states::StateId) {
            self.edits
                .lock()
                .expect("edits lock poisoned")
                .insert((x, y, z), state);
        }
    }

    /// **The discriminating fixture**: zero anchors in `Dimension::Nether`,
    /// exactly "nobody is standing in this dimension" — the premise
    /// `crate::tick_area::an_anchor_in_another_dimension_is_ignored` already
    /// proves the *follow area* handles correctly. This test proves the
    /// *loop* runs at all under that premise, which nothing before this
    /// change did: before `spawn_for_dimension` existed, no second loop was
    /// ever started, so a `ScheduledTickHandle` for the Nether could hold a
    /// due tick forever with nothing draining it.
    ///
    /// A control (below) proves the assertion is not vacuous: the same
    /// scheduled tick, left undrained, does not resolve on its own.
    #[tokio::test]
    async fn an_unattended_dimension_still_drains_its_own_scheduled_tick() {
        let source: Arc<dyn ChunkSource> = Arc::new(StubSource::default());
        let scheduled = ScheduledTickHandle::default();
        // A pending fluid-shaped tick at a cell nothing else touches, due
        // immediately (delay 0) so one loop iteration is enough to observe it
        // fire — the same "predict the tick, not the direction" standard as
        // every other gate in this crate: the assertion below is "it drained
        // to zero", not "it changed somehow".
        scheduled.with(|queues| {
            queues.fluid.schedule(
                (5, 60, 5),
                crate::scheduled_tick::ScheduledTickKind::Extension("minecraft:water".to_owned()),
                0,
                TickPriority::Normal,
            );
        });
        assert_eq!(
            pending_count(&scheduled),
            1,
            "the fixture must start with exactly the one tick this test schedules"
        );

        let world_state = WorldStateHandle::new();
        let shutdown = crate::integrated::ShutdownSignal::new();
        let ctx = DimensionTickContext {
            world_state: world_state.clone(),
            shutdown: Arc::clone(&shutdown),
        };
        // No anchor published at all: this is the "nobody is in the Nether"
        // case, relying entirely on the fallback area `spawn_for_dimension`
        // builds around the origin, which is where `(5, 60, 5)` (chunk
        // `(0, 0)`) falls.
        spawn_for_dimension(
            Dimension::Nether,
            source,
            BlockEntityHandle::default(),
            scheduled.clone(),
            BlockTickFeed::default(),
            &ctx,
        );

        // A few tick periods' worth of real waiting, polled rather than
        // slept-then-checked-once, so a slow CI box does not turn into a
        // flaky failure the way a single fixed sleep would.
        let mut drained = false;
        for _ in 0..40 {
            tokio::time::sleep(crate::tick::TICK_PERIOD).await;
            if pending_count(&scheduled) == 0 {
                drained = true;
                break;
            }
        }
        shutdown.trigger();
        assert!(
            drained,
            "a fluid tick queued in a dimension with no player anchor must still be \
             drained by that dimension's own tick loop within a few ticks"
        );
    }

    /// The control: the same scheduled tick, on the same kind of handle, with
    /// no loop ever spawned to drain it. Proves the assertion above is not
    /// vacuously true (e.g. from an empty queue reading as "drained" by
    /// construction) — an undrained handle must still report the pending
    /// tick after the same wait the real test uses.
    #[tokio::test]
    async fn the_control_an_undrained_handle_never_reports_zero_on_its_own() {
        let scheduled = ScheduledTickHandle::default();
        scheduled.with(|queues| {
            queues.fluid.schedule(
                (5, 60, 5),
                crate::scheduled_tick::ScheduledTickKind::Extension("minecraft:water".to_owned()),
                0,
                TickPriority::Normal,
            );
        });
        tokio::time::sleep(crate::tick::TICK_PERIOD * 40).await;
        assert_eq!(
            pending_count(&scheduled),
            1,
            "with no loop draining it, the tick must still be pending"
        );
    }

    /// Pending fluid ticks still in the queue, regardless of whether they are
    /// due yet — `drain_due(0, usize::MAX)` would also remove them, which is
    /// exactly what this test must not do to its own fixture from the polling
    /// loop, so this reads via the non-destructive [`ScheduledTickQueue::iter`].
    fn pending_count(scheduled: &ScheduledTickHandle) -> usize {
        scheduled.with(|queues| queues.fluid.iter().count())
    }

    #[tokio::test]
    async fn sibling_ticks_refresh_idle_presence_without_advancing_time_or_consuming_weather() {
        let world = WorldStateHandle::new();
        world.set_rule("spawn_mobs", "false").unwrap();
        world.set_day_time(13_670);
        let request = crate::world_state::WeatherRequest::Rain { duration: 37 };
        world.request_weather(request);
        let before = world.time();
        let runtime = world.ensure_dimension_runtime(Dimension::Nether);
        let players = world.player_registry();
        let foreign = players.join("Foreign", uuid::Uuid::from_u128(1), lodestone_model::Vec3::new(8.5, 61.0, 8.5));
        let first = players.join_in_dimension("First", uuid::Uuid::from_u128(2), lodestone_model::Vec3::new(-17.25, 61.0, 8.5), Dimension::Nether);
        let second = players.join_in_dimension("Second", uuid::Uuid::from_u128(3), lodestone_model::Vec3::new(8.5, 61.0, -33.75), Dimension::Nether);
        let mut inventory = crate::inventory::PlayerInventory::new();
        inventory.set_native(0, Some(lodestone_model::ItemStack::new("minecraft:wheat".parse().unwrap(), 1)));
        players.set_inventory(first.uuid(), &inventory);
        players.set_rotation(first.entity_id(), lodestone_model::Rotation { yaw: 90.0, pitch: 0.0 });
        let (horse, boat) = runtime.mobs().with(|sim| {
            let horse = sim.spawn_species("minecraft:horse".parse().unwrap(), lodestone_model::Vec3::new(-17.25, 61.0, 8.5)).id();
            let boat = sim.spawn_vehicle("minecraft:oak_boat".parse().unwrap(), lodestone_model::Vec3::new(8.5, 61.0, -33.75), 0.0);
            assert!(sim.mount_mob(horse, first.entity_id()));
            assert!(sim.mount_vehicle(boat, second.entity_id(), false));
            (horse, boat)
        });
        let expected = players.perceptions(Dimension::Nether);
        assert_eq!(expected.len(), 2);
        assert_eq!(world.tick_anchors().snapshot().len(), 3);
        assert!(runtime.mobs().with(|sim| sim.players().is_empty()), "the unticked control has no perception input");
        let shutdown = crate::integrated::ShutdownSignal::new();
        let context = DimensionTickContext { world_state: world.clone(), shutdown: Arc::clone(&shutdown) };
        spawn_for_dimension(Dimension::Nether, Arc::new(StubSource::default()), BlockEntityHandle::default(),
            ScheduledTickHandle::default(), BlockTickFeed::default(), &context);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while runtime.mobs().with(|sim| sim.players() != expected || sim.tick_count() == 0) {
                tokio::time::sleep(crate::tick::TICK_PERIOD).await;
            }
        }).await.expect("idle joins must reach the actual sibling tick");
        assert_eq!(world.time(), before);
        assert_eq!(world.take_weather_request(), Some(request));
        assert_eq!(expected[0].perception.held_item.as_ref().unwrap().to_string(), "minecraft:wheat");
        assert!((expected[0].perception.view_direction.x + 1.0).abs() < 1e-12);
        assert_eq!(runtime.mobs().with(|sim| sim.mob_rider(horse)), Some(first.entity_id()),
            "the connected rider control must remain mounted");
        assert_eq!(runtime.mobs().with(|sim| sim.vehicle_rider(boat)), Some(second.entity_id()));
        drop(first);
        drop(second);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !runtime.mobs().with(|sim| sim.players().is_empty()) {
                tokio::time::sleep(crate::tick::TICK_PERIOD).await;
            }
        }).await.expect("disconnect must clear the tick's perception input");
        assert_eq!(runtime.mobs().with(|sim| sim.mob_rider(horse)), None,
            "the last departing rider must not freeze a mob");
        assert_eq!(runtime.mobs().with(|sim| sim.vehicle_rider(boat)), None,
            "the last departing rider must not freeze a vehicle");
        shutdown.trigger();
        assert_eq!(world.tick_anchors().snapshot().len(), 1);
        drop(foreign);
        assert!(world.tick_anchors().snapshot().is_empty());
    }

    struct NaturalTerrain {
        dimension: Dimension,
        column: ChunkColumn,
    }

    impl ChunkSource for NaturalTerrain {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            assert!((-3..=3).contains(&cx) && (-3..=3).contains(&cz), "fixture generation escaped its 49-column territory: {cx},{cz}");
            self.column.clone()
        }
        fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
            self.column.block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
        }
        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            self.column.biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16)).to_owned()
        }
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {
            panic!("natural-spawn fixture terrain is immutable");
        }
        fn dimension(&self) -> Option<Dimension> { Some(self.dimension) }
    }

    struct NaturalSource {
        store: crate::chunk_store::ChunkStore<NaturalTerrain>,
        resident_reads: std::sync::atomic::AtomicUsize,
    }

    impl ChunkSource for NaturalSource {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn { self.store.column(cx, cz) }
        fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> { self.store.resident_column(cx, cz) }
        fn try_resident_column(&self, cx: i32, cz: i32) -> Option<crate::chunk_store::TryResident<ChunkColumn>> {
            self.resident_reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(self.store.try_resident_column(cx, cz))
        }
        fn try_resident_column_presence(&self, cx: i32, cz: i32) -> Option<crate::chunk_store::TryResident<()>> {
            Some(self.store.try_resident_column_presence(cx, cz))
        }
        fn is_column_resident(&self, cx: i32, cz: i32) -> bool { self.store.is_column_resident(cx, cz) }
        fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId { self.store.block_state_id(x, y, z) }
        fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<lodestone_data::block_states::StateId> {
            self.store.resident_block_state_id(x, y, z)
        }
        fn try_resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<crate::chunk_store::TryResident<lodestone_data::block_states::StateId>> {
            Some(self.store.try_resident_block_state_id(x, y, z))
        }
        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String { self.store.biome_state_at(x, y, z) }
        fn set_block(&self, x: i32, y: i32, z: i32, state: lodestone_data::block_states::StateId) { self.store.set_block(x, y, z, state); }
        fn dimension(&self) -> Option<Dimension> { self.store.dimension() }
        fn ticket_store(&self) -> Option<crate::ticket::TicketStoreHandle> { Some(self.store.tickets()) }
        fn reconcile_ticket_residency(&self) { self.store.reconcile_ticket_residency(); }
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_natural_spawning_uses_stationary_presence_in_every_dimension() {
        use crate::chunk_store::TryResident;
        use crate::mob_spawn::MobCategory;
        use crate::server::EntitySource;
        use lodestone_data::block_states::StateId;
        use std::sync::atomic::Ordering;

        for dimension in [Dimension::Overworld, Dimension::Nether, Dimension::End] {
            let (floor, ground, biome, category, allowed): (_, _, _, _, &[&str]) = match dimension {
                Dimension::Overworld => (70, "minecraft:grass_block", "minecraft:plains", MobCategory::Creature,
                    &["minecraft:sheep", "minecraft:pig", "minecraft:chicken", "minecraft:cow", "minecraft:horse", "minecraft:donkey"]),
                Dimension::Nether => (60, "minecraft:netherrack", "minecraft:crimson_forest", MobCategory::Monster,
                    &["minecraft:hoglin", "minecraft:piglin", "minecraft:zombified_piglin"]),
                Dimension::End => (60, "minecraft:end_stone", "minecraft:the_end", MobCategory::Monster,
                    &["minecraft:enderman"]),
            };
            let ground = StateId::from_state_str(ground).unwrap();
            let mut column = ChunkColumn::new(dimension.min_y(), dimension.height());
            for y in dimension.min_y()..=floor {
                for z in 0..16 {
                    for x in 0..16 { column.set_block_id(x, y, z, ground); }
                }
            }
            for qy in 0..column.biome_y_quarts() {
                for qz in 0..4 {
                    for qx in 0..4 { column.set_biome_cell(qx, qy, qz, biome); }
                }
            }
            column.prime_client_heightmaps();
            let source = Arc::new(NaturalSource {
                store: crate::chunk_store::ChunkStore::with_capacity(NaturalTerrain { dimension, column }, 49),
                resident_reads: std::sync::atomic::AtomicUsize::new(0),
            });
            let tickets = source.store.tickets();
            let admission = tickets.grant_player(91, (0, 0), 3);
            tickets.tick();
            for cz in -3..=3 {
                for cx in -3..=3 {
                    assert_eq!(tickets.status((cx, cz)), crate::ticket::ChunkStatus::Full);
                    assert!(tickets.is_simulating((cx, cz)));
                    source.column(cx, cz);
                    let Some(TryResident::Present(resident)) = source.try_resident_column(cx, cz) else { panic!("admitted column must be resident"); };
                    assert_eq!(resident.generation_stage(), crate::chunk::ChunkGenerationStage::Full);
                    assert_eq!(resident.block_state_id(0, floor, 0), ground);
                    assert_eq!(resident.biome_state_at(0, floor + 1, 0), biome);
                }
            }
            assert_eq!(source.store.len(), 49);
            assert_eq!(source.store.generated(), 49);
            assert!(matches!(source.try_resident_column(4, 0), Some(TryResident::Absent)));
            assert_eq!(source.store.generated(), 49, "resident absence cannot start generation");
            source.resident_reads.store(0, Ordering::Relaxed);

            let world = WorldStateHandle::new();
            for (rule, value) in [("random_tick_speed", "0"), ("spawn_patrols", "false"), ("spawn_wandering_traders", "false"),
                ("spawn_phantoms", "false"), ("advance_time", "false"), ("advance_weather", "false")] {
                world.set_rule(rule, value).unwrap();
            }
            assert!(world.set_difficulty(lodestone_model::Difficulty::Normal));
            let runtime = world.ensure_dimension_runtime(dimension);
            let position = lodestone_model::Vec3::new(8.5, f64::from(floor + 1), 8.5);
            let foreign_dimension = if dimension == Dimension::Overworld { Dimension::Nether } else { Dimension::Overworld };
            let foreign = world.player_registry().join_in_dimension("Foreign", uuid::Uuid::from_u128(11), position, foreign_dimension);
            assert_eq!(world.player_registry().perceptions(foreign_dimension).len(), 1);
            assert!(world.player_registry().perceptions(dimension).is_empty());
            let clock = Arc::new(TickClock::new());
            let tick_clock = Arc::clone(&clock);
            let tick_source = Arc::clone(&source);
            let tick_world = world.clone();
            let tick_runtime = Arc::clone(&runtime);
            let task = tokio::spawn(async move {
                let sleep_vote = SleepVote::new();
                let sleep_feed = SleepFeed::default();
                let follow = TickFollow { dimension, radius: 3, anchors: tick_world.tick_anchors().clone() };
                if dimension == Dimension::Overworld {
                    let mut server_world = crate::ecs::ServerApp::bootstrap().into_world();
                    let snapshot: Arc<dyn ChunkSource> = tick_source.clone();
                    server_world.insert_resource(crate::ecs::ServerWorldSnapshot::new(snapshot));
                    crate::tick::run_primary_tick_loop_with_weather(server_world,
                        tick_runtime.mobs().clone(), tick_runtime.entities().clone(), BlockEntityHandle::default(), tick_clock,
                        tick_source, BlockTickFeed::default(), (-3..=3, -3..=3), ExplosionFeed::default(), WeatherFeed::default(),
                        WeatherState::default(), &sleep_vote, &sleep_feed, ScheduledTickHandle::default(), tick_world, follow,
                        crate::border::BorderFeed::default()).await;
                } else {
                    crate::tick::run_dimension_tick_loop_with_weather(
                        tick_runtime.mobs().clone(), tick_runtime.entities().clone(), BlockEntityHandle::default(), tick_clock,
                        tick_source, BlockTickFeed::default(), (-3..=3, -3..=3), ExplosionFeed::default(), WeatherFeed::default(),
                        WeatherState::default(), &sleep_vote, &sleep_feed, ScheduledTickHandle::default(), tick_world, follow,
                        crate::border::BorderFeed::default(), crate::tick::WorldClockRole::Follower).await;
                }
            });
            tokio::task::yield_now().await;
            for _ in 0..10 {
                tokio::time::advance(crate::tick::TICK_PERIOD).await;
                tokio::task::yield_now().await;
            }
            assert_eq!(clock.tick_count(), 10, "the withheld-presence control must actually tick: {dimension:?}");
            assert_eq!(runtime.mobs().with(|sim| sim.tick_count()), 10);
            assert!(runtime.mobs().with(|sim| sim.players().is_empty() && sim.is_empty()));
            assert_eq!(runtime.mobs().with(|sim| sim.census(289).count(category)), 0);
            assert!(runtime.entities().snapshots().is_empty());
            let withheld_reads = source.resident_reads.load(Ordering::Relaxed);
            assert!(withheld_reads > 0, "the empty-population control must still exercise resident terrain work");

            let player = world.player_registry().join_in_dimension("Idle", uuid::Uuid::from_u128(12), position, dimension);
            let mut positive_ticks = 0;
            // Creatures only open every 400th game tick, so allow two openings.
            for _ in 0..820 {
                tokio::time::advance(crate::tick::TICK_PERIOD).await;
                tokio::task::yield_now().await;
                positive_ticks += 1;
                if !runtime.entities().snapshots().is_empty() { break; }
            }
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            let published = runtime.entities().snapshots();
            let census = runtime.mobs().with(|sim| sim.census(289));
            assert!(!published.is_empty(), "stationary natural spawning failed: {dimension:?}, ticks={positive_ticks}, reads={}, resident={}, census={}",
                source.resident_reads.load(Ordering::Relaxed), source.store.len(), census.count(category));
            assert_eq!(clock.tick_count(), 10 + positive_ticks);
            assert_eq!(runtime.mobs().with(|sim| sim.players().len()), 1);
            assert!(source.resident_reads.load(Ordering::Relaxed) >= withheld_reads + 49, "the positive control must exercise the 49-column spawn snapshot");
            assert_eq!(source.store.len(), 49);
            assert_eq!(source.store.generated(), 49, "ticks must not generate more fixture columns");
            assert_eq!(census.count(category), published.len() as i32);
            assert_eq!(census.global_cap(category), if category == MobCategory::Creature { 10 } else { 70 });
            assert!(census.count(category) <= census.global_cap(category));
            for entity in published {
                assert!(allowed.contains(&entity.entity_type.to_string().as_str()), "unexpected natural species in {dimension:?}: {}", entity.entity_type);
                assert_eq!(entity.position.y, f64::from(floor + 1));
                let x = entity.position.x.floor() as i32;
                let z = entity.position.z.floor() as i32;
                assert_eq!(source.resident_block_state_id(x, floor, z), Some(ground));
                assert_eq!(source.resident_block_state_id(x, floor + 1, z), Some(StateId::AIR));
                assert_eq!(source.resident_block_state_id(x, floor + 2, z), Some(StateId::AIR));
                let distance = (entity.position.x - position.x).powi(2) + (entity.position.z - position.z).powi(2);
                assert!(distance > 24.0 * 24.0 && distance <= 128.0 * 128.0);
                runtime.mobs().with(|sim| {
                    let mob = sim.get(entity.id).expect("publication must name a live natural mob");
                    assert_eq!(mob.entity_type(), &entity.entity_type);
                    assert_eq!(mob.position(), entity.position);
                    assert_eq!(mob.category(), category);
                });
            }
            drop(player);
            drop(foreign);
            drop(admission);
        }
    }
}
