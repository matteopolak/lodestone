//! **A source scan, not a behavioural test**: no test in this crate may reach
//! the developer's real `saves/` directory.
//!
//! # What it is
//!
//! `crate::saves`'s no-argument helpers ([`lodestone::saves::saves_dir`],
//! `create_world`) resolve against
//! `crate::menu::servers::data_dir()`, which on this machine is
//! `~/Library/Application Support/lodestone`. A test that called `create_world`
//! would create a world folder there — and, unlike an assertion failure, that
//! defect **has no symptom the suite can see**: every test still passes and the
//! only trace is a directory in the owner's game. That is the same species as the
//! unit test that opened `login.live.com` in the owner's browser on every
//! `cargo test -p lodestone-shell` (`CLAUDE.md` §12.44), and `CLAUDE.md`'s
//! prescription for it is this file's shape: **grep for the effect, not the
//! feature**.
//!
//! # How it works
//!
//! Every `.rs` file under this crate's `src/` and `tests/` is read, its
//! `#[cfg(test)]` / `#[test]`-bearing regions are **not** distinguished (see the
//! gotcha below), and any call to a no-argument helper is reported unless the
//! file is on an explicit allowlist. The allowlist is two entries and each is
//! justified at its line.
//!
//! The reason a scan can be exhaustive where an in-process test cannot: nothing
//! running inside the suite can observe "a test somewhere else wrote to the data
//! directory", because the write is indistinguishable from production behaviour.
//! Only the *source* says which layer made the call.
//!
//! # How to change it
//!
//! If a new no-argument helper lands in `saves.rs`, add it to [`FORBIDDEN`] —
//! and note that the scan is **name-based**, so a helper reached through an alias
//! or a re-export is invisible to it. The structural defence is the one in
//! `MenuNav`: it derives its saves root from the same directory it takes
//! `servers.json` from, so a test that points it at a temp path cannot get the
//! real root even by accident. This file is the second layer, not the first.
//!
//! **The scanner is deliberately dumb about comments and strings.** A
//! hand-rolled Rust lexer will be wrong about lifetimes (`CLAUDE.md`'s
//! `&'static str` incident disabled comment detection in three scanners), so
//! this one does not try: it matches on the call text and tolerates a false
//! positive in prose, which is why `saves.rs`'s own module doc writes the names
//! inside `[`…`]` links that do not look like calls. A false positive is a
//! one-line allowlist entry; a false *negative* is a world folder in the owner's
//! game.
//!
//! # Dependencies
//!
//! `std::fs` and `CARGO_MANIFEST_DIR`. No cargo feature, so it runs in the
//! `--no-default-features` build too.

use std::path::{Path, PathBuf};

/// The calls that resolve against the real data directory.
///
/// **Qualified with `saves::` and closed with a `(`**, and both halves are
/// load-bearing rather than tidy. The `(` keeps a name in prose from matching;
/// the `saves::` keeps `create_world(` from matching `ui.open_create_world()`,
/// `nav.key_create_world(` and four other `menu::create_world` helpers whose
/// names end in the same substring — which is what the first draft of this array
/// did, reporting seven files that were entirely correct. A gate that cries wolf
/// gets its allowlist grown until it means nothing.
const FORBIDDEN: [&str; 3] = [
    "saves::saves_dir()",
    "saves::default_world_dir()",
    "saves::create_world(",
];

/// Files allowed to name them, with the reason.
fn allowed(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match name {
        // Defines them, and its own tests use the `_in` twins exclusively — which
        // is what `the_real_root_is_reachable_only_through_the_no_argument_helpers`
        // asserts from the inside.
        "saves.rs" => true,
        // This file quotes them.
        "no_test_touches_the_real_saves_dir.rs" => true,
        _ => false,
    }
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_source_file_outside_saves_rs_calls_a_no_argument_saves_helper() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rs_files(&root.join("src"), &mut files);
    rs_files(&root.join("tests"), &mut files);

    // **An audit that prints nothing is a failure to run, never a pass**
    // (`CLAUDE.md`). The floor is a count, and a verdict that depends on it.
    assert!(
        files.len() > 50,
        "the scan found only {} .rs files under {}, which cannot be right — it \
         measured nothing",
        files.len(),
        root.display()
    );

    let mut offences: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    for file in &files {
        if allowed(file) {
            continue;
        }
        scanned += 1;
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            for needle in FORBIDDEN {
                if line.contains(needle) {
                    offences.push(format!(
                        "{}:{}: {}",
                        file.strip_prefix(root).unwrap_or(file).display(),
                        i + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(scanned > 40, "only {scanned} files were actually scanned");
    assert!(
        offences.is_empty(),
        "these call a no-argument `crate::saves` helper, which resolves against \
         the developer's real data directory — use the `_in` twin with a temp \
         root instead (see `saves.rs`'s module doc):\n{}",
        offences.join("\n")
    );
}

/// **The control**: the detector fires on text that really does contain a
/// forbidden call.
///
/// Without it, "no offences" is equally consistent with a scan whose needles
/// never match anything — which is exactly what a stale [`FORBIDDEN`] entry
/// would produce after a rename, silently.
#[test]
fn the_scanner_detects_a_planted_call() {
    let planted = "    let root = crate::saves::saves_dir();";
    assert!(
        FORBIDDEN.iter().any(|needle| planted.contains(needle)),
        "the forbidden-call detector does not match a real call, so the scan \
         above proves nothing"
    );
    // And it does *not* fire on the `_in` twins, or every correct test would be
    // reported and the allowlist would grow until the gate was meaningless.
    for ok in [
        "let worlds = crate::saves::list_worlds_in(&root);",
        "crate::saves::create_world_in(&root, \"x\", 0)",
    ] {
        assert!(
            !FORBIDDEN.iter().any(|needle| ok.contains(needle)),
            "the detector fires on the safe form: {ok}"
        );
    }
}
