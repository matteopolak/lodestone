//! The follow radius comes from the configured simulation distance, and the
//! natural-spawn density it produces over a real generated world.
//!
//! The tick loop runs against the real 26.3 overworld generator with one
//! stationary player at night. Nothing between the tick loop and the mob
//! simulation is stubbed, so the spread of the monsters it leaves alive is the
//! spread the candidate-chunk rule produces.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::block_entities::BlockEntityHandle;
use crate::chunk::ChunkSource;
use crate::dimension::Dimension;
use crate::mob_spawn::MobCategory;
use crate::scheduled_tick::ScheduledTickHandle;
use crate::sleep::{SleepFeed, SleepVote};
use crate::tick::{BlockTickFeed, ExplosionFeed, TickClock};
use crate::tick_area::{FollowArea, SPAWN_DISTANCE_CHUNKS, TickAnchor, TickFollow};
use crate::weather::{WeatherFeed, WeatherState};
use crate::world_state::WorldStateHandle;

const SEED: i64 = 1;
/// Inland of the coast `real_world_spawn_tests` uses: forest and beaches run east.
const PLAYER_CHUNK: (i32, i32) = (14, 8);
/// The streamed view the player holds resident, wider than the default
/// simulation distance so the whole follow square is resident.
const VIEW_RADIUS: i32 = 10;

/// A player standing at the origin chunk of an otherwise empty overworld loop.
fn area_at_origin(world: &WorldStateHandle) -> FollowArea {
    world.tick_anchors().publish(vec![TickAnchor { dimension: Dimension::Overworld, cx: 0, cz: 0 }]);
    FollowArea::new(
        TickFollow {
            dimension: Dimension::Overworld,
            radius: world.simulation_distance(),
            anchors: world.tick_anchors().clone(),
        },
        0..=0,
        0..=0,
    )
}

/// The default follow area is the 21x21 square of simulation distance 10, and a
/// configured distance replaces it. The expected counts are arithmetic over the
/// reference rules: `(2 * 10 + 1)^2` columns, and the lattice points strictly
/// inside a circle of radius 8 chunks (128 blocks) for spawn candidates.
#[test]
fn the_follow_area_is_the_configured_simulation_distance() {
    let world = WorldStateHandle::new();
    assert_eq!(world.simulation_distance(), 10, "the reference default");
    let mut area = area_at_origin(&world);
    assert!(area.recompute());
    assert_eq!(area.chunks().len(), 21 * 21);
    // Points (dx, dz) with dx^2 + dz^2 < 64: the 197 with <= 64 less the four
    // axis points at exactly 8 chunks.
    assert_eq!(area.spawn_candidate_chunks().len(), 193);
    assert_eq!(area.spawn_cap_chunks(), 17 * 17);

    // Past the spawn distance only the follow square grows; candidates do not.
    world.set_simulation_distance(14);
    assert!(area.recompute_with_radius(world.simulation_distance()));
    assert_eq!(area.chunks().len(), 29 * 29);
    assert_eq!(area.spawn_candidate_chunks().len(), 193);

    // Inside it the candidates are the followed chunks only.
    world.set_simulation_distance(3);
    assert!(area.recompute_with_radius(world.simulation_distance()));
    assert_eq!(area.chunks().len(), 7 * 7);
    // Lattice points of the 7x7 square (|d| <= 3) all have d^2 <= 18 < 64.
    assert_eq!(area.spawn_candidate_chunks().len(), 49);

    // The setting is clamped to the property's range.
    world.set_simulation_distance(1000);
    assert_eq!(world.simulation_distance(), crate::chunk_store::MAX_SIMULATION_DISTANCE);
    world.set_simulation_distance(-4);
    assert_eq!(world.simulation_distance(), crate::chunk_store::MIN_SIMULATION_DISTANCE);
}

/// What a night's run left alive, and what each tick cost.
struct Outcome {
    /// Chunk-space offset (Chebyshev, then Euclidean squared) of every monster
    /// from the player's chunk.
    offsets: Vec<(i32, i32)>,
    tick_times: Vec<Duration>,
}

impl Outcome {
    fn beyond(&self, chebyshev: i32) -> usize {
        self.offsets.iter().filter(|&&(dx, dz)| dx.abs().max(dz.abs()) > chebyshev).count()
    }

    fn farthest(&self) -> i32 {
        self.offsets.iter().map(|&(dx, dz)| dx.abs().max(dz.abs())).max().unwrap_or(0)
    }

    fn tick_stats(&self) -> (Duration, Duration, Duration) {
        let mut sorted = self.tick_times.clone();
        sorted.sort_unstable();
        let mean = sorted.iter().sum::<Duration>() / sorted.len() as u32;
        (mean, sorted[sorted.len() * 99 / 100], *sorted.last().unwrap())
    }
}

async fn run(
    terrain: &Arc<crate::chunk::Terrain263ChunkSource>,
    simulation_distance: i32,
    random_tick_speed: &str,
    ticks: u64,
) -> Outcome {
    let (px, pz) = PLAYER_CHUNK;
    let window: Vec<(i32, i32)> = (pz - VIEW_RADIUS..=pz + VIEW_RADIUS)
        .flat_map(|cz| (px - VIEW_RADIUS..=px + VIEW_RADIUS).map(move |cx| (cx, cz)))
        .collect();
    let source = Arc::new(crate::chunk_store::ChunkStore::with_capacity(Arc::clone(terrain), window.len() + 1));
    let tickets = source.tickets();
    let admission = tickets.grant_player(91, PLAYER_CHUNK, VIEW_RADIUS);
    tickets.tick();
    for &(cx, cz) in &window {
        source.column(cx, cz);
    }
    assert_eq!(source.len(), window.len());

    let world = WorldStateHandle::new();
    world.set_simulation_distance(simulation_distance);
    for (rule, value) in [
        ("random_tick_speed", random_tick_speed),
        ("spawn_patrols", "false"),
        ("spawn_wandering_traders", "false"),
        ("spawn_phantoms", "false"),
        ("advance_time", "false"),
        ("advance_weather", "false"),
    ] {
        world.set_rule(rule, value).unwrap();
    }
    assert!(world.set_difficulty(lodestone_model::Difficulty::Normal));
    // Midnight: monsters spawn under open sky.
    world.set_day_time(18_000);
    let runtime = world.ensure_dimension_runtime(Dimension::Overworld);
    let position = lodestone_model::Vec3::new(f64::from(px * 16) + 8.5, 80.0, f64::from(pz * 16) + 8.5);
    let player = world
        .player_registry()
        .join_in_dimension("Inland", uuid::Uuid::from_u128(22), position, Dimension::Overworld);

    let clock = Arc::new(TickClock::new());
    let tick_clock = Arc::clone(&clock);
    let tick_source = Arc::clone(&source);
    let tick_world = world.clone();
    let tick_runtime = Arc::clone(&runtime);
    let task = tokio::spawn(async move {
        let sleep_vote = SleepVote::new();
        let sleep_feed = SleepFeed::default();
        // The radius here is the loop's initial value; the configured distance
        // on the world state replaces it from the first tick.
        let follow = TickFollow {
            dimension: Dimension::Overworld,
            radius: crate::chunk_store::FALLBACK_TICK_RADIUS,
            anchors: tick_world.tick_anchors().clone(),
        };
        let mut server_world = crate::ecs::ServerApp::bootstrap().into_world();
        let snapshot: Arc<dyn ChunkSource> = tick_source.clone();
        server_world.insert_resource(crate::ecs::ServerWorldSnapshot::new(snapshot));
        crate::tick::run_primary_tick_loop_with_weather(
            server_world,
            tick_runtime.mobs().clone(),
            tick_runtime.entities().clone(),
            BlockEntityHandle::default(),
            tick_clock,
            tick_source,
            BlockTickFeed::default(),
            (px - 3..=px + 3, pz - 3..=pz + 3),
            ExplosionFeed::default(),
            WeatherFeed::default(),
            WeatherState::default(),
            &sleep_vote,
            &sleep_feed,
            ScheduledTickHandle::default(),
            tick_world,
            follow,
            crate::border::BorderFeed::default(),
        )
        .await;
    });
    tokio::task::yield_now().await;

    let mut tick_times = Vec::with_capacity(ticks as usize);
    for _ in 1..=ticks {
        let started = Instant::now();
        tokio::time::advance(crate::tick::TICK_PERIOD).await;
        tokio::task::yield_now().await;
        tick_times.push(started.elapsed());
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(clock.tick_count() >= ticks, "the loop must have run every requested tick");

    let mut offsets = Vec::new();
    runtime.mobs().with(|sim| {
        for mob in sim.iter().filter(|mob| mob.category() == MobCategory::Monster) {
            let at = mob.position();
            offsets.push(((at.x / 16.0).floor() as i32 - px, (at.z / 16.0).floor() as i32 - pz));
        }
    });
    drop(player);
    drop(admission);
    Outcome { offsets, tick_times }
}

/// The reference spreads monsters over the chunks near the player; the old
/// 7x7 follow square confined them to three chunks.
fn spreads_like_the_reference(outcome: &Outcome) -> bool {
    let total = outcome.offsets.len();
    // Natural monsters fill a circle of 8 chunks (a few drift past it), so most
    // of them stand more than three chunks out and none stands beyond the
    // spawn distance plus a mob's wander.
    total >= 20 && outcome.beyond(3) * 2 >= total && outcome.farthest() <= SPAWN_DISTANCE_CHUNKS + 1
}

async fn generated_world() -> Arc<crate::chunk::Terrain263ChunkSource> {
    let terrain = Arc::new(crate::overworld_chunk_source(SEED));
    let (px, pz) = PLAYER_CHUNK;
    let window: Vec<(i32, i32)> = (pz - VIEW_RADIUS..=pz + VIEW_RADIUS)
        .flat_map(|cz| (px - VIEW_RADIUS..=px + VIEW_RADIUS).map(move |cx| (cx, cz)))
        .collect();
    drop(terrain.columns(&window));
    terrain
}

/// Monster density follows the reference, with the old 49-column square as the
/// control: the same world and ticks leave every monster inside the old box.
#[tokio::test(start_paused = true)]
async fn monsters_spread_over_the_spawn_distance_not_the_old_box() {
    let terrain = generated_world().await;
    let wide = run(&terrain, crate::chunk_store::DEFAULT_SIMULATION_DISTANCE, "0", 400).await;
    eprintln!(
        "simulation distance 10: {} monsters, {} beyond 3 chunks, farthest {}",
        wide.offsets.len(), wide.beyond(3), wide.farthest(),
    );
    assert!(spreads_like_the_reference(&wide), "{:?}", wide.offsets);

    // Control: the old constant. The detector must reject this run, so a pass
    // above is a measurement of the radius and not of an always-true predicate.
    let narrow = run(&terrain, 3, "0", 400).await;
    eprintln!(
        "simulation distance 3: {} monsters, {} beyond 3 chunks, farthest {}",
        narrow.offsets.len(), narrow.beyond(3), narrow.farthest(),
    );
    assert!(narrow.offsets.len() >= 20, "the control must still spawn monsters: {:?}", narrow.offsets);
    assert_eq!(narrow.beyond(3), 0, "a radius-3 follow square cannot place a monster past 3 chunks");
    assert!(!spreads_like_the_reference(&narrow));
}

/// Tick cost at the old radius and the default, over the same resident world
/// with the default random tick speed. Run with
/// `cargo test -p lodestone-server --lib simulation_distance_tests::tick_cost -- --ignored --nocapture`.
#[tokio::test(start_paused = true)]
#[ignore = "measurement"]
async fn tick_cost() {
    let terrain = generated_world().await;
    for distance in [3, 10, 10, 3] {
        let outcome = run(&terrain, distance, "3", 600).await;
        let (mean, p99, max) = outcome.tick_stats();
        eprintln!(
            "simulation distance {distance:>2}: mean {mean:?}  p99 {p99:?}  max {max:?}  monsters {}",
            outcome.offsets.len(),
        );
    }
}
