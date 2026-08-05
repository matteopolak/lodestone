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

// --- The consumer: the tree reaching the line on screen (issue #471) --------
//
// Everything above proves `complete()` walks a real tree correctly. That is
// still not a pixel: until #471 steps 2 and 3 the *only* callers of `complete`
// were tests and a `command_block_frame` argument every production caller
// passed as `None`, which is this repo's island shape exactly. The gates below
// drive the seams a keystroke actually reaches — `ChatInput::tab` (the chat
// box's Tab key) and `CommandBlockState::apply_completion` (the edit screen's)
// — against the same real server tree, so the assertion is on the text that
// ends up in the field, not on a completion object nobody reads.
//
// What only a live session can confirm: that a joined 26.2 server's tree lands
// in `net::CommandTreeCell` in time, and that the completed line is legible in
// the chat box's own font. The tree here is that server's own bytes, and the
// splice is asserted exactly, so what is left to a live run is the wiring
// either side of these two calls.

use lodestone::chat::ChatInput;
use lodestone::menu::command_block::{CommandBlockOpen, CommandBlockState};
use lodestone_client::ClientAction;
use lodestone_model::command_tree::{CommandSuggestionEntry, CommandSuggestionsResponse};

/// Tab against a **half-typed command name** — no trailing space, the input
/// shape that has now hidden two separate bugs in this feature (a `.trim()`ing
/// canonicalize, and a `parse_line` that offered a fully-typed token's own
/// children instead of its parent). The chat line itself must change, and
/// pressing Tab again must cycle rather than recompute.
#[test]
fn tab_completes_a_half_typed_command_name_and_then_cycles() {
    let tree = real_server_tree();
    let mut input = ChatInput::new();
    input.set("/game");

    assert!(
        input.tab(Some(&tree)).is_none(),
        "a locally-answerable position needs no server round trip"
    );
    assert_eq!(
        input.as_str(),
        "/gamemode",
        "Tab must splice the first candidate into the line — this is the pixel"
    );
    assert_eq!(
        input
            .completion_candidates()
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        vec!["gamemode", "gamerule"],
        "and the offered set is the real server's own pair for the prefix `game`"
    );

    assert!(input.tab(Some(&tree)).is_none());
    assert_eq!(input.as_str(), "/gamerule", "a second Tab cycles");
    assert!(input.tab(Some(&tree)).is_none());
    assert_eq!(input.as_str(), "/gamemode", "and the cycle wraps");

    // Editing the line abandons the cycle: the next Tab must recompute against
    // what is actually typed, not advance a list about older text.
    input.push_str("x");
    assert!(input.tab(Some(&tree)).is_none());
    assert_eq!(
        input.as_str(),
        "/gamemodex",
        "`gamemodex` matches no command, so Tab leaves the line alone rather \
         than cycling the stale `gamemode`/`gamerule` list"
    );
}

/// The trailing-space half at the *consumer*, with the splice offset asserted
/// as a whole line: a correct list spliced at the wrong `start` overwrites the
/// wrong span, and a line comparison catches that where a candidate-set
/// comparison cannot.
#[test]
fn tab_after_a_trailing_space_splices_the_argument_domain_at_the_servers_start() {
    let tree = real_server_tree();
    let mut input = ChatInput::new();
    input.set("/gamemode ");

    assert!(input.tab(Some(&tree)).is_none());
    assert_eq!(
        input.as_str(),
        "/gamemode adventure",
        "the four modes the live server itself returned (start=10), spliced at 10 — \
         a splice at any other offset produces a different line"
    );
    assert_eq!(
        input
            .completion_candidates()
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        vec!["adventure", "creative", "spectator", "survival"],
    );

    // The half-typed *argument* token, again with no trailing space.
    let mut input = ChatInput::new();
    input.set("/gamemode s");
    assert!(input.tab(Some(&tree)).is_none());
    assert_eq!(input.as_str(), "/gamemode spectator");
    assert!(input.tab(Some(&tree)).is_none());
    assert_eq!(input.as_str(), "/gamemode survival");
}

/// The control, run rather than described: with the cell empty — exactly the
/// state before `minecraft:commands` arrives, and the state every caller was
/// stuck in before #471 — the identical keystroke must leave the line
/// untouched. Two hypotheses over the same input, so the gate above cannot be
/// satisfied by a completer that ignores its tree.
#[test]
fn with_no_tree_the_same_tab_press_changes_nothing() {
    let tree = real_server_tree();

    let mut with = ChatInput::new();
    with.set("/game");
    let _ = with.tab(Some(&tree));

    let mut without = ChatInput::new();
    without.set("/game");
    let action = without.tab(None);

    assert_eq!(with.as_str(), "/gamemode");
    assert_eq!(
        without.as_str(),
        "/game",
        "no tree must mean no completion — never a guess, and never an empty list \
         treated as an answer"
    );
    assert!(action.is_none(), "and nothing goes on the wire either");
    assert!(without.completion_candidates().is_empty());
    assert_ne!(
        with.as_str(),
        without.as_str(),
        "if these ever agree, the gate above is measuring something other than the tree"
    );
}

/// The server round trip, including the property the transaction id exists for.
///
/// `/summon ` is a resource argument: no local domain, so the walker defers to
/// the server rather than guessing — the `Completion::NeedsServer` path, whose
/// `SuggestionRequests::request`/`::receive` pair had **no production caller**
/// at all before this change.
#[test]
fn a_suggestion_reply_is_applied_only_when_it_answers_the_request_in_flight() {
    let tree = real_server_tree();
    let mut input = ChatInput::new();
    input.set("/summon ");

    let Some(ClientAction::CommandSuggestion { id, command }) = input.tab(Some(&tree)) else {
        panic!("a resource argument must produce a server round trip, not a local guess");
    };
    assert_eq!(command, "/summon ", "the request carries the line as typed");
    assert_eq!(input.as_str(), "/summon ", "and the line waits, unchanged");

    let entry = |text: &str| CommandSuggestionEntry {
        text: text.to_string(),
        tooltip: None,
    };

    // A reply to a *different* request — the stale case. Vanilla's own
    // `completeCustomSuggestions` id check; ignored, not rendered.
    let stale = CommandSuggestionsResponse {
        id: id.wrapping_add(1),
        start: 8,
        length: 0,
        suggestions: vec![entry("minecraft:creeper")],
    };
    assert!(
        !input.apply_suggestions(&stale),
        "a reply whose id does not match the request in flight must be dropped"
    );
    assert_eq!(input.as_str(), "/summon ", "and must not touch the line");

    // A reply with the right id but a `start` outside the line it answers.
    let out_of_range = CommandSuggestionsResponse {
        id,
        start: 999,
        length: 0,
        suggestions: vec![entry("minecraft:creeper")],
    };
    assert!(!input.apply_suggestions(&out_of_range));
    assert_eq!(input.as_str(), "/summon ");

    // The in-date reply. `start` is read from the response, not re-derived.
    let mut input = ChatInput::new();
    input.set("/summon ");
    let Some(ClientAction::CommandSuggestion { id, .. }) = input.tab(Some(&tree)) else {
        panic!("expected a second round trip");
    };
    let good = CommandSuggestionsResponse {
        id,
        start: 8,
        length: 0,
        suggestions: vec![entry("minecraft:creeper"), entry("minecraft:zombie")],
    };
    assert!(input.apply_suggestions(&good));
    assert_eq!(input.as_str(), "/summon minecraft:creeper");
    assert_eq!(
        input
            .completion_candidates()
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        vec!["minecraft:creeper", "minecraft:zombie"],
    );

    // Polling the same response again — which is exactly what the per-frame
    // pump in `app::menus::pump_command_suggestions` does — is stale by
    // construction, because the id match consumed the pending request.
    assert!(
        !input.apply_suggestions(&good),
        "the second poll of one response must be a no-op, or a frame loop would \
         re-splice it forever"
    );
    assert_eq!(input.as_str(), "/summon minecraft:creeper");
}

/// `start` is load-bearing, stated as two hypotheses over one identical
/// suggestion list: the same texts at two different offsets produce two
/// different lines. An implementation that ignored the response's `start` (or
/// re-derived its own) could not tell these apart.
#[test]
fn the_replies_own_start_decides_where_the_text_lands() {
    let tree = real_server_tree();
    let entry = CommandSuggestionEntry {
        text: "minecraft:zombie".to_string(),
        tooltip: None,
    };

    let mut at_token = ChatInput::new();
    at_token.set("/summon ");
    let Some(ClientAction::CommandSuggestion { id, .. }) = at_token.tab(Some(&tree)) else {
        panic!("expected a round trip");
    };
    assert!(at_token.apply_suggestions(&CommandSuggestionsResponse {
        id,
        start: 8,
        length: 0,
        suggestions: vec![entry.clone()],
    }));

    let mut at_zero = ChatInput::new();
    at_zero.set("/summon ");
    let Some(ClientAction::CommandSuggestion { id, .. }) = at_zero.tab(Some(&tree)) else {
        panic!("expected a round trip");
    };
    // Not a shape vanilla sends — a deliberately different offset, so the two
    // hypotheses are distinguishable at all.
    assert!(at_zero.apply_suggestions(&CommandSuggestionsResponse {
        id,
        start: 0,
        length: 0,
        suggestions: vec![entry],
    }));

    assert_eq!(at_token.as_str(), "/summon minecraft:zombie");
    assert_eq!(at_zero.as_str(), "minecraft:zombie");
    assert_ne!(at_token.as_str(), at_zero.as_str());
}

/// Step 2's seam: the command block edit screen's own Tab key, against the same
/// real tree, plus the `None` control it degraded to before #471.
///
/// The command field holds a **slash-less** line, so this also covers
/// `menu::command_block::complete`'s offset shift — a completion spliced one
/// byte out would show `gamemod` or `ggamemode` here.
#[test]
fn the_command_block_screens_tab_completes_from_the_same_tree() {
    let tree = real_server_tree();
    let open = CommandBlockOpen {
        command: "game".to_string(),
        ..CommandBlockOpen::default()
    };

    let mut state = CommandBlockState::new(open.clone());
    assert!(state.apply_completion(Some(&tree)));
    assert_eq!(
        state.command.value(),
        "gamemode",
        "no leading slash on this screen, and the splice must land at 0 — an \
         unshifted offset would produce `amemode`"
    );

    let mut without = CommandBlockState::new(open);
    assert!(
        !without.apply_completion(None),
        "the honest degrade: no tree, no completion"
    );
    assert_eq!(without.command.value(), "game");
    assert_ne!(state.command.value(), without.command.value());
}
