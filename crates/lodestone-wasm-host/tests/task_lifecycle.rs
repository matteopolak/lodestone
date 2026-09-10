//! Scheduled guest tasks must obey the host's enable, disable, and reload
//! boundaries on the composed client path.

mod support;

use std::path::{Path, PathBuf};

use lodestone_ecs::player::ActionQueue;
use lodestone_ecs::{app::App, GameTick};
use lodestone_model::ClientAction;
use lodestone_physics::{PlayerState, Vec3d};
use lodestone_wasm_host::{Capability, CapabilitySet, PluginHost, WasmHostPlugin, WasmPlugins};

fn fresh_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("task-lifecycle-conformance");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create plugin root");
    root
}

fn install(root: &Path, wasm: &Path) {
    let plugin = root.join("scheduler");
    std::fs::create_dir_all(&plugin).expect("create plugin directory");
    std::fs::write(
        plugin.join("plugin.toml"),
        r#"
name = "scheduler"
version = "0.1.0"
abi = "lodestone:plugin@0.28.0"
module = "scheduler.wasm"
priority = "normal"
capabilities = ["log", "schedule:tasks", "act:chat"]
"#,
    )
    .expect("write plugin manifest");
    std::fs::copy(wasm, plugin.join("scheduler.wasm")).expect("copy plugin module");
}

fn scheduler_actions(app: &App) -> Vec<ClientAction> {
    app.world()
        .resource::<ActionQueue>()
        .0
        .iter()
        .filter(|action| matches!(action, ClientAction::SendChat { .. }))
        .cloned()
        .collect()
}

fn plugin_count(app: &App) -> usize {
    app.world()
        .resource::<WasmPlugins>()
        .with_host(|host| host.plugins().len())
}

fn clear_actions(app: &mut App) {
    app.world_mut().resource_mut::<ActionQueue>().0.clear();
}

/// Removing a guest disables its pending tasks as one lifecycle transaction;
/// reloading it creates a fresh scheduler rather than replaying old callbacks.
#[test]
fn disabling_and_reenabling_a_guest_cleans_up_scheduled_tasks() {
    let wasm = support::build_example_plugin(&["scheduler"]);
    let root = fresh_root();
    install(&root, &wasm);

    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ScheduleTasks);
    let mut host = PluginHost::new(policy).expect("engine");
    let results = host.load_directory(&root);
    assert_eq!(results.len(), 1);
    results[0].as_ref().expect("scheduler guest must load");

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    lodestone_app::spawn_session(&mut app, PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0));

    app.world_mut().run_schedule(GameTick);
    assert_eq!(
        scheduler_actions(&app),
        vec![
            ClientAction::SendChat {
                text: "task: repeating 1".to_owned(),
            },
            ClientAction::SendChat {
                text: "task: zero period 1".to_owned(),
            },
        ],
        "the enabled guest must receive its first due callbacks"
    );
    assert_eq!(plugin_count(&app), 1);

    // Removing the installed directory is the explicit disable input. Reload
    // must drop the old store and its task queue together, not only hide the
    // guest from future event delivery.
    std::fs::remove_dir_all(root.join("scheduler")).expect("remove scheduler guest");
    clear_actions(&mut app);
    lodestone_wasm_host::reload_wasm_plugins(&mut app, &root, &Default::default())
        .expect("an empty directory is a valid disabled set");
    assert_eq!(plugin_count(&app), 0, "disable must remove the guest store");
    app.world_mut().run_schedule(GameTick);
    assert!(
        scheduler_actions(&app).is_empty(),
        "callbacks queued by a disabled guest must not fire after replacement"
    );

    // Re-enable from the same module. Fresh guest state schedules the initial
    // callbacks once; callbacks from the retired store would create duplicates.
    install(&root, &wasm);
    lodestone_wasm_host::reload_wasm_plugins(&mut app, &root, &Default::default())
        .expect("the scheduler guest must re-enable");
    assert_eq!(plugin_count(&app), 1);
    clear_actions(&mut app);
    app.world_mut().run_schedule(GameTick);
    assert_eq!(
        scheduler_actions(&app),
        vec![
            ClientAction::SendChat {
                text: "task: repeating 1".to_owned(),
            },
            ClientAction::SendChat {
                text: "task: zero period 1".to_owned(),
            },
        ],
        "re-enable must run one fresh task schedule, not old and new queues"
    );
}
