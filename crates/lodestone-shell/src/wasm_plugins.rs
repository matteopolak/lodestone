//! Runtime discovery of native-hosted WebAssembly plugins.
//!
//! The shipped windowed client scans [`lodestone_wasm_host::DEFAULT_PLUGIN_DIR`]
//! before `WindowApp` adopts its `App`. Tests and embedders use
//! [`install_from_directory`] or [`install_from_directory_with_grants`] with an
//! explicit path; both routes call the same
//! [`lodestone_wasm_host::PluginHost::load_directory_with_grants`] implementation.

use std::path::Path;

use lodestone_wasm_host::{CapabilitySet, HostError, PluginGrantPolicy, PluginHost, WasmHostPlugin};

/// Load every valid plugin below `directory` under the default fail-closed
/// capability policy and install their conductor into `app`.
///
/// An absent directory is the ordinary empty installation. A malformed or
/// denied plugin is logged and excluded without preventing valid sibling
/// plugins from loading. If the caller already installed [`WasmHostPlugin`],
/// that host remains authoritative and no second loader is added.
///
/// # Errors
///
/// Returns an error only when the Wasmtime engine itself cannot be created.
pub fn install_from_directory(
    app: &mut lodestone_app::App,
    directory: &Path,
) -> Result<(), HostError> {
    install_from_directory_with_grants(app, directory, &PluginGrantPolicy::default())
}

/// Load every valid plugin below `directory`, adding only the explicit
/// per-manifest grants in `grants` to the shell's fail-closed baseline.
///
/// The grant key contains both a root-relative `plugin.toml` path and its
/// manifest `name`; a module's self-reported name is never an authority key.
/// This makes a configured placement/break grant follow the same plugin
/// instance after a reload, without granting a sibling copy of its module.
///
/// An absent directory is the ordinary empty installation. A malformed or
/// denied plugin is logged and excluded without preventing valid sibling
/// plugins from loading. If the caller already installed [`WasmHostPlugin`],
/// that host remains authoritative and no second loader is added.
///
/// # Errors
///
/// Returns an error only when the Wasmtime engine itself cannot be created.
pub fn install_from_directory_with_grants(
    app: &mut lodestone_app::App,
    directory: &Path,
    grants: &PluginGrantPolicy,
) -> Result<(), HostError> {
    if app.is_plugin_added::<WasmHostPlugin>() {
        tracing::debug!(
            path = %directory.display(),
            "a WASM host is already installed; keeping the caller's host"
        );
        return Ok(());
    }

    let mut host = PluginHost::new(CapabilitySet::default_policy())?;
    for result in host.load_directory_with_grants(directory, grants) {
        if let Err(error) = result {
            tracing::error!(
                path = %directory.display(),
                "refused a discovered WASM plugin: {error}"
            );
        }
    }
    app.add_plugins(WasmHostPlugin::new(host));
    Ok(())
}
