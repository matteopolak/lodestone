//! The cancelable-action wrapper's coverage, as a gate rather than a table in a doc comment.
//!
//! `CLAUDE.md`'s island rule: a mechanism nothing calls is a defect report, not
//! a status update, and "individually built, individually tested, reaches zero
//! pixels" is this repo's dominant defect class. `ActionVetoes` is exactly that
//! shape of risk — a registry whose own unit tests are a closed loop and pass
//! whether or not a single engine call site exists.
//!
//! So this scans the engine for each verb's ask site and asserts, per verb,
//! whether one exists. A verb losing its wiring fails a test here instead of
//! becoming a plugin author's bug report; a verb *gaining* one also fails, with
//! an instruction to move it into the wired list.
//!
//! # What this gate is in scope for
//!
//! It answers "does a source file in the engine mention this verb's
//! `VerbContext` constructor". It is blind to:
//!
//! - **Whether the ask is in the right place.** A `VerbContext::BlockBreak`
//!   built *after* the predictor advanced would satisfy this and be useless.
//!   Placement is argued in the call site's own comment and in
//!   `docs/packet-wiring.md`; only a behavioural test can check it, and the
//!   registry's own unit tests plus `crates/lodestone-shell/tests/break_intent.rs`
//!   are where that lives.
//! - **Whether the ask is reachable.** A dead function containing one would pass.
//! - **Anything outside the three scanned crates.**
//!
//! It is deliberately a *source* scan and not a behavioural one: driving
//! `drive_mining` end to end needs the shell's whole fixture apparatus, which is
//! that crate's tests, not this crate's. What this catches is the specific
//! regression the island rule warns about — the wiring silently disappearing
//! while every unit test stays green.

use std::path::{Path, PathBuf};

/// Verbs with a live engine ask site, and where.
const WIRED: &[(&str, &str)] = &[
    ("BlockBreak", "lodestone-shell/src/interact.rs"),
    ("BlockPlace", "lodestone-shell/src/interact.rs"),
    ("EntityDamage", "lodestone-shell/src/sim/actions.rs"),
    ("InventoryClick", "lodestone-client/src/state.rs"),
    ("PlayerMove", "lodestone-controller/src/ecs.rs"),
];

/// Verbs the registry defines that **nothing asks about yet**, with the reason.
///
/// - `PlayerInteract` commits in three branches of `Sim::use_item_live`, each of
///   which also runs the placement predictor and takes a use-sequence number.
///   Denying after the sequence is taken forks the counter, which
///   `docs/baritone-port.md` §3.6 forbids outright, so the ask has to go in
///   ahead of it in all three branches.
///
/// A plugin can register a predicate today; it simply will not be consulted,
/// and `docs/packet-wiring.md` says so in the same table.
const NOT_WIRED_YET: &[&str] = &["PlayerInteract"];

const SCANNED: &[&str] = &["lodestone-shell", "lodestone-controller", "lodestone-client"];

fn crates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/lodestone-ecs has a parent")
        .to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Files under the scanned crates' `src/` that construct `VerbContext::<variant>`
/// outside a comment.
fn files_asking_about(variant: &str) -> Vec<String> {
    let needle = format!("VerbContext::{variant}");
    let root = crates_dir();
    let mut found = Vec::new();
    for name in SCANNED {
        let mut files = Vec::new();
        rust_files(&root.join(name).join("src"), &mut files);
        files.sort();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let hit = text.lines().any(|raw| {
                let line = raw.trim();
                !line.starts_with("//") && !line.starts_with("///") && line.contains(&needle)
            });
            if hit {
                found.push(
                    file.strip_prefix(&root)
                        .expect("scanned files live under crates/")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    found.sort();
    found
}

/// Every verb in [`WIRED`] really is asked about, in the file claimed.
///
/// This is the anti-island assertion: without it, `veto.rs`'s own tests would be
/// a closed loop that passes with zero engine call sites.
#[test]
fn every_verb_claimed_wired_has_a_real_engine_ask_site() {
    for (variant, expected_file) in WIRED {
        let found = files_asking_about(variant);
        assert!(
            !found.is_empty(),
            "VerbContext::{variant} is claimed wired but NOTHING in {SCANNED:?} \
             constructs it -- the veto for that verb is an island and a plugin \
             registering one would be silently ignored"
        );
        assert!(
            found.iter().any(|f| f == expected_file),
            "VerbContext::{variant} should be asked in {expected_file}, found in {found:?}"
        );
    }
}

/// **The control.** The verbs in [`NOT_WIRED_YET`] must have **no** ask site.
///
/// Two jobs. It keeps the doc honest — a table claiming a verb is unwired while
/// it quietly works is as wrong as the reverse. And it proves the scanner
/// *discriminates*: a scanner that returned every file for every query would
/// pass the test above and fail this one. Without this pair, the wired test
/// could be passing on a scanner that always says yes.
#[test]
fn the_verbs_documented_as_unwired_really_have_no_ask_site() {
    for variant in NOT_WIRED_YET {
        let found = files_asking_about(variant);
        assert!(
            found.is_empty(),
            "VerbContext::{variant} is documented as not wired yet, but {found:?} \
             constructs it. If it was wired (good), move it from NOT_WIRED_YET to \
             WIRED in this file and update docs/packet-wiring.md's table."
        );
    }
}

/// Every verb the registry defines is accounted for in exactly one of the two
/// lists — so adding a `Verb` variant and forgetting to wire *or* document it
/// fails here rather than shipping a verb nobody can use and nobody knows about.
#[test]
fn every_verb_variant_is_either_wired_or_explicitly_deferred() {
    use lodestone_ecs::veto::Verb;

    let accounted: Vec<String> = WIRED
        .iter()
        .map(|(v, _)| (*v).to_owned())
        .chain(NOT_WIRED_YET.iter().map(|v| (*v).to_owned()))
        .collect();

    assert_eq!(
        accounted.len(),
        Verb::ALL.len(),
        "there are {} Verb variants but {} accounted for in this file: {accounted:?}",
        Verb::ALL.len(),
        accounted.len()
    );

    for verb in Verb::ALL {
        let name = format!("{verb:?}");
        assert!(
            accounted.contains(&name),
            "Verb::{name} is defined but appears in neither WIRED nor NOT_WIRED_YET"
        );
    }
}

/// The scanner's own control: it finds a real construction and ignores one that
/// only appears in prose. Both crates' call sites document the veto in comments
/// right beside the ask, so a scanner counting comments would report every verb
/// as wired — inverting the result in the reassuring direction.
#[test]
fn the_scanner_ignores_verb_contexts_named_only_in_comments() {
    let hit = |text: &str, needle: &str| {
        text.lines().any(|raw| {
            let line = raw.trim();
            !line.starts_with("//") && !line.starts_with("///") && line.contains(needle)
        })
    };
    let commented = "// asks VerbContext::InventoryClick one day\n\
                     /// See VerbContext::InventoryClick.\nfn f() {}\n";
    let real = "fn f() { v.allows(&VerbContext::InventoryClick { window_id: 0, slot: 0, button: 0 }); }";
    assert!(!hit(commented, "VerbContext::InventoryClick"));
    assert!(hit(real, "VerbContext::InventoryClick"));
}

/// The scanned crates exist and hold sources — a typo in a crate name would
/// silently scan nothing, making `the_verbs_documented_as_unwired...` vacuously
/// true and the wired test loudly wrong.
#[test]
fn every_scanned_crate_exists_and_contains_sources() {
    let root = crates_dir();
    for name in SCANNED {
        let src = root.join(name).join("src");
        assert!(src.is_dir(), "{} is not a directory", src.display());
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        // Floors measured, not guessed: lodestone-shell has 119 files,
        // lodestone-client 10, and lodestone-controller only **4** — an earlier
        // `> 5` here failed on the controller, which is a genuinely small crate
        // (`ecs.rs`, `action.rs`, `input.rs`, `lib.rs`). Guessing a uniform floor
        // is how a control ends up asserting something untrue about the tree.
        assert!(
            !files.is_empty(),
            "{} holds no rust files -- scan path is wrong",
            src.display()
        );
        let floor = if *name == "lodestone-controller" { 3 } else { 5 };
        assert!(
            files.len() >= floor,
            "{} holds only {} rust files (floor {floor}) -- scan path is probably wrong",
            src.display(),
            files.len()
        );
    }
}
