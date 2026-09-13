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

/// The Minecraft version the live gate and census target. Named explicitly so
/// jar selection never depends on `read_dir` iteration order.
pub const GATE_VERSION: &str = "26.2";

/// The command that populates `.cache/mc/<version>/client.jar`.
pub const FETCH_HINT: &str = "cargo run -p xtask -- fetch-assets --version 26.2";

/// `.cache/mc` under the workspace root, if it exists.
#[must_use]
pub fn cache_root() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .to_path_buf();
    let cache = root.join(".cache/mc");
    cache.is_dir().then_some(cache)
}

/// Pure path construction: `<cache_root>/<version>/client.jar`. Does not touch
/// the filesystem and does not scan — the version is named, so the result is
/// independent of what other version directories happen to exist.
#[must_use]
pub fn jar_path_for_version(cache_root: &Path, version: &str) -> PathBuf {
    cache_root.join(version).join("client.jar")
}

/// Resolves the [`GATE_VERSION`] client jar, or an actionable error message.
/// Pure over its `cache_root` argument (aside from the final `is_file` probe),
/// so the fail-closed behaviour is unit-testable without touching the real
/// cache. `require_client_jar` is the panicking wrapper CI callers use.
///
/// # Errors
/// Returns an actionable message when the cache or the named-version jar is
/// absent.
pub fn resolve_jar(cache_root: Option<&Path>) -> Result<PathBuf, String> {
    let Some(cache) = cache_root else {
        return Err(format!(
            "live gate requires a populated `.cache/mc` under the workspace root, but none \
             exists. Fetch the {GATE_VERSION} assets with:\n    {FETCH_HINT}"
        ));
    };
    let jar = jar_path_for_version(cache, GATE_VERSION);
    if jar.is_file() {
        Ok(jar)
    } else {
        Err(format!(
            "live gate requires {} specifically (selected by version, not by directory scan \
             order), but it is missing. Fetch it with:\n    {FETCH_HINT}",
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

/// The [`GATE_VERSION`] client jar, or a loud actionable panic. **Fails
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
        let cache = Path::new("/nonexistent/.cache/mc");
        let jar = jar_path_for_version(cache, GATE_VERSION);

        assert!(
            jar.ends_with("26.2/client.jar"),
            "expected the {GATE_VERSION} jar, got {}",
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
        let no_cache = resolve_jar(None).unwrap_err();
        assert!(
            no_cache.contains(FETCH_HINT),
            "missing-cache error must tell the user how to fetch: {no_cache}",
        );

        let missing = resolve_jar(Some(Path::new("/nonexistent/.cache/mc"))).unwrap_err();
        assert!(
            missing.contains("26.2/client.jar") && missing.contains(FETCH_HINT),
            "missing-jar error must name the {GATE_VERSION} jar and the fetch command: {missing}",
        );
    }

    /// The registry check fails closed too, pointing at the data generator.
    #[test]
    fn missing_registry_fails_closed_with_actionable_message() {
        let jar = Path::new("/nonexistent/.cache/mc/26.2/client.jar");
        let err = resolve_blocks_report(jar).unwrap_err();
        assert!(
            err.contains("blocks.json") && err.contains("--reports"),
            "missing-registry error must name the report and how to generate it: {err}",
        );
    }
}
