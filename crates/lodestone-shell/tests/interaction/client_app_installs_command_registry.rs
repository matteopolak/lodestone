//! The rendered client's `App` must actually install
//! `PluginCommandsPlugin`, or the entire command path is an island.
//!
//! # Why this gate exists at all
//!
//! Everything downstream of it was already correct and already tested when this
//! was written. `CHAT_COMMAND` decodes, crosses the host-installed
//! `CommandSink` seam, and reaches `dispatch` — with a four-test end-to-end wire
//! gate in `crates/versions/26.2/tests/command_wire_path.rs` proving it, both of
//! whose negative controls were observed failing.
//!
//! And **no player could run a command**, because `dispatch` reads a
//! `CommandRegistry` that only `PluginCommandsPlugin` inserts, and nothing in
//! production added that plugin. Zero registrations. This is the repo's dominant
//! defect class in its purest form: every component individually built,
//! individually gated, reaching zero pixels because one line was missing from an
//! `add_plugins` tuple.
//!
//! # Why it asserts the resource rather than the plugin
//!
//! `App::is_plugin_added::<PluginCommandsPlugin>()` would be the obvious check
//! and is the weaker one: it passes for a plugin whose `build` was changed to
//! stop inserting the registry. The resource is what `dispatch` actually reads,
//! so the resource is what this asserts — the same reason the wire gate above
//! drives the registry rather than naming the command.
//!
//! `Permissions` is asserted alongside deliberately. A missing `Permissions`
//! must never be read as "allow everything", and its absence is exactly the
//! shape that would make the wire gate's permission control silently vacuous.

use lodestone::sim::Sim;
use lodestone_ecs::Permissions;
use lodestone_ecs::commands::CommandRegistry;

/// The one line `sim/build.rs` was missing, stated as a property of the built
/// `App` rather than as the presence of a line of source.
#[test]
fn the_rendered_clients_app_carries_the_command_registry_dispatch_reads() {
    let app = Sim::client_app();
    let world = app.world();

    assert!(
        world.get_resource::<CommandRegistry>().is_some(),
        "Sim::client_app() must install PluginCommandsPlugin — without its \
         CommandRegistry, `dispatch` has nothing to look a command up in and \
         the whole decoded wire path (#464) reaches no player (#467)"
    );

    assert!(
        world.get_resource::<Permissions>().is_some(),
        "Permissions must be present on the same App: a missing Permissions \
         resource must never resolve as 'allow everything', and its absence \
         would make the wire path's permission control vacuous rather than red"
    );
}
