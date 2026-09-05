//! `plugin.toml`: what a plugin says about itself, and what it asks to
//! be allowed to do.
//!
//! # The format
//!
//! ```toml
//! name = "chat-responder"
//! version = "0.1.0"
//! # The WIT world the module was built against. Checked against
//! # `lodestone_wasm_host::ABI_WORLD` before the module is compiled.
//! abi = "lodestone:plugin@0.14.0"
//! # The `.wasm`, relative to this file. A core module or a component; the host
//! # encodes the former.
//! module = "chat_responder.wasm"
//! # Conductor ordering, using `EventPriority`'s own six names.
//! priority = "normal"
//! description = "Replies pong to any chat message containing ping."
//! # Everything this plugin may do. Anything not listed is denied.
//! capabilities = ["log", "observe:chat", "act:chat"]
//! ```
//!
//! Dependencies are declared in a typed table. Required dependencies must be
//! present in the same discovery directory; optional dependencies affect order
//! only when present.
//!
//! ```toml
//! [dependencies]
//! required = ["shared-state"]
//! optional = ["metrics"]
//! ```
//!
//! TOML rather than YAML: Cargo's own format and this codebase's preference. The
//! *shape* is `plugin.yml`'s — Bukkit's `permissions` block is the nearest analogue —
//! but Bukkit plugins do not declare capabilities in the sandbox sense at all,
//! because the JVM's trust model has no concept of them for plugins.
//!
//! # Manifest and graph validation
//!
//! | case | error | why not a warning |
//! |---|---|---|
//! | a capability name the host does not recognise | [`ManifestError::UnknownCapability`], listing every name it *does* know | skipping it would grant the plugin less than it asked for and run it anyway: it does not work, and nothing says why |
//! | a field the host does not recognise | [`ManifestError::Toml`], via `deny_unknown_fields` | the common case is a typo in a capability list or a priority, and a silently ignored `capabilties = […]` is a plugin running with none |
//! | an ABI world this host does not speak | [`ManifestError::AbiMismatch`], naming both | the alternative is discovering it as an unresolved-import error at instantiation, which names an interface rather than a version |
//! | a capability host policy withholds | [`crate::HostError::CapabilityDenied`], from the loader | this one is the operator's decision rather than the plugin's mistake, so it is the loader's error and not the manifest's |
//!
//! The directory planner also rejects duplicate plugin names, missing required
//! dependencies, and dependency cycles. Optional edges are ignored when their
//! target is absent, but participate in cycle detection when both plugins are
//! present. A graph is ordered topologically; priority and name are stable
//! tie-breakers among plugins that are ready to load.
//!
//! # The trust boundary, stated once
//!
//! **A manifest is a declaration. It is not trusted, and it does not need to be.**
//! Nothing stops a plugin author writing `capabilities = []` and shipping a module
//! that calls `filesystem.read-file` — and nothing needs to, because the host builds
//! its `Linker` from the *granted* set, so that module fails to instantiate. The
//! manifest's job is to let an honest plugin be refused **politely and early**, with
//! a message an operator can act on. The `Linker` is what makes a dishonest one
//! harmless. See `tests/capability_denial.rs` for both halves.
//!
//! # How to change it
//!
//! Adding a field means adding it to [`Manifest`] *and* deciding its default. A
//! field with no default is a breaking change to every existing `plugin.toml`;
//! `serde`'s `#[serde(default)]` is the way to add one compatibly. Adding a
//! capability is `crate::capability`'s business, and this module needs no change —
//! the name list here is generated from [`crate::Capability::ALL`], deliberately, so
//! a new capability is declarable the moment it exists.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::capability::{Capability, CapabilitySet};
use crate::host::ABI_WORLD;

/// Conductor ordering, mirroring `lodestone_ecs::EventPriority`'s six Bukkit tiers.
///
/// # Why this is a separate enum rather than `EventPriority` itself
///
/// `EventPriority` is a `SystemSet`, and its derive list is
/// `(SystemSet, Debug, Clone, PartialEq, Eq, Hash)` — **no `Ord`**, because a
/// `SystemSet` has no business having one. Sorting loaded plugins needs a total
/// order, so this enum derives one and [`Priority::event_priority`] converts. The
/// names and the meanings are identical on purpose; if `EventPriority` ever gains
/// `Ord`, this type should collapse into it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Lowest,
    Low,
    /// The tier a plugin with no stated opinion gets.
    #[default]
    Normal,
    High,
    Highest,
    /// Runs after every other tier. Note the native tier *enforces* that a
    /// `Monitor` system does not mutate the `World`
    /// (`lodestone_ecs::assert_monitor_system_is_read_only`); a wasm guest at this
    /// tier is **not** yet held to the equivalent — it can still return actions. See
    /// `docs/wasm-plugin-host.md` §"Pending on other work".
    Monitor,
}

impl Priority {
    #[must_use]
    pub const fn event_priority(self) -> lodestone_ecs::EventPriority {
        match self {
            Self::Lowest => lodestone_ecs::EventPriority::Lowest,
            Self::Low => lodestone_ecs::EventPriority::Low,
            Self::Normal => lodestone_ecs::EventPriority::Normal,
            Self::High => lodestone_ecs::EventPriority::High,
            Self::Highest => lodestone_ecs::EventPriority::Highest,
            Self::Monitor => lodestone_ecs::EventPriority::Monitor,
        }
    }
}

/// Dependencies declared by one plugin manifest.
///
/// The two lists are intentionally separate rather than a stringly typed
/// `depends` field. Required edges gate loading; optional edges contribute an
/// ordering constraint only when their target is installed. Names are matched
/// against the manifest's authoritative `name`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependencies {
    /// Plugins that must be present and loadable in the same directory.
    #[serde(default)]
    pub required: Vec<String>,
    /// Plugins that are ordered before this plugin when present, but may be absent.
    #[serde(default)]
    pub optional: Vec<String>,
}

/// Everything that can be wrong with a `plugin.toml`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    #[error("reading plugin manifest `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing plugin manifest `{path}`: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error(
        "plugin `{plugin}` requests unknown capability `{name}`.\n\
         note: this host knows: {known}\n\
         note: if the plugin targets a newer ABI than this host, the `abi` field is where that \
         should have been declared"
    )]
    UnknownCapability {
        plugin: String,
        name: String,
        known: String,
    },
    #[error(
        "plugin `{plugin}` targets ABI world `{found}`, but this host speaks `{expected}` — \
         rebuild the plugin against the host's wit/lodestone-plugin.wit, or run a host that \
         speaks `{found}`"
    )]
    AbiMismatch {
        plugin: String,
        found: String,
        expected: String,
    },
    #[error("plugin `{plugin}`'s module `{module}` does not exist (expected at `{path}`)")]
    MissingModule {
        plugin: String,
        module: String,
        path: PathBuf,
    },
    #[error("plugin `{plugin}` declares dependency `{dependency}` more than once")]
    DuplicateDependency { plugin: String, dependency: String },
    #[error("plugin `{plugin}` declares an empty {kind} dependency name")]
    InvalidDependencyName { plugin: String, kind: &'static str },
    #[error("multiple manifests declare plugin `{plugin}`")]
    DuplicatePluginName { plugin: String },
    #[error("plugin `{plugin}` requires missing dependency `{dependency}`")]
    MissingRequiredDependency { plugin: String, dependency: String },
    #[error("plugin dependency cycle: {plugins:?}")]
    DependencyCycle { plugins: Vec<String> },
}

/// A parsed `plugin.toml`.
#[derive(Debug, Clone, Deserialize)]
// `deny_unknown_fields` is load-bearing rather than pedantic: the failure it catches
// is a *typo in a capability list*, and a silently-ignored `capabilties = [...]` is a
// plugin that loads with no capabilities and mysteriously does nothing.
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Identifies the plugin in logs, in errors, and in the host's plugin list. Must
    /// match the `name` the module's own `init` returns.
    pub name: String,
    pub version: String,
    /// The WIT world this module was built against.
    pub abi: String,
    /// The `.wasm`, relative to the manifest.
    pub module: String,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default)]
    pub description: String,
    /// Everything this plugin may do. Absent means the empty set — a plugin that
    /// declared nothing, which loads and can do nothing, rather than a plugin that
    /// gets a default grant.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Required and optional plugin edges used by directory discovery.
    #[serde(default)]
    pub dependencies: Dependencies,
}

impl Manifest {
    /// Parse a manifest from TOML text, validating the ABI world and every capability
    /// name. `path` is used only for error messages.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(text).map_err(|source| ManifestError::Toml {
            path: path.to_path_buf(),
            source,
        })?;
        if manifest.abi != ABI_WORLD {
            return Err(ManifestError::AbiMismatch {
                plugin: manifest.name,
                found: manifest.abi,
                expected: ABI_WORLD.to_owned(),
            });
        }
        // Validated here rather than lazily at first use, so a typo is a load-time
        // error rather than a capability that turns out to be missing much later.
        manifest.requested_capabilities()?;
        manifest.validate_dependencies()?;
        Ok(manifest)
    }

    /// Read and parse a manifest from disk.
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text, path)
    }

    /// The declared capabilities, or the first unrecognised name.
    pub fn requested_capabilities(&self) -> Result<CapabilitySet, ManifestError> {
        let mut set = CapabilitySet::empty();
        for name in &self.capabilities {
            let capability =
                Capability::parse(name).ok_or_else(|| ManifestError::UnknownCapability {
                    plugin: self.name.clone(),
                    name: name.clone(),
                    // Generated from `Capability::ALL`, so a new capability appears in
                    // this message the moment it exists rather than when someone
                    // remembers to update a list.
                    known: Capability::ALL
                        .iter()
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                })?;
            set.insert(capability);
        }
        Ok(set)
    }

    fn validate_dependencies(&self) -> Result<(), ManifestError> {
        let mut seen = std::collections::BTreeSet::new();
        for (kind, names) in [
            ("required", &self.dependencies.required),
            ("optional", &self.dependencies.optional),
        ] {
            for dependency in names {
                if dependency.trim().is_empty() {
                    return Err(ManifestError::InvalidDependencyName {
                        plugin: self.name.clone(),
                        kind,
                    });
                }
                if !seen.insert(dependency) {
                    return Err(ManifestError::DuplicateDependency {
                        plugin: self.name.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Where the module is, given where the manifest was.
    pub fn module_path(&self, manifest_path: &Path) -> PathBuf {
        manifest_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(&self.module)
    }

    /// Resolve the module path and confirm it exists, so a missing `.wasm` is
    /// reported against the manifest that named it rather than as a bare file-not-found.
    pub fn resolved_module(&self, manifest_path: &Path) -> Result<PathBuf, ManifestError> {
        let path = self.module_path(manifest_path);
        if !path.is_file() {
            return Err(ManifestError::MissingModule {
                plugin: self.name.clone(),
                module: self.module.clone(),
                path,
            });
        }
        Ok(path)
    }
}

/// Find every `*/plugin.toml` under `dir`, parse them, and return them in a
/// deterministic dependency-aware load order.
///
/// # Why the name is the tiebreaker, and why that matters
///
/// Directory iteration order is **not** stable across filesystems. Dependencies
/// are therefore topologically sorted first, and [`Priority`] then name then
/// manifest path provide a total order among ready plugins. Load order is the
/// order the conductor drives guests, which is observable in `ActionQueue` and
/// therefore on the wire.
///
/// Errors are returned per-plugin rather than aborting the scan: one bad manifest in
/// a plugins directory must not stop the other plugins loading, and the operator
/// needs to see every problem rather than the alphabetically-first one. The
/// [`Err`] variants carry the path, so the caller can log and continue.
///
pub fn scan_directory(dir: &Path) -> Vec<Result<(PathBuf, Manifest), ManifestError>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // A missing plugins directory is the normal case for a fresh install, not an
        // error: it means "no plugins".
        return Vec::new();
    };
    let mut found: Vec<Result<(PathBuf, Manifest), ManifestError>> = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let manifest_path = entry.path().join("plugin.toml");
        if manifest_path.is_file() {
            paths.push(manifest_path);
        }
    }
    // Sort the *paths* first so that the error cases — which have no `Manifest` to
    // sort by — also come out deterministically.
    paths.sort();
    for path in paths {
        match Manifest::load(&path) {
            Ok(manifest) => found.push(Ok((path, manifest))),
            Err(e) => found.push(Err(e)),
        }
    }
    order_entries(found)
}

/// Topologically order parsed manifests while retaining one result per discovered
/// file. Invalid graph entries are returned as errors after the valid load order,
/// which preserves startup's partial-discovery behavior. Transactional staging
/// treats any such error as a rejection and never commits the partial candidate.
fn order_entries(
    entries: Vec<Result<(PathBuf, Manifest), ManifestError>>,
) -> Vec<Result<(PathBuf, Manifest), ManifestError>> {
    struct Node {
        path: PathBuf,
        manifest: Manifest,
        error: Option<ManifestError>,
    }

    let mut nodes = Vec::new();
    let mut parse_errors = Vec::new();
    for entry in entries {
        match entry {
            Ok((path, manifest)) => nodes.push(Node {
                path,
                manifest,
                error: None,
            }),
            Err(error) => parse_errors.push(Err(error)),
        }
    }

    let mut by_name = std::collections::BTreeMap::<String, Vec<usize>>::new();
    for (index, node) in nodes.iter().enumerate() {
        by_name
            .entry(node.manifest.name.clone())
            .or_default()
            .push(index);
    }
    for (name, indexes) in by_name {
        if indexes.len() > 1 {
            for index in indexes {
                nodes[index].error = Some(ManifestError::DuplicatePluginName {
                    plugin: name.clone(),
                });
            }
        }
    }

    // A dependency is available to the graph only when its own manifest is
    // uniquely named and locally valid. A required edge to anything else is a
    // missing usable dependency, which is the fail-closed result we want. The
    // fixed point matters for A -> B -> missing: both A and B must be refused,
    // rather than allowing A after only B is marked invalid.
    loop {
        let available: std::collections::BTreeSet<String> = nodes
            .iter()
            .filter(|node| node.error.is_none())
            .map(|node| node.manifest.name.clone())
            .collect();
        let mut changed = false;
        for node in &mut nodes {
            if node.error.is_some() {
                continue;
            }
            if let Some(dependency) = node
                .manifest
                .dependencies
                .required
                .iter()
                .find(|dependency| !available.contains(*dependency))
            {
                node.error = Some(ManifestError::MissingRequiredDependency {
                    plugin: node.manifest.name.clone(),
                    dependency: dependency.clone(),
                });
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let active: std::collections::BTreeSet<String> = nodes
        .iter()
        .filter(|node| node.error.is_none())
        .map(|node| node.manifest.name.clone())
        .collect();
    let mut indegree = std::collections::BTreeMap::<String, usize>::new();
    let mut dependents = std::collections::BTreeMap::<String, Vec<String>>::new();
    for node in nodes.iter().filter(|node| node.error.is_none()) {
        let mut degree = 0;
        for dependency in node
            .manifest
            .dependencies
            .required
            .iter()
            .chain(node.manifest.dependencies.optional.iter())
        {
            if active.contains(dependency) {
                degree += 1;
                dependents
                    .entry(dependency.clone())
                    .or_default()
                    .push(node.manifest.name.clone());
            }
        }
        indegree.insert(node.manifest.name.clone(), degree);
    }

    let node_key = |name: &str| {
        let node = nodes
            .iter()
            .find(|node| node.manifest.name == name)
            .expect("active graph node has a manifest");
        (node.manifest.priority, node.manifest.name.clone(), node.path.clone())
    };
    let mut ready = std::collections::BTreeSet::new();
    for (name, degree) in &indegree {
        if *degree == 0 {
            ready.insert(node_key(name));
        }
    }
    let mut ordered_names = Vec::with_capacity(active.len());
    while let Some(key) = ready.iter().next().cloned() {
        ready.remove(&key);
        let name = key.1;
        ordered_names.push(name.clone());
        if let Some(children) = dependents.get(&name) {
            for child in children {
                let degree = indegree
                    .get_mut(child)
                    .expect("dependency graph child has an indegree");
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(node_key(child));
                }
            }
        }
    }

    if ordered_names.len() != active.len() {
        let cycle: Vec<String> = active
            .iter()
            .filter(|name| !ordered_names.contains(name))
            .cloned()
            .collect();
        for node in &mut nodes {
            if node.error.is_none() && cycle.contains(&node.manifest.name) {
                node.error = Some(ManifestError::DependencyCycle {
                    plugins: cycle.clone(),
                });
            }
        }
    }

    let mut output = Vec::with_capacity(nodes.len() + parse_errors.len());
    for name in ordered_names {
        let index = nodes
            .iter()
            .position(|node| node.error.is_none() && node.manifest.name == name)
            .expect("ordered graph node has a manifest");
        let node = nodes.swap_remove(index);
        output.push(Ok((node.path, node.manifest)));
    }
    nodes.sort_by(|a, b| {
        (a.manifest.priority, &a.manifest.name, &a.path)
            .cmp(&(b.manifest.priority, &b.manifest.name, &b.path))
    });
    output.extend(nodes.into_iter().map(|node| {
        Err(node
            .error
            .expect("every un-ordered graph node has a validation error"))
    }));
    output.extend(parse_errors);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
name = "chat-responder"
version = "0.1.0"
abi = "lodestone:plugin@0.14.0"
module = "chat_responder.wasm"
priority = "normal"
description = "Replies pong to ping."
capabilities = ["log", "observe:chat", "act:chat"]
"#;

    fn parse(text: &str) -> Result<Manifest, ManifestError> {
        Manifest::parse(text, Path::new("plugin.toml"))
    }

    /// A scratch directory for the two scan tests.
    ///
    /// `std::env::temp_dir()` rather than `CARGO_TARGET_TMPDIR`, which cargo sets only
    /// for *integration test* targets — a `env!` on it inside the lib's own unit tests
    /// is a compile error, and a confusing one, since the variable exists two
    /// directories away in `tests/`. The name is specific because this directory is
    /// shared with every other process on the machine.
    fn scratch(label: &str) -> PathBuf {
        std::env::temp_dir()
            .join("lodestone-wasm-host-manifest-unit-tests")
            .join(label)
    }

    #[test]
    fn a_well_formed_manifest_parses_with_its_capabilities() {
        let m = parse(GOOD).expect("must parse");
        assert_eq!(m.name, "chat-responder");
        assert_eq!(m.priority, Priority::Normal);
        assert_eq!(m.dependencies, Dependencies::default());
        let caps = m.requested_capabilities().expect("capabilities");
        assert_eq!(caps.len(), 3);
        assert!(caps.contains(Capability::ObserveChat));
        assert!(caps.contains(Capability::ActChat));
        // The one that matters: a manifest declaring three capabilities does not
        // quietly acquire a fourth.
        assert!(!caps.contains(Capability::FsRead));
    }

    #[test]
    fn required_and_optional_dependencies_parse_as_typed_lists() {
        let text = format!(
            "{GOOD}\n[dependencies]\nrequired = [\"core\"]\noptional = [\"metrics\"]\n"
        );
        let manifest = parse(&text).expect("dependency table must parse");
        assert_eq!(manifest.dependencies.required, vec!["core"]);
        assert_eq!(manifest.dependencies.optional, vec!["metrics"]);
    }

    #[test]
    fn duplicate_or_empty_dependency_names_are_rejected_at_parse_time() {
        let duplicate = format!(
            "{GOOD}\n[dependencies]\nrequired = [\"core\"]\noptional = [\"core\"]\n"
        );
        assert!(matches!(
            parse(&duplicate),
            Err(ManifestError::DuplicateDependency { .. })
        ));

        let empty = format!("{GOOD}\n[dependencies]\nrequired = [\"  \"]\n");
        assert!(matches!(
            parse(&empty),
            Err(ManifestError::InvalidDependencyName { .. })
        ));
    }

    /// A manifest requesting an unrecognised capability is rejected loudly by
    /// name, rather than silently dropped, and the error lists what the host
    /// does know.
    #[test]
    fn an_unknown_capability_is_rejected_by_name_with_the_known_list() {
        let text = GOOD.replace(r#""act:chat""#, r#""fs:write""#);
        let err = parse(&text).expect_err("must reject");
        let msg = err.to_string();
        assert!(msg.contains("fs:write"), "{msg}");
        assert!(msg.contains("fs:read"), "the known list must be shown: {msg}");
        assert!(matches!(err, ManifestError::UnknownCapability { .. }), "{err:?}");
    }

    /// A typo in a *field* name is an error too, not a silently ignored line. This is
    /// the case `deny_unknown_fields` exists for and the one that would otherwise
    /// produce a plugin with no capabilities and no explanation.
    #[test]
    fn a_misspelled_field_is_rejected_rather_than_ignored() {
        let text = GOOD.replace("capabilities =", "capabilties =");
        let err = parse(&text).expect_err("must reject a misspelled field");
        assert!(matches!(err, ManifestError::Toml { .. }), "{err:?}");

        // The control: with the field spelled correctly, the same text parses and the
        // capabilities are present — so the rejection above is about the spelling and
        // not about the value.
        let caps = parse(GOOD).unwrap().requested_capabilities().unwrap();
        assert_eq!(caps.len(), 3);
    }

    /// An ABI world this host does not speak is rejected before any module is read,
    /// with both versions named — the legible failure that an unresolved-import error
    /// at instantiation would not have been.
    #[test]
    fn a_newer_abi_world_is_rejected_naming_both_versions() {
        let text = GOOD.replace("lodestone:plugin@0.14.0", "lodestone:plugin@0.12.0");
        let err = parse(&text).expect_err("must reject");
        let msg = err.to_string();
        assert!(msg.contains("0.12.0"), "{msg}");
        assert!(msg.contains(ABI_WORLD), "{msg}");
        assert!(matches!(err, ManifestError::AbiMismatch { .. }), "{err:?}");
    }

    /// Omitting `capabilities` yields the empty set — a plugin that can do nothing —
    /// rather than a default grant. The safe direction, asserted rather than assumed.
    #[test]
    fn an_absent_capability_list_grants_nothing() {
        let text = GOOD
            .lines()
            .filter(|l| !l.starts_with("capabilities"))
            .collect::<Vec<_>>()
            .join("\n");
        let m = parse(&text).expect("must parse without a capability list");
        assert!(m.requested_capabilities().unwrap().is_empty());
    }

    /// All six priority names parse, and they sort in Bukkit's order.
    #[test]
    fn every_priority_name_parses_and_the_order_is_bukkits() {
        let names = [
            ("lowest", Priority::Lowest),
            ("low", Priority::Low),
            ("normal", Priority::Normal),
            ("high", Priority::High),
            ("highest", Priority::Highest),
            ("monitor", Priority::Monitor),
        ];
        let mut parsed = Vec::new();
        for (name, expected) in names {
            let text = GOOD.replace(r#"priority = "normal""#, &format!("priority = \"{name}\""));
            let m = parse(&text).unwrap_or_else(|e| panic!("`{name}` must parse: {e}"));
            assert_eq!(m.priority, expected);
            parsed.push(m.priority);
        }
        let mut sorted = parsed.clone();
        sorted.sort_unstable();
        assert_eq!(parsed, sorted, "the declaration order above must be the sort order");
        // A name that is not one of the six is an error, not a silent `Normal`.
        let text = GOOD.replace(r#"priority = "normal""#, r#"priority = "urgent""#);
        assert!(parse(&text).is_err());
    }

    /// Omitting `priority` defaults to `normal` — the tier for a plugin with no
    /// stated opinion.
    #[test]
    fn an_absent_priority_defaults_to_normal() {
        let text = GOOD
            .lines()
            .filter(|l| !l.starts_with("priority"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse(&text).unwrap().priority, Priority::Normal);
    }

    /// A missing module is reported against the manifest that named it.
    #[test]
    fn a_missing_module_names_the_plugin_and_the_path_it_looked_at() {
        let m = parse(GOOD).unwrap();
        let err = m
            .resolved_module(Path::new("/nonexistent.invalid/plugin.toml"))
            .expect_err("must report the missing module");
        let msg = err.to_string();
        assert!(msg.contains("chat-responder"), "{msg}");
        assert!(msg.contains("chat_responder.wasm"), "{msg}");
    }

    /// `scan_directory` on a directory that does not exist is empty, not an error: a
    /// fresh install has no `plugins/` and that means "no plugins".
    #[test]
    fn scanning_a_missing_directory_yields_no_plugins() {
        assert!(scan_directory(Path::new("/nonexistent.invalid/plugins")).is_empty());
    }

    /// Load order is priority-then-name, and it is a total order — so it does not
    /// depend on the filesystem's directory iteration order, which is not stable
    /// across machines.
    #[test]
    fn scan_directory_orders_by_priority_then_name() {
        let root = scratch("manifest-order");
        let _ = std::fs::remove_dir_all(&root);
        // Deliberately created in an order that is neither the priority order nor the
        // alphabetical one, so a scan that just echoed `read_dir` would fail.
        for (dir, name, priority) in [
            ("z-first", "aaa", "lowest"),
            ("a-last", "zzz", "monitor"),
            ("m-mid", "mmm", "normal"),
            ("b-mid", "bbb", "normal"),
        ] {
            let d = root.join(dir);
            std::fs::create_dir_all(&d).expect("mkdir");
            let text = GOOD
                .replace(r#"name = "chat-responder""#, &format!("name = \"{name}\""))
                .replace(r#"priority = "normal""#, &format!("priority = \"{priority}\""));
            std::fs::write(d.join("plugin.toml"), text).expect("write");
        }

        let order: Vec<String> = scan_directory(&root)
            .into_iter()
            .map(|r| r.expect("all four manifests are valid").1.name)
            .collect();
        assert_eq!(
            order,
            vec!["aaa", "bbb", "mmm", "zzz"],
            "lowest first, then the two `normal` tiers by name, then monitor last"
        );
    }

    #[test]
    fn scan_directory_orders_dependencies_before_priority_tiebreakers() {
        let root = scratch("manifest-dependency-order");
        let _ = std::fs::remove_dir_all(&root);
        let manifests = [
            (
                "child-dir",
                "child",
                "lowest",
                Some(("base", "metrics")),
            ),
            ("base-dir", "base", "monitor", None),
            ("metrics-dir", "metrics", "monitor", None),
            ("independent-dir", "independent", "lowest", None),
        ];
        for (dir, name, priority, dependencies) in manifests {
            let d = root.join(dir);
            std::fs::create_dir_all(&d).unwrap();
            let mut text = GOOD
                .replace(r#"name = "chat-responder""#, &format!("name = \"{name}\""))
                .replace(r#"priority = "normal""#, &format!("priority = \"{priority}\""));
            if let Some((required, optional)) = dependencies {
                text.push_str(&format!(
                    "\n[dependencies]\nrequired = [\"{required}\"]\noptional = [\"{optional}\"]\n"
                ));
            }
            std::fs::write(d.join("plugin.toml"), text).unwrap();
        }

        let order: Vec<String> = scan_directory(&root)
            .into_iter()
            .map(|result| result.expect("all graph entries are valid").1.name)
            .collect();
        assert_eq!(
            order,
            vec!["independent", "base", "metrics", "child"],
            "dependency edges win over priority, then priority/name orders ready plugins"
        );
    }

    #[test]
    fn a_missing_required_dependency_is_reported_but_unrelated_plugins_remain_usable() {
        let root = scratch("manifest-missing-dependency");
        let _ = std::fs::remove_dir_all(&root);
        for (dir, name, dependency) in [
            ("good-dir", "good", None),
            ("broken-dir", "broken", Some("not-installed")),
        ] {
            let d = root.join(dir);
            std::fs::create_dir_all(&d).unwrap();
            let mut text = GOOD
                .replace(r#"name = "chat-responder""#, &format!("name = \"{name}\""));
            if let Some(dependency) = dependency {
                text.push_str(&format!(
                    "\n[dependencies]\nrequired = [\"{dependency}\"]\n"
                ));
            }
            std::fs::write(d.join("plugin.toml"), text).unwrap();
        }

        let results = scan_directory(&root);
        assert_eq!(results.len(), 2);
        assert_eq!(
            results
                .iter()
                .filter_map(|result| result.as_ref().ok())
                .map(|(_, manifest)| manifest.name.as_str())
                .collect::<Vec<_>>(),
            vec!["good"]
        );
        let error = results
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("the missing dependency must be reported");
        assert!(matches!(
            error,
            ManifestError::MissingRequiredDependency {
                plugin,
                dependency
            } if plugin == "broken" && dependency == "not-installed"
        ));
    }

    #[test]
    fn a_dependency_cycle_is_reported_for_every_member() {
        let root = scratch("manifest-dependency-cycle");
        let _ = std::fs::remove_dir_all(&root);
        for (dir, name, dependencies) in [
            ("a-dir", "a", ("b", "")),
            ("b-dir", "b", ("", "a")),
        ] {
            let d = root.join(dir);
            std::fs::create_dir_all(&d).unwrap();
            let mut text = GOOD.replace(
                r#"name = "chat-responder""#,
                &format!("name = \"{name}\""),
            );
            if dependencies.0.is_empty() {
                text.push_str(&format!(
                    "\n[dependencies]\noptional = [\"{}\"]\n",
                    dependencies.1
                ));
            } else {
                text.push_str(&format!(
                    "\n[dependencies]\nrequired = [\"{}\"]\n",
                    dependencies.0
                ));
            }
            std::fs::write(d.join("plugin.toml"), text).unwrap();
        }

        let results = scan_directory(&root);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| matches!(
            result,
            Err(ManifestError::DependencyCycle { plugins })
                if plugins == &vec!["a".to_owned(), "b".to_owned()]
        )));
    }

    /// A bad manifest does not stop the good ones, and it is reported rather than
    /// dropped.
    #[test]
    fn one_bad_manifest_does_not_abort_the_scan() {
        let root = scratch("manifest-mixed");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("good")).unwrap();
        std::fs::write(root.join("good/plugin.toml"), GOOD).unwrap();
        std::fs::create_dir_all(root.join("bad")).unwrap();
        std::fs::write(
            root.join("bad/plugin.toml"),
            GOOD.replace(r#""act:chat""#, r#""act:everything""#),
        )
        .unwrap();

        let results = scan_directory(&root);
        assert_eq!(results.len(), 2, "both manifests must be reported");
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "the good one must still load"
        );
        let err = results
            .iter()
            .find_map(|r| r.as_ref().err())
            .expect("the bad one must be reported as an error");
        assert!(err.to_string().contains("act:everything"), "{err}");
    }
}
