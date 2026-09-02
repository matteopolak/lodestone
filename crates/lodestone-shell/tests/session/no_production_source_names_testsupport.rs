//! **A source scan, not a behavioural test**: no production source file may name
//! the test-support crate.
//!
//! # What it is
//!
//! `lodestone-testsupport` exists to hand live gates a *fresh* identity per run:
//! its `unique_username()` cannot return the same name twice, by construction and
//! by its own test. `crates/lodestone-shell/src/net.rs` re-exported that helper
//! and the offline join arm called it, so the shipped client invented a new
//! offline account on every launch. Offline UUIDs derive from the username, so
//! that is a new *account*, not just a new label, and no per-player save could
//! ever be found even once one is written.
//!
//! It has the same shape as the two defects `CLAUDE.md` §12.44 records: **no
//! health check here can see it.** The tree was green, every test passed, and the
//! only symptom was in the owner's game.
//!
//! # How it works
//!
//! The **first** and much stronger layer is Cargo, not this file: the crate is a
//! `[dev-dependencies]` entry of this one, so the lib target cannot name it at
//! all and the defect is a compile error. It was the only crate in the workspace
//! where that dependency sat under plain `[dependencies]`: 11 others have it
//! under `[dev-dependencies]`, and `lodestone-sound` has it `optional` behind a
//! feature and names it only from its own `tests/`.
//!
//! This file is the second layer, for the case where someone adds the normal edge
//! back — a one-line Cargo change that makes the compile error go away and looks
//! entirely innocuous in a diff. It reads every `.rs` file under this crate's
//! `src/` and reports any mention outside a `#[cfg(test)]` region.
//!
//! **Why not scan `Cargo.toml` instead?** Because that gate would have to know
//! about every dependency table (`[dependencies]`, a `cfg`-gated one, workspace
//! inheritance, an `optional` entry behind a feature), and getting one wrong fails
//! silently in the safe direction. Scanning for the *effect* — the crate being
//! named in production source — is `CLAUDE.md`'s prescription and needs no model
//! of Cargo.
//!
//! # How to change it
//!
//! The scan is **name-based**, so a helper reached through an alias or a
//! re-export in a third crate is invisible to it, exactly as
//! `no_test_touches_the_real_saves_dir.rs` documents for its own needles.
//! [`FORBIDDEN`] is the crate *path* rather than any one function, so adding a
//! second helper needs no change here.
//!
//! Two exclusions, and each has its own control because each could swallow a real
//! offence:
//!
//! * **Inline `#[cfg(test)] mod tests { .. }`** — excluded by a brace-depth walk
//!   from the attribute. Deliberately dumb about strings and comments: a
//!   hand-rolled Rust lexer will be wrong about lifetimes (`CLAUDE.md`'s
//!   `&'static str` incident silently disabled comment detection in three
//!   scanners), so a `{` inside a string inside such a module would end the
//!   region *early* and produce a false positive. That is the safe direction, and
//!   it is the whole reason for the choice.
//! * **A `src/**/tests.rs` file module** — `app/tests.rs` and `sim/tests.rs` carry
//!   no attribute of their own; it lives on the `#[cfg(test)] mod tests;`
//!   declaration in the *parent* file. This is **verified rather than assumed**:
//!   [`parent_declares_cfg_test_mod`] opens the parent module's source and
//!   requires the attribute to actually be there, so a `tests.rs` that is
//!   compiled into production is not excused by its name.
//!
//! A first draft used the bare needle `testsupport`, which matched this file's own
//! prose and two explanatory doc comments in `net.rs` — the same too-broad-needle
//! mistake `no_test_touches_the_real_saves_dir.rs` records making. The needle is
//! now the underscored Rust path, which is the only way to actually *name* an item
//! from the crate. Prose may then use the hyphenated package name freely, as this
//! file's own docs do.
//!
//! # Dependencies
//!
//! `std::fs` and `CARGO_MANIFEST_DIR`. No cargo feature, so it runs in the
//! `--no-default-features` build too.

use std::path::{Path, PathBuf};

/// The forms that mean "this file depends on the test-support crate".
///
/// The underscored Rust path is the load-bearing one: it is the only way to name
/// an item from the crate. `extern crate` is listed separately because it is the
/// one remaining route, and keeping it explicit documents that it was considered.
const FORBIDDEN: [&str; 2] = ["lodestone_testsupport", "extern crate lodestone_testsupport"];

/// Files allowed to name it from a production region, by file name.
///
/// **Empty, deliberately.** An entry here would mean a production file legitimately
/// depends on a test helper, which is the thing this gate exists to prevent, so
/// adding one needs a reason written at the line. The `#[cfg(test)]` exclusions
/// are not allowlist entries — they are computed, and each has a control.
const ALLOWED: [&str; 0] = [];

fn allowed(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    ALLOWED.contains(&name)
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

/// Whether `parent_text` declares `mod {name};` under a `#[cfg(test)]` attribute.
///
/// The pure half of the file-module exclusion, so it can be controlled against
/// synthetic text — including the case that matters, a declaration with **no**
/// attribute, which must not be excused.
fn declares_cfg_test_mod(parent_text: &str, name: &str) -> bool {
    let target = format!("mod {name};");
    let lines: Vec<&str> = parent_text.lines().collect();
    lines.iter().enumerate().any(|(i, line)| {
        line.trim() == target
            && i > 0
            && lines[..i]
                .iter()
                .rev()
                // Skip doc comments and blank lines between the attribute and the
                // declaration; stop at the first line that is neither.
                .take_while(|l| {
                    let t = l.trim();
                    t.is_empty() || t.starts_with("//") || t.starts_with("#[")
                })
                .any(|l| l.trim().starts_with("#[cfg(test)]"))
    })
}

/// Whether `path` is a module file whose declaration in its parent is
/// `#[cfg(test)]`-gated — e.g. `src/app/tests.rs`, declared by `src/app.rs`.
fn parent_declares_cfg_test_mod(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    let Some(dir) = path.parent() else {
        return false;
    };
    // `src/app/tests.rs` is declared by `src/app.rs` or `src/app/mod.rs`.
    let mut candidates = Vec::new();
    if let Some(parent_name) = dir.file_name().and_then(|s| s.to_str()) {
        if let Some(grandparent) = dir.parent() {
            candidates.push(grandparent.join(format!("{parent_name}.rs")));
        }
    }
    candidates.push(dir.join("mod.rs"));
    candidates
        .iter()
        .filter_map(|c| std::fs::read_to_string(c).ok())
        .any(|text| declares_cfg_test_mod(&text, stem))
}

/// Line numbers (0-based) that sit inside an inline `#[cfg(test)]` item.
fn cfg_test_lines(text: &str) -> Vec<bool> {
    let lines: Vec<&str> = text.lines().collect();
    let mut inside = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].trim_start().starts_with("#[cfg(test)]") {
            i += 1;
            continue;
        }
        inside[i] = true;
        let mut j = i + 1;
        let mut depth = 0usize;
        let mut opened = false;
        while j < lines.len() {
            inside[j] = true;
            for c in lines[j].chars() {
                match c {
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' => depth = depth.saturating_sub(1),
                    // `mod tests;` — a file module. Nothing further belongs to
                    // the region; the contents are scanned in their own file and
                    // excused by `parent_declares_cfg_test_mod`.
                    ';' if !opened => {
                        depth = 0;
                        opened = true;
                    }
                    _ => {}
                }
            }
            if opened && depth == 0 {
                break;
            }
            j += 1;
        }
        i = j + 1;
    }
    inside
}

#[test]
fn no_production_source_file_in_this_crate_names_the_test_support_crate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rs_files(&root.join("src"), &mut files);

    // **An audit that prints nothing is a failure to run, never a pass**
    // (`CLAUDE.md`). The floor is a count, and the verdict depends on it.
    assert!(
        files.len() > 50,
        "the scan found only {} .rs files under {}/src, which cannot be right — \
         it measured nothing",
        files.len(),
        root.display()
    );

    let mut offences: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    let mut excused_inline = 0usize;
    let mut excused_file_modules = 0usize;
    for file in &files {
        if allowed(file) {
            continue;
        }
        scanned += 1;
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let hits: Vec<usize> = text
            .lines()
            .enumerate()
            .filter(|(_, l)| FORBIDDEN.iter().any(|n| l.contains(n)))
            .map(|(i, _)| i)
            .collect();
        if hits.is_empty() {
            continue;
        }
        if parent_declares_cfg_test_mod(file) {
            excused_file_modules += hits.len();
            continue;
        }
        let in_cfg_test = cfg_test_lines(&text);
        for i in hits {
            if in_cfg_test.get(i).copied().unwrap_or(false) {
                excused_inline += 1;
                continue;
            }
            offences.push(format!(
                "{}:{}: {}",
                file.strip_prefix(root).unwrap_or(file).display(),
                i + 1,
                text.lines().nth(i).unwrap_or("").trim()
            ));
        }
    }

    assert!(scanned > 50, "only {scanned} files were actually scanned");
    // Both exclusion paths must have had *something* to exclude, or an untested
    // branch could be swallowing production hits. `net.rs`'s inline live gates
    // and `app/tests.rs`/`sim/tests.rs` make both non-zero by construction, so a
    // zero here means the exclusion logic stopped matching, not that the code got
    // tidier.
    assert!(
        excused_inline > 0,
        "no inline `#[cfg(test)]` mention was found, so that exclusion never ran"
    );
    assert!(
        excused_file_modules > 0,
        "no `#[cfg(test)] mod x;` file-module mention was found, so that \
         exclusion never ran"
    );
    assert!(
        offences.is_empty(),
        "these production sources name the test-support crate, whose helpers are \
         unique-by-construction and must never reach a shipped join path (see \
         `lodestone::offline_identity`):\n{}",
        offences.join("\n")
    );
}

/// **The control**: the detector fires on text that really does name the crate,
/// and does not fire on the prose forms this file and `net.rs` use.
///
/// Without it, "no offences" is equally consistent with a needle that matches
/// nothing — which is exactly what a stale [`FORBIDDEN`] entry would produce after
/// a rename, silently.
#[test]
fn the_scanner_detects_a_planted_dependency() {
    for planted in [
        "pub use lodestone_testsupport::unique_username;",
        "    let name = lodestone_testsupport::unique_username();",
        "extern crate lodestone_testsupport;",
        "use lodestone_testsupport::{RconClient, unique_username};",
    ] {
        assert!(
            FORBIDDEN.iter().any(|needle| planted.contains(needle)),
            "the detector does not match a real dependency, so the scan above \
             proves nothing: {planted}"
        );
    }
    // And it does not fire on prose that mentions the crate by its hyphenated
    // package name, or the allowlist would grow until the gate meant nothing —
    // the too-broad-needle mistake this file's docs record.
    for ok in [
        "//! `lodestone-testsupport` hands live gates a fresh identity per run.",
        "// Live gates need a fresh name; production must not.",
    ] {
        assert!(
            !FORBIDDEN.iter().any(|needle| ok.contains(needle)),
            "the detector fires on prose: {ok}"
        );
    }
}

/// **The control for the inline exclusion**, the half that could silently swallow
/// a real offence: a `#[cfg(test)]` module's body is excused, and the code after
/// its closing brace is not.
#[test]
fn the_cfg_test_exclusion_ends_at_the_closing_brace() {
    let text = "\
fn production() {
    let a = lodestone_testsupport::unique_username();
}

#[cfg(test)]
mod tests {
    use lodestone_testsupport::unique_username;
    fn helper() {
        if true { let _ = unique_username(); }
    }
}

fn also_production() {
    let b = lodestone_testsupport::unique_username();
}
";
    let inside = cfg_test_lines(text);
    let flagged: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(i, l)| {
            FORBIDDEN.iter().any(|n| l.contains(n)) && !inside.get(*i).copied().unwrap_or(false)
        })
        .map(|(i, _)| i + 1)
        .collect();
    // Lines 2 and 14 of the fixture above. The first draft of this expectation
    // said `15`, and the control caught the miscount — which is the point of
    // asserting an exact list rather than a length.
    assert_eq!(
        flagged,
        vec![2, 14],
        "both production lines must be flagged and the `#[cfg(test)]` body must \
         not be; a nested brace inside the module must not end the region early"
    );

    // The file-module form covers only its own two lines, so production code
    // after it is still scanned.
    let file_mod = "\
#[cfg(test)]
mod tests;

fn production() { let _ = lodestone_testsupport::unique_username(); }
";
    let inside = cfg_test_lines(file_mod);
    assert!(inside[0] && inside[1], "the declaration itself is excused");
    assert!(
        !inside[3],
        "code after a `mod tests;` declaration must still be scanned"
    );
}

/// **The control for the file-module exclusion.** The dangerous case is a
/// `tests.rs` that is compiled into production — declared with no attribute, or
/// under a different `cfg` — which must **not** be excused by its name.
#[test]
fn only_a_cfg_test_gated_module_declaration_excuses_a_file_module() {
    assert!(declares_cfg_test_mod("#[cfg(test)]\nmod tests;\n", "tests"));
    assert!(
        declares_cfg_test_mod(
            "#[cfg(test)]\n/// Docs between the attribute and the item.\nmod tests;\n",
            "tests"
        ),
        "a doc comment between the attribute and the declaration is still gated"
    );
    assert!(
        !declares_cfg_test_mod("mod tests;\n", "tests"),
        "an ungated `mod tests;` is production code and must be scanned"
    );
    assert!(
        !declares_cfg_test_mod("#[cfg(feature = \"live\")]\nmod tests;\n", "tests"),
        "a different cfg is not `#[cfg(test)]`"
    );
    assert!(
        !declares_cfg_test_mod("#[cfg(test)]\nmod other;\nmod tests;\n", "tests"),
        "the attribute must belong to *this* declaration, not an earlier one"
    );
    assert!(
        !declares_cfg_test_mod("#[cfg(test)]\nmod tests;\n", "helpers"),
        "a different module name must not match"
    );

    // And the real files this exclusion exists for are genuinely gated — a
    // property of the tree, so if `app.rs`/`sim.rs` ever stop gating them the
    // gate reports it here rather than starting to excuse production code.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for module in ["app", "sim"] {
        let path = src.join(module).join("tests.rs");
        assert!(path.exists(), "{} is missing", path.display());
        assert!(
            parent_declares_cfg_test_mod(&path),
            "{} must be declared under `#[cfg(test)]` by src/{module}.rs",
            path.display()
        );
    }
}
