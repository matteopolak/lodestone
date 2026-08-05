//! Tab completion driven by a **real vanilla 26.2 server's** command tree
//! (issue #470).
//!
//! ## Why this gate exists and why it lives here
//!
//! `crates/protocol/v770/tests/command_tree.rs` proves the bytes decode. That is
//! not the same claim as "tab completion works": a tree can decode perfectly,
//! every wire link green, and still yield zero suggestions — the
//! connected-wire-carrying-a-wrong-value failure `cargo xtask connectedness`
//! structurally cannot see, and the shape this repo has hit four times
//! (`MOVE_PLAYER_POS_ROT` discarding yaw/pitch, block placement writing `STONE`,
//! unlisted species reading the registry default speed, `SET_TIME`'s wall-clock
//! value). So the assertion here is on the *suggestion set*, not on the decode.
//!
//! It lives in `lodestone-shell` because this is the only crate that links both
//! ends: `lodestone_registry::adapter_for_protocol` (the same call the live
//! client makes — driving the registry rather than naming `V770Adapter`) and
//! `lodestone::chat::complete`, the completion walker. The fixture is read from
//! `crates/protocol/v770/tests/fixtures/`, where the capture gate that authored
//! it lives; a second copy would be a second thing to keep in sync.
//!
//! ## Where the expected values come from
//!
//! **Outside our tree entirely.** The same live session that captured the tree
//! also sent a real serverbound `command_suggestion` for `/gamemode ` and
//! captured vanilla's own reply, which was:
//!
//! ```text
//! id=0 start=10 length=0 texts=["adventure", "creative", "spectator", "survival"]
//! ```
//!
//! That exact list is what `complete()` must independently produce by walking
//! the tree and applying `GameType`'s own value set — two different mechanisms
//! (the server's Brigadier `listSuggestions`, and our local domain table)
//! landing on the same four strings in the same order. Asserting merely
//! "non-empty" would pass for a walker that returned every literal in the tree.

#![cfg(feature = "live")]

use std::path::PathBuf;

use lodestone::chat::{Candidate, Completion, complete};
use lodestone_model::command_tree::CommandTree;
use lodestone_model::{ClientEvent, ConnectionState, Directive};
use lodestone_world::World;

/// Protocol 776 (MC 26.2). Resolved through the registry, never by naming a
/// version crate — the shell names no version in code.
const PROTOCOL: i32 = 776;
/// `minecraft:commands`, clientbound play id 16.
const COMMANDS: i32 = 16;

/// The tree the live gate captured, decoded through the same seam the running
/// client uses.
fn real_server_tree() -> CommandTree {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../protocol/v770/tests/fixtures/command_tree_creative.hex");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "read {}: {err} — this fixture is captured by \
             `cargo test -p lodestone-v770 --features live-commands --test live_command_tree \
             -- --ignored` against ./scripts/live-oracles/creative.sh",
            path.display()
        )
    });
    let payload: Vec<u8> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).expect("fixture hex byte"))
        .collect();

    let adapter = lodestone_registry::adapter_for_protocol(PROTOCOL)
        .expect("protocol 776 must resolve to an adapter with the `live` feature on");
    let directives = adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, COMMANDS, &payload)
        .expect("a real server's commands payload must decode");
    for directive in directives {
        if let Directive::Emit(ClientEvent::CommandTreeUpdated { tree }) = directive {
            return *tree;
        }
    }
    panic!(
        "the adapter decoded `minecraft:commands` without emitting \
         ClientEvent::CommandTreeUpdated — the decode arm is missing, so tab completion has \
         no data source (issue #470)"
    );
}

fn texts(completion: &Completion) -> Vec<&str> {
    match completion {
        Completion::Local { candidates, .. } => {
            candidates.iter().map(|c: &Candidate| c.text.as_str()).collect()
        }
        other => panic!("expected Completion::Local, got {other:?}"),
    }
}

/// The headline gate: our local walk of the real tree must produce **exactly**
/// the four strings the real server itself produced for the same input, in the
/// same order.
#[test]
fn completing_gamemode_yields_the_exact_set_the_real_server_sent() {
    let tree = real_server_tree();
    let completion = complete(&tree, "/gamemode ");

    assert_eq!(
        texts(&completion),
        vec!["adventure", "creative", "spectator", "survival"],
        "our completion of a real server's tree must match the reply that same server sent \
         for the same input (captured live: start=10 length=0 \
         texts=[adventure, creative, spectator, survival])"
    );
    match completion {
        Completion::Local { start, .. } => assert_eq!(
            start,
            "/gamemode ".len(),
            "the replaced range must start where the server said it did (start=10)"
        ),
        other => panic!("expected Completion::Local, got {other:?}"),
    }
}

/// The trailing-space distinction, gated explicitly.
///
/// This is the trap already measured in the consumer: a `canonicalize` that
/// `.trim()`s destroys the difference between "finish this token" and "start the
/// next one", and the symptom is silent — every tree-level completion collapses
/// to just the command name. Both halves are asserted here, so losing the
/// distinction fails rather than degrades.
#[test]
fn a_trailing_space_decides_between_finishing_a_token_and_starting_the_next() {
    let tree = real_server_tree();

    // No trailing space: still typing `gamemode` itself, so the answer is the
    // literal, not its arguments.
    let partial = complete(&tree, "/gamemode");
    assert_eq!(
        texts(&partial),
        vec!["gamemode"],
        "without a trailing space the half-typed literal is what completes"
    );

    // With it: the argument's domain, and emphatically *not* the command name.
    let next = complete(&tree, "/gamemode ");
    assert!(
        !texts(&next).contains(&"gamemode"),
        "with a trailing space the command name must NOT be suggested — that is exactly \
         the collapse a trimming canonicalize causes"
    );
    assert_eq!(texts(&next).len(), 4);
}

/// A half-typed argument token filters the domain. `/gamemode s` must narrow to
/// the two modes beginning `s`, in Brigadier's own case-insensitive order —
/// predicting the exact pair, not asserting "fewer than four".
#[test]
fn a_half_typed_argument_token_filters_the_domain_to_an_exact_pair() {
    let tree = real_server_tree();
    assert_eq!(
        texts(&complete(&tree, "/gamemode s")),
        vec!["spectator", "survival"],
    );
    assert_eq!(texts(&complete(&tree, "/gamemode sp")), vec!["spectator"]);
}

/// Completing at the root of a real tree must offer real commands. The exact set
/// is permission- and datapack-dependent, so this predicts membership of
/// commands vanilla always has plus the *count* being large — a walker that
/// returned only the first child, or only the root, fails both.
#[test]
fn completing_an_empty_command_offers_the_servers_own_command_set() {
    let tree = real_server_tree();
    let completion = complete(&tree, "/");
    let names = texts(&completion);

    for expected in ["gamemode", "give", "help", "kill", "summon", "teleport"] {
        assert!(
            names.contains(&expected),
            "vanilla 26.2 has `/{expected}`; the root completion offered {} entries without it",
            names.len()
        );
    }
    assert!(
        names.len() > 20,
        "a real server's root offers dozens of commands; got {}",
        names.len()
    );

    // Brigadier sorts case-insensitively; assert the walker did, rather than
    // trusting insertion order to have happened to be sorted.
    let mut sorted = names.clone();
    sorted.sort_by_key(|s| s.to_ascii_lowercase());
    assert_eq!(names, sorted, "suggestions must come out in Brigadier's order");
}

// --- Unknown-parser tolerance, at the completion level -------------------
//
// `crates/protocol/v770/tests/command_tree.rs` asserts an unmodeled parser id
// decodes to an `Unrecognized` node instead of failing the packet. That is only
// half the property: tolerance is worth having because the *rest of the tree
// stays usable for completion*, which is a claim about `complete()`, not about
// the decoder. This is that positive case.

fn var_i32(out: &mut Vec<u8>, mut value: u32) {
    loop {
        if value & !0x7f == 0 {
            out.push(value as u8);
            return;
        }
        out.push(((value & 0x7f) | 0x80) as u8);
        value >>= 7;
    }
}

fn string(out: &mut Vec<u8>, value: &str) {
    var_i32(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

/// Root -> { literal `hello`, literal `help`, argument `x` with `parser_id` }.
/// Two literals so the assertion below is about a *set*, not a single survivor.
fn tree_bytes_with_parser(parser_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    var_i32(&mut out, 4);
    out.push(0x00); // 0: root
    var_i32(&mut out, 3);
    var_i32(&mut out, 1);
    var_i32(&mut out, 2);
    var_i32(&mut out, 3);
    out.push(0x01 | 0x04); // 1: literal "hello", executable
    var_i32(&mut out, 0);
    string(&mut out, "hello");
    out.push(0x01 | 0x04); // 2: literal "help", executable
    var_i32(&mut out, 0);
    string(&mut out, "help");
    out.push(0x02 | 0x04); // 3: argument "x", executable
    var_i32(&mut out, 0);
    string(&mut out, "x");
    var_i32(&mut out, parser_id);
    var_i32(&mut out, 0); // root index
    out
}

fn tree_from_bytes(payload: &[u8]) -> CommandTree {
    let adapter = lodestone_registry::adapter_for_protocol(PROTOCOL).expect("adapter");
    let directives = adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, COMMANDS, payload)
        .expect("payload decodes");
    for directive in directives {
        if let Directive::Emit(ClientEvent::CommandTreeUpdated { tree }) = directive {
            return *tree;
        }
    }
    panic!("no CommandTreeUpdated emitted");
}

/// The tolerance property, stated as behaviour: a parser id this build does not
/// model must cost exactly its own node, and everything else must still
/// complete normally.
#[test]
fn a_tree_containing_an_unknown_parser_still_completes_its_other_branches() {
    // 900 is outside 26.2's `command_argument_type` range (0..=56).
    let tree = tree_from_bytes(&tree_bytes_with_parser(900));

    assert_eq!(
        texts(&complete(&tree, "/hel")),
        vec!["hello", "help"],
        "the unmodeled sibling must not suppress the branches we do understand"
    );
    assert_eq!(texts(&complete(&tree, "/hell")), vec!["hello"]);
}

/// The control for the tolerance test, and its premise check: with a parser id
/// this build **does** model in the same slot, the completion set gains that
/// argument's domain. Without this, the test above passes for a walker that
/// ignores argument nodes entirely — which would make "tolerance" vacuous.
#[test]
fn the_same_tree_with_a_modeled_parser_offers_that_parsers_domain() {
    // 0 is `brigadier:bool`, whose local domain is exactly ["true", "false"].
    let tree = tree_from_bytes(&tree_bytes_with_parser(0));

    let completion = complete(&tree, "/");
    assert_eq!(
        texts(&completion),
        vec!["false", "hello", "help", "true"],
        "a modeled argument contributes its domain alongside the literals, so the \
         unknown-parser test above is measuring recognition and not a walker that \
         drops every argument"
    );
}

/// A prefix of a real command name narrows to exactly the commands sharing it.
/// `gamemode` and `gamerule` are the classic pair, and both exist in vanilla
/// 26.2 — so the expected value is a property of vanilla's command set, not of
/// our decode.
#[test]
fn a_shared_prefix_narrows_to_exactly_the_commands_that_share_it() {
    let tree = real_server_tree();
    assert_eq!(
        texts(&complete(&tree, "/gamem")),
        vec!["gamemode"],
        "`gamem` is unique to /gamemode in vanilla 26.2"
    );
    assert_eq!(
        texts(&complete(&tree, "/game")),
        vec!["gamemode", "gamerule"],
        "`game` is shared by exactly /gamemode and /gamerule in vanilla 26.2"
    );
}
