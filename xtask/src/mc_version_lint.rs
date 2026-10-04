//! `cargo xtask check-mc-version` — fails on a hard-coded `.cache/mc/<digit...>`
//! path outside the allowlist.
//!
//! # Why this exists
//!
//! The repo's reference cache is read from `.cache/mc/<version>`, and the
//! current version lives in one place (`mc-version`, read through
//! `lodestone_mc_cache` and the `mc_version` just variable). A literal
//! `.cache/mc/26.3` in code reintroduces the many-places bump this design
//! removed. A reader that is genuinely tied to one release says so by naming
//! a pin (`lodestone_mc_cache::PINNED_26_2`, the `pinned_mc` just variable),
//! which has no literal path in it; a literal that must remain (a legacy
//! version's fixture, an oracle script whose server is that release) is
//! listed in `xtask/check-mc-version.toml` with a reason.
//!
//! # Scope
//!
//! Rust files: non-comment lines only. `.py`/`.sh`/`.toml`/`Justfile`: every
//! line that does not start with `#`. Docs are prose and are not scanned.
//! A stale allowlist entry (matching no hit) fails, so the list cannot rot.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Default allowlist, relative to the workspace root.
pub const DEFAULT_ALLOWLIST: &str = "xtask/check-mc-version.toml";

const EXCLUDED_DIRS: &[&str] = &["target", ".git", ".cache", ".worktrees", "node_modules", "dist"];

/// Paths that define or test the rule itself.
const SELF_PATHS: &[&str] = &["crates/lodestone-mc-cache/", "xtask/src/mc_version_lint.rs"];

/// One literal hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub file: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone)]
struct Allow {
    path: String,
    reason: String,
}

fn has_literal(line: &str) -> bool {
    const NEEDLE: &str = ".cache/mc/";
    let mut rest = line;
    while let Some(at) = rest.find(NEEDLE) {
        let after = &rest[at + NEEDLE.len()..];
        if after.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return true;
        }
        rest = after;
    }
    false
}

fn scanned(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some("Justfile")
        || matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("rs" | "py" | "sh" | "toml")
        )
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read dir {}", dir.display())),
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !EXCLUDED_DIRS.contains(&name) {
                walk(&path, out)?;
            }
        } else if scanned(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// Every literal under `root`, as `(file relative to root, line, text)`.
///
/// # Errors
///
/// Returns an error when a directory cannot be read.
pub fn scan(root: &Path) -> Result<Vec<Hit>> {
    let mut files = Vec::new();
    walk(root, &mut files)?;
    files.sort();
    let mut hits = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if SELF_PATHS.iter().any(|p| rel.starts_with(p)) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rust = rel.ends_with(".rs");
        for (index, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            let comment = if rust {
                trimmed.starts_with("//")
            } else {
                trimmed.starts_with('#')
            };
            if !comment && has_literal(line) {
                hits.push(Hit {
                    file: rel.clone(),
                    line: index + 1,
                    text: trimmed.chars().take(120).collect(),
                });
            }
        }
    }
    Ok(hits)
}

fn parse_allowlist(contents: &str) -> Result<Vec<Allow>> {
    let mut out = Vec::new();
    let mut path: Option<String> = None;
    let mut reason: Option<String> = None;
    let mut open = false;
    let mut finish = |path: &mut Option<String>, reason: &mut Option<String>| -> Result<()> {
        match (path.take(), reason.take()) {
            (Some(path), Some(reason)) if !path.is_empty() && !reason.trim().is_empty() => {
                out.push(Allow { path, reason });
                Ok(())
            }
            _ => bail!("an [[allow]] entry needs a non-empty path and reason"),
        }
    };
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[allow]]" {
            if open {
                finish(&mut path, &mut reason)?;
            }
            open = true;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("unparseable allowlist line {line:?}");
        };
        let value = value.trim().trim_matches('"').to_owned();
        match key.trim() {
            "path" => path = Some(value),
            "reason" => reason = Some(value),
            other => bail!("unknown allowlist key {other:?}"),
        }
    }
    if open {
        finish(&mut path, &mut reason)?;
    }
    Ok(out)
}

/// A path entry covers a file equal to it, or any file under it when it ends in `/`.
fn covers(allow: &Allow, file: &str) -> bool {
    file == allow.path || (allow.path.ends_with('/') && file.starts_with(&allow.path))
}

/// The hits not covered by `allowlist`, plus allowlist entries that covered nothing.
///
/// # Errors
///
/// Returns an error when the tree or the allowlist cannot be read.
pub fn violations(root: &Path, allowlist: &Path) -> Result<(Vec<Hit>, Vec<String>)> {
    let text = std::fs::read_to_string(root.join(allowlist))
        .with_context(|| format!("read {}", allowlist.display()))?;
    let allows = parse_allowlist(&text)?;
    let hits = scan(root)?;
    let bad = hits
        .iter()
        .filter(|h| !allows.iter().any(|a| covers(a, &h.file)))
        .cloned()
        .collect();
    let stale = allows
        .iter()
        .filter(|a| !hits.iter().any(|h| covers(a, &h.file)))
        .map(|a| format!("{} ({})", a.path, a.reason))
        .collect();
    Ok((bad, stale))
}

/// Entry point for `cargo xtask check-mc-version`.
///
/// # Errors
///
/// Fails when a literal is not allowlisted or an allowlist entry is stale.
pub fn run_check_mc_version(root: &Path, allowlist: &Path) -> Result<()> {
    let (bad, stale) = violations(root, allowlist)?;
    for hit in &bad {
        println!("VIOLATION {}:{}: {}", hit.file, hit.line, hit.text);
    }
    for entry in &stale {
        println!("STALE allowlist entry: {entry}");
    }
    if !bad.is_empty() || !stale.is_empty() {
        bail!(
            "RESULT: FAIL -- {} hard-coded .cache/mc/<version> literal(s), {} stale allowlist \
             entr(ies). Fix: read the version through lodestone_mc_cache (or the mc_version \
             just variable / scripts/mc_version.py); a reader tied to one release names \
             PINNED_26_2 instead. A literal that must remain goes in {} with a reason.",
            bad.len(),
            stale.len(),
            allowlist.display()
        );
    }
    println!("RESULT: PASS -- no hard-coded .cache/mc/<version> literals outside the allowlist");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    /// The control: the detector fires on a planted literal, and does not fire
    /// on a comment, on the helper, or on a covered file. Without the first
    /// assertion a pass on the real tree would prove nothing.
    #[test]
    fn fires_on_a_planted_literal_and_spares_the_rest() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        write(root, "crates/a/src/lib.rs", "fn f() { let _ = \".cache/mc/26.3/client.jar\"; }\n");
        write(root, "crates/b/src/lib.rs", "// see .cache/mc/26.3/client.jar\nfn g() {}\n");
        write(root, "crates/c/src/lib.rs", "fn h() { let _ = lodestone_mc_cache::cache_root(); }\n");
        write(root, "crates/lodestone-mc-cache/src/lib.rs", "const X: &str = \".cache/mc/26.3\";\n");
        write(root, "scripts/oracle.sh", "CACHE=.cache/mc/26.2\n");
        write(root, "scripts/note.py", "# .cache/mc/26.2 is a comment here\n");
        write(root, "allow.toml", "[[allow]]\npath = \"scripts/oracle.sh\"\nreason = \"pinned oracle\"\n");
        let (bad, stale) = violations(root, Path::new("allow.toml"))?;
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert_eq!(bad[0].file, "crates/a/src/lib.rs");
        assert!(stale.is_empty());

        write(root, "allow.toml", "[[allow]]\npath = \"scripts/gone.sh\"\nreason = \"x\"\n");
        let (_, stale) = violations(root, Path::new("allow.toml"))?;
        assert_eq!(stale.len(), 1, "a stale entry must be reported");
        Ok(())
    }

    #[test]
    fn digit_must_follow_the_cache_prefix() {
        assert!(has_literal("a/.cache/mc/1.8.9/x"));
        assert!(!has_literal("a/.cache/mc/<ver>/x"));
        assert!(!has_literal("a/.cache/mc/creative/world"));
    }

    /// The real tree is clean; this is what makes the rule a gate under
    /// `cargo test -p xtask`.
    #[test]
    fn the_workspace_has_no_unlisted_literals() -> Result<()> {
        let (bad, stale) =
            violations(&lodestone_mc_cache::workspace_root(), Path::new(DEFAULT_ALLOWLIST))?;
        assert!(bad.is_empty(), "unlisted literals: {bad:#?}");
        assert!(stale.is_empty(), "stale allowlist entries: {stale:#?}");
        Ok(())
    }
}
