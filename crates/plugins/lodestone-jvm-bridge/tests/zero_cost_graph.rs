//! The guard on "zero cost when no Java plugin is loaded".
//!
//! # What this measures, and the trap it exists to avoid
//!
//! The issue's packaging constraint is that a user who loads no Java plugin
//! pays nothing: no `libjvm` linkage, no JVM startup, no code reached. Two
//! things must hold, and they are checked separately because they fail
//! separately:
//!
//! 1. **Production consumers explicitly opt in.** Checked by scanning every
//!    workspace manifest: the dedicated host may name this crate only through
//!    an optional dependency and its default-off `jvm` feature. Nested
//!    standalone workspaces are executable experiments, not production graph
//!    members, so they are excluded explicitly.
//! 2. **This crate names no JVM linkage of its own**, and if it ever does, it
//!    does so behind an optional dependency rather than an unconditional one.
//!
//! ## Why this is a manifest scan and not a `Cargo.lock` grep
//!
//! Because a lockfile grep would report a violation that does not exist.
//! **`jni` 0.22.4 is already in this workspace's `Cargo.lock`** — pulled in by
//! `android-activity`, `cpal`, `hickory-*` and `rustls-platform-verifier`, all
//! of which reach it only on targets we do not build. Measured:
//! `cargo tree -p lodestone-shell -e normal -i jni` prints *"nothing to
//! print"* for the host. So a lockfile grep answers a different question than
//! the one being asked, and answers it wrongly — the same class of mistake as
//! grepping for a field name and finding every struct that has one.
//!
//! ## Why not shell out to `cargo tree`
//!
//! `lodestone-app`'s `renderer_free_graph.rs` settled this for the identical
//! problem and the reasoning is borrowed wholesale: a nested cargo invocation
//! inside a test contends on the package-cache lock in a shared checkout where
//! other builds run concurrently, and a test that can hang for an unrelated
//! reason is worse than one with a narrower subject. `cargo tree` is the
//! *measurement*, recorded in `docs/java-plugin-bridge.md`; this is the guard.
//!
//! # The control
//!
//! [`the_detector_finds_a_dependent_where_one_really_exists`] runs the same
//! parser and the same predicate looking for a crate that **is** widely
//! depended upon, and must find it. Without that, a parser that silently
//! returned no dependencies would certify this crate as unreferenced forever —
//! which is exactly the shape of "an audit that prints nothing is a failure to
//! run, never a pass".

use std::path::{Path, PathBuf};

/// This crate. Production consumers must keep its edge default-off.
const SELF_NAME: &str = "lodestone-jvm-bridge";

/// Crate names that imply JVM linkage. Substrings are not used — an exact
/// dependency-key match is what is wanted, since `jni` must not match
/// `jni-sys` transitively through a name comparison that was too loose.
const JVM_LINKING_CRATES: &[&str] = &["jni", "jni-sys", "j4rs", "jvm-rs", "robusta_jni"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("the crate has three ancestors up to the workspace root")
        .to_path_buf()
}

/// Every workspace-member `Cargo.toml` under `crates/` and `xtask/`.
fn workspace_manifests(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.join("crates"), root.join("xtask")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // `target/` inside a nested workspace member is build output,
                // not source, and can be enormous.
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "Cargo.toml") {
                out.push(path);
            }
        }
    }
    let root_manifest = root.join("Cargo.toml");
    out.retain(|manifest| !declares_standalone_workspace(manifest));
    out.push(root_manifest);
    out
}

/// A nested `[workspace]` root is not a member of the production workspace.
fn declares_standalone_workspace(manifest: &Path) -> bool {
    std::fs::read_to_string(manifest).is_ok_and(|text| {
        text.lines().any(|line| line.trim() == "[workspace]")
    })
}

/// Dependency-table keys from a manifest, from every `[dependencies]`,
/// `[dev-dependencies]`, `[build-dependencies]` and
/// `[target.'cfg(...)'.dependencies]` table.
///
/// Hand-parsed, following `lodestone-app`'s `renderer_free_graph.rs`: one file,
/// `key = ...` lines under `[...dependencies]` headers, and a guard crate
/// growing a TOML dependency would be its own small irony.
fn dependency_names(manifest: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(manifest) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut in_deps = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = trimmed.ends_with("dependencies]");
            continue;
        }
        if !in_deps || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, _)) = trimmed.split_once('=') {
            let key = key.trim().trim_matches('"');
            if !key.is_empty() && !key.contains(' ') {
                names.push(key.to_owned());
            }
        }
    }
    names
}

/// Production consumers keep the runtime behind an optional dependency and
/// an explicitly default-off feature, so default builds cannot reach JNI.
#[test]
fn production_bridge_edges_are_explicitly_default_off() {
    let root = workspace_root();
    let manifests = workspace_manifests(&root);

    assert!(
        manifests.len() > 20,
        "premise failed: found only {} manifests, so the scan is not reaching \
         the workspace. A pass below would mean nothing.",
        manifests.len()
    );

    let mut dependents = Vec::new();
    for manifest in &manifests {
        // The crate's own manifest names itself in `[package]`, not in a
        // dependency table, so it is not a special case — but skip it anyway
        // so a future dev-dependency on itself reads clearly.
        if manifest.parent().is_some_and(|p| {
            p.file_name().is_some_and(|n| n == SELF_NAME)
        }) {
            continue;
        }
        if dependency_names(manifest)
            .iter()
            .any(|name| name == SELF_NAME)
        {
            let text = std::fs::read_to_string(manifest).expect("read dependent manifest");
            if manifest != &root.join("crates/lodestone-dedicated-server/Cargo.toml")
                || !bridge_edge_is_default_off(&text)
            {
                dependents.push(manifest.display().to_string());
            }
        }
    }

    assert!(
        dependents.is_empty(),
        "{SELF_NAME} has an unguarded dependency in {dependents:?}. The bridge must not \
         be reachable from a default build — a user who loads no Java plugin \
         pays no libjvm linkage, no JVM startup and no per-tick cost. If this \
         edge is deliberate, it belongs behind an OPTIONAL dependency and a \
         default-off feature, and this test should be changed to assert that \
         rather than removed. See docs/java-plugin-bridge.md."
    );
}

fn bridge_edge_is_default_off(text: &str) -> bool {
    let lines: Vec<String> = text.lines()
        .map(|line| line.split('#').next().unwrap_or("").chars()
            .filter(|character| !character.is_whitespace()).collect())
        .collect();
    lines.iter().any(|line| line.starts_with("default=[") && !line.contains("\"jvm\""))
        && lines.iter().any(|line| line == "jvm=[\"dep:lodestone-jvm-bridge\"]")
        && lines.iter().any(|line| line.starts_with("lodestone-jvm-bridge=")
            && line.contains("optional=true") && line.contains("features=[\"jvm\"]"))
}

#[test]
fn optional_edge_detector_rejects_unconditional_and_default_enabled_controls() {
    let manifest = r#"
[features]
default = []
jvm = ["dep:lodestone-jvm-bridge"]
[dependencies]
lodestone-jvm-bridge = { path = "bridge", optional = true, features = ["jvm"] }
"#;
    assert!(bridge_edge_is_default_off(manifest));
    assert!(bridge_edge_is_default_off(&manifest.replace("default = []", "default = [\"v26-2\"]")));
    assert!(!bridge_edge_is_default_off(&manifest.replace("optional = true", "optional = false")));
    assert!(!bridge_edge_is_default_off(&manifest.replace("default = []", "default = [\"jvm\"]")));
    assert!(!bridge_edge_is_default_off(&manifest.replace("dep:lodestone-jvm-bridge", "unrelated")));
}

/// The control. The same parser and the same predicate, looking for a crate
/// that is genuinely depended upon all over the workspace — it must find it.
///
/// Without this, a parser that returned an empty list for every manifest would
/// report the bridge as unreferenced forever, and the pass above would be a
/// measurement of nothing.
#[test]
fn the_detector_finds_a_dependent_where_one_really_exists() {
    let root = workspace_root();
    let mut dependents = 0;
    for manifest in workspace_manifests(&root) {
        if dependency_names(&manifest)
            .iter()
            .any(|name| name == "lodestone-ecs")
        {
            dependents += 1;
        }
    }
    assert!(
        dependents >= 3,
        "control failed: the scan found only {dependents} crates naming \
         lodestone-ecs, which is depended upon widely. The parser is not \
         reading dependency tables, so a clean result for the bridge proves \
         nothing."
    );
}

/// The control on the standalone-workspace exclusion: the invocation spike
/// really does name the bridge, but it must not be classified as a production
/// workspace consumer.
#[test]
fn the_standalone_invocation_spike_is_outside_the_production_graph() {
    let root = workspace_root();
    let spike_root = root.join(
        "crates/plugins/lodestone-jvm-bridge/spike/invocation",
    );
    let spike = spike_root.join("Cargo.toml");
    assert!(
        dependency_names(&spike).iter().any(|name| name == SELF_NAME),
        "premise failed: the invocation spike no longer depends on the bridge"
    );
    assert!(
        declares_standalone_workspace(&spike),
        "the invocation spike must remain a standalone Cargo workspace"
    );
    assert!(
        !workspace_manifests(&root).contains(&spike),
        "a standalone spike was classified as a production workspace member"
    );

    let ignore = std::fs::read_to_string(spike_root.join(".gitignore"))
        .expect("the standalone spike must define its artifact exclusions");
    assert!(
        ignore.lines().any(|line| line.trim() == "target/"),
        "an ordinary standalone Cargo run must not expose target/ as untracked source"
    );
}

/// This crate must not link a JVM by default. The optional `jvm` feature is the
/// explicit opt-in boundary for hosts that want to start one.
#[test]
fn the_bridge_names_no_unconditional_jvm_linkage() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).expect("read own manifest");
    let names = dependency_names(&manifest);

    assert!(
        !names.is_empty(),
        "premise failed: this crate's own dependency table did not parse"
    );

    for candidate in JVM_LINKING_CRATES {
        if !names.iter().any(|name| name == candidate) {
            continue;
        }
        // Present — permitted only if it is optional.
        let declared = text
            .lines()
            .find(|line| line.trim_start().starts_with(candidate))
            .unwrap_or("");
        assert!(
            declared.contains("optional = true"),
            "{candidate} is an unconditional dependency of {SELF_NAME}. A JVM \
             bridge cannot be WASM-sandboxed and must never be linked into a \
             build that did not ask for it: declare it `optional = true` behind \
             a default-off feature. Line was: {declared:?}"
        );
    }
}

/// The production runtime boundary must be mechanically default-off. Keeping
/// this in the graph guard makes a future feature edit fail here rather than
/// silently adding JVM linkage to ordinary server builds.
#[test]
fn jvm_support_is_explicitly_default_off() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).expect("read own manifest");
    assert!(
        text.lines().any(|line| line.trim() == "default = []"),
        "the bridge must have an explicit empty default feature set"
    );
    assert!(
        text.lines()
            .any(|line| line.trim() == "paper-preflight = [\"dep:zip\"]"),
        "operator-jar preflight must stay behind its own named default-off feature"
    );
    assert!(
        text.lines()
            .any(|line| line.trim() == "jvm = [\"paper-preflight\", \"dep:jni\"]"),
        "JVM startup must include archive preflight but keep JNI behind the \
         named jvm feature"
    );
    let zip = text.lines().find(|line| line.trim_start().starts_with("zip ="));
    assert!(
        zip.is_some_and(|line| line.contains("optional = true")),
        "the archive reader is part of operator-jar discovery and must not enter \
         the default graph"
    );
}
