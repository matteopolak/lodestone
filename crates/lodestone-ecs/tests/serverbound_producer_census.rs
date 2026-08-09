//! Issue #304's other half, as a gate: which serverbound actions the client can
//! **encode** but never **produces**.
//!
//! # Why this exists
//!
//! `cargo xtask connectedness` counts serverbound *encoders*. It cannot see a
//! producer, so an encoder with nothing upstream of it counts as coverage. That
//! asymmetry has shipped four times now and it is always found the same way — by
//! a server rejecting us, or by someone grepping on a hunch:
//!
//! | action | how it was found |
//! |---|---|
//! | `ClientAction::SetFlying` | four adapters encoded it, **zero** producers; the server kicked us with `multiplayer.disconnect.flying` |
//! | `ClientAction::ChangeGameMode` | zero producers until a game-mode switcher was added |
//! | `ClientAction::PlaceRecipe` | zero producers; the shell synthesises three container clicks instead |
//! | `PlayerCommand::StartFallFlying` | four adapter encoders, zero producers, until riptide (#208) added the first |
//! | `ClientAction::MoveVehicle`, `ClientAction::PaddleBoat`, `PlayerCommand::StartRidingJump` | encoded byte-exactly by the v770 adapter with its own round-trip tests; nothing moved a ridden vehicle at all until `lodestone_ecs::vehicle` simulated one |
//!
//! So the set is **snapshotted**, and landing the first producer for something on
//! the list fails this gate — the fix being to delete the line, which is the
//! point. The snapshot is the hand-verified set, **not** a whole-enum sweep; see
//! `every_listed_action_still_has_no_producer` for the measurement that made that
//! the right scope.
//!
//! # What this gate is in scope for
//!
//! Exactly one question: "does any non-test, non-protocol source construct this
//! variant **by name**". It is blind to:
//!
//! - **A producer behind an indirection.** `lodestone-shell/src/app/menus.rs`
//!   produces `ClientAction::SignUpdate` through `submit.into_action()`
//!   (`menu/command_block.rs:541`), and no name scanner can follow that. This is
//!   why the list is hand-verified rather than swept.
//! - **Whether the producer is reachable.** A construction inside a system nothing
//!   schedules counts here. That is the island class `CLAUDE.md` §1 covers and this
//!   gate is not a substitute for it.
//! - **A producer inside a `#[cfg(test)] mod tests` in a `src` file.** `tests/`
//!   directories and `src/**/tests.rs` are excluded, but an *inline* test module
//!   is not, and that is a deliberate retreat rather than an oversight. The first
//!   draft tracked `#[cfg(test)]` with a line scanner and it was wrong in the worst
//!   direction: the flag was sticky, so it tripped on a comment mentioning
//!   `#[cfg(test)]` at `lodestone-shell/src/sim/session.rs:111` and skipped the
//!   remaining ~800 lines of that file — including the real
//!   `ClientAction::ContainerClose` producer at line 899. The gate reported
//!   **21 false gaps**. `CLAUDE.md` names exactly this ("a hand-rolled Rust lexer
//!   will be wrong"), so the tracking is gone; over-reporting *produced* costs a
//!   missed entry, and the alternative cost a snapshot nobody could trust.
//! - **`crates/protocol/**` and `lodestone-model` itself**, which is where the
//!   encoders and the enum live. Counting those would make every variant look
//!   produced.
//!
//! # Being on the list is not a bug
//!
//! Most entries are actions whose producer is a **screen that does not exist**.
//! That is a legitimate state and the reason each line carries its blocker. The
//! defect is an entry with no explanation, because that is one nobody has decided
//! about.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Variants with **no** producer outside `crates/protocol/` and `lodestone-model`,
/// each with what it is waiting for. Measured by running this gate, not derived
/// from reading.
///
/// Adding an encoder for something no screen can trigger is fine — put it here
/// with its blocker. Landing the producer means deleting the line.
const KNOWN_UNPRODUCED: &[(&str, &str)] = &[
    // --- issue #304: the operator/creative editor set -----------------------
    // Encoders landed with #304; every one of these is gated on an editor screen
    // that has not been built. Named individually rather than as a group so the
    // first one to get a screen is a one-line diff.
    ("ClientAction::SetStructureBlock", "structure block screen"),
    ("ClientAction::SetJigsawBlock", "jigsaw block screen"),
    (
        "ClientAction::GenerateJigsawStructure",
        "jigsaw block screen's Generate button",
    ),
    ("ClientAction::SetTestBlock", "test block screen"),
    (
        "ClientAction::TestInstanceBlockAction",
        "test instance block screen",
    ),
    (
        "ClientAction::SetCommandMinecart",
        "command minecart screen (#47 covers the command *block* only)",
    ),
    (
        "ClientAction::SetGameRules",
        "game-rule editor; #32's settings menu is the likely home",
    ),
    (
        "ClientAction::ChangeDifficulty",
        "singleplayer difficulty control in #32's settings menu",
    ),
    (
        "ClientAction::LockDifficulty",
        "singleplayer difficulty control in #32's settings menu",
    ),
    (
        "ClientAction::CustomClickAction",
        "dialog screen; show_dialog decodes into SessionServerInfo but nothing renders it yet",
    ),
    (
        "ClientAction::QueryBlockEntityTag",
        "F3+I debug copy-NBT keybind (shell input)",
    ),
    (
        "ClientAction::QueryEntityTag",
        "F3+I debug copy-NBT keybind (shell input)",
    ),
    (
        "ClientAction::SubscribeDebug",
        "debug overlay toggle; the reply half folds into SessionDebugFeeds",
    ),
    // --- pre-existing, recorded here for the first time ----------------------
    (
        "ClientAction::PlaceRecipe",
        "brokered separately: the shell synthesises three container clicks instead",
    ),
    // --- the `PlayerCommand` family, all four shell-input verbs --------------
    // Found by grepping the family after `StartFallFlying` turned out to be the
    // fourth instance of this shape. All four are keypresses, so all four are
    // shell input.
    (
        "PlayerCommand::StopSleeping",
        "leave-bed input (shell); the sleeping pose arrives as entity metadata",
    ),
    // `StartRidingJump` left this list when `lodestone_ecs::vehicle::charge_riding_jump`
    // became its producer. `StopRidingJump` is a different case and will never
    // leave: it exists in `ServerboundPlayerCommandPacket.Action` and **the vanilla
    // client never sends it** — `LocalPlayer` has only `sendRidingJump`, and
    // `AbstractHorse.handleStopJump` is an empty method. So this is not a missing
    // producer waiting on a screen; the correct number of producers is zero.
    (
        "PlayerCommand::StopRidingJump",
        "nothing: the vanilla client never sends this action, so a producer would be a divergence",
    ),
    (
        "PlayerCommand::OpenInventory",
        "inventory key while riding a chested horse (shell input)",
    ),
];

fn crates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/lodestone-ecs has a parent")
        .to_path_buf()
}

/// Every variant name declared by an enum in `lodestone-model/src/action.rs`.
///
/// Parsed from the source rather than listed here, so a new variant cannot be
/// added without this gate seeing it. Substring/prefix scanning, not real
/// parsing — the question is only "what identifiers does this enum declare".
fn declared_variants(enum_name: &str) -> BTreeSet<String> {
    let source = std::fs::read_to_string(crates_dir().join("lodestone-model/src/action.rs"))
        .expect("lodestone-model/src/action.rs is readable");
    let start = source
        .find(&format!("pub enum {enum_name} {{"))
        .unwrap_or_else(|| panic!("{enum_name} is not declared in action.rs"));
    let body = &source[start..];
    let mut depth = 0usize;
    let mut variants = BTreeSet::new();
    for raw in body.lines().skip(1) {
        let line = raw.trim();
        // Track brace depth so a variant's own struct body does not contribute
        // its field names.
        let opens = line.matches('{').count();
        let closes = line.matches('}').count();
        if depth == 0
            && !line.starts_with("//")
            && !line.starts_with('#')
            && !line.is_empty()
        {
            if line == "}" {
                break;
            }
            let name: String = line
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
            {
                variants.insert(name);
            }
        }
        depth = depth + opens - closes.min(depth + opens);
        if depth == 0 && closes > opens && line == "}" {
            break;
        }
    }
    variants
}

/// Recursively collect non-test `.rs` files under `dir`.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // `tests` directories and this repo's `src/**/tests.rs` convention
            // are both excluded; see the module doc on why test producers do not
            // count.
            if name == "tests" || name == "target" {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "tests.rs" {
                continue;
            }
            out.push(path);
        }
    }
}

/// Which of `variants` are constructed somewhere that is neither a protocol crate,
/// `lodestone-model` itself, nor a test.
fn produced(prefix: &str, variants: &BTreeSet<String>) -> BTreeSet<String> {
    let root = crates_dir();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&root).expect("crates/ is readable").flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "protocol" || name == "lodestone-model" {
            continue;
        }
        rust_sources(&path.join("src"), &mut files);
        // `crates/plugins/*` is a directory of crates, not a crate.
        if name == "plugins" {
            for plugin in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                rust_sources(&plugin.path().join("src"), &mut files);
            }
        }
    }

    let mut found = BTreeSet::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for raw in text.lines() {
            let line = raw.trim();
            if line.starts_with("//") || line.starts_with("///") {
                continue;
            }
            for variant in variants {
                if line.contains(&format!("{prefix}::{variant}")) {
                    found.insert(variant.clone());
                }
            }
        }
    }
    found
}

/// The snapshot, checked in the direction that matters: every entry must still
/// have no producer, and landing one fails here so the line gets deleted.
///
/// **Deliberately not a whole-enum sweep.** The first draft swept every
/// `ClientAction` variant and reported 18 further gaps — and at least one,
/// `SignUpdate`, was a false positive: `lodestone-shell/src/app/menus.rs` produces
/// it through `submit.into_action()`
/// (`lodestone-shell/src/menu/command_block.rs:541`), an indirection no name
/// scanner can follow. A 35-entry snapshot with unverified members is the
/// "confident and wrong claim" `CLAUDE.md`'s evidence section exists to forbid, so
/// the list is the set that was checked **by hand**, and the sweep is not here.
///
/// The unswept remainder is a real open question, not a resolved one: see
/// `docs/serverbound-coverage.md`, which names the eighteen and says which were
/// verified.
#[test]
fn every_listed_action_still_has_no_producer() {
    let client_variants = declared_variants("ClientAction");
    let command_variants = declared_variants("PlayerCommand");
    let produced_client = produced("ClientAction", &client_variants);
    let produced_command = produced("PlayerCommand", &command_variants);

    let mut regressions = Vec::new();
    for (name, _) in KNOWN_UNPRODUCED {
        let (prefix, variant) = name
            .split_once("::")
            .expect("every entry is written Enum::Variant");
        let (declared, produced_set) = match prefix {
            "ClientAction" => (&client_variants, &produced_client),
            "PlayerCommand" => (&command_variants, &produced_command),
            other => panic!("unknown enum prefix {other} in KNOWN_UNPRODUCED"),
        };
        assert!(
            declared.contains(variant),
            "{name} is listed but no longer declared -- delete the line"
        );
        if produced_set.contains(variant) {
            regressions.push(*name);
        }
    }
    assert!(
        regressions.is_empty(),
        "these are listed as unproduced but now have a producer: {regressions:?}\n\n\
         Delete their lines from KNOWN_UNPRODUCED -- that is the good outcome."
    );
}

/// The control. Without it, a scanner that returned an empty `produced` set would
/// pass the snapshot forever (every variant would look unproduced, and the list
/// would just grow to match).
#[test]
fn the_producer_scanner_actually_finds_producers() {
    let variants = declared_variants("ClientAction");
    let produced = produced("ClientAction", &variants);
    assert!(
        produced.len() > 20,
        "the scanner found only {} produced ClientAction variants; the client \
         certainly produces more than that, so the scan is broken rather than the \
         client being unwired",
        produced.len()
    );
    // Three specific ones whose producers are load-bearing and long-standing. If
    // these read as unproduced the scanner is wrong, not the tree.
    for expected in ["Move", "SwingArm", "UseItemOn"] {
        assert!(
            produced.contains(expected),
            "ClientAction::{expected} must have a producer -- the scanner is broken"
        );
    }
}

/// Every entry must carry a blocker. An entry with an empty reason is one nobody
/// decided about, which is the actual defect this file is trying to prevent.
#[test]
fn every_listed_action_says_what_it_is_waiting_for() {
    for (name, blocker) in KNOWN_UNPRODUCED {
        assert!(
            blocker.len() > 10,
            "{name} is listed with no real blocker ({blocker:?})"
        );
    }
}
