//! A mechanical guard on the one text function a render surface must not call.
//!
//! # Why this is a test and not a doc comment
//!
//! `Text::to_spans` expands legacy `§` codes and
//! `Text::to_spans_ignoring_legacy_codes` does not, and a surface that reaches
//! for the second draws `§7` on screen as two glyphs. The long name is a
//! signpost, and its doc comment says so in prose — but prose is not a guard.
//! This repo has a measured instance of a rule stated in a doc comment
//! (`lodestone-server`'s ban on `Instant::now`) being violated four times while
//! the comment sat there being correct, which is why `scripts/wasm-check.sh` now
//! bans those paths mechanically. Same shape, same remedy: whenever the type
//! system cannot express a constraint, make it *checkable* and check it.
//!
//! # Scope, stated so a reader can tell what would fall outside it
//!
//! Every `.rs` file under `crates/`, `xtask/` and `web/` — the three Rust source
//! roots in the tree (`ls -d */`: the others hold docs, scripts, logs, vendored
//! sources, screenshots and build output). If a fourth root is ever added, this
//! scan goes silent about it rather than wrong, which is exactly how the
//! docs-index gate missed `docs/plans/`; add it to [`SOURCE_ROOTS`].
//!
//! It matches text, not syntax, so it cannot tell a call from a mention in a
//! comment — which is deliberate. An `ALLOWED` entry is a file that has been
//! *read*, and a doc comment naming the function is exactly as much of a
//! precedent as a call to it.

use std::path::{Path, PathBuf};

/// The Rust source roots, relative to the repo root. See the module doc on what
/// happens if a fourth appears.
const SOURCE_ROOTS: [&str; 3] = ["crates", "xtask", "web"];

/// The function no render surface may reach.
const BANNED: &str = "to_spans_ignoring_legacy_codes";

/// Where the banned name is legitimate, as repo-root-relative paths.
///
/// Both entries are re-serialisation or the expansion pass itself, i.e. code that
/// is *about* legacy codes rather than code that draws them. Adding an entry here
/// means claiming a third such case exists; the bar is "this code puts codes back
/// or takes them apart", not "this call site is convenient".
const ALLOWED: [&str; 3] = [
    // `to_spans` (the expanding pass) and `to_legacy_string` (which re-emits the
    // codes and must not double-expand them) are both defined here.
    "crates/lodestone-model/src/text.rs",
    // The gate proving the two functions really differ.
    "crates/lodestone-model/src/tests.rs",
    // This scanner: it necessarily spells the name it is looking for.
    "crates/lodestone-model/tests/legacy_expansion_guard.rs",
];

fn repo_root() -> PathBuf {
    // `crates/lodestone-model` -> repo root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root above crates/lodestone-model")
        .to_path_buf()
}

/// Every `.rs` file under `root`, skipping `target` directories.
fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "rs") {
                found.push(path);
            }
        }
    }
    found
}

/// No file outside [`ALLOWED`] may name the non-expanding flatten.
///
/// The two assertions at the end are not decoration. The first is the
/// "an audit that prints nothing is a failure to run, never a pass" rule: a scan
/// that walked the wrong directory finds zero offenders and reads as green. The
/// second is stronger and is the reason a bare `offenders.is_empty()` would be a
/// vacuous control — if the banned function is ever *renamed*, every occurrence
/// disappears, the allowlist stops matching anything, and this test would
/// certify a constraint it is no longer checking. Requiring the sanctioned sites
/// to still be found is what ties the guard to a live subject.
#[test]
fn render_surfaces_do_not_bypass_legacy_expansion() {
    let root = repo_root();
    let mut scanned = 0usize;
    let mut offenders = Vec::new();
    let mut allowed_hits = Vec::new();

    for source_root in SOURCE_ROOTS {
        let dir = root.join(source_root);
        assert!(
            dir.is_dir(),
            "source root {} is missing — this scan's coverage is not what it claims",
            dir.display()
        );
        for path in rust_sources(&dir) {
            scanned += 1;
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !text.contains(BANNED) {
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .display()
                .to_string();
            if ALLOWED.contains(&relative.as_str()) {
                allowed_hits.push(relative);
            } else {
                offenders.push(relative);
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these files name `Text::{BANNED}`, which leaves a server's legacy `§` \
         codes in a span for the renderer to draw as literal glyphs. Call \
         `Text::to_spans` instead — expansion is the default because vanilla's \
         own `StringDecomposer.iterateFormatted` applies the codes at draw time. \
         If a file genuinely re-serialises codes rather than drawing them, add it \
         to this test's `ALLOWED`: {offenders:?}"
    );
    assert!(
        scanned > 500,
        "only {scanned} .rs files scanned under {:?} — the walk is not reaching \
         the tree, so finding no offenders means nothing",
        SOURCE_ROOTS
    );
    assert_eq!(
        allowed_hits.len(),
        ALLOWED.len(),
        "expected every `ALLOWED` file to still name `{BANNED}`, found {allowed_hits:?}. \
         A rename would empty this guard without failing it."
    );
}
