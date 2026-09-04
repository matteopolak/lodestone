//! Shipped-shell reach gate for runtime-discovered WASM plugins.

#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use lodestone::config::Config;
use lodestone::sim::Sim;
use lodestone_ecs::events::GameEvent;
use lodestone_ecs::player::ActionQueue;
use lodestone_ecs::GameTick;
use lodestone_model::{ClientAction, ClientEvent, Text};
use lodestone_wasm_host::WasmPlugins;

fn build_example_plugin() -> PathBuf {
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plugins/lodestone-chat-responder-wasm")
        .canonicalize()
        .expect("the example WASM plugin must exist");
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("shipped-shell-guest");
    let output = Command::new(env!("CARGO"))
        .current_dir(plugin_dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--target-dir",
        ])
        .arg(&target_dir)
        .args(["-j", "2"])
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .output()
        .expect("spawn the guest build");
    assert!(
        output.status.success(),
        "guest build failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    target_dir.join(
        "wasm32-unknown-unknown/release/lodestone_chat_responder_wasm.wasm",
    )
}

fn install_fixture(root: &Path, wasm: &Path, capabilities: &str) {
    let plugin = root.join("chat-responder");
    std::fs::create_dir_all(&plugin).expect("create plugin directory");
    std::fs::copy(wasm, plugin.join("chat_responder.wasm")).expect("copy guest module");
    let manifest = format!(
        "name = \"chat-responder\"\n\
         version = \"0.1.0\"\n\
         abi = \"{}\"\n\
         module = \"chat_responder.wasm\"\n\
         priority = \"normal\"\n\
         capabilities = [{capabilities}]\n",
        lodestone_wasm_host::ABI_WORLD
    );
    std::fs::write(plugin.join("plugin.toml"), manifest).expect("write plugin manifest");
}

fn client_sim(plugin_dir: &Path) -> Sim {
    let mut app = Sim::client_app();
    lodestone::wasm_plugins::install_from_directory(&mut app, plugin_dir)
        .expect("create the WASM host");
    Sim::from_app(app, Config::default())
}

fn send_chat_and_tick(sim: &Sim, text: &str) {
    let mut ecs = sim.ecs().write();
    ecs.write_message(GameEvent(ClientEvent::Chat {
        text: Text::literal(text),
        kind: lodestone_model::event::ChatKind::Chat,
        sender: None,
        ack: None,
    }));
    ecs.run_schedule(GameTick);
}

fn chat_actions(sim: &Sim) -> Vec<ClientAction> {
    sim.ecs()
        .read()
        .resource::<ActionQueue>()
        .0
        .iter()
        .filter(|action| matches!(action, ClientAction::SendChat { .. }))
        .cloned()
        .collect()
}

#[test]
fn discovered_plugins_reach_the_real_queue_while_absent_and_denied_plugins_do_not() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("shipped-plugin-directory-{}", std::process::id()));
    let absent = root.join("absent");
    let absent_sim = client_sim(&absent);
    send_chat_and_tick(&absent_sim, "ping");
    assert_eq!(chat_actions(&absent_sim), Vec::<ClientAction>::new());
    assert_eq!(
        absent_sim
            .ecs()
            .read()
            .resource::<WasmPlugins>()
            .with_host(|host| host.plugins().len()),
        0,
        "an absent directory must install no guest"
    );

    let wasm = build_example_plugin();
    let denied = root.join("denied");
    install_fixture(
        &denied,
        &wasm,
        "\"log\", \"observe:chat\", \"act:chat\", \"fs:read\"",
    );
    let denied_sim = client_sim(&denied);
    send_chat_and_tick(&denied_sim, "ping");
    assert_eq!(
        chat_actions(&denied_sim),
        Vec::<ClientAction>::new(),
        "a capability-denied guest must not emit an action"
    );
    assert_eq!(
        denied_sim
            .ecs()
            .read()
            .resource::<WasmPlugins>()
            .with_host(|host| host.plugins().len()),
        0,
        "a plugin denied by default policy must not enter the conductor"
    );

    let allowed = root.join("allowed");
    install_fixture(
        &allowed,
        &wasm,
        "\"log\", \"observe:chat\", \"act:chat\"",
    );
    let sim = client_sim(&allowed);
    send_chat_and_tick(&sim, "hello ping there");
    assert_eq!(
        chat_actions(&sim),
        vec![ClientAction::SendChat {
            text: "pong (chat messages seen: 1)".to_owned(),
        }],
        "the discovered guest must reach the shell's real ActionQueue"
    );
}
