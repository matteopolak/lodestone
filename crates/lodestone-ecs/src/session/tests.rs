use super::*;
use bevy_app::App;
use bevy_ecs::prelude::{Query, With};
use bevy_ecs::schedule::IntoScheduleConfigs;
use lodestone_model::event::{
    CollisionRule, DisplaySlot, ObjectiveMode, TeamAction, TeamColor, TeamParameters, Visibility,
};
use lodestone_model::{
    BossAction, BossColor, BossOverlay, ClientEvent, Difficulty, DimensionId, DimensionTypeInfo,
    GameMode, PlayerListEntry, Text,
};
use crate::player::{LocalPlayer, SelectedSlot};
use crate::schedules::{GameTick, NetIngest};
use crate::sets::IngestSet;
use uuid::Uuid;

#[derive(bevy_ecs::prelude::Resource, Debug, Default, PartialEq, Eq)]
struct ObservedSimulationDistance(Option<i32>);

/// A consumer ordered after the complete fold set. Kept outside the test
/// body so the scheduler validates the exact function signature when the
/// registration tuple changes.
fn observe_simulation_distance_after_fold(
    distances: Query<&ServerSimulationDistance, With<LocalPlayer>>,
    mut observed: bevy_ecs::prelude::ResMut<ObservedSimulationDistance>,
) {
    observed.0 = distances
        .single()
        .expect("the test owns exactly one local session")
        .0;
}

/// Build the net-thread shape: `SessionPlugin` plus one session entity.
fn session_app() -> (App, bevy_ecs::entity::Entity) {
    let mut app = App::new();
    app.add_plugins(SessionPlugin);
    let entity = spawn_session(app.world_mut());
    (app, entity)
}

fn fold(app: &mut App, event: ClientEvent) {
    app.world_mut()
        .resource_mut::<crate::ingest::IngestQueue>()
        .push(event);
    app.world_mut().run_schedule(NetIngest);
}

fn fold_batch(app: &mut App, events: impl IntoIterator<Item = ClientEvent>) {
    {
        let mut queue = app
            .world_mut()
            .resource_mut::<crate::ingest::IngestQueue>();
        for event in events {
            queue.push(event);
        }
    }
    app.world_mut().run_schedule(NetIngest);
}

fn dim(path: &str) -> DimensionId {
    format!("minecraft:{path}")
        .parse()
        .expect("valid dimension id")
}

fn key(s: &str) -> lodestone_model::ids::ResourceKey {
    s.parse().expect("valid resource key")
}

mod routing;
mod player_state;
mod session_scalars;
mod menus;
mod scoreboard;
mod world_state;
mod coverage;
