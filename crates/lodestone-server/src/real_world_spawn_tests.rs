//! Natural spawning end to end over the real 26.3 overworld generator.
//!
//! The server tick loop runs against generated terrain around a coastline
//! (seed 1: ocean west of chunk x 3, forest and beaches east of it, along chunk z 8) with one
//! stationary player. Nothing is stubbed between the tick loop and the mob
//! simulation: the player registry, the follow area, the residency tickets, the
//! real biome spawn lists, the light engine and the placement rules are all the
//! production ones.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::block_entities::BlockEntityHandle;
use crate::chunk::ChunkSource;
use crate::dimension::Dimension;
use crate::mob_spawn::MobCategory;
use crate::server::EntitySource;
use crate::scheduled_tick::ScheduledTickHandle;
use crate::sleep::{SleepFeed, SleepVote};
use crate::tick::{BlockTickFeed, ExplosionFeed, TickClock};
use crate::tick_area::TickFollow;
use crate::weather::{WeatherFeed, WeatherState};
use crate::world_state::WorldStateHandle;

const SEED: i64 = 1;
/// The chunk the player stands in: on the coastline, the sea to its west.
const PLAYER_CHUNK: (i32, i32) = (4, 8);

/// What a run left alive: species counts, and the first tick each category appeared.
struct Outcome {
    species: BTreeMap<String, usize>,
    categories: HashMap<MobCategory, usize>,
    first_seen: HashMap<MobCategory, u64>,
    snapshots: usize,
}

async fn run(terrain: &Arc<crate::chunk::Terrain263ChunkSource>, spawn_mobs: bool, ticks: u64) -> Outcome {
    let (px, pz) = PLAYER_CHUNK;
    let window: Vec<(i32, i32)> = (pz - 3..=pz + 3)
        .flat_map(|cz| (px - 3..=px + 3).map(move |cx| (cx, cz)))
        .collect();
    let source = Arc::new(crate::chunk_store::ChunkStore::with_capacity(Arc::clone(terrain), 49));
    let tickets = source.tickets();
    let admission = tickets.grant_player(91, PLAYER_CHUNK, 3);
    tickets.tick();
    for &(cx, cz) in &window {
        source.column(cx, cz);
    }
    assert_eq!(source.len(), 49);

    let world = WorldStateHandle::new();
    for (rule, value) in [
        ("random_tick_speed", "0"),
        ("spawn_patrols", "false"),
        ("spawn_wandering_traders", "false"),
        ("spawn_phantoms", "false"),
        ("advance_time", "false"),
        ("advance_weather", "false"),
    ] {
        world.set_rule(rule, value).unwrap();
    }
    if !spawn_mobs {
        world.set_rule("spawn_mobs", "false").unwrap();
    }
    assert!(world.set_difficulty(lodestone_model::Difficulty::Normal));
    let runtime = world.ensure_dimension_runtime(Dimension::Overworld);
    // Sea level is 63; the player stands at the middle of its chunk, at sea level plus one.
    let position = lodestone_model::Vec3::new(f64::from(px * 16) + 8.5, 64.0, f64::from(pz * 16) + 8.5);
    let player = world
        .player_registry()
        .join_in_dimension("Coast", uuid::Uuid::from_u128(21), position, Dimension::Overworld);

    let clock = Arc::new(TickClock::new());
    let tick_clock = Arc::clone(&clock);
    let tick_source = Arc::clone(&source);
    let tick_world = world.clone();
    let tick_runtime = Arc::clone(&runtime);
    let task = tokio::spawn(async move {
        let sleep_vote = SleepVote::new();
        let sleep_feed = SleepFeed::default();
        let follow = TickFollow {
            dimension: Dimension::Overworld,
            radius: 3,
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

    let mut first_seen = HashMap::new();
    for tick in 1..=ticks {
        tokio::time::advance(crate::tick::TICK_PERIOD).await;
        tokio::task::yield_now().await;
        if tick % 10 == 0 {
            runtime.mobs().with(|sim| {
                for mob in sim.iter() {
                    first_seen.entry(mob.category()).or_insert(tick);
                }
            });
        }
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(clock.tick_count() >= ticks, "the loop must have run every requested tick");

    let mut species = BTreeMap::new();
    let mut categories = HashMap::new();
    runtime.mobs().with(|sim| {
        for mob in sim.iter() {
            *species.entry(mob.entity_type().to_string()).or_insert(0) += 1;
            *categories.entry(mob.category()).or_insert(0) += 1;
        }
    });
    let snapshots = runtime.entities().snapshots().len();
    drop(player);
    drop(admission);
    Outcome { species, categories, first_seen, snapshots }
}

/// With the rule on, the sea and the land beside it both fill up; with it off the
/// same world and the same tick count leave nothing.
#[tokio::test(start_paused = true)]
async fn a_coastline_fills_with_water_mobs_and_animals() {
    let terrain = Arc::new(crate::overworld_chunk_source(SEED));
    let (px, pz) = PLAYER_CHUNK;
    let window: Vec<(i32, i32)> = (pz - 3..=pz + 3)
        .flat_map(|cz| (px - 3..=px + 3).map(move |cx| (cx, cz)))
        .collect();
    // Generation dominates the cost; do it once, in parallel.
    drop(terrain.columns(&window));

    // Four openings of the persistent categories (game ticks 0, 400, 800, 1200).
    // Land animals come mostly from generation: a natural creature attempt only
    // lands on the one layer of grass in roughly a hundred and thirty candidate
    // heights, and only every 400th tick.
    let on = run(&terrain, true, 1_250).await;
    eprintln!("species {:?}\ncategories {:?}\nfirst seen {:?}", on.species, on.categories, on.first_seen);

    let water_ambient = on.categories.get(&MobCategory::WaterAmbient).copied().unwrap_or(0);
    let water_creature = on.categories.get(&MobCategory::WaterCreature).copied().unwrap_or(0);
    let creature = on.categories.get(&MobCategory::Creature).copied().unwrap_or(0);
    assert!(water_ambient >= 1, "no fish after 1250 ticks beside an ocean: {:?}", on.species);
    assert!(water_creature >= 1, "no squid or dolphin after 1250 ticks beside an ocean: {:?}", on.species);
    assert!(creature >= 1, "no land animal after 1250 ticks beside a forest (generation-time animals): {:?}", on.species);
    // The reference caps for one player (17x17 = 289 chunks scale nothing down).
    assert!(water_ambient <= 20 && water_creature <= 5 && creature <= 10, "{:?}", on.categories);
    assert!(on.snapshots >= water_ambient + water_creature + creature);

    // Control: the same world with the rule off, run through the whole span the
    // first mob took to appear plus a margin.
    let first = on.first_seen.values().copied().min().expect("the run spawned something");
    let off = run(&terrain, false, first + 400).await;
    assert!(off.species.is_empty() && off.snapshots == 0, "spawn_mobs=false still spawned {:?}", off.species);
}
