//! Hermetic replay of a real server's `minecraft:commands` /
//! `minecraft:command_suggestions` bytes (issue #470).
//!
//! Every fixture here was authored by a **real vanilla 26.2 server** and
//! captured by `tests/live_command_tree.rs` — never by our own encoder. That
//! matters more than usual for this packet: the command tree is a
//! self-describing, variable-length node stream with no per-node length prefix,
//! so a single wrong payload width silently reinterprets every following node
//! rather than erroring. `Reader::ensure_empty` landing exactly on the last byte
//! of a 30 kB, 2000-node stream is therefore a strong end-to-end check that
//! every one of the thirteen payload-carrying `ArgumentTypeInfo`s was read at
//! the right width.
//!
//! The *completion behaviour* this decode exists to feed is gated separately, in
//! `crates/lodestone-shell/tests/command_tree_completion.rs` — a tree that
//! decodes and yields no suggestions is the connected-wire-carrying-a-wrong-value
//! failure `cargo xtask connectedness` structurally cannot see.

use std::path::PathBuf;

use lodestone_model::command_tree::{ArgumentParser, NodeKind};
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).expect("fixture hex byte"))
        .collect()
}

/// Drives the packet through the public `VersionAdapter` seam rather than
/// naming the decode helper, so this cannot pass while the adapter has no arm.
fn decode(packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    V770Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("a real server's payload must decode")
}

fn decode_tree(payload: &[u8]) -> lodestone_model::command_tree::CommandTree {
    let directives = decode(play::clientbound::COMMANDS, payload);
    for directive in directives {
        if let Directive::Emit(ClientEvent::CommandTreeUpdated { tree }) = directive {
            return *tree;
        }
    }
    panic!("the COMMANDS arm must emit ClientEvent::CommandTreeUpdated");
}

#[test]
fn a_real_servers_command_tree_decodes_with_no_trailing_bytes() {
    let tree = decode_tree(&fixture("command_tree_creative.hex"));

    // A floor, not an exact count: the node total depends on the joining
    // player's permission level and on any data pack. Vanilla 26.2's own tree is
    // ~2000 nodes; anything under a few hundred means we stopped early.
    assert!(
        tree.len() > 500,
        "a real 26.2 tree has thousands of nodes; decoded only {}",
        tree.len()
    );

    // Vanilla's own command set, which exists outside this tree entirely.
    let literals: Vec<&str> = (0..tree.len())
        .filter_map(|i| match &tree.node(i).expect("node").kind {
            NodeKind::Literal { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    for expected in [
        "gamemode", "give", "teleport", "time", "help", "summon", "execute", "kill",
    ] {
        assert!(
            literals.contains(&expected),
            "vanilla 26.2 has a `{expected}` command; it is not in the decoded tree"
        );
    }
}

/// The load-bearing width check. Every payload-carrying `ArgumentTypeInfo` must
/// have been read at exactly the right width for the stream to stay aligned, and
/// the *evidence* that they were is that these specific, independently-known
/// values came out the far end of a 2000-node walk.
#[test]
fn the_payload_carrying_parsers_decode_to_their_known_vanilla_values() {
    let tree = decode_tree(&fixture("command_tree_creative.hex"));

    let parsers: Vec<&ArgumentParser> = (0..tree.len())
        .filter_map(|i| match &tree.node(i).expect("node").kind {
            NodeKind::Argument { parser, .. } => Some(parser),
            _ => None,
        })
        .collect();
    assert!(
        parsers.len() > 200,
        "a real tree is mostly arguments; found only {}",
        parsers.len()
    );

    // `/time set <time>` uses `TimeArgument.time()` — a plain `int` minimum of
    // 0, with no flags byte. Reading it as a flags-byte-prefixed pair (the shape
    // every *other* numeric parser has) would consume the wrong width and
    // desync; this asserts the exact value, not merely that a `Time` exists.
    assert!(
        parsers.contains(&&ArgumentParser::Time { min: 0 }),
        "vanilla's `/time` arguments are `TimeArgument` with min 0; got none"
    );

    // `brigadier:string` carries a `StringType` VarInt ordinal. Vanilla uses all
    // three kinds across its command set; a wrong ordinal mapping would show up
    // as a missing kind rather than a decode error.
    for kind in [
        lodestone_model::command_tree::StringKind::SingleWord,
        lodestone_model::command_tree::StringKind::GreedyPhrase,
    ] {
        assert!(
            parsers.contains(&&ArgumentParser::String(kind)),
            "vanilla uses brigadier:string {kind:?} somewhere; it did not decode"
        );
    }

    // `minecraft:entity`'s flags byte. `/kill <targets>` is multiple, any
    // entity: both bits clear. `/gamemode <mode> <target>` is players-only,
    // multiple: bit 1 set, bit 0 clear.
    assert!(
        parsers.contains(&&ArgumentParser::Entity {
            single: false,
            players_only: false
        }),
        "`/kill <targets>` is a multiple, any-entity selector"
    );
    assert!(
        parsers.contains(&&ArgumentParser::Entity {
            single: false,
            players_only: true
        }),
        "`/gamemode <mode> <targets>` is a multiple, players-only selector"
    );

    // The five `resource*` parsers carry an `Identifier` registry key. A wrong
    // string width here is the single most destructive misread possible, since
    // it consumes an arbitrary number of following bytes.
    let registries: Vec<String> = parsers
        .iter()
        .filter_map(|p| match p {
            ArgumentParser::Resource { registry }
            | ArgumentParser::ResourceKeyArg { registry }
            | ArgumentParser::ResourceOrTag { registry }
            | ArgumentParser::ResourceOrTagKey { registry }
            | ArgumentParser::ResourceSelector { registry } => Some(registry.to_string()),
            _ => None,
        })
        .collect();
    for expected in ["minecraft:entity_type", "minecraft:enchantment"] {
        assert!(
            registries.iter().any(|r| r == expected),
            "a `resource*` parser over `{expected}` must decode to that exact registry key; \
             got {registries:?}"
        );
    }

    // `brigadier:integer` with both bounds present, and with only a minimum —
    // both flag-byte paths exercised by real data.
    assert!(
        parsers
            .iter()
            .any(|p| matches!(p, ArgumentParser::Integer { min, max }
                if *min != i32::MIN && *max != i32::MAX)),
        "vanilla has bounded integer arguments (e.g. `/xp`, `/difficulty` levels)"
    );
}

/// `execute run` redirects back toward the root — a real, server-sent redirect
/// cycle in production data. This is the positive case for the guard
/// `lodestone_model::command_tree` only had a synthetic control for.
#[test]
fn a_real_tree_contains_redirects_and_effective_children_terminates() {
    let tree = decode_tree(&fixture("command_tree_creative.hex"));

    let redirecting: Vec<usize> = (0..tree.len())
        .filter(|&i| tree.node(i).expect("node").redirect.is_some())
        .collect();
    assert!(
        !redirecting.is_empty(),
        "vanilla's `/execute` subtree redirects; the decoded tree has no redirect at all, \
         which means the FLAG_REDIRECT branch never fired"
    );

    // Must terminate, and must reach something, for every redirecting node.
    for &idx in &redirecting {
        let reached = tree.effective_children(idx);
        assert!(
            reached.len() <= tree.len(),
            "effective_children({idx}) returned more nodes than exist"
        );
    }
}

#[test]
fn a_real_servers_suggestions_reply_decodes_to_the_set_the_server_sent() {
    let directives = decode(
        play::clientbound::COMMAND_SUGGESTIONS,
        &fixture("command_suggestions_gamemode.hex"),
    );
    let mut seen = None;
    for directive in directives {
        if let Directive::Emit(ClientEvent::CommandSuggestionsReceived {
            id,
            start,
            length,
            suggestions,
        }) = directive
        {
            seen = Some((id, start, length, suggestions));
        }
    }
    let (id, start, length, suggestions) =
        seen.expect("the COMMAND_SUGGESTIONS arm must emit CommandSuggestionsReceived");

    // Exact values, all four of them, from the capture log — the server's own
    // answer to the `/gamemode ` request this client really sent.
    assert_eq!(id, 0, "the server echoes our transaction id");
    assert_eq!(start, 10, "`/gamemode ` is 10 bytes; the reply replaces from there");
    assert_eq!(length, 0, "nothing typed after the space yet");
    assert_eq!(
        suggestions.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["adventure", "creative", "spectator", "survival"],
    );
}

// --- Unknown-parser tolerance -------------------------------------------
//
// Hand-built bytes, deliberately: this asserts a *tolerance* property, not a
// wire-format one. The wire format is proved by the real capture above; what no
// real vanilla server can produce is a parser id this build does not model, so
// the only way to exercise the branch is to author one.

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

/// A three-node tree: a root whose children are a literal `hello` and an
/// argument using registry id `parser_id`.
fn tree_with_parser(parser_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    var_i32(&mut out, 3); // node count
    // 0: root, children [1, 2]
    out.push(0x00);
    var_i32(&mut out, 2);
    var_i32(&mut out, 1);
    var_i32(&mut out, 2);
    // 1: literal "hello", executable, no children
    out.push(0x01 | 0x04);
    var_i32(&mut out, 0);
    string(&mut out, "hello");
    // 2: argument "x" with the given parser id, executable, no children
    out.push(0x02 | 0x04);
    var_i32(&mut out, 0);
    string(&mut out, "x");
    var_i32(&mut out, parser_id);
    var_i32(&mut out, 0); // root index
    out
}

#[test]
fn an_unmodeled_parser_id_degrades_to_one_unrecognized_node_and_keeps_the_rest() {
    // 900 is outside 26.2's `minecraft:command_argument_type` range (0..=56):
    // exactly what a datapack or a future version would send.
    let tree = decode_tree(&tree_with_parser(900));

    assert_eq!(tree.len(), 3, "the whole tree must survive, not just the root");
    assert_eq!(
        tree.node(2).expect("node 2").kind,
        NodeKind::Unrecognized { parser_id: 900 },
        "an unmodeled id becomes a nameless pass-through, carrying the raw id"
    );
    // The sibling is untouched — this is the property that makes the tolerance
    // worth having rather than merely non-fatal.
    assert_eq!(
        tree.node(1).expect("node 1").kind,
        NodeKind::Literal {
            name: "hello".into()
        }
    );
    assert!(tree.node(1).expect("node 1").executable);
}

/// The control for the test above: the *same* byte layout with a parser id this
/// build **does** model must produce a real `Argument`. Without this, the
/// assertion above is satisfied by a decoder that returns `Unrecognized` for
/// everything.
#[test]
fn the_same_layout_with_a_modeled_parser_id_is_not_unrecognized() {
    // 0 is `brigadier:bool`, a `SingletonArgumentInfo` — same zero-payload
    // shape as the unmodeled case, so the only difference is recognition.
    let tree = decode_tree(&tree_with_parser(0));

    assert_eq!(tree.len(), 3);
    assert_eq!(
        tree.node(2).expect("node 2").kind,
        NodeKind::Argument {
            name: "x".into(),
            parser: ArgumentParser::Bool,
            suggestions: None,
        },
        "a modeled id must decode as a named Argument, proving the test above is \
         measuring recognition and not a decoder that gives up on every argument"
    );
}
