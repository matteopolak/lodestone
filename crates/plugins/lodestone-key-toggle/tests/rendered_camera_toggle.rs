//! A compiled-in plugin must reach the real rendered-client camera consumer,
//! not merely leave a component in an isolated `World`.

use lodestone::config::{Config, Mode};
use lodestone::sim::Sim;
use lodestone_ecs::{CameraOverride, GameTick, PendingPluginKeyEvents, PhysicalKey};
use lodestone_key_toggle::CameraTogglePlugin;
use lodestone_model::Vec3;

fn test_config() -> Config {
    Config {
        mode: Mode::Headless,
        render_distance: 2,
        ..Config::default()
    }
}

/// The production route is `external Plugin` -> `Sim::client_app` ->
/// `Sim::from_app` -> `GameTick` -> `CameraOverride` -> `render_camera`.
///
/// The baseline control uses the same composed simulation without the plugin;
/// a component-only test would not distinguish a camera hook that no renderer
/// reads from a live one.
#[test]
fn camera_toggle_drives_the_real_rendered_camera_and_releases_it() {
    let pose = CameraOverride {
        position: Vec3::new(14.0, 80.0, -9.0),
        yaw: 35.0,
        pitch: -20.0,
    };
    let mut app = Sim::client_app();
    app.add_plugins(CameraTogglePlugin::new(PhysicalKey::named("KeyC"), pose).0);
    let sim = Sim::from_app(app, test_config());

    let baseline = sim.render_camera(1.0);
    assert_ne!(
        (
            baseline.position.x,
            baseline.position.y,
            baseline.position.z,
            baseline.yaw,
            baseline.pitch,
        ),
        (
            pose.position.x as f32,
            pose.position.y as f32,
            pose.position.z as f32,
            pose.yaw,
            pose.pitch,
        ),
        "control: an unpressed camera plugin must not already replace the shell camera"
    );

    {
        let mut world = sim.ecs().write();
        world
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(lodestone_ecs::PluginKeyEvent {
                key: PhysicalKey::named("KeyC"),
                pressed: true,
            });
        world.run_schedule(GameTick);
    }
    let directed = sim.render_camera(1.0);
    assert_eq!(
        (
            directed.position.x,
            directed.position.y,
            directed.position.z,
            directed.yaw,
            directed.pitch,
        ),
        (
            pose.position.x as f32,
            pose.position.y as f32,
            pose.position.z as f32,
            pose.yaw,
            pose.pitch,
        ),
        "a plugin pose must reach Sim::render_camera, the same consumer the windowed renderer reads"
    );

    {
        let mut world = sim.ecs().write();
        world
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(lodestone_ecs::PluginKeyEvent {
                key: PhysicalKey::named("KeyC"),
                pressed: true,
            });
        world.run_schedule(GameTick);
    }
    let released = sim.render_camera(1.0);
    assert_eq!(
        (
            released.position.x,
            released.position.y,
            released.position.z,
            released.yaw,
            released.pitch,
        ),
        (
            baseline.position.x,
            baseline.position.y,
            baseline.position.z,
            baseline.yaw,
            baseline.pitch,
        ),
        "a second press must remove the override rather than leaving the frame captive"
    );
}
