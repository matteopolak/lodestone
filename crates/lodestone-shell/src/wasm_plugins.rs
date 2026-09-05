//! Runtime discovery of native-hosted WebAssembly plugins.
//!
//! The shipped windowed client scans [`lodestone_wasm_host::DEFAULT_PLUGIN_DIR`]
//! before `WindowApp` adopts its `App`. Tests and embedders use
//! [`install_from_directory`] or [`install_from_directory_with_grants`] with an
//! explicit path; both routes call the same
//! [`lodestone_wasm_host::PluginHost::load_directory_with_grants`] implementation.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path};

use lodestone_wasm_host::{
    Capability, CapabilitySet, HostError, PluginGrantPolicy, PluginHost, PluginIdentity,
    WasmHostPlugin,
};

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
