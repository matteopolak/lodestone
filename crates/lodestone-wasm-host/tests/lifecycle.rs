//! Lifecycle conformance on the composed client: directory load ordering,
//! failure isolation, and transactional replacement all reach the real
//! conductor rather than stopping at `PluginHost::tick_all`.

mod support;

use std::path::{Path, PathBuf};

use lodestone_ecs::events::GameEvent;
use lodestone_ecs::player::ActionQueue;
use lodestone_ecs::{app::App, GameTick};
use lodestone_model::{ClientAction, ClientEvent, Text};
use lodestone_physics::{PlayerState, Vec3d};
use lodestone_wasm_host::{
    reload_wasm_plugins, CapabilitySet, PluginGrantPolicy, PluginHost, WasmHostPlugin,
    WasmPlugins, WasmReloadError,
};

fn chat(text: &str) -> GameEvent {
    GameEvent(ClientEvent::Chat {
        text: Text::literal(text),
        kind: lodestone_model::event::ChatKind::Chat,
        sender: None,
        ack: None,
    })
}

fn fresh_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("lifecycle-conformance");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create plugin root");
    root
}

fn install(root: &Path, directory: &str, manifest: &str, wasm: &Path) -> PathBuf {
    let plugin = root.join(directory);
    std::fs::create_dir_all(&plugin).expect("create plugin directory");
    std::fs::write(plugin.join("plugin.toml"), manifest).expect("write plugin manifest");
    std::fs::copy(wasm, plugin.join("chat_responder.wasm")).expect("copy plugin module");
    plugin
}

fn manifest(name: &str, priority: &str, required: Option<&str>) -> String {
    let dependencies = required.map_or_else(String::new, |dependency| {
        format!("\n[dependencies]\nrequired = [\"{dependency}\"]\n")
    });
    format!(
        "name = \"{name}\"\n\
         version = \"0.1.0\"\n\
         abi = \"lodestone:plugin@0.27.0\"\n\
         module = \"chat_responder.wasm\"\n\
         priority = \"{priority}\"\n\
         capabilities = [\"log\", \"observe:chat\", \"act:chat\"]\n\
         {dependencies}"
    )
}

fn chat_actions(app: &App) -> Vec<ClientAction> {
    app.world()
        .resource::<ActionQueue>()
        .0
        .iter()
        .filter(|action| matches!(action, ClientAction::SendChat { .. }))
        .cloned()
        .collect()
}

fn plugin_names(app: &App) -> Vec<String> {
    app.world()
        .resource::<WasmPlugins>()
        .with_host(|host| host.plugins().iter().map(|plugin| plugin.name().to_owned()).collect())
}

/// The production lifecycle is ordered and transactional: a required dependent
/// follows its dependency, a trapped earlier guest cannot suppress a healthy
/// later guest, a rejected replacement keeps the old set active, and a
/// successful replacement disables removed guests while resetting guest state.
#[test]
fn ordered_failure_isolated_and_transactional_plugin_lifecycle() {
    let spinner = support::build_example_plugin(&["spin"]);
    let responder = support::build_example_plugin(&[]);
    let root = fresh_root();

    install(
        &root,
        "dependency",
        &manifest("dependency", "lowest", None),
        &spinner,
    );
    let dependent_dir = install(
        &root,
        "dependent",
        &manifest("dependent", "highest", Some("dependency")),
        &responder,
    );

    let mut host = PluginHost::new(CapabilitySet::default_policy())
        .expect("engine")
        .with_fuel(2_000_000);
    let results = host.load_directory(&root);
    assert_eq!(results.len(), 2, "both manifests must be discovered");
    for result in results {
        result.expect("the initial dependency graph must load");
    }
    assert_eq!(
        host.plugins().iter().map(|plugin| plugin.name()).collect::<Vec<_>>(),
        vec!["dependency", "dependent"],
        "dependency edges must precede priority/name ordering"
    );

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    lodestone_app::spawn_session(&mut app, PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0));

    app.world_mut().write_message(chat("hello ping there"));
    app.world_mut().run_schedule(GameTick);
    assert_eq!(
        chat_actions(&app),
        vec![ClientAction::SendChat {
            text: "pong (chat messages seen: 1)".to_owned(),
        }],
        "a failed dependency guest must not suppress its healthy dependent"
    );
    app.world()
        .resource::<WasmPlugins>()
        .with_host(|host| {
            assert!(host.plugins()[0].failure().is_some(), "the dependency must fail on tick");
            assert_eq!(host.plugins()[1].failure(), None, "the dependent must remain healthy");
        });

    // Removing the dependency while leaving the required edge makes the
    // candidate unloadable. The conductor must retain the old, still-running
    // set instead of partially applying the edit.
    std::fs::remove_dir_all(root.join("dependency")).expect("remove dependency fixture");
    let rejected = reload_wasm_plugins(&mut app, &root, &PluginGrantPolicy::default())
        .expect_err("a missing required dependency must reject reload");
    assert!(matches!(rejected, WasmReloadError::Reload(_)), "{rejected:?}");
    assert_eq!(plugin_names(&app), vec!["dependency", "dependent"]);

    // Once the dependent no longer requires the removed guest, the replacement
    // is valid. It is a fresh store, so its observable counter starts at one and
    // the failed dependency is no longer active.
    std::fs::write(
        dependent_dir.join("plugin.toml"),
        manifest("dependent", "highest", None),
    )
    .expect("remove dependency edge");
    reload_wasm_plugins(&mut app, &root, &PluginGrantPolicy::default())
        .expect("the edited independent guest must reload");
    assert_eq!(plugin_names(&app), vec!["dependent"]);

    app.world_mut().resource_mut::<ActionQueue>().0.clear();
    app.world_mut().write_message(chat("hello ping again"));
    app.world_mut().run_schedule(GameTick);
    assert_eq!(
        chat_actions(&app),
        vec![ClientAction::SendChat {
            text: "pong (chat messages seen: 1)".to_owned(),
        }],
        "a reloaded guest must be enabled once with fresh lifecycle state"
    );
}
