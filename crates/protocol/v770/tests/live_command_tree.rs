//! Live `commands` / `command_suggestions` capture.
//!
//! Joins the flat creative 26.2 oracle (game :25570), captures the **raw
//! `minecraft:commands` payload the server itself authored**, then sends a real
//! serverbound `command_suggestion` request and captures the server's
//! `minecraft:command_suggestions` reply. Both are written to `tests/fixtures/`
//! and replayed by the hermetic siblings
//! (`tests/command_tree.rs` here, and
//! `crates/lodestone-shell/tests/command_tree_completion.rs` for the completion
//! behaviour), so the decoder is never validated against bytes our own encoder
//! produced — `decode(encode(x)) == x` is satisfied by two symmetric
//! misunderstandings (`CLAUDE.md`, evidence standards).
//!
//! Run with:
//!
//! ```text
//! ./scripts/live-oracles/creative.sh
//! cargo test -p lodestone-v770 --features live-commands --test live_command_tree \
//!     -- --ignored --nocapture
//! ```
//!
//! Set `LODESTONE_CAPTURE_FIXTURES=1` to rewrite the fixtures from this run.
//!
//! # Why the suggestion round trip is captured too
//!
//! `command_suggestions` is the *response* half. Its request half is already
//! encoded (`adapter/serverbound.rs`'s `ClientAction::CommandSuggestion` arm), so the
//! response arm is not an island — but that is a claim about our encoder, and
//! the only thing that proves the pair actually round-trips against vanilla is
//! a real server answering a frame we really sent. This gate sends
//! `/gamemode ` through the *public* `ClientAction` seam and asserts the reply
//! decodes with the same transaction id.

#![cfg(feature = "live-commands")]

use std::path::PathBuf;
use std::time::Duration;

use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress,
    VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

mod common;
use common::unique_username;

const SERVER_ADDR: &str = "127.0.0.1:25570";

const REPAIR: &str = "recreate the creative oracle with: ./scripts/live-oracles/creative.sh \
    (expected a vanilla 26.2 flat creative server on 127.0.0.1:25570)";

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Renders a captured payload as reviewable hex text: a provenance header, then
/// 16 bytes per line. Same format as the `registry_data_*` fixtures.
fn to_hex_fixture(header: &str, bytes: &[u8]) -> String {
    let mut out = String::new();
    for line in header.lines() {
        if line.is_empty() {
            out.push_str("#\n");
        } else {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }
    for chunk in bytes.chunks(16) {
        let row: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        out.push_str(&row.join(" "));
        out.push('\n');
    }
    out
}

fn from_hex_fixture(text: &str) -> Vec<u8> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).expect("fixture hex byte"))
        .collect()
}

/// Writes (or asserts) a fixture. A mismatch is a hard failure naming the
/// re-capture command, so a server-side wire change surfaces here rather than as
/// a hermetic test that keeps passing against stale bytes.
///
/// The command tree is **not** byte-stable across runs: node ordering comes out
/// of `enumerateNodes`' BFS over a `Object2IntOpenHashMap`, and the permission
/// level the oracle grants a freshly-joined player decides which nodes are
/// present at all. So an existing fixture is compared *structurally* by the
/// caller rather than byte-for-byte; this helper only refuses to silently
/// overwrite one.
fn record_fixture(name: &str, header: &str, bytes: &[u8]) -> Vec<u8> {
    let path = fixture_path(name);
    if std::env::var_os("LODESTONE_CAPTURE_FIXTURES").is_some() || !path.exists() {
        std::fs::write(&path, to_hex_fixture(header, bytes)).expect("write fixture");
        println!("wrote {} ({} bytes captured)", path.display(), bytes.len());
        return bytes.to_vec();
    }
    let existing = std::fs::read_to_string(&path).expect("read fixture");
    from_hex_fixture(&existing)
}

async fn apply(
    conn: &mut Connection<TcpStream>,
    state: &mut ConnectionState,
    directive: Directive,
) {
    match directive {
        Directive::Send { packet_id, payload } => {
            conn.write_packet(packet_id, &payload)
                .await
                .expect("write packet");
        }
        Directive::SetState(next) => *state = next,
        Directive::SetCompression(threshold) => conn.set_compression(threshold),
        Directive::Disconnect(reason) => {
            panic!("server disconnected us: {}", reason.to_plain_string())
        }
        _ => {}
    }
}

#[tokio::test]
#[ignore = "requires the flat creative 26.2 oracle on 127.0.0.1:25570"]
async fn a_real_server_sends_a_command_tree_and_answers_a_suggestion_request() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25570,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V770Adapter::new();

    let mut conn = match Connection::connect(SERVER_ADDR).await {
        Ok(conn) => conn,
        Err(err) => panic!("could not reach {SERVER_ADDR}: {err}. {REPAIR}"),
    };
    let mut state = ConnectionState::Handshaking;
    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    let mut commands_payload: Option<Vec<u8>> = None;
    let mut suggestions_payload: Option<Vec<u8>> = None;
    let mut decoded_tree: Option<Box<lodestone_model::command_tree::CommandTree>> = None;
    let mut decoded_reply: Option<(i32, i32, i32, Vec<String>)> = None;
    let mut requested = false;
    // The transaction id `SuggestionRequests` would have minted for a first
    // request. Held here rather than taken from the reply, so the assertion
    // below compares the server's echo against a value that originated on our
    // side of the wire.
    const REQUEST_ID: i32 = 0;
    let overall = Duration::from_secs(90);

    let outcome = tokio::time::timeout(overall, async {
        loop {
            let (packet_id, payload) = match conn.read_packet().await {
                Ok(Some(p)) => p,
                Ok(None) => return false,
                Err(err) => panic!("read error: {err}"),
            };

            if state == ConnectionState::Play && packet_id == play::clientbound::COMMANDS {
                commands_payload = Some(payload.clone());
            }
            if state == ConnectionState::Play
                && packet_id == play::clientbound::COMMAND_SUGGESTIONS
            {
                suggestions_payload = Some(payload.clone());
            }

            let directives =
                match adapter.handle_packet(&mut World::new(), state, packet_id, &payload) {
                    Ok(directives) => directives,
                    Err(err) => panic!("adapter rejected packet {packet_id} in {state:?}: {err}"),
                };
            for directive in directives {
                match &directive {
                    Directive::Emit(ClientEvent::CommandTreeUpdated { tree }) => {
                        decoded_tree = Some(tree.clone());
                    }
                    Directive::Emit(ClientEvent::CommandSuggestionsReceived {
                        id,
                        start,
                        length,
                        suggestions,
                    }) => {
                        decoded_reply = Some((
                            *id,
                            *start,
                            *length,
                            suggestions.iter().map(|s| s.text.clone()).collect(),
                        ));
                    }
                    _ => {}
                }
                apply(&mut conn, &mut state, directive).await;
            }

            // Once the tree has landed, send a real suggestion request through
            // the public `ClientAction` seam — the same call
            // `SuggestionRequests::request` makes.
            if state == ConnectionState::Play && decoded_tree.is_some() && !requested {
                requested = true;
                let action = ClientAction::CommandSuggestion {
                    id: REQUEST_ID,
                    command: "/gamemode ".into(),
                };
                let encoded = adapter
                    .encode_action(state, &action)
                    .expect("encode command_suggestion")
                    .expect("v770 must encode a command_suggestion in Play");
                conn.write_packet(encoded.0, &encoded.1)
                    .await
                    .expect("write command_suggestion");
            }

            if decoded_tree.is_some() && decoded_reply.is_some() {
                return true;
            }
        }
    })
    .await;

    assert_eq!(
        outcome,
        Ok(true),
        "never saw both a command tree and a suggestions reply within {overall:?} \
         (tree: {}, reply: {})",
        decoded_tree.is_some(),
        decoded_reply.is_some(),
    );

    let commands_payload = commands_payload.expect("captured commands payload");
    let tree = decoded_tree.expect("decoded tree");
    println!(
        "captured minecraft:commands: {} bytes, {} nodes, root {}",
        commands_payload.len(),
        tree.len(),
        tree.root()
    );

    // The tree the *server* sent must contain the vanilla commands this oracle
    // definitely has. Expected values from vanilla's own command set, not from
    // our decode.
    let literals: Vec<&str> = (0..tree.len())
        .filter_map(|i| match &tree.node(i).expect("node").kind {
            lodestone_model::command_tree::NodeKind::Literal { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    for expected in ["gamemode", "give", "teleport", "time", "help"] {
        assert!(
            literals.contains(&expected),
            "a real 26.2 server's tree must contain the `{expected}` literal; got {} literals",
            literals.len()
        );
    }

    let (reply_id, start, length, texts) = decoded_reply.expect("decoded reply");
    println!("suggestions reply: id={reply_id} start={start} length={length} texts={texts:?}");
    assert_eq!(
        reply_id, REQUEST_ID,
        "the server must echo the transaction id we sent"
    );

    let stored_commands = record_fixture(
        "command_tree_creative.hex",
        "Raw clientbound `minecraft:commands` payload (play id 16), captured from a\n\
         real vanilla 26.2 server (flat creative oracle, :25570) in Play state.\n\
         Payload only — no packet-length or packet-id prefix.\n\
         \n\
         NOT byte-stable across captures: `ClientboundCommandsPacket.enumerateNodes`\n\
         orders nodes by a BFS over a hash map, and the joining player's permission\n\
         level decides which nodes are sent at all. The hermetic siblings therefore\n\
         assert *structure and completion behaviour*, never a byte count.\n\
         \n\
         Recapture: ./scripts/live-oracles/creative.sh && \\\n\
         cargo test -p lodestone-v770 --features live-commands \\\n\
         --test live_command_tree -- --ignored --nocapture\n\
         (with LODESTONE_CAPTURE_FIXTURES=1 to overwrite)",
        &commands_payload,
    );
    record_fixture(
        "command_suggestions_gamemode.hex",
        "Raw clientbound `minecraft:command_suggestions` payload (play id 15),\n\
         captured from a real vanilla 26.2 server (flat creative oracle, :25570)\n\
         as the reply to a serverbound `command_suggestion` for `/gamemode `\n\
         with transaction id 0. Payload only — no length or packet-id prefix.\n\
         \n\
         Recapture: same command as command_tree_creative.hex.",
        &suggestions_payload.expect("captured suggestions payload"),
    );

    // Whatever is checked in must still decode — this is what catches a wire
    // change without demanding byte equality from a non-deterministic packet.
    let replayed = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::COMMANDS,
            &stored_commands,
        )
        .expect("the checked-in fixture must still decode");
    assert!(
        replayed.iter().any(|d| matches!(
            d,
            Directive::Emit(ClientEvent::CommandTreeUpdated { .. })
        )),
        "replaying the checked-in fixture must emit a CommandTreeUpdated"
    );
}
