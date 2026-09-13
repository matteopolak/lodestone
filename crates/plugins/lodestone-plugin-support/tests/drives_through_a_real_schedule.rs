//! The "what consumes this" gate `CLAUDE.md`'s island rule asks for:
//! [`EntityDataStore`]/[`ChunkDataStore`] are just `HashMap`-backed
//! `Resource`s, so a unit test calling `.set()`/`.get()` directly proves the
//! container works but nothing about whether a real plugin *system*, run
//! through the real `GameTick` schedule, can see it. This test builds a real
//! `bevy_ecs` `App` with `lodestone_ecs::CorePlugin` +
//! [`PersistentDataPlugin`] + a toy consumer plugin, and drives `GameTick`
//! for real ticks — the same idiom `lodestone-autopilot`'s own gate uses for
//! the same reason (`docs/plugin-api.md`'s "what actually consumes this").

use bevy_ecs::entity::Entity;
use bevy_ecs::system::{Query, ResMut};
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::entity::MinecraftEntityId;
use lodestone_ecs::GameTick;
use lodestone_plugin_support::{ChunkDataStore, EntityDataStore, PersistentDataPlugin};
use lodestone_world::ChunkPos;

const TICKS_KEY: &str = "toy-economy:ticks_alive";
const VISITS_KEY: &str = "toy-worldedit:visits";

/// A toy consumer: increments a per-entity tick counter and a per-chunk visit
/// counter every `GameTick`, exactly the shape a real economy or protection
/// plugin would use this store for.
#[derive(Debug, Default)]
struct ToyConsumerPlugin;

impl Plugin for ToyConsumerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(GameTick, (tick_entity_counters, tick_chunk_visits));
    }
}

fn tick_entity_counters(query: Query<&MinecraftEntityId>, mut store: ResMut<EntityDataStore>) {
    for id in &query {
        let current: u32 = store.get(*id, TICKS_KEY).unwrap_or(0);
        store.set(*id, TICKS_KEY, &(current + 1)).unwrap();
    }
}

fn tick_chunk_visits(mut store: ResMut<ChunkDataStore>) {
    let pos = ChunkPos::new(0, 0);
    let current: u32 = store.get(pos, VISITS_KEY).unwrap_or(0);
    store.set(pos, VISITS_KEY, &(current + 1)).unwrap();
}

fn spawn_entity(app: &mut App, id: i32) -> Entity {
    app.world_mut().spawn(MinecraftEntityId(id)).id()
}

fn run_ticks(app: &mut App, n: u32) {
    for _ in 0..n {
        app.world_mut().run_schedule(GameTick);
    }
}

fn new_app() -> App {
    let mut app = App::new();
    app.add_plugins((
        lodestone_ecs::CorePlugin,
        PersistentDataPlugin,
        ToyConsumerPlugin,
    ));
    app
}

#[test]
fn a_real_system_on_the_real_schedule_accumulates_per_entity_state() {
    let mut app = new_app();

    let alice = spawn_entity(&mut app, 1);
    let bob = spawn_entity(&mut app, 2);

    run_ticks(&mut app, 5);

    let store = app.world().resource::<EntityDataStore>();
    let alice_id = *app.world().get::<MinecraftEntityId>(alice).unwrap();
    let bob_id = *app.world().get::<MinecraftEntityId>(bob).unwrap();
    assert_eq!(
        store.get::<u32>(alice_id, TICKS_KEY),
        Some(5),
        "a system reading/writing the store through a real GameTick schedule \
         must actually accumulate — this is the control that fails if the \
         plugin were silently unregistered"
    );
    assert_eq!(store.get::<u32>(bob_id, TICKS_KEY), Some(5));
}

#[test]
fn an_entity_spawned_partway_through_only_accumulates_its_own_ticks() {
    // The negative control: if `tick_entity_counters` ran once at startup and
    // never again (the "island" failure mode — a system that looks wired but
    // whose registration silently never reaches the schedule), an entity
    // spawned after the first run would read back `None` forever rather than
    // catching up on the ticks that happen after it exists.
    let mut app = new_app();

    run_ticks(&mut app, 3);
    let late = spawn_entity(&mut app, 99);
    run_ticks(&mut app, 4);

    let store = app.world().resource::<EntityDataStore>();
    let id = *app.world().get::<MinecraftEntityId>(late).unwrap();
    assert_eq!(
        store.get::<u32>(id, TICKS_KEY),
        Some(4),
        "an entity that only existed for the last 4 of 7 ticks must show 4, \
         proving the system runs every tick rather than once"
    );
}

#[test]
fn chunk_state_accumulates_across_ticks_independent_of_any_entity() {
    let mut app = new_app();

    run_ticks(&mut app, 6);

    let store = app.world().resource::<ChunkDataStore>();
    assert_eq!(store.get::<u32>(ChunkPos::new(0, 0), VISITS_KEY), Some(6));
    assert_eq!(
        store.get::<u32>(ChunkPos::new(1, 0), VISITS_KEY),
        None,
        "a chunk the toy plugin never visited must read back nothing"
    );
}
