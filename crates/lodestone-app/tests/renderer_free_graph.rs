//! The guard on milestone zero's load-bearing property: **`lodestone-app` must
//! not reach the renderer.**
//!
//! # What this measures, and what it does not
//!
//! The *measurement* is a `cargo tree`, recorded in
//! `docs/plugin-registration.md` — 329 crates reachable from this one (normal +
//! dev + build), of which zero match `wgpu` or `winit`, against 448 crates and
//! 12 matches for `lodestone-shell` run through the identical command and the
//! identical grep. That negative control is what makes the zero meaningful; this
//! repo has a documented case of a search reporting absence because the search
//! itself was broken.
//!
//! This test is the *guard*, not the measurement. It asserts the **direct**
//! dependency lists in `Cargo.toml` against an explicit allowlist, because that
//! is the edge a future change actually adds — nobody grows the transitive graph
//! except by naming a new crate here. It deliberately does not shell out to
//! `cargo tree`: a nested cargo invocation inside a test contends on the package
//! cache lock in a checkout where other builds run concurrently, and a test that
//! can hang for an unrelated reason is worse than one with a narrower subject.
//!
//! # The control
//!
//! [`the_detector_finds_the_renderer_in_the_shells_manifest`] runs the same
//! parser and the same predicate over `lodestone-shell/Cargo.toml`, which must
//! **find** `wgpu` and `winit`. Without it, a parser that silently returned an
//! empty dependency list would pass the positive assertion forever.

use std::path::{Path, PathBuf};

/// Every crate `lodestone-app` is allowed to name directly. Each entry is
/// justified in `crates/lodestone-app/Cargo.toml`; adding one here without
/// adding it there does nothing, and the reverse fails this test.
const ALLOWED: &[&str] = &[
    // [dependencies]
    "lodestone-ecs",
    "lodestone-controller",
    "lodestone-physics",
    "bevy_ecs",
    // [dev-dependencies] -- the conformance plugin and its fixture world.
    "lodestone-autopilot",
    "lodestone-world",
    "lodestone-model",
];

/// Names that must never appear, at any depth, in a headless consumer's graph.
const FORBIDDEN: &[&str] = &["wgpu", "winit"];

fn workspace_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `crates/lodestone-app`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/lodestone-app has two ancestors")
        .to_path_buf()
}

/// The dependency-table keys of a manifest, from every `[dependencies]`,
/// `[dev-dependencies]` and `[build-dependencies]` table including
/// `[target.'cfg(...)'.dependencies]`.
///
/// Hand-parsed rather than pulling in a TOML crate: this is one file, the
/// structure is `key = ...` lines under `[...dependencies]` headers, and adding
/// a dependency to the crate whose job is to have almost none would be its own
/// small irony.
fn dependency_names(manifest: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(manifest)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));
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

/// The positive assertion: this crate names nothing outside the allowlist, and
/// nothing forbidden.
#[test]
fn lodestone_app_names_only_allowlisted_dependencies() {
    let manifest = workspace_root().join("crates/lodestone-app/Cargo.toml");
    let names = dependency_names(&manifest);

    assert!(
        !names.is_empty(),
        "premise: the parser must find this crate's dependencies at all"
    );

    for name in &names {
        assert!(
            ALLOWED.contains(&name.as_str()),
            "`{name}` is a new direct dependency of lodestone-app. If it is renderer-free \
             and genuinely belongs below the shell, add it to ALLOWED with the reason; if \
             it reaches wgpu/winit at any depth it belongs in lodestone-shell instead. \
             Re-run the `cargo tree` measurement in docs/plugin-registration.md either way."
        );
    }
    for forbidden in FORBIDDEN {
        assert!(
            !names.iter().any(|n| n == forbidden),
            "lodestone-app must never name `{forbidden}`"
        );
    }
}

/// The control. The same parser and the same predicate over the shell's
/// manifest, which **must** find both forbidden names — proving a pass above is
/// a real absence and not a parser that reads nothing.
#[test]
fn the_detector_finds_the_renderer_in_the_shells_manifest() {
    let manifest = workspace_root().join("crates/lodestone-shell/Cargo.toml");
    let names = dependency_names(&manifest);

    for forbidden in FORBIDDEN {
        assert!(
            names.iter().any(|n| n == forbidden),
            "control failed: the shell's manifest must name `{forbidden}`, so a clean \
             result for lodestone-app means something. Found: {names:?}"
        );
    }
}
