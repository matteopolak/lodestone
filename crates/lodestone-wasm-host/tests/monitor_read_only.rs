//! The manifest `monitor` tier is a real read-only admission boundary, not only
//! an ordering label. The control loads the same separately-built guest at a
//! mutating priority and proves its action still reaches the composed client.

mod support;

use std::path::{Path, PathBuf};

use lodestone_ecs::events::GameEvent;
use lodestone_ecs::player::ActionQueue;
use lodestone_ecs::{app::App, GameTick};
use lodestone_model::{ClientAction, ClientEvent, Text};
use lodestone_physics::{PlayerState, Vec3d};
use lodestone_wasm_host::{
    CapabilitySet, LoadError, ManifestError, PluginHost, WasmHostPlugin,
};

fn fresh_root(label: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(label);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create plugin root");
    root
}

fn install(root: &Path, manifest: &str, wasm: &Path) {
    let plugin = root.join("chat-responder");
    std::fs::create_dir_all(&plugin).expect("create plugin directory");
    std::fs::write(plugin.join("plugin.toml"), manifest).expect("write plugin manifest");
    std::fs::copy(wasm, plugin.join("chat_responder.wasm")).expect("copy plugin module");
}

fn chat(text: &str) -> GameEvent {
    GameEvent(ClientEvent::Chat {
        text: Text::literal(text),
        kind: lodestone_model::event::ChatKind::Chat,
        sender: None,
        ack: None,
    })
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

#[test]
fn monitor_manifests_are_read_only_but_normal_guests_still_reach_the_client() {
    let wasm = support::build_example_plugin(&[]);
    let base = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/lodestone-chat-responder-wasm/plugin.toml"),
    )
    .expect("read shipped plugin manifest");
    let monitor_manifest = base.replace(r#"priority = "normal""#, r#"priority = "monitor""#);
    let monitor_root = fresh_root("monitor-read-only");
    install(&monitor_root, &monitor_manifest, &wasm);

    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    let results = host.load_directory(&monitor_root);
    assert_eq!(results.len(), 1, "the monitor manifest must be discovered");
    assert!(matches!(
        &results[0],
        Err(LoadError::Manifest(ManifestError::MonitorMutation {
            plugin,
            capability,
        })) if plugin == "chat-responder" && capability == "act:chat"
    ));
    assert!(host.is_empty(), "a monitor guest with an action grant must not load");

    // Independent control: the same guest and capabilities are valid at the
    // normal tier, and the real conductor must deliver its authored response.
    let normal_root = fresh_root("monitor-normal-control");
    let normal_manifest = base.replace(r#"priority = "normal""#, r#"priority = "normal""#);
    install(&normal_root, &normal_manifest, &wasm);
    let mut normal_host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    let normal_results = normal_host.load_directory(&normal_root);
    assert_eq!(normal_results.len(), 1);
    normal_results[0].as_ref().expect("normal guest must load");

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(normal_host));
    lodestone_app::spawn_session(&mut app, PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0));
    app.world_mut().write_message(chat("hello ping there"));
    app.world_mut().run_schedule(GameTick);
    assert_eq!(
        chat_actions(&app),
        vec![ClientAction::SendChat {
            text: "pong (chat messages seen: 1)".to_owned(),
        }],
        "the normal-priority control must retain the production action path"
    );
}
