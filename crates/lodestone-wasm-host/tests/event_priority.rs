//! Event-derived actions from independent guests must preserve manifest priority
//! through the composed client event bus and WASM conductor.

mod support;

use std::path::{Path, PathBuf};

use lodestone_ecs::events::GameEvent;
use lodestone_ecs::player::ActionQueue;
use lodestone_ecs::{app::App, GameTick};
use lodestone_model::{ClientAction, ClientEvent, ItemStack, Text};
use lodestone_physics::{PlayerState, Vec3d};
use lodestone_wasm_host::{Capability, CapabilitySet, PluginHost, WasmHostPlugin};

fn fresh_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("event-priority-conformance");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create plugin root");
    root
}

fn install(root: &Path, directory: &str, manifest: &str, wasm: &Path) {
    let plugin = root.join(directory);
    std::fs::create_dir_all(&plugin).expect("create plugin directory");
    std::fs::write(plugin.join("plugin.toml"), manifest).expect("write plugin manifest");
    std::fs::copy(wasm, plugin.join("chat_responder.wasm")).expect("copy plugin module");
}

fn manifest(name: &str, priority: &str, capabilities: &str) -> String {
    format!(
        "name = \"{name}\"\n\
         version = \"0.1.0\"\n\
         abi = \"lodestone:plugin@0.28.0\"\n\
         module = \"chat_responder.wasm\"\n\
         priority = \"{priority}\"\n\
         capabilities = [{capabilities}]\n"
    )
}

fn chat(text: &str) -> GameEvent {
    GameEvent(ClientEvent::Chat {
        text: Text::literal(text),
        kind: lodestone_model::event::ChatKind::Chat,
        sender: None,
        ack: None,
    })
}

fn inventory_event() -> GameEvent {
    GameEvent(ClientEvent::InventorySlotChanged {
        slot: 4,
        item: Some(ItemStack::new(
            "minecraft:gold_ingot".parse().expect("valid item key"),
            13,
        )),
    })
}

fn guest_chat_actions(app: &App) -> Vec<ClientAction> {
    app.world()
        .resource::<ActionQueue>()
        .0
        .iter()
        .filter(|action| matches!(action, ClientAction::SendChat { .. }))
        .cloned()
        .collect()
}

/// Independent guests receive different event kinds, but their actions share
/// one production queue. The lower-priority guest must be dispatched first even
/// when directory names put the higher-priority guest first alphabetically.
#[test]
fn independent_event_consumers_preserve_manifest_priority_in_the_conductor() {
    let chat_responder = support::build_example_plugin(&[]);
    let inventory_responder = support::build_example_plugin(&["inventory"]);
    let root = fresh_root();

    // Deliberately invert directory order: discovery must use priority, not the
    // filesystem enumeration order, to decide which guest runs first.
    install(
        &root,
        "aaa-high",
        &manifest(
            "inventory-high",
            "highest",
            "\"log\", \"observe:inventory\", \"act:chat\"",
        ),
        &inventory_responder,
    );
    install(
        &root,
        "zzz-low",
        &manifest(
            "chat-low",
            "lowest",
            "\"log\", \"observe:chat\", \"act:chat\"",
        ),
        &chat_responder,
    );

    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ObserveInventory);
    let mut host = PluginHost::new(policy).expect("engine");
    let results = host.load_directory(&root);
    assert_eq!(results.len(), 2, "both event consumers must be discovered");
    for result in results {
        result.expect("both event consumers must load");
    }
    assert_eq!(
        host.plugins().iter().map(|plugin| plugin.name()).collect::<Vec<_>>(),
        vec!["chat-low", "inventory-high"],
        "manifest priority must determine conductor order"
    );

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    lodestone_app::spawn_session(&mut app, PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0));
    app.world_mut().write_message(chat("hello ping there"));
    app.world_mut().write_message(inventory_event());
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        guest_chat_actions(&app),
        vec![
            ClientAction::SendChat {
                text: "pong (chat messages seen: 1)".to_owned(),
            },
            ClientAction::SendChat {
                text: "inventory: slot=4 item=minecraft:gold_ingotx13".to_owned(),
            },
        ],
        "the event bus and conductor must retain the lower-priority action first"
    );
}
