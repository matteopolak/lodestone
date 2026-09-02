//! The outbound egress hook's *limit*, as a gate rather than a paragraph.
//!
//! `EgressFilters` hooks `ActionQueue`, which `lodestone_ecs::player::ActionQueue`'s own doc calls "the one
//! sanctioned egress". For anything a **plugin** queues, it is. It is **not**
//! the only path to the socket: several call sites reach
//! `NetClient::send_action` / `ClientHandle::send_action` directly, deliberately,
//! to control wire ordering for discrete clicks. A filter cannot see those.
//!
//! That is the kind of claim `CLAUDE.md` says rots into a false belief — true
//! and evidenced when written, and nothing about it looks wrong later. So the
//! set of direct-send sites is **snapshotted per file**, and a new one fails
//! this test.
//!
//! # What this gate is in scope for
//!
//! It answers exactly one question: "has the set of files that bypass the
//! `ActionQueue` hook changed". It is blind to:
//!
//! - **How many** sites are in a file, or which functions they are in. Per-file
//!   granularity is chosen so ordinary refactoring inside a file does not churn
//!   the snapshot, at the cost of not noticing a *fourth* bypass added to a file
//!   that already has one.
//! - **Whether a bypass matters.** `lodestone-controller`'s test helpers and the
//!   net thread's own forwarding loop are legitimate; the list records what
//!   exists, and this file's prose says which are the user-visible ones.
//! - **Anything outside the two scanned crates.**
//!
//! Fix the gap rather than the snapshot where you can: routing a direct send
//! through `ActionQueue` is the real repair, and then this list shrinks.

use std::path::{Path, PathBuf};

/// Files that call `send_action` directly, relative to the workspace's
/// `crates/` directory. Regenerate the reasoning, not just the list, if this
/// fails: each entry is a path the `EgressFilters` hook cannot see.
/// Every entry was **measured** by running this gate, not derived from reading.
/// An earlier draft of this list had four entries, guessed from a survey of the
/// call sites; the gate's first run reported nine. That is worth recording,
/// because the four-entry version would have shipped a confident and wrong claim
/// about how much of the client's egress this hook covers — the gate
/// caught its own author.
const KNOWN_DIRECT_SEND_FILES: &[&str] = &[
    // --- not bypasses ---
    // The net thread's own drain of the action *channel*. Downstream of
    // everything including the hook; this is where every action ends up.
    "lodestone-shell/src/net.rs",
    // The hook's own call site: `Sim::drain_action_queue`. Covered by definition.
    "lodestone-shell/src/sim/step.rs",
    // Test harnesses that push actions straight at a handle to drive a scenario.
    // Not user-visible paths.
    "lodestone-shell/src/app/tests.rs",
    "lodestone-shell/src/sim/tests.rs",
    //
    // --- REAL BYPASSES: a filter cannot see any of these ---
    // Verbs 3 and 6 of the cancelable-action wrapper — attack, interact-entity, use-item:
    // `Sim::attack_entity`, `Sim::interact_entity`, `Sim::use_item_generic`,
    // `Sim::use_item_live`. Deliberate, to control wire ordering for discrete
    // clicks.
    "lodestone-shell/src/sim/actions.rs",
    // Verb 4 of the cancelable-action wrapper — container clicks: `ClientHandle::menu_click`.
    "lodestone-client/src/handle.rs",
    // Container-screen clicks from the app layer (two sites).
    "lodestone-shell/src/app/container_input.rs",
    // A sign-edit / menu submission reaching the wire from the app layer.
    "lodestone-shell/src/app/menus.rs",
    // Respawn, container-close, and carried-item selection from session code
    // (four sites) — `ClientAction::Respawn`, `ContainerClose`, `SetCarriedItem`.
    "lodestone-shell/src/sim/session.rs",
    // The live render-distance slider re-declaring `SetClientSettings` mid-session.
    // A real bypass, but a settings re-declaration rather than a gameplay verb, so
    // there is nothing for a filter to police.
    "lodestone-shell/src/app/session.rs",
    // The anvil rename box's responder — `ClientAction::RenameItem`, one site in the
    // `KeyOutcome::AnvilRename` arm. Vanilla's `EditBox::setResponder` fires
    // `onNameChanged` after **every** keystroke, and `ActionQueue` drains only inside
    // the tick loop, so queuing it would let the container click that takes the result
    // overtake the rename that names it. Same justification as the two
    // `container_input.rs` sites above, which this arm is the keyboard half of.
    //
    // Listed as a real bypass rather than resolved: making it filterable means giving
    // `ActionQueue` an ordering guarantee against direct container sends, which is a
    // change to the queue's contract and not to this call site.
    "lodestone-shell/src/app/lifecycle.rs",
];

fn crates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/lodestone-ecs has a parent")
        .to_path_buf()
}

/// Recursively collect `.rs` files under `dir`.
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

/// Files under `crates/<crate>/src` containing a `send_action(` call.
///
/// Substring scanning, not parsing — the question is only "is this identifier
/// named here". Lines that are comments or doc comments are skipped, because
/// both of the scanned crates discuss `send_action` at length in prose (the
/// `ActionQueue` doctrine is documented at its own call sites) and counting
/// those would make the snapshot a record of the documentation.
fn files_calling_send_action(crate_names: &[&str]) -> Vec<String> {
    let root = crates_dir();
    let mut found = Vec::new();
    for name in crate_names {
        let src = root.join(name).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        files.sort();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let hit = text.lines().any(|raw| {
                let line = raw.trim();
                !line.starts_with("//") && !line.starts_with("///") && line.contains("send_action(")
            });
            if hit {
                let rel = file
                    .strip_prefix(&root)
                    .expect("scanned files live under crates/")
                    .to_string_lossy()
                    .replace('\\', "/");
                found.push(rel);
            }
        }
    }
    found.sort();
    found
}

const SCANNED: &[&str] = &["lodestone-shell", "lodestone-client"];

/// The gate: the set of files that reach the socket directly must not change
/// without someone deciding whether the new one needs the hook.
#[test]
fn the_set_of_direct_send_action_sites_has_not_changed() {
    let found = files_calling_send_action(SCANNED);
    let mut expected: Vec<String> = KNOWN_DIRECT_SEND_FILES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    expected.sort();

    assert_eq!(
        found, expected,
        "\n\nThe set of files calling send_action directly changed.\n\n\
         EgressFilters hooks the ActionQueue drain only. A NEW file \
         here is an action path a plugin cannot inspect or suppress.\n\n\
         Decide which it is, then update KNOWN_DIRECT_SEND_FILES in this file:\n\
         - if it should be filterable, queue into ActionQueue instead of calling \
         send_action (that is the real fix, and this list then shrinks);\n\
         - if it must bypass for wire-ordering reasons, add it with a comment \
         saying why, as the existing entries do.\n"
    );
}

/// **The scanner's control.** The gate above is worthless if the scanner finds
/// nothing — an empty `found` would only fail while `KNOWN_DIRECT_SEND_FILES` is
/// non-empty, and the natural "fix" is to empty the list, after which it passes
/// forever measuring nothing. This requires a real, non-trivial number of hits.
#[test]
fn the_scanner_actually_finds_send_action_calls() {
    let found = files_calling_send_action(SCANNED);
    assert!(
        found.len() >= 3,
        "expected several direct send_action sites, found {found:?} -- if this is \
         empty the scanner is broken and the gate above proves nothing"
    );
}

/// The two crates really were scanned. A typo in a crate name would silently
/// scan nothing, which is the same vacuous failure in a different place.
#[test]
fn both_scanned_crates_exist_and_contain_rust_sources() {
    let root = crates_dir();
    for name in SCANNED {
        let src = root.join(name).join("src");
        assert!(src.is_dir(), "{} is not a directory", src.display());
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        assert!(
            files.len() > 5,
            "{} contains only {} rust files -- scan path is probably wrong",
            src.display(),
            files.len()
        );
    }
}

/// The comment-skipping is load-bearing and could silently invert the result:
/// both crates document the `ActionQueue` doctrine in prose at the very call
/// sites that matter, so a scanner counting comments would report nearly every
/// file. Checked on a synthetic input rather than trusted.
#[test]
fn the_scanner_ignores_send_action_mentioned_only_in_comments() {
    // Mirrors `interact.rs`'s real doc comment: "Never call
    // `ClientHandle::send_action` from a system".
    let commented = "/// Never call `ClientHandle::send_action` from a system.\n\
                     // net.send_action(thing);\nfn f() {}\n";
    let real = "fn f() { net.send_action(thing); }\n";
    let hit = |text: &str| {
        text.lines().any(|raw| {
            let line = raw.trim();
            !line.starts_with("//") && !line.starts_with("///") && line.contains("send_action(")
        })
    };
    assert!(!hit(commented), "prose about send_action must not count as a call");
    assert!(hit(real), "a real call must count -- otherwise nothing ever does");
}
