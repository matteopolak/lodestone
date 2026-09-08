//! Runtime discovery of native-hosted WebAssembly plugins.
//!
//! The shipped windowed client scans [`lodestone_wasm_host::DEFAULT_PLUGIN_DIR`]
//! before `WindowApp` adopts its `App`. Tests and embedders use
//! [`install_from_directory`] or [`install_from_directory_with_grants`] with an
//! explicit path; the desktop launcher uses
//! [`install_from_directory_with_grants_for_protocol`] so privileged plugins
//! receive the negotiated registry-backed broker. All routes call the same
//! [`lodestone_wasm_host::PluginHost::load_directory_with_grants`] implementation.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path};
use std::sync::Arc;

use lodestone_wasm_host::{
    Capability, CapabilitySet, HostError, PluginGrantPolicy, PluginHost, PluginIdentity,
    VersionBroker, VersionBrokerDescriptor, VersionBrokerRecord, WasmHostPlugin,
    WasmReloadError, VERSION_BROKER_ABI,
};
use lodestone_model::{BlockHardness, VersionAdapter};

/// Key for the negotiated protocol number exposed by [`RegistryVersionBroker`].
pub const VERSION_BROKER_PROTOCOL_KEY: &str = "protocol";
/// Key for the comma-separated release labels exposed by [`RegistryVersionBroker`].
pub const VERSION_BROKER_RELEASES_KEY: &str = "release-names";
/// Prefix for a copied block-state name lookup. The suffix is a decimal state id.
pub const VERSION_BROKER_BLOCK_NAME_PREFIX: &str = "block-name/";
/// Prefix for a copied block-state hardness lookup. The suffix is a decimal state id.
pub const VERSION_BROKER_BLOCK_HARDNESS_PREFIX: &str = "block-hardness/";

/// The production WASM broker backed by the registry's version-free adapter seam.
///
/// This is deliberately a small, read-only projection of [`VersionAdapter`].
/// It exposes protocol identity, release labels, and two block-state facts as
/// copied strings; it never returns the adapter, a registry, packet bytes, ECS
/// access, or a borrowed value. The key vocabulary is finite and documented by
/// the constants above, so a plugin cannot turn the broker into a general
/// reflection API.
#[derive(Debug)]
pub struct RegistryVersionBroker {
    descriptor: VersionBrokerDescriptor,
    adapter: Box<dyn VersionAdapter>,
}

impl RegistryVersionBroker {
    /// Select the adapter and family row for one negotiated protocol.
    #[must_use]
    pub fn for_protocol(protocol: i32) -> Option<Self> {
        let family = lodestone_registry::family_for_protocol(protocol)?;
        let adapter = lodestone_registry::adapter_for_protocol(protocol)?;
        Some(Self {
            descriptor: VersionBrokerDescriptor::new(family, protocol, VERSION_BROKER_ABI),
            adapter,
        })
    }

    fn block_hardness_value(hardness: BlockHardness) -> String {
        format!(
            "hardness={};requires-correct-tool={}",
            hardness.hardness, hardness.requires_correct_tool
        )
    }
}

impl VersionBroker for RegistryVersionBroker {
    fn descriptor(&self) -> VersionBrokerDescriptor {
        self.descriptor.clone()
    }

    fn lookup(&self, key: &str) -> Option<VersionBrokerRecord> {
        if key == VERSION_BROKER_PROTOCOL_KEY {
            return Some(VersionBrokerRecord::new(
                VERSION_BROKER_PROTOCOL_KEY,
                self.adapter.protocol_version().to_string(),
            ));
        }
        if key == VERSION_BROKER_RELEASES_KEY {
            return Some(VersionBrokerRecord::new(
                VERSION_BROKER_RELEASES_KEY,
                self.adapter.minecraft_versions().join(","),
            ));
        }
        if let Some(id) = key.strip_prefix(VERSION_BROKER_BLOCK_NAME_PREFIX) {
            let state_id = id.parse::<u32>().ok()?;
            return self.adapter.block_name(state_id).map(|name| {
                VersionBrokerRecord::new(
                    format!("{VERSION_BROKER_BLOCK_NAME_PREFIX}{state_id}"),
                    name,
                )
            });
        }
        if let Some(id) = key.strip_prefix(VERSION_BROKER_BLOCK_HARDNESS_PREFIX) {
            let state_id = id.parse::<u32>().ok()?;
            return self.adapter.block_hardness(state_id).map(|hardness| {
                VersionBrokerRecord::new(
                    format!("{VERSION_BROKER_BLOCK_HARDNESS_PREFIX}{state_id}"),
                    Self::block_hardness_value(hardness),
                )
            });
        }
        None
    }
}

#[cfg(test)]
mod version_broker_tests {
    use super::*;

    #[test]
    fn an_uncompiled_protocol_has_no_privileged_source() {
        assert!(RegistryVersionBroker::for_protocol(-1).is_none());
    }

    #[cfg(feature = "live")]
    #[test]
    fn the_production_broker_projects_only_owned_registry_values() {
        let broker = RegistryVersionBroker::for_protocol(776)
            .expect("the default live shell compiles the 776 family");
        let descriptor = broker.descriptor();
        assert_eq!(descriptor.family(), "v26-2");
        assert_eq!(descriptor.protocol(), 776);
        assert_eq!(descriptor.abi(), VERSION_BROKER_ABI);

        let protocol = broker
            .lookup(VERSION_BROKER_PROTOCOL_KEY)
            .expect("protocol is part of the bounded broker vocabulary");
        assert_eq!(protocol.key(), VERSION_BROKER_PROTOCOL_KEY);
        assert_eq!(protocol.value(), "776");
        assert!(broker.lookup("adapter-pointer").is_none());
        assert!(broker.lookup("block-name/not-a-number").is_none());
    }
}

/// An invalid persisted WASM grant configuration.
///
/// A file selected with `--plugin-grants` is operator authority, so every
/// malformed entry is an error rather than a best-effort omission. The shell
/// consequently starts with no newly granted capabilities until the operator
/// fixes the file and explicitly launches it again.
#[derive(Debug)]
pub struct PluginGrantsError(String);

impl fmt::Display for PluginGrantsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PluginGrantsError {}

/// A persisted-policy reload that could not commit.
///
/// Policy parsing happens before any guest is staged, so a malformed file
/// leaves the old stores and their authority intact. The host-side variant is
/// likewise transactional: it retains the old stores when a candidate manifest
/// or module is refused.
#[derive(Debug)]
pub enum PluginReloadError {
    Grants(PluginGrantsError),
    Host(WasmReloadError),
}

impl fmt::Display for PluginReloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Grants(error) => write!(f, "could not reload persisted plugin grants: {error}"),
            Self::Host(error) => write!(f, "could not reload WASM plugins: {error}"),
        }
    }
}

impl std::error::Error for PluginReloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Grants(error) => Some(error),
            Self::Host(error) => Some(error),
        }
    }
}

/// Load explicit per-instance grants from a JSON file selected by the operator.
///
/// The file has one `grants` array. Each entry must carry a root-relative
/// `manifest_path`, the matching manifest `name`, and an array of known
/// capability wire names:
///
/// ```json
/// {"grants":[{"manifest_path":"trusted/plugin.toml","name":"trusted","capabilities":["fs:read"]}]}
/// ```
///
/// Paths containing `..`, a root/prefix, or a filename other than
/// `plugin.toml` are refused. A capability typo, duplicate identity, missing
/// field, or extra field is likewise refused: silently accepting a spelling
/// mistake is not an authority model. Loading is deliberately explicit; this
/// function does not watch the file, and a changed policy takes effect only
/// when the caller builds a fresh host and applies it again.
///
/// # Errors
/// Returns an error for an unreadable file or invalid schema. Callers must not
/// substitute a partial policy for an error.
pub fn load_grants_from_file(path: &Path) -> Result<PluginGrantPolicy, PluginGrantsError> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        PluginGrantsError(format!("could not read plugin grants `{}`: {error}", path.display()))
    })?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| {
            PluginGrantsError(format!("could not parse plugin grants `{}`: {error}", path.display()))
        })?;
    parse_grants_value(&value).map_err(|message| {
        PluginGrantsError(format!("invalid plugin grants `{}`: {message}", path.display()))
    })
}

fn parse_grants_value(value: &serde_json::Value) -> Result<PluginGrantPolicy, String> {
    let object = value.as_object().ok_or("the root must be a JSON object")?;
    reject_unknown_fields(object, &["grants"], "root")?;
    let grants = object
        .get("grants")
        .and_then(serde_json::Value::as_array)
        .ok_or("`grants` must be an array")?;

    let mut policy = PluginGrantPolicy::default();
    let mut identities = BTreeSet::new();
    for (index, entry) in grants.iter().enumerate() {
        let context = format!("grants[{index}]");
        let entry = entry
            .as_object()
            .ok_or_else(|| format!("{context} must be an object"))?;
        reject_unknown_fields(entry, &["manifest_path", "name", "capabilities"], &context)?;
        let manifest_path = required_string(entry, "manifest_path", &context)?;
        validate_manifest_path(manifest_path, &context)?;
        let name = required_string(entry, "name", &context)?;
        if name.is_empty() {
            return Err(format!("{context}.name must not be empty"));
        }
        let capabilities = entry
            .get("capabilities")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("{context}.capabilities must be an array"))?;
        let capabilities = capabilities
            .iter()
            .enumerate()
            .map(|(capability_index, capability)| {
                let capability = capability.as_str().ok_or_else(|| {
                    format!("{context}.capabilities[{capability_index}] must be a string")
                })?;
                Capability::parse(capability).ok_or_else(|| {
                    format!(
                        "{context}.capabilities[{capability_index}] names unknown capability `{capability}`"
                    )
                })
            })
            .collect::<Result<CapabilitySet, String>>()?;
        let identity = PluginIdentity::new(manifest_path, name);
        if !identities.insert(identity.clone()) {
            return Err(format!(
                "{context} repeats the manifest path and name of an earlier grant"
            ));
        }
        policy.grant(identity, capabilities);
    }
    Ok(policy)
}

fn reject_unknown_fields(
    object: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
    context: &str,
) -> Result<(), String> {
    if let Some(field) = object.keys().find(|field| !allowed.contains(&field.as_str())) {
        return Err(format!("{context} has unknown field `{field}`"));
    }
    Ok(())
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{context}.{field} must be a string"))
}

fn validate_manifest_path(path: &str, context: &str) -> Result<(), String> {
    let has_ambiguous_separator = path.ends_with('/') || path.contains("//");
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || has_ambiguous_separator
        || !path.components().all(|component| matches!(component, Component::Normal(_)))
        || path
            .to_string_lossy()
            .split(std::path::MAIN_SEPARATOR)
            .any(|component| matches!(component, "." | ".."))
        || path.file_name().is_none_or(|name| name != "plugin.toml")
    {
        return Err(format!(
            "{context}.manifest_path must be a root-relative path ending in `plugin.toml` without `.` or `..`"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_wasm_host::Capability;

    #[test]
    fn persisted_grants_bind_path_and_name_and_reject_unknown_capabilities() {
        let policy = parse_grants_value(&serde_json::json!({
            "grants": [{
                "manifest_path": "trusted/plugin.toml",
                "name": "trusted",
                "capabilities": ["fs:read", "act:place"]
            }]
        }))
        .expect("a complete explicit grant must parse");
        let matching = policy
            .grants_for(&PluginIdentity::new("trusted/plugin.toml", "trusted"))
            .expect("the exact configured identity must have grants");
        assert!(matching.contains(Capability::FsRead));
        assert!(matching.contains(Capability::ActPlace));
        assert!(
            policy
                .grants_for(&PluginIdentity::new("trusted/plugin.toml", "different-name"))
                .is_none(),
            "the path alone must not select a grant"
        );
        assert!(
            parse_grants_value(&serde_json::json!({
                "grants": [{
                    "manifest_path": "trusted/plugin.toml",
                    "name": "trusted",
                    "capabilities": ["fs:write"]
                }]
            }))
            .expect_err("an unrecognised capability must fail closed")
            .contains("unknown capability `fs:write`")
        );
    }

    #[test]
    fn persisted_grants_reject_ambiguous_paths_and_duplicate_identities() {
        for path in [
            "/plugin.toml",
            "../plugin.toml",
            "trusted/../plugin.toml",
            "trusted/./plugin.toml",
            "trusted//plugin.toml",
            "trusted/plugin.toml/",
        ] {
            assert!(
                parse_grants_value(&serde_json::json!({
                    "grants": [{"manifest_path": path, "name": "trusted", "capabilities": []}]
                }))
                .is_err(),
                "{path} must not escape or ambiguously name the discovery root"
            );
        }
        assert!(
            parse_grants_value(&serde_json::json!({
                "grants": [
                    {"manifest_path": "trusted/plugin.toml", "name": "trusted", "capabilities": []},
                    {"manifest_path": "trusted/plugin.toml", "name": "trusted", "capabilities": ["act:place"]}
                ]
            }))
            .expect_err("two entries for one identity are an ambiguous review surface")
            .contains("repeats")
        );
    }
}

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
    install_from_directory_with_optional_broker(app, directory, grants, None)
}

/// Load plugins with the registry-backed broker selected for `protocol`.
///
/// The desktop launcher uses this entry point so a plugin that explicitly
/// requests `version:broker` sees the same negotiated adapter as the client.
/// A protocol that is not compiled into this shell gets no broker; a plugin
/// requesting it then fails closed with [`HostError::VersionBrokerUnavailable`].
pub fn install_from_directory_with_grants_for_protocol(
    app: &mut lodestone_app::App,
    directory: &Path,
    grants: &PluginGrantPolicy,
    protocol: i32,
) -> Result<(), HostError> {
    let broker = RegistryVersionBroker::for_protocol(protocol)
        .map(|broker| Arc::new(broker) as Arc<dyn VersionBroker>);
    install_from_directory_with_optional_broker(app, directory, grants, broker)
}

fn install_from_directory_with_optional_broker(
    app: &mut lodestone_app::App,
    directory: &Path,
    grants: &PluginGrantPolicy,
    broker: Option<Arc<dyn VersionBroker>>,
) -> Result<(), HostError> {
    if app.is_plugin_added::<WasmHostPlugin>() {
        tracing::debug!(
            path = %directory.display(),
            "a WASM host is already installed; keeping the caller's host"
        );
        return Ok(());
    }

    let mut host = PluginHost::new(CapabilitySet::default_policy())?;
    if let Some(broker) = broker {
        host = host.with_version_broker(broker);
    }
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

/// Revalidate `grants_file`, stage a fresh directory snapshot, and atomically
/// replace the installed guest stores.
///
/// This is the explicit reload boundary for an embedding that owns an installed
/// [`WasmHostPlugin`]. It does not watch either path. A caller chooses when to
/// invoke it, and a changed grants file is parsed from disk again on every call
/// before its contents can influence a replacement host.
///
/// The shipped desktop launcher currently invokes discovery only at startup;
/// this public function is for a native embedding's deliberate reload action.
/// It is absent from browser builds with the rest of this module, and it does
/// not install a host for headless or connect-only shell modes.
///
/// # Errors
/// Returns an error without replacing active guests if the policy file is
/// unreadable or malformed, a candidate plugin is rejected, or its command
/// roots would shadow a non-WASM command.
pub fn reload_from_directory_with_grant_file(
    app: &mut lodestone_app::App,
    directory: &Path,
    grants_file: &Path,
) -> Result<(), PluginReloadError> {
    let grants = load_grants_from_file(grants_file).map_err(PluginReloadError::Grants)?;
    lodestone_wasm_host::reload_wasm_plugins(app, directory, &grants)
        .map_err(PluginReloadError::Host)
}

/// Replace the installed guest stores using an already validated grant policy.
///
/// Use [`reload_from_directory_with_grant_file`] for a persisted policy. This
/// lower-level form exists for embeddings that keep their reviewed policy in a
/// different configuration backend.
pub fn reload_from_directory_with_grants(
    app: &mut lodestone_app::App,
    directory: &Path,
    grants: &PluginGrantPolicy,
) -> Result<(), WasmReloadError> {
    lodestone_wasm_host::reload_wasm_plugins(app, directory, grants)
}
