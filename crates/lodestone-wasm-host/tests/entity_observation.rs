//! The entity-observation production gate: one decoded event reaches a native
//! plugin reader and a separately built WASM guest with the same copied facts.

mod support;

use bevy_ecs::prelude::{IntoScheduleConfigs, MessageReader, ResMut, Resource};
use lodestone_app::client_app;
use lodestone_ecs::events::GameEvent;
use lodestone_ecs::{GameTick, TickSet};
use lodestone_model::{ClientAction, ClientEvent, ResourceKey, Rotation, Vec3};
use lodestone_wasm_host::{Capability, CapabilitySet, PluginHost, WasmHostPlugin};

#[derive(Resource, Default, Debug, PartialEq, Eq)]
struct NativeEntitySpawns(Vec<(i32, String)>);

/// A compiled-in plugin's normal observation route. It deliberately reads the
/// same `GameEvent` message the WASM conductor consumes; neither tier gets an
/// ECS entity or component borrow from this boundary.
fn observe_entity_spawns(
    mut events: MessageReader<GameEvent>,
    mut seen: ResMut<NativeEntitySpawns>,
) {
    for GameEvent(event) in events.read() {
        let ClientEvent::EntitySpawned {
            entity_id,
            entity_type,
            ..
        } = event
        else {
            continue;
        };
        seen.0.push((*entity_id, entity_type.to_string()));
    }
}

fn entity_spawn() -> GameEvent {
    GameEvent(ClientEvent::EntitySpawned {
        entity_id: 41,
        uuid: None,
        entity_type: "minecraft:pig".parse::<ResourceKey>().expect("valid entity key"),
        pos: Vec3::new(1.25, 64.0, -3.5),
        rotation: Rotation::new(90.0, -15.0),
        velocity: Some(Vec3::new(0.125, 0.0, -0.25)),
    })
}

/// A real guest artifact is loaded from disk, while a native system receives the
/// same decoded event in the composed client app. The returned chat action is
/// intentionally just a test witness: `ActionQueue` remains the production
/// egress owner, and the observation itself has no mutation path.
#[test]
fn entity_spawn_reaches_native_and_wasm_plugins_without_an_ecs_handle() {
    let wasm = support::build_example_plugin(&["entity-observation"]);
    let granted = CapabilitySet::from_iter([
        Capability::Log,
        Capability::ObserveEntities,
        Capability::ActChat,
    ]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("host engine");
    host.load_file("entity-observer", &wasm, &granted)
        .expect("the guest's declared copied observation must load");

    let mut app = client_app();
    app.init_resource::<NativeEntitySpawns>();
    app.add_systems(
        GameTick,
        observe_entity_spawns.in_set(TickSet::Intent),
    );
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().write_message(entity_spawn());
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        app.world().resource::<NativeEntitySpawns>().0,
        vec![(41, "minecraft:pig".to_owned())],
        "control: the native plugin must receive the source event"
    );
    assert_eq!(
        app.world().resource::<lodestone_ecs::player::ActionQueue>().0,
        vec![ClientAction::SendChat {
            text: "entity: id=41 generation=1 kind=minecraft:pig x=1.25".to_owned(),
        }],
        "the same spawn must cross the generation-scoped WIT boundary and return through the real action queue"
    );
}
