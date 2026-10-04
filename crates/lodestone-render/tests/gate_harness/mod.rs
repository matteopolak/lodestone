//! Shared jar/registry discovery for the live gate and the model census.
//!
//! Both integration tests read a fetched vanilla `client.jar` and Mojang's
//! `generated/reports/blocks.json` out of `.cache/mc/<version>/`. Discovery is
//! centralised here for two reasons, both learned the hard way:
//!
//! 1. **Select the jar by version, never by iteration order.** `read_dir` order
//!    is neither deterministic nor sorted. Multiple version jars now coexist in
//!    the shared cache (a sibling agent doing multi-version asset work added
//!    1.8.9 and 1.12.2 alongside 26.2). A first-match-wins scan silently
//!    redirected the chunk-to-pixels gate at the *wrong* jar — one without a
//!    `blocks.json` — which made the gate a 0.00s no-op that asserted nothing.
//!    Naming the version removes the ordering dependency entirely.
//!
//! 2. **Fail closed, not open.** The tests that call these helpers are
//!    `#[ignore]`d, so running them is already an explicit opt-in. At that point
//!    a missing jar, missing registry, or unreachable server is an *environment
//!    failure* worth a loud, actionable panic — never a silent skip that reports
//!    `ok` while asserting nothing. A panic tells the next person exactly how to
//!    fix their environment; a green `ok` lets them believe the gate ran.
//!
//! Shared across two integration-test binaries (one of which is feature-gated),
//! so some helpers are unused from any single binary's point of view.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The Minecraft version the live gate and census target: the release the live
/// oracle server runs, which is the current reference version. Resolved to a
/// named directory so jar selection never depends on `read_dir` iteration order.
#[must_use]
pub fn gate_version() -> String {
    lodestone_mc_cache::current_version()
}

/// The command that populates `.cache/mc/<version>/client.jar`.
#[must_use]
pub fn fetch_hint() -> String {
    format!("cargo run -p xtask -- fetch-assets --version {}", gate_version())
}

/// `.cache/mc` under the workspace root, if it exists.
#[must_use]
pub fn cache_root() -> Option<PathBuf> {
    let cache = lodestone_mc_cache::workspace_root().join(".cache/mc");
    cache.is_dir().then_some(cache)
}

/// Pure path construction: `<cache_root>/<version>/client.jar`. Does not touch
/// the filesystem and does not scan — the version is named, so the result is
/// independent of what other version directories happen to exist.
#[must_use]
pub fn jar_path_for_version(cache_root: &Path, version: &str) -> PathBuf {
    cache_root.join(version).join("client.jar")
}

/// Resolves the [`gate_version`] client jar, or an actionable error message.
/// Pure over its `cache_root` argument (aside from the final `is_file` probe),
/// so the fail-closed behaviour is unit-testable without touching the real
/// cache. `require_client_jar` is the panicking wrapper CI callers use.
///
/// # Errors
/// Returns an actionable message when the cache or the named-version jar is
/// absent.
pub fn resolve_jar(cache_root: Option<&Path>) -> Result<PathBuf, String> {
    let (gate_version, fetch_hint) = (gate_version(), fetch_hint());
    let Some(cache) = cache_root else {
        return Err(format!(
            "live gate requires a populated `.cache/mc` under the workspace root, but none \
             exists. Fetch the {gate_version} assets with:\n    {fetch_hint}"
        ));
    };
    let jar = jar_path_for_version(cache, &gate_version);
    if jar.is_file() {
        Ok(jar)
    } else {
        Err(format!(
            "live gate requires {} specifically (selected by version, not by directory scan \
             order), but it is missing. Fetch it with:\n    {fetch_hint}",
            jar.display(),
        ))
    }
}

/// Resolves the `generated/reports/blocks.json` beside `jar`, or an actionable
/// error message.
///
/// # Errors
/// Returns an actionable message when the report is absent.
pub fn resolve_blocks_report(jar: &Path) -> Result<PathBuf, String> {
    let version_dir = jar
        .parent()
        .expect("client.jar path always has a parent version directory");
    let report = version_dir.join("generated/reports/blocks.json");
    if report.is_file() {
        Ok(report)
    } else {
        Err(format!(
            "live gate requires {} — Mojang's data-generator block report. Generate it into the \
             version directory with the data generator, e.g.:\n    \
             java -DbundlerMainClass=net.minecraft.data.Main -jar {} --reports",
            report.display(),
            jar.display(),
        ))
    }
}

/// The [`gate_version`] client jar, or a loud actionable panic. **Fails
/// closed:** callers are `#[ignore]`d tests, so absence is an environment
/// failure, not a reason to skip.
#[must_use]
pub fn require_client_jar() -> PathBuf {
    resolve_jar(cache_root().as_deref()).unwrap_or_else(|msg| panic!("{msg}"))
}

/// The `generated/reports/blocks.json` registry beside `jar`, or a loud
/// actionable panic. **Fails closed** for the same reason as
/// [`require_client_jar`].
#[must_use]
pub fn require_blocks_report(jar: &Path) -> PathBuf {
    resolve_blocks_report(jar).unwrap_or_else(|msg| panic!("{msg}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this whole module exists to prevent: selection must be by
    /// *named version*, never by directory iteration order. A first-match-wins
    /// `read_dir` scan picked 1.12.2 over 26.2 and voided the gate; a named
    /// lookup cannot, regardless of which sibling version dirs coexist.
    #[test]
    fn harness_selects_jar_by_named_version_not_scan_order() {
        let gate_version = gate_version();
        let cache = Path::new("/nonexistent/.cache/mc");
        let jar = jar_path_for_version(cache, &gate_version);

        assert!(
            jar.ends_with(format!("{gate_version}/client.jar")),
            "expected the {gate_version} jar, got {}",
            jar.display(),
        );
        // Coexisting sibling versions must not be selectable through the named
        // path — the selector never scans, so their presence is irrelevant.
        for sibling in ["1.8.9", "1.12.2", "creative", "online262", "oracle"] {
            assert_ne!(
                jar,
                jar_path_for_version(cache, sibling),
                "named selection must not collide with sibling version {sibling}",
            );
        }
    }

    /// Demonstrate the fail-closed property directly: with no cache and with a
    /// missing named-version jar, resolution is an `Err` carrying an actionable
    /// message — never a silent success that lets a gate pass asserting nothing.
    #[test]
    fn missing_jar_fails_closed_with_actionable_message() {
        let (gate_version, fetch_hint) = (gate_version(), fetch_hint());
        let no_cache = resolve_jar(None).unwrap_err();
        assert!(
            no_cache.contains(&fetch_hint),
            "missing-cache error must tell the user how to fetch: {no_cache}",
        );

        let missing = resolve_jar(Some(Path::new("/nonexistent/.cache/mc"))).unwrap_err();
        assert!(
            missing.contains(&format!("{gate_version}/client.jar")) && missing.contains(&fetch_hint),
            "missing-jar error must name the {gate_version} jar and the fetch command: {missing}",
        );
    }

    /// The registry check fails closed too, pointing at the data generator.
    #[test]
    fn missing_registry_fails_closed_with_actionable_message() {
        let jar = Path::new("/nonexistent/.cache/mc/pinned/client.jar");
        let err = resolve_blocks_report(jar).unwrap_err();
        assert!(
            err.contains("blocks.json") && err.contains("--reports"),
            "missing-registry error must name the report and how to generate it: {err}",
        );
    }
}
