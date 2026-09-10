//! Production-conductor coverage for bounded inbound and outbound WASM packet
//! observation. The guest receives copied component-model values while the ECS
//! message queue remains the only native producer.

mod support;

use std::path::{Path, PathBuf};

use lodestone_ecs::events::{OutboundRawPacket, RawPacket};
use lodestone_ecs::player::ActionQueue;
use lodestone_ecs::{app::App, GameTick};
use lodestone_model::{ClientAction, ConnectionState};
use lodestone_physics::{PlayerState, Vec3d};
use lodestone_wasm_host::{CapabilitySet, PluginHost, WasmHostPlugin};

fn fresh_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("raw-packet-observation");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create plugin root");
    root
}

fn install(root: &Path, wasm: &Path) {
    let plugin = root.join("observer");
    std::fs::create_dir_all(&plugin).expect("create plugin directory");
    std::fs::write(
        plugin.join("plugin.toml"),
        "name = \"raw-observer\"\n\
         version = \"0.1.0\"\n\
         abi = \"lodestone:plugin@0.28.0\"\n\
         module = \"chat_responder.wasm\"\n\
         capabilities = [\"log\", \"observe:packets\", \"act:chat\"]\n",
    )
    .expect("write plugin manifest");
    std::fs::copy(wasm, plugin.join("chat_responder.wasm")).expect("copy plugin module");
}

fn chats(app: &App) -> Vec<String> {
    app.world()
        .resource::<ActionQueue>()
        .0
        .iter()
        .filter_map(|action| match action {
            ClientAction::SendChat { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn production_conductor_delivers_both_raw_packet_directions() {
    let wasm = support::build_example_plugin(&["raw-observation"]);
    let root = fresh_root();
    install(&root, &wasm);

    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    for result in host.load_directory(&root) {
        result.expect("raw observer must load");
    }
    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    lodestone_app::spawn_session(&mut app, PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0));

    app.world_mut().write_message(RawPacket {
        protocol: 776,
        state: ConnectionState::Play,
        packet_id: 0x2a,
        payload: vec![0, 255],
    });
    app.world_mut().write_message(OutboundRawPacket {
        protocol: 776,
        state: ConnectionState::Configuration,
        packet_id: 7,
        payload: vec![3],
    });
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        chats(&app),
        [
            "raw:in protocol=776 phase=Play id=42 bytes=2".to_owned(),
            "raw:out protocol=776 phase=Configuration id=7 bytes=1".to_owned(),
        ]
    );
}
