//! A runtime-built guest command reaches the native command registry that a
//! shipped client application owns.
//!
//! The controls use the same guest artifact. One removes the registration grant;
//! the other removes `WasmHostPlugin` itself. That distinguishes a guest command
//! from a command that happened to be present in the client app already.

mod support;

use lodestone_ecs::commands::{CommandOutcome, CommandSource, dispatch};
use lodestone_wasm_host::{Capability, CapabilitySet, PluginHost, WasmHostPlugin};

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
