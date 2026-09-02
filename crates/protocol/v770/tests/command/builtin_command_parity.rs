//! **Per-command wire parity against a real vanilla 26.2 server's own tree.**
//!
//! `crates/protocol/v770/tests/fixtures/command_tree_creative.hex` is 30,248
//! bytes of `minecraft:commands` payload captured from the flat creative oracle
//! by `tests/live_command_tree.rs` — 2,017 nodes, authored by vanilla, never by
//! our encoder. This gate decodes it, extracts the subtree under a command's root
//! literal, and compares it structurally with
//! `ServerCommands::wire_tree()`'s projection of the same command.
//!
//! # What "structurally" covers
//!
//! Node kind, name, `ArgumentParser` variant **including its payload**,
//! executable bit, restricted bit, redirect topology and suggestion-provider id —
//! recursively, in child order. The payload is the part that matters most: a node
//! that is `minecraft:entity` with the wrong flags byte, or
//! `brigadier:integer { min: 0 }` where vanilla says `min: 1`, is a client that
//! autocompletes something the server rejects, and no amount of "it decodes"
//! catches it.
//!
//! # Why this is an *outside* expectation
//!
//! The expected value originates in Mojang's own dispatcher, serialised by their
//! own `ArgumentTypeInfo`s, captured off a socket. Nothing in this repo authored
//! it. That is the difference between this and `decode(encode(x)) == x`, which two
//! symmetric misunderstandings satisfy.
//!
//! # Two fixture caveats, both real
//!
//! * The capture came from a **dedicated** server, so `/publish` is absent and the
//!   dedicated-only block is present. Irrelevant to the commands gated here, but
//!   it is why "the fixture has no X" is not evidence about X.
//! * **Vanilla filters the sent tree by the capturing client's permission level.**
//!   A command missing from the fixture needs a fresh capture
//!   (`scripts/live-oracles/creative.sh`, Apple `container`, **not** Docker) —
//!   never a hand-written expectation.
//!
//! # This gate is about the projection's *shape*, not about the wire
//!
//! `V770ServerProtocol::encode_commands` now exists and `server.rs` sends the tree
//! at join, so tab completion against the server's own commands does work end to
//! end. That is gated separately, in `command_tree_encode.rs`, which re-encodes
//! this same fixture and requires byte identity with it. Keep the two apart: this
//! one asks "does our declared tree agree with vanilla's per command", that one
//! asks "do our bytes agree with vanilla's bytes", and a single test doing both
//! would fail for either reason with one message.

use std::path::PathBuf;

use lodestone_model::command_tree::{CommandTree, NodeKind};
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_server::ServerCommands;
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).expect("fixture hex byte"))
        .collect()
}

/// Decodes the captured tree through the public `VersionAdapter` seam, so this
/// cannot pass while the adapter has no `COMMANDS` arm.
fn vanilla_tree() -> CommandTree {
    let directives = V770Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::COMMANDS,
            &fixture("command_tree_creative.hex"),
        )
        .expect("a real server's payload must decode");
    for directive in directives {
        if let Directive::Emit(ClientEvent::CommandTreeUpdated { tree }) = directive {
            return *tree;
        }
    }
    panic!("the COMMANDS arm must emit ClientEvent::CommandTreeUpdated");
}

/// A command's root literal in `tree`, by name.
fn root_literal(tree: &CommandTree, name: &str) -> usize {
    let root = tree.node(tree.root()).expect("root");
    for &child in &root.children {
        if let NodeKind::Literal { name: literal } = &tree.node(child).expect("child").kind
            && literal == name
        {
            return child;
        }
    }
    panic!(
        "`/{name}` is not in this tree. If it is the *fixture* that lacks it, that is a permission \
         filter or a dedicated-vs-integrated difference — recapture with \
         scripts/live-oracles/creative.sh rather than hand-writing an expectation."
    );
}

/// One node flattened into everything the wire carries about it, for comparison.
///
/// A `String` rather than a struct because the whole point is the *failure
/// message*: a mismatch prints both sides at the path where they diverged, which
/// is what turns "the trees differ" into "vanilla says `min: 1` and we say
/// `min: 0`".
fn describe(tree: &CommandTree, index: usize) -> String {
    let node = tree.node(index).expect("node in range");
    let kind = match &node.kind {
        NodeKind::Root => "root".to_string(),
        NodeKind::Literal { name } => format!("literal {name:?}"),
        NodeKind::Argument { name, parser, suggestions } => {
            format!("argument {name:?} parser={parser:?} suggests={suggestions:?}")
        }
        NodeKind::Unrecognized { parser_id } => format!("unrecognized parser id {parser_id}"),
    };
    format!(
        "{kind} executable={} restricted={} redirects={} children={}",
        node.executable,
        node.restricted,
        node.redirect.is_some(),
        node.children.len()
    )
}

/// Recursively compare two subtrees, reporting the first divergence with the path
/// to it.
///
/// Children are compared **in order**: vanilla's `ClientboundCommandsPacket`
/// writes the dispatcher's own child order, and a tree that agrees as a set but
/// not as a sequence would autocomplete in a different order than vanilla does.
fn compare(
    path: &str,
    ours: &CommandTree,
    our_index: usize,
    theirs: &CommandTree,
    their_index: usize,
) {
    let ours_described = describe(ours, our_index);
    let theirs_described = describe(theirs, their_index);
    assert_eq!(
        ours_described, theirs_described,
        "at {path}:\n  ours:    {ours_described}\n  vanilla: {theirs_described}"
    );

    let our_node = ours.node(our_index).expect("node");
    let their_node = theirs.node(their_index).expect("node");

    // Redirect topology, expressed by *name* rather than by index: the two trees
    // number their nodes differently, so comparing indices would fail on every
    // redirect while comparing nothing meaningful. Redirect-to-root is the only
    // shape either tree has today (`/execute … run`), and that is what this
    // distinguishes.
    let redirect_name = |tree: &CommandTree, node: &lodestone_model::command_tree::RawCommandNode| {
        node.redirect.map(|target| {
            if target == tree.root() {
                "<root>".to_string()
            } else {
                match &tree.node(target).expect("redirect target").kind {
                    NodeKind::Literal { name } | NodeKind::Argument { name, .. } => name.clone(),
                    other => format!("{other:?}"),
                }
            }
        })
    };
    assert_eq!(
        redirect_name(ours, our_node),
        redirect_name(theirs, their_node),
        "at {path}: redirect targets differ"
    );

    for (position, (&our_child, &their_child)) in
        our_node.children.iter().zip(their_node.children.iter()).enumerate()
    {
        let name = match &ours.node(our_child).expect("child").kind {
            NodeKind::Literal { name } | NodeKind::Argument { name, .. } => name.clone(),
            _ => format!("#{position}"),
        };
        compare(&format!("{path}/{name}"), ours, our_child, theirs, their_child);
    }
}

/// Assert `command`'s whole subtree matches vanilla's.
fn assert_parity(command: &str) {
    let ours = ServerCommands::new().wire_tree();
    let theirs = vanilla_tree();
    compare(
        &format!("/{command}"),
        &ours,
        root_literal(&ours, command),
        &theirs,
        root_literal(&theirs, command),
    );
}

/// `/gamemode` — literal → `minecraft:gamemode` → `minecraft:entity`.
///
/// The three things this catches that a hand-written expectation would not:
/// the mode node is **one parser node, not four literals** (the four-literal shape
/// is from a much older version); the target node's flags byte is
/// `single: false, players_only: true`; and **both** the mode node and the target
/// node are executable, which is what the optional trailing argument really looks
/// like on the wire.
#[test]
fn gamemode_matches_a_real_26_2_servers_own_subtree() {
    assert_parity("gamemode");
}

/// `/give` — literal → `targets` → `item` → `count`.
///
/// Note the argument order: `<targets>` comes **before** `<item>`, which is the
/// opposite of how the command reads in English and is what a from-memory
/// reconstruction gets wrong. Also `count` is `integer { min: 1 }`, not `min: 0`.
#[test]
fn give_matches_a_real_26_2_servers_own_subtree() {
    assert_parity("give");
}

/// The gate's own control: the comparison actually *can* fail.
///
/// Without this, `compare` silently doing nothing (a wrong child iterator, an
/// `assert_eq!` on two identical `describe` calls of the same tree) would leave
/// both parity tests green while measuring nothing. `/gamemode` and `/give`
/// genuinely differ, so pointing the comparison at mismatched roots must panic.
#[test]
#[should_panic(expected = "ours:")]
fn the_comparison_fails_when_the_subtrees_differ() {
    let ours = ServerCommands::new().wire_tree();
    let theirs = vanilla_tree();
    compare(
        "/control",
        &ours,
        root_literal(&ours, "gamemode"),
        &theirs,
        root_literal(&theirs, "give"),
    );
}

/// `/gamerule`'s one remaining known divergence, asserted rather than left to
/// be discovered. Found by running this gate, not by reading the code.
///
/// **We expose one rule vanilla's tree does not: `max_minecart_speed`.** Not a
/// version skew — it is `registerInteger("max_minecart_speed", …,
/// vanilla's own feature flag set's own of(vanilla's own feature flags's own minecart improvements))`
/// (`vanilla's own game rules's own java`), i.e. gated behind an **experimental feature flag**
/// the oracle world does not enable, so vanilla legitimately omits it from the
/// tree it sends. Our `GAME_RULES` carries no feature-flag concept and therefore
/// offers it unconditionally. That is the honest description of the gap and it is
/// pinned here rather than papered over with an inequality.
///
/// **Vanilla's own two-literals-per-rule shape is now matched.**
/// `commands::gamerule::register_rule_literal` builds both `keep_inventory`
/// *and* `minecraft:keep_inventory` (`vanilla's own game rule command's own register`'s own
/// `unqualified`/`qualified` pair, `.cache/mc/26.2/src`), so the child count
/// is a plain `theirs == (ours - FEATURE_FLAGGED_RULES) * 2` no longer — it is
/// `theirs == (ours - FEATURE_FLAGGED_RULES * 2)`, both sides counting **all**
/// literals (bare and namespaced) now that we register both too.
///
/// The per-rule subtrees themselves *do* match, both spellings, which is the
/// part that decides whether a client autocompletes a value the server
/// rejects; that is asserted below for one boolean rule and one
/// bounded-integer rule, each checked under both its bare and namespaced
/// literal.
#[test]
fn gamerule_has_every_rule_subtree_right_and_every_literal_too() {
    let ours = ServerCommands::new().wire_tree();
    let theirs = vanilla_tree();
    let our_root = root_literal(&ours, "gamerule");
    let their_root = root_literal(&theirs, "gamerule");

    let ours_children = ours.node(our_root).expect("node").children.len();
    let theirs_children = theirs.node(their_root).expect("node").children.len();
    /// `max_minecart_speed`, behind `vanilla's own feature flags's own minecart improvements` —
    /// counted once per spelling (bare + namespaced), since both sides now
    /// register both literals for every rule they carry at all.
    const FEATURE_FLAGGED_LITERALS: usize = 2;
    assert_eq!(
        theirs_children,
        ours_children - FEATURE_FLAGGED_LITERALS,
        "both trees now register a namespaced alias per rule; the only remaining gap is the \
         feature-flagged rule. ours={ours_children} vanilla={theirs_children}"
    );
    // Which rule is the extra one, named — so a *second* divergence appearing
    // later cannot hide inside the arithmetic above. Compared by *bare* name
    // only (namespaced aliases filtered out) so the feature-flagged rule is
    // named once, not twice.
    let literal_names = |tree: &CommandTree, root: usize| -> Vec<String> {
        tree.node(root)
            .expect("node")
            .children
            .iter()
            .filter_map(|&child| match &tree.node(child).expect("child").kind {
                NodeKind::Literal { name } if !name.starts_with("minecraft:") => Some(name.clone()),
                _ => None,
            })
            .collect()
    };
    let theirs_names = literal_names(&theirs, their_root);
    let extra: Vec<String> = literal_names(&ours, our_root)
        .into_iter()
        .filter(|name| !theirs_names.contains(name))
        .collect();
    assert_eq!(
        extra,
        ["max_minecart_speed"],
        "the only rule we may offer that vanilla's tree does not is the feature-flagged one"
    );

    // Two rules with different value types, so this separates "the literal is
    // there" from "its value node is right" — checked under both spellings,
    // since a redirect-free duplicate subtree is exactly the shape a
    // half-built alias (present but pointing at the wrong executor, or built
    // with a stale spec) would still pass a bare-name-only check on.
    for rule in ["keep_inventory", "max_snow_accumulation_height"] {
        for spelling in [rule.to_string(), format!("minecraft:{rule}")] {
            let find = |tree: &CommandTree, root: usize| {
                *tree
                    .node(root)
                    .expect("node")
                    .children
                    .iter()
                    .find(|&&child| {
                        matches!(&tree.node(child).expect("child").kind,
                            NodeKind::Literal { name } if *name == spelling)
                    })
                    .unwrap_or_else(|| panic!("`/gamerule {spelling}` is missing"))
            };
            compare(
                &format!("/gamerule/{spelling}"),
                &ours,
                find(&ours, our_root),
                &theirs,
                find(&theirs, their_root),
            );
        }
    }
}
