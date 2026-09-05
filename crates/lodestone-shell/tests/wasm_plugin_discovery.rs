//! Shipped-shell reach gate for runtime-discovered WASM plugins.

#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use lodestone::config::Config;
use lodestone::sim::Sim;
use lodestone_ecs::events::GameEvent;
use lodestone_ecs::player::{ActionQueue, Egress, PlaceOutcome, PlaceStatus};
use lodestone_ecs::GameTick;
use lodestone_model::{ClientAction, ClientEvent, Text};
use lodestone_wasm_host::{
    Capability, CapabilitySet, PluginGrantPolicy, PluginHost, PluginIdentity, WasmHostPlugin,
    WasmPlugins,
};

fn build_example_plugin(features: &[&str]) -> PathBuf {
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plugins/lodestone-chat-responder-wasm")
        .canonicalize()
        .expect("the example WASM plugin must exist");
    let label = if features.is_empty() { "default".to_owned() } else { features.join("-") };
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("shipped-shell-guest-{label}"));
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(plugin_dir)
        .args(["build", "--release", "--target", "wasm32-unknown-unknown", "--target-dir"])
        .arg(&target_dir)
        .args(["-j", "2"])
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS");
    if !features.is_empty() {
        command.arg("--features").arg(features.join(","));
    }
    let output = command.output().expect("spawn the guest build");
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

fn install_fixture_named(
    root: &Path,
    directory_name: &str,
    manifest_name: &str,
    wasm: &Path,
    capabilities: &str,
) {
    let plugin = root.join(directory_name);
    std::fs::create_dir_all(&plugin).expect("create plugin directory");
    std::fs::copy(wasm, plugin.join("chat_responder.wasm")).expect("copy guest module");
    let manifest = format!(
        "name = \"{manifest_name}\"\n\
         version = \"0.1.0\"\n\
         abi = \"{}\"\n\
         module = \"chat_responder.wasm\"\n\
         priority = \"normal\"\n\
         capabilities = [{capabilities}]\n",
        lodestone_wasm_host::ABI_WORLD
    );
    std::fs::write(plugin.join("plugin.toml"), manifest).expect("write plugin manifest");
}

fn install_fixture(root: &Path, wasm: &Path, capabilities: &str) {
    install_fixture_named(root, "chat-responder", "chat-responder", wasm, capabilities);
}

fn client_sim(plugin_dir: &Path) -> Sim {
    client_sim_with_grants(plugin_dir, &PluginGrantPolicy::default())
}

fn client_sim_with_grants(plugin_dir: &Path, grants: &PluginGrantPolicy) -> Sim {
    let mut app = Sim::client_app();
    lodestone::wasm_plugins::install_from_directory_with_grants(&mut app, plugin_dir, grants)
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

    let wasm = build_example_plugin(&[]);
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

/// Discovery starts fail-closed, but an embedding can deliberately grant the
/// placement capability pair to exactly one configured manifest instance. The
/// sibling copies use the same compiled guest and request the same capabilities;
/// their unchanged denial proves path and manifest-name matching, not a guest
/// behavioural difference, confines the exception.
#[test]
fn discovery_grants_default_denied_placement_only_to_the_configured_manifest_instance() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("shipped-plugin-grant-directory-{}", std::process::id()));
    let wasm = build_example_plugin(&["place"]);
    let requested = "\"log\", \"act:chat\", \"act:place\", \"observe:place\"";
    install_fixture_named(&root, "trusted", "placement-owner", &wasm, requested);
    install_fixture_named(&root, "same-name-other-path", "placement-owner", &wasm, requested);
    install_fixture_named(&root, "same-path-shape-other-name", "other-owner", &wasm, requested);

    let mut grants = PluginGrantPolicy::default();
    grants.grant(
        PluginIdentity::new("trusted/plugin.toml", "placement-owner"),
        CapabilitySet::from_iter([Capability::ActPlace, Capability::ObservePlace]),
    );
    grants.grant(
        PluginIdentity::new("same-path-shape-other-name/plugin.toml", "not-other-owner"),
        CapabilitySet::from_iter([Capability::ActPlace, Capability::ObservePlace]),
    );
    let sim = client_sim_with_grants(&root, &grants);
    assert_eq!(
        sim.ecs()
            .read()
            .resource::<WasmPlugins>()
            .with_host(|host| host.plugins().len()),
        1,
        "only the configured path and manifest name may receive the exception"
    );

    {
        let mut ecs = sim.ecs().write();
        *ecs.resource_mut::<Egress>() = Egress { in_world: true, live: true };
        ecs.run_schedule(GameTick);
    }
    let outcome = {
        let mut ecs = sim.ecs().write();
        let mut players = ecs.query_filtered::<&PlaceOutcome, bevy_ecs::query::With<lodestone_ecs::player::LocalPlayer>>();
        *players.single(&ecs).expect("the local player has a placement outcome")
    };
    assert_eq!(
        outcome.generation, 1,
        "the configured guest must reach the shell-owned placement lifecycle"
    );

    sim.ecs().write().run_schedule(GameTick);
    assert_eq!(
        chat_actions(&sim),
        vec![ClientAction::SendChat {
            text: "place: generation=1 status=rejected".to_owned(),
        }],
        "the matching observe grant must return the one bounded production outcome"
    );
}

/// The persisted grant file is not merely a parser: its exact path-and-name
/// identity reaches the production discovery loader. The trusted copy obtains
/// the import/data-flow exception while the byte-identical sibling remains
/// denied, proving a saved policy cannot accidentally widen to every plugin
/// that declares the same capability.
#[test]
fn persisted_grants_file_allows_only_the_exact_discovered_plugin() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("shipped-persisted-plugin-grant-directory-{}", std::process::id()));
    let wasm = build_example_plugin(&["place"]);
    let requested = "\"log\", \"act:chat\", \"act:place\", \"observe:place\"";
    install_fixture_named(&root, "trusted", "placement-owner", &wasm, requested);
    install_fixture_named(&root, "untrusted", "placement-owner", &wasm, requested);
    let grants_file = root.join("grants.json");
    std::fs::write(
        &grants_file,
        r#"{
  "grants": [
    {
      "manifest_path": "trusted/plugin.toml",
      "name": "placement-owner",
      "capabilities": ["act:place", "observe:place"]
    }
  ]
}"#,
    )
    .expect("write explicit persisted grant policy");
    let grants = lodestone::wasm_plugins::load_grants_from_file(&grants_file)
        .expect("the explicit persisted policy must parse before application");
    let sim = client_sim_with_grants(&root, &grants);
    assert_eq!(
        sim.ecs()
            .read()
            .resource::<WasmPlugins>()
            .with_host(|host| host.plugins().len()),
        1,
        "only the exact persisted identity may enter production discovery"
    );
}

/// A reload reads its persisted authority again before it asks the host to stage
/// a replacement. The loaded guest is the control: if parsing a bad replacement
/// policy had first unloaded it, merely reporting the parse error would hide a
/// privilege-breaking availability failure.
#[test]
fn a_malformed_persisted_policy_rejects_reload_without_unloading_the_active_guest() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("shipped-plugin-reload-policy-{}", std::process::id()));
    let wasm = build_example_plugin(&[]);
    install_fixture(
        &root,
        &wasm,
        "\"log\", \"observe:chat\", \"act:chat\"",
    );

    let mut app = Sim::client_app();
    lodestone::wasm_plugins::install_from_directory(&mut app, &root)
        .expect("the initial WASM host must install");
    assert_eq!(
        app.world().resource::<WasmPlugins>().with_host(|host| host.plugins().len()),
        1,
        "the initial guest is the rollback control"
    );

    let grants_file = root.join("grants.json");
    std::fs::write(&grants_file, r#"{"grants":[{"manifest_path":"../plugin.toml"}]}"#)
        .expect("write malformed grant policy");
    let error = lodestone::wasm_plugins::reload_from_directory_with_grant_file(
        &mut app,
        &root,
        &grants_file,
    )
    .expect_err("a malformed persisted policy must fail before staging guests");
    assert!(
        matches!(error, lodestone::wasm_plugins::PluginReloadError::Grants(_)),
        "the policy parse must be the reported boundary: {error}"
    );
    assert_eq!(
        app.world().resource::<WasmPlugins>().with_host(|host| host.plugins().len()),
        1,
        "a rejected policy reload must retain the active guest"
    );
}

/// A separately-built guest reaches the shell's real placement system without a
/// world handle or packet constructor. Its target is intentionally outside the
/// live placement context, so the established production rejection path produces
/// a finite outcome that the host returns once on the following tick.
#[test]
fn wasm_placement_uses_the_shell_lifecycle_and_observes_one_bounded_result() {
    let wasm = build_example_plugin(&["place"]);
    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ActPlace);
    policy.insert(Capability::ObservePlace);
    let mut host = PluginHost::new(policy).expect("create runtime host");
    host.load_file(
        "place-owner",
        &wasm,
        &CapabilitySet::from_iter([
            Capability::Log,
            Capability::ActChat,
            Capability::ActPlace,
            Capability::ObservePlace,
        ]),
    )
    .expect("load placement fixture");

    let mut app = Sim::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    let sim = Sim::from_app(app, Config::default());
    {
        let mut ecs = sim.ecs().write();
        *ecs.resource_mut::<Egress>() = Egress { in_world: true, live: true };
        ecs.run_schedule(GameTick);
    }
    let outcome = {
        let mut ecs = sim.ecs().write();
        let mut players = ecs.query_filtered::<&PlaceOutcome, bevy_ecs::query::With<lodestone_ecs::player::LocalPlayer>>();
        *players.single(&ecs).expect("the local player has a placement outcome")
    };
    assert_eq!(
        outcome.generation,
        1,
        "the shell's production placement lifecycle must consume the guest request exactly once"
    );
    assert!(
        matches!(outcome.status, PlaceStatus::Rejected(_)),
        "the fixture has no live placement context, so the shell must report a bounded rejection: {outcome:?}"
    );

    sim.ecs().write().run_schedule(GameTick);
    assert_eq!(
        chat_actions(&sim),
        vec![ClientAction::SendChat {
            text: "place: generation=1 status=rejected".to_owned(),
        }],
        "the guest receives exactly the finite generation-tagged outcome"
    );
}

/// The same guest still tries to place when `act:place` is absent. The unchanged
/// production outcome and refusal counter prove the conductor, not an unrelated
/// placement legality check, stopped it.
#[test]
fn wasm_placement_without_its_capability_never_reaches_the_shell_lifecycle() {
    let wasm = build_example_plugin(&["place"]);
    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ObservePlace);
    let mut host = PluginHost::new(policy).expect("create runtime host");
    host.load_file(
        "place-owner",
        &wasm,
        &CapabilitySet::from_iter([Capability::Log, Capability::ActChat, Capability::ObservePlace]),
    )
    .expect("load placement fixture without its action grant");

    let mut app = Sim::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    let sim = Sim::from_app(app, Config::default());
    {
        let mut ecs = sim.ecs().write();
        *ecs.resource_mut::<Egress>() = Egress { in_world: true, live: true };
        ecs.run_schedule(GameTick);
    }
    let outcome = {
        let mut ecs = sim.ecs().write();
        let mut players = ecs.query_filtered::<&PlaceOutcome, bevy_ecs::query::With<lodestone_ecs::player::LocalPlayer>>();
        *players.single(&ecs).expect("the local player has a placement outcome")
    };
    assert_eq!(outcome, PlaceOutcome::default());
    assert_eq!(
        sim.ecs().read().resource::<WasmPlugins>().refused_actions(),
        1,
        "the guest returned the action but the data-flow gate must count its refusal"
    );
}
