//! **The `COMMANDS` encoder, against vanilla's own bytes.**
//!
//! `lodestone-shell`'s chat box, its `CommandTreeCell`, its highlighter and its
//! suggestion list were all complete and all starved: no protocol family in this
//! workspace encoded `minecraft:commands` (clientbound id 16), so the client was
//! never sent a tree and had nothing to complete against. This gate is on the
//! encoder that fixed that, and on the join sequence that calls it.
//!
//! # The outside expectation, and why it is not a round trip
//!
//! `tests/fixtures/command_tree_creative.hex` is 30,248 bytes of real
//! `minecraft:commands` payload captured from a vanilla 26.2 server by
//! `tests/live_command_tree.rs` — 2,017 nodes, authored by Mojang's own
//! `ClientboundCommandsPacket::write` through their own `ArgumentTypeInfo`s.
//!
//! [`vanilla_bytes_reencode_byte_identically`] decodes that payload and encodes it
//! again, and requires the result to be **byte-identical to the capture**. The
//! expected value is therefore vanilla's, not ours. That is the difference from
//! `decode(encode(x)) == x`, which two symmetric misunderstandings satisfy: here a
//! transposed child/redirect pair, a flag bit in the wrong position, a parser id
//! off by one, a `hasMin`/`hasMax` byte built the wrong way round or a
//! suggestions id written before its payload instead of after all move the output
//! away from 30,248 bytes Mojang wrote.
//!
//! It is also broad in a way our own four commands are not: the capture exercises
//! `brigadier:{bool,integer,long,float,double,string}`, `minecraft:entity`,
//! `minecraft:score_holder`, `minecraft:time`, all five `resource*` parsers with
//! their registry-key payloads, `minecraft:ask_server` suggestion ids, and the
//! redirect topology `/execute` builds — none of which our tree contains.
//!
//! # What the *other* gates add
//!
//! Byte identity says the encoder reproduces a tree it was given. It cannot say
//! the server sends one, that the tree it sends is the server's real command set,
//! or that permission pruning happens. Those are the end-to-end and filtering
//! gates below, and the pruning one is measured by *predicted node counts* rather
//! than by "fewer nodes", because a direction-only assertion is satisfied by
//! dropping the wrong thing.

use std::path::PathBuf;
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::command_tree::{
    ArgumentParser, CommandTree, NodeKind, RawCommandNode, StringKind,
};
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    CommandDispatch, NoEntities, PlayerRegistry, ServerCommands, ServerDirective, ServerProtocol,
    WorldgenChunkSource,
};
use lodestone_v770::packet_ids::play;
use lodestone_v770::{V770Adapter, V770ServerProtocol, adapter};
use lodestone_world::World;
use lodestone_worldgen::density::Density;

fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|token| u8::from_str_radix(token, 16).expect("fixture hex byte"))
        .collect()
}

/// Decodes a `minecraft:commands` payload through the public [`VersionAdapter`]
/// seam, so nothing here can pass against a private helper the real client does
/// not use.
fn decode(payload: &[u8]) -> CommandTree {
    let directives = V770Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Play, play::clientbound::COMMANDS, payload)
        .expect("the payload must decode");
    for directive in directives {
        if let Directive::Emit(ClientEvent::CommandTreeUpdated { tree }) = directive {
            return *tree;
        }
    }
    panic!("the COMMANDS arm must emit ClientEvent::CommandTreeUpdated");
}

/// Encodes through the public [`ServerProtocol`] seam and returns the payload,
/// asserting the packet id on the way — a tree sent under the wrong id is a tree
/// the client never sees.
fn encode(tree: &CommandTree) -> Vec<u8> {
    match V770ServerProtocol.encode_commands(tree) {
        ServerDirective::Send { packet_id, payload } => {
            assert_eq!(
                packet_id,
                play::clientbound::COMMANDS,
                "the tree must go out as `minecraft:commands`"
            );
            payload
        }
        other => panic!("encode_commands must send a packet, got {other:?}"),
    }
}

/// **The gate.** Vanilla's own 30 KB of `minecraft:commands` survives a decode and
/// a re-encode with every byte in place.
#[test]
fn vanilla_bytes_reencode_byte_identically() {
    let captured = fixture_bytes("command_tree_creative.hex");
    let tree = decode(&captured);

    // Stated so a shrunken or swapped fixture cannot quietly weaken this: the
    // capture's own node count, from `builtin_command_parity.rs`'s header.
    assert_eq!(tree.len(), 2_017, "the captured creative tree is 2,017 nodes");
    assert_eq!(captured.len(), 30_248, "the captured payload is 30,248 bytes");

    let reencoded = encode(&tree);
    if reencoded != captured {
        let first = reencoded
            .iter()
            .zip(&captured)
            .position(|(ours, theirs)| ours != theirs)
            .unwrap_or_else(|| reencoded.len().min(captured.len()));
        let window = first.saturating_sub(8)..(first + 8).min(reencoded.len().min(captured.len()));
        panic!(
            "re-encode diverged from vanilla's bytes at offset {first} \
             (ours {} bytes, vanilla {} bytes)\n  ours:    {:02x?}\n  vanilla: {:02x?}",
            reencoded.len(),
            captured.len(),
            &reencoded[window.clone()],
            &captured[window],
        );
    }
}

/// **The control the gate above needs.** An assertion of byte identity is only
/// worth what the evidence that a difference *would* be caught is worth.
///
/// Two deliberate corruptions of the decoded tree, each of a kind the encoder
/// could plausibly get wrong on its own, must both make the comparison fail — and
/// the second is the one that matters, because it is the mistake a flat index
/// array invites: swapping two adjacent same-typed VarInts.
#[test]
fn a_corrupted_tree_does_not_reencode_to_vanillas_bytes() {
    let captured = fixture_bytes("command_tree_creative.hex");
    let tree = decode(&captured);
    assert_eq!(encode(&tree), captured, "premise: the untouched tree matches");

    // 1. One parser id moved. `/gamemode`'s argument becomes a bool.
    let mut nodes: Vec<RawCommandNode> =
        (0..tree.len()).map(|index| tree.node(index).expect("in range").clone()).collect();
    let argument = nodes
        .iter()
        .position(|node| matches!(&node.kind, NodeKind::Argument { parser, .. } if *parser == ArgumentParser::GameMode))
        .expect("the captured tree has a minecraft:gamemode argument");
    if let NodeKind::Argument { parser, .. } = &mut nodes[argument].kind {
        *parser = ArgumentParser::Bool;
    }
    let corrupted = CommandTree::new(nodes, tree.root()).expect("still a consistent graph");
    assert_ne!(
        encode(&corrupted),
        captured,
        "a wrong parser id must change the bytes — otherwise the gate above measures nothing"
    );

    // 2. Two adjacent child indices transposed. This is the corruption a flat node
    //    array invites and the one that round-trips perfectly through our own code:
    //    both are VarInts, neither carries a type, and nothing downstream of the
    //    encoder can tell they were swapped. **No node in this capture has both a
    //    redirect and a child** (measured: 74 nodes redirect, 0 of them have
    //    children), so the child/redirect pair cannot be transposed here — the
    //    adjacent-children swap is the available form of the same hazard.
    let mut nodes: Vec<RawCommandNode> =
        (0..tree.len()).map(|index| tree.node(index).expect("in range").clone()).collect();
    let swappable = nodes
        .iter()
        .position(|node| node.children.len() >= 2 && node.children[0] != node.children[1])
        .expect("the captured tree has a node with two pairwise-distinct children");
    nodes[swappable].children.swap(0, 1);
    let corrupted = CommandTree::new(nodes, tree.root()).expect("still a consistent graph");
    assert_ne!(
        encode(&corrupted),
        captured,
        "transposing two adjacent child indices must change the bytes"
    );

    // 3. A redirect retargeted. Separate from the above because a redirect index is
    //    written under a *flag* and an encoder that emitted it unconditionally, or
    //    skipped it, would pass both of the first two.
    let mut nodes: Vec<RawCommandNode> =
        (0..tree.len()).map(|index| tree.node(index).expect("in range").clone()).collect();
    let redirecting = nodes
        .iter()
        .position(|node| node.redirect.is_some_and(|target| target != 0))
        .expect("the captured tree has a node redirecting somewhere other than the root");
    nodes[redirecting].redirect = Some(0);
    let corrupted = CommandTree::new(nodes, tree.root()).expect("still a consistent graph");
    assert_ne!(
        encode(&corrupted),
        captured,
        "moving a redirect target must change the bytes"
    );
}

/// The capture is broad but not total: it uses **55 of the 57**
/// `minecraft:command_argument_type` ids, missing `brigadier:long` (4) and
/// `minecraft:angle` (28) because no vanilla command declares either.
///
/// So those two are the only parsers the byte-identity gate leaves un-evidenced,
/// and they are gated here: the id written is the `protocol_id` from
/// `.cache/mc/26.2/generated/reports/registries.json`, read out of the encoded
/// bytes rather than asserted through a round trip, because a round trip agrees
/// with itself on any id.
#[test]
fn the_two_parsers_the_capture_never_uses_still_carry_their_registry_ids() {
    for (parser, id, payload_len) in [
        // `LongArgumentInfo`: a flags byte then only the present bounds, so a
        // both-bounds template is 1 + 8 + 8 bytes.
        (ArgumentParser::Long { min: -5, max: 9 }, 4u8, 17usize),
        // `SingletonArgumentInfo`: no payload at all.
        (ArgumentParser::Angle, 28u8, 0usize),
    ] {
        let tree = CommandTree::new(
            vec![
                RawCommandNode {
                    kind: NodeKind::Root,
                    executable: false,
                    restricted: false,
                    redirect: None,
                    children: vec![1],
                },
                RawCommandNode {
                    kind: NodeKind::Argument {
                        name: "v".to_string(),
                        parser: parser.clone(),
                        suggestions: None,
                    },
                    executable: true,
                    restricted: false,
                    redirect: None,
                    children: vec![],
                },
            ],
            0,
        )
        .expect("consistent");
        let bytes = encode(&tree);
        // count(1) root[flags,childcount,child](3) argument[flags,childcount](2)
        // name["v"](2) then the parser id.
        let id_at = 1 + 3 + 2 + 2;
        assert_eq!(bytes[id_at], id, "registry id for {parser:?}");
        // …and everything after the id is the payload plus the trailing root index.
        assert_eq!(
            bytes.len() - id_at - 1,
            payload_len + 1,
            "payload length for {parser:?}"
        );
        assert_eq!(decode(&bytes), tree, "{parser:?} must survive the real decoder");
    }
}

/// The one field order in `ArgumentNodeStub::write` that cannot be guessed from
/// field names: the custom-suggestions identifier is written **after** the
/// parser's payload, not before it.
///
/// Asserted against bytes derived by hand from the record rather than by
/// comparison with our own decoder, and the node is built with **pairwise-distinct
/// indices** (one child at 2, a redirect to 1) so a transposition cannot survive
/// it. Every byte below is annotated with the call that emits it.
#[test]
fn an_argument_nodes_suggestions_id_is_written_after_its_parser_payload() {
    let argument = RawCommandNode {
        kind: NodeKind::Argument {
            name: "targets".to_string(),
            // `single` and `players_only` both set: flags byte 0b11 = 3, an
            // asymmetric value, so bit 0 and bit 1 cannot be confused.
            parser: ArgumentParser::Entity { single: true, players_only: true },
            suggestions: Some("minecraft:ask_server".parse().expect("valid key")),
        },
        executable: true,
        restricted: true,
        redirect: Some(1),
        children: vec![2],
    };
    let filler = RawCommandNode {
        kind: NodeKind::Literal { name: "x".to_string() },
        executable: false,
        restricted: false,
        redirect: None,
        children: vec![],
    };
    let tree = CommandTree::new(vec![argument, filler.clone(), filler], 0).expect("consistent");

    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        0x03,                                     // writeVarInt(3) — three entries
        // entry 0: TYPE_ARGUMENT(2) | EXECUTABLE(4) | REDIRECT(8) | CUSTOM_SUGGESTIONS(16) | RESTRICTED(32)
        0x3e,
        0x01, 0x02,                               // writeVarIntArray([2])
        0x01,                                     // writeVarInt(redirect = 1), present only under FLAG_REDIRECT
        0x07, b't', b'a', b'r', b'g', b'e', b't', b's', // writeUtf("targets")
        0x06,                                     // parser id 6 = minecraft:entity
        0x03,                                     // EntityArgument.Info: single | playersOnly << 1
        0x14, b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':',
              b'a', b's', b'k', b'_', b's', b'e', b'r', b'v', b'e', b'r', // writeIdentifier, AFTER the payload
        // entries 1 and 2: TYPE_LITERAL, no children, no redirect, writeUtf("x")
        0x01, 0x00, 0x01, b'x',
        0x01, 0x00, 0x01, b'x',
        0x00,                                     // writeVarInt(rootIndex = 0)
    ];
    assert_eq!(encode(&tree), expected, "byte layout must match ArgumentNodeStub::write");

    // And the real decoder must read back exactly what went in, which is what
    // proves the hand-derived bytes are not merely self-consistent.
    let round_tripped = decode(&expected);
    assert_eq!(round_tripped, tree);
}

/// An argument-type id this build does not model costs **that node** and nothing
/// else — the tolerance `lodestone_model::command_tree` documents, here driven
/// from the encode side.
///
/// This is also the "would a wrong parser id be noticed?" control the brief asks
/// for: it fires. The node comes back nameless and unparseable, so a single wrong
/// id silently deletes a command from the client's tree even though the packet
/// still decodes.
#[test]
fn an_unmodeled_parser_id_degrades_only_its_own_node() {
    let root = RawCommandNode {
        kind: NodeKind::Root,
        executable: false,
        restricted: false,
        redirect: None,
        children: vec![1, 2],
    };
    let bad = RawCommandNode {
        kind: NodeKind::Argument {
            name: "mystery".to_string(),
            parser: ArgumentParser::Unknown(9_999),
            suggestions: None,
        },
        executable: true,
        restricted: false,
        redirect: None,
        children: vec![],
    };
    let good = RawCommandNode {
        kind: NodeKind::Literal { name: "survives".to_string() },
        executable: true,
        restricted: false,
        redirect: None,
        children: vec![],
    };
    let tree = CommandTree::new(vec![root, bad, good], 0).expect("consistent");

    let decoded = decode(&encode(&tree));
    assert_eq!(
        decoded.node(1).expect("node 1").kind,
        NodeKind::Unrecognized { parser_id: 9_999 },
        "an unknown id must degrade to a nameless pass-through, not fail the packet"
    );
    assert_eq!(
        decoded.node(2).expect("node 2").kind,
        NodeKind::Literal { name: "survives".to_string() },
        "the sibling after the degraded node must still decode — the reader stayed aligned"
    );
}

// ---------------------------------------------------------------------------
// Permission pruning
// ---------------------------------------------------------------------------

/// Every built-in root is level-gated, so a level-0 player's tree is the root
/// **and nothing else** — and the counts are predicted, not compared.
///
/// `ServerCommands::wire_tree()` (unfiltered) and `wire_tree_for(4)` must agree on
/// size because level 4 denies nothing; `wire_tree_for(0)` must be exactly one
/// node. A "fewer nodes at level 0" assertion would pass while pruning the wrong
/// branch.
#[test]
fn permission_pruning_predicts_both_ends_exactly() {
    let commands = ServerCommands::new();
    let full = commands.wire_tree();
    let admin = commands.wire_tree_for(4);
    let nobody = commands.wire_tree_for(0);

    assert_eq!(
        admin.len(),
        full.len(),
        "level 4 denies nothing, so the filtered walk must reach every node"
    );
    assert_eq!(
        nobody.len(),
        1,
        "every built-in root carries a level requirement, so a level-0 player is sent the bare root"
    );
    assert!(
        nobody.node(nobody.root()).expect("root").children.is_empty(),
        "the bare root must have no children left pointing at pruned nodes"
    );

    // The pruned tree still has to be a *valid* tree on the wire, which is the
    // part renumbering can break: encode it and decode it back through the real
    // adapter, whose `CommandTree::new` validates every index.
    let round_tripped = decode(&encode(&nobody));
    assert_eq!(round_tripped, nobody);
}

/// Pruning is by **subtree**, matching `Commands.fillUsableCommands`' recursion
/// sitting inside the `canUse` branch — and the surviving indices are renumbered
/// against the pruned list rather than left pointing into the unfiltered arena.
///
/// # Why the shipped tree cannot measure this
///
/// All four built-ins are `Commands.LEVEL_GAMEMASTERS` (2), so the real tree is
/// all-or-nothing across every level: 1 node at levels 0–1 and the whole thing at
/// 2–4. An input where the right and the wrong hypothesis coincide is not a test,
/// so this uses `ServerCommands::from_registrar` — the seam
/// `lodestone_server::commands`' own doc names for exercising the substrate — to
/// build a tree with **mixed** levels.
///
/// # Both hypotheses, computed
///
/// `/secret` is gated at level 2 and has two ungated descendants; `/open` is
/// ungated with one child. At level 0:
///
/// | hypothesis | nodes |
/// |---|---|
/// | prune the subtree (vanilla, correct) | root + open + open's child = **3** |
/// | prune only the gated node itself | those 3 plus `inner` and `deeper`, reattached or orphaned = **5** |
///
/// The two answers differ, which is the whole point of choosing this shape.
#[test]
fn a_denied_node_takes_its_whole_subtree_with_it() {
    use lodestone_server::commands::Registrar;

    let mut registrar = Registrar::new();
    let root = registrar.root();
    let open = registrar.literal(root, "open");
    let shallow = registrar.literal(open, "shallow");
    registrar.exec(shallow, |_| Ok(1));
    let secret = registrar.literal(root, "secret");
    registrar.require_level(secret, 2);
    // Neither descendant carries a requirement of its own: they must disappear
    // because their *parent* was denied, which is the behaviour under test.
    let inner = registrar.literal(secret, "inner");
    let deeper = registrar.literal(inner, "deeper");
    registrar.exec(deeper, |_| Ok(1));
    let commands = ServerCommands::from_registrar(registrar);

    let admin = commands.wire_tree_for(4);
    assert_eq!(
        admin.len(),
        6,
        "root, open, shallow, secret, inner, deeper — level 4 denies nothing"
    );

    let nobody = commands.wire_tree_for(0);
    assert_eq!(
        nobody.len(),
        3,
        "subtree pruning leaves root + open + shallow; leaving `inner`/`deeper` behind would be 5"
    );
    let mut visible = root_literals(&nobody);
    visible.sort();
    assert_eq!(visible, ["open"], "`/secret` must not appear at level 0");

    // Renumbering: the surviving indices must be a consistent graph over the
    // *pruned* list. The adapter's `CommandTree::new` validates every child and
    // redirect index, so a stale arena index shows up as a decode failure here
    // rather than as a client that quietly loses a branch.
    assert_eq!(decode(&encode(&nobody)), nobody);
    assert_eq!(decode(&encode(&admin)), admin);
}

fn root_literals(tree: &CommandTree) -> Vec<String> {
    tree.node(tree.root())
        .expect("root")
        .children
        .iter()
        .filter_map(|&child| match &tree.node(child).expect("child").kind {
            NodeKind::Literal { name } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// End to end
// ---------------------------------------------------------------------------

fn cheap_source() -> WorldgenChunkSource {
    WorldgenChunkSource::new(
        Density::YClampedGradient { from_y: -64.0, to_y: 64.0, from_value: 1.0, to_value: -1.0 },
        -64,
        384,
    )
}

/// **The island gate.** A real client joined to a real server over a real wire
/// receives a `COMMANDS` packet carrying the server's *own* command set.
///
/// Nothing here is a double: `V770ServerProtocol` encodes, `lodestone-server`'s
/// join sequence sends, and `lodestone-client`'s `V770Adapter` decodes. The
/// assertion is on the decoded tree's contents, so a server that sent a
/// well-formed but empty tree fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_join_sequence_sends_the_servers_own_command_tree() {
    let (client_end, server_end) = memory_pair();
    let source = cheap_source();
    let players = PlayerRegistry::new();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        let _ = lodestone_server::serve_connection_with_commands(
            &mut conn,
            &V770ServerProtocol,
            &source,
            &lodestone_server::PlayerAwareSource::new(NoEntities, players),
            0,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &CommandDispatch::none(),
        )
        .await;
    });

    let (mut handle, mut events) = ClientBuilder::new(
        ServerAddress { host: "memory".into(), port: 0 },
        LoginProfile { username: "Completer".into(), uuid: uuid::Uuid::from_u128(0x5eed_0016) },
        Box::new(adapter()),
    )
    .connect_with(client_end);

    handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");

    let mut received: Option<CommandTree> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while received.is_none() && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::CommandTreeUpdated { tree })) => received = Some(*tree),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    handle.shutdown();
    server.abort();

    let tree = received.expect(
        "the client received no COMMANDS packet — this is the island the encoder was written for",
    );

    // The server's tree, at the level the default (unconfigured) access lists
    // grant a connection: `AccessLists::command_permission_level` answers
    // `MAX_PERMISSION_LEVEL` while unconfigured, so the whole tree goes out.
    let expected = ServerCommands::new().wire_tree_for(4);
    assert_eq!(
        tree, expected,
        "the tree the client decoded must be the projection the server's own dispatcher declares"
    );

    // Named explicitly as well, because equality with a projection would also
    // hold if both were empty.
    let mut literals = root_literals(&tree);
    literals.sort();
    assert_eq!(literals, ["effect", "gamemode", "gamerule", "give"]);

    // And the one thing the client's completion actually reads off an argument
    // node: `/give <targets> <item> [count]`'s count is `integer(1, ..)`, so its
    // parser payload must carry `min: 1` — not the default `i32::MIN` a
    // "properties are optional" encoder would emit.
    let give = tree
        .node(tree.root())
        .expect("root")
        .children
        .iter()
        .copied()
        .find(|&child| {
            matches!(&tree.node(child).expect("child").kind,
                NodeKind::Literal { name } if name == "give")
        })
        .expect("`/give` in the decoded tree");
    let count = descendant_argument(&tree, give, "count").expect("`/give`'s count argument");
    assert_eq!(
        count,
        ArgumentParser::Integer { min: 1, max: i32::MAX },
        "the bounds must survive the round trip through the real wire"
    );

    // A parser with a *non*-numeric payload too, so the flags-byte path is not the
    // only one exercised end to end: `/effect`'s effect id is a single word.
    let effect = tree
        .node(tree.root())
        .expect("root")
        .children
        .iter()
        .copied()
        .find(|&child| {
            matches!(&tree.node(child).expect("child").kind,
                NodeKind::Literal { name } if name == "effect")
        })
        .expect("`/effect` in the decoded tree");
    assert_eq!(
        descendant_argument(&tree, effect, "effect"),
        Some(ArgumentParser::String(StringKind::SingleWord)),
    );
}

/// The parser of the first argument node named `name` anywhere under `start`.
fn descendant_argument(tree: &CommandTree, start: usize, name: &str) -> Option<ArgumentParser> {
    let mut pending = vec![start];
    let mut seen = std::collections::HashSet::new();
    while let Some(index) = pending.pop() {
        if !seen.insert(index) {
            continue;
        }
        let node = tree.node(index)?;
        if let NodeKind::Argument { name: found, parser, .. } = &node.kind
            && found == name
        {
            return Some(parser.clone());
        }
        pending.extend(node.children.iter().copied());
    }
    None
}
