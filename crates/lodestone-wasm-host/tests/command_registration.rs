//! A runtime-built guest command reaches the native command registry that a
//! shipped client application owns.
//!
//! The controls use the same guest artifact. One removes the registration grant;
//! the other removes `WasmHostPlugin` itself. That distinguishes a guest command
//! from a command that happened to be present in the client app already.

mod support;

use std::path::{Path, PathBuf};
use std::str::FromStr;

use lodestone_ecs::commands::{
    CommandAnchor, CommandExecutionContext, CommandOutcome, CommandSource, dispatch, suggest,
};
use lodestone_ecs::permissions::{PermissionDefault, PermissionSubject, Permissions};
use lodestone_model::{ResourceKey, Rotation, Vec3};
use lodestone_wasm_host::{
    reload_wasm_plugins, Capability, CapabilitySet, PluginHost, WasmHostPlugin,
};
use uuid::Uuid;

fn command_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::RegisterCommands])
}

fn client_app_with_guest(grant_commands: bool) -> lodestone_app::App {
    let wasm = support::build_example_plugin(&["commands"]);
    let policy = if grant_commands {
        command_capabilities()
    } else {
        CapabilitySet::from_iter([Capability::Log])
    };
    let requested = if grant_commands {
        command_capabilities()
    } else {
        CapabilitySet::from_iter([Capability::Log])
    };
    let mut host = PluginHost::new(policy).expect("engine");
    host.load_file("command-fixture", &wasm, &requested)
        .expect("the guest must load");

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    app
}

fn command_reload_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("command-registration-reload");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create command reload root");
    root
}

fn install_command_guest(root: &Path, wasm: &Path) {
    let plugin = root.join("command-fixture");
    std::fs::create_dir_all(&plugin).expect("create command fixture directory");
    std::fs::write(
        plugin.join("plugin.toml"),
        r#"
name = "command-fixture"
version = "0.1.0"
abi = "lodestone:plugin@0.29.0"
module = "command_fixture.wasm"
priority = "normal"
capabilities = ["log", "commands:register"]
"#,
    )
    .expect("write command fixture manifest");
    std::fs::copy(wasm, plugin.join("command_fixture.wasm"))
        .expect("copy command fixture module");
}

#[test]
fn a_runtime_loaded_guest_registers_a_command_on_the_real_client_registry() {
    let mut app = client_app_with_guest(true);

    // The alias must be rewritten by `CommandRegistry` before the guest sees it:
    // the fixture only succeeds for its canonical `wasm-ping` spelling.
    assert_eq!(
        dispatch(app.world_mut(), &CommandSource::console(), "/wp"),
        Ok(CommandOutcome::Success(37)),
        "the guest's non-round result proves its handler ran through the registry"
    );

    // The host's greedy tail receives a complete canonical command line, not
    // merely the root declaration.
    assert_eq!(
        dispatch(app.world_mut(), &CommandSource::console(), "/wasm-ping extra words"),
        Ok(CommandOutcome::Failure(
            "unexpected command input: wasm-ping extra words".to_owned()
        ))
    );
}

/// The context originates at the server-to-shell command sink, survives the
/// native command registry's canonicalisation and permission gate, then reaches a
/// separately-built guest. The guest's distinct non-round result means this is
/// not a host-side reconstruction of the expected answer.
#[test]
fn a_contextual_command_reaches_the_guest_with_value_only_execution_state() {
    let mut app = client_app_with_guest(true);
    let source = CommandSource::contextual(
        PermissionSubject::Console,
        "Alex",
        CommandExecutionContext {
            entity: None,
            position: Vec3::new(12.5, 64.0, -3.25),
            rotation: Rotation::new(90.0, -15.0),
            dimension: ResourceKey::from_str("minecraft:overworld").expect("resource key"),
            anchor: CommandAnchor::Eyes,
            permission_level: 3,
        },
    );

    assert_eq!(
        dispatch(app.world_mut(), &source, "/wp"),
        Ok(CommandOutcome::Success(61)),
        "the alias must be canonicalised before the guest sees the complete copied context"
    );
}

#[test]
fn a_guest_typed_schema_parses_and_suggests_through_the_real_registry() {
    let mut app = client_app_with_guest(true);
    let source = CommandSource::console();

    assert_eq!(
        suggest(app.world(), &source, "/wasm-typed "),
        vec!["fast".to_owned(), "safe".to_owned()],
        "the closed choices declared by the guest must reach native completion"
    );
    assert_eq!(
        suggest(app.world(), &source, "/wasm-typed safe "),
        vec!["1".to_owned(), "42".to_owned(), "7".to_owned()],
        "static suggestions on a typed argument must be prefix-filtered by the registry"
    );
    assert_eq!(
        dispatch(app.world_mut(), &source, "/wt safe 7"),
        Ok(CommandOutcome::Success(47)),
        "aliases and typed argument parsing must complete before the guest handler runs"
    );
    assert!(
        dispatch(app.world_mut(), &source, "/wasm-typed invalid 7").is_err(),
        "a closed choices argument must reject values outside the guest declaration"
    );
    assert!(
        dispatch(app.world_mut(), &source, "/wasm-typed safe nope").is_err(),
        "an integer argument must reject non-integer input before guest invocation"
    );
}

#[test]
fn a_guest_command_permission_gates_typed_dispatch_and_suggestions() {
    let mut app = client_app_with_guest(true);
    let source = CommandSource::player(Uuid::from_u128(0x734), "Player");

    app.world_mut()
        .resource_mut::<Permissions>()
        .declare("wasm.command.use", PermissionDefault::False);

    assert!(
        suggest(app.world(), &source, "/wasm").is_empty(),
        "a player without the declared command permission must not see guest roots"
    );
    let denied = dispatch(app.world_mut(), &source, "/wt safe 7")
        .expect_err("the native registry must reject the typed guest command first");
    assert!(
        denied.is_permission_denied(),
        "guest command denial must remain a permission parse error, got {denied:?}"
    );

    app.world_mut()
        .resource_mut::<Permissions>()
        .grant(Uuid::from_u128(0x734), "wasm.command.use");

    assert_eq!(
        suggest(app.world(), &source, "/wasm"),
        vec!["wasm-ping".to_owned(), "wasm-typed".to_owned()],
        "granting the node must reveal both guest command roots"
    );
    assert_eq!(
        dispatch(app.world_mut(), &source, "/wt safe 7"),
        Ok(CommandOutcome::Success(47)),
        "the same typed input must reach the separately-built guest after the grant"
    );
}

#[test]
fn control_without_the_command_capability_the_declared_root_is_not_registered() {
    let mut app = client_app_with_guest(false);

    assert!(
        matches!(
            dispatch(app.world_mut(), &CommandSource::console(), "/wasm-ping"),
            Err(lodestone_ecs::commands::CommandDispatchError::UnknownCommand { .. })
        ),
        "a guest declaration without `commands:register` must not claim a root"
    );
}

#[test]
fn control_without_the_wasm_host_plugin_the_client_has_no_guest_command() {
    let mut app = lodestone_app::client_app();
    // Match the host's native registry installation. Without this, the control
    // would only prove a bare `lodestone_app::client_app` has no command registry.
    app.add_plugins(lodestone_ecs::PluginCommandsPlugin);

    assert!(
        matches!(
            dispatch(app.world_mut(), &CommandSource::console(), "/wasm-ping"),
            Err(lodestone_ecs::commands::CommandDispatchError::UnknownCommand { .. })
        ),
        "the client command registry must not manufacture the guest root"
    );
}

/// Reload is a production registry transaction, not just a host-store swap:
/// removing a guest must unregister only its roots, and adding it back must
/// make the same roots executable through the real client command registry.
#[test]
fn a_wasm_command_reload_unregisters_and_reinstalls_guest_roots() {
    let wasm = support::build_example_plugin(&["commands"]);
    let root = command_reload_root();
    install_command_guest(&root, &wasm);

    let mut host = PluginHost::new(command_capabilities()).expect("engine");
    let results = host.load_directory(&root);
    assert_eq!(results.len(), 1, "the command fixture must be discovered");
    results[0].as_ref().expect("the command fixture must load");

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    let source = CommandSource::console();
    assert_eq!(
        dispatch(app.world_mut(), &source, "/wp"),
        Ok(CommandOutcome::Success(37)),
        "the initial directory load must register the guest root"
    );

    std::fs::remove_dir_all(root.join("command-fixture")).expect("disable command fixture");
    reload_wasm_plugins(&mut app, &root, &Default::default())
        .expect("removing the guest is a valid reload");
    assert!(
        matches!(
            dispatch(app.world_mut(), &source, "/wp"),
            Err(lodestone_ecs::commands::CommandDispatchError::UnknownCommand { .. })
        ),
        "a successful reload must unregister retired guest roots"
    );

    install_command_guest(&root, &wasm);
    reload_wasm_plugins(&mut app, &root, &Default::default())
        .expect("reinstalling the guest is a valid reload");
    assert_eq!(
        dispatch(app.world_mut(), &source, "/wp"),
        Ok(CommandOutcome::Success(37)),
        "a replacement guest must re-register its alias through the real registry"
    );
}
