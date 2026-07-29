//! Live capture + acceptance gate for a dropped item's *identity*.
//!
//! Gated behind the `live-item-entity` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run it against the real vanilla 26.2 survival
//! server (`lodestone-survival`, game `127.0.0.1:25565`, RCON `:25566`,
//! password `lodestone`) with:
//!
//! ```text
//! cargo test -p lodestone-v770 --features live-item-entity \
//!     --test live_item_entity_metadata -- --ignored --nocapture
//! ```
//!
//! # What this gate is for
//!
//! A server-spawned `minecraft:item` streams to the client fine, but until the
//! `ITEM_STACK` metadata serializer was decoded it arrived with no idea what it
//! was. This joins the real server, `/summon`s a drop, and captures the **raw
//! `set_entity_data` payload the server authored** — then feeds exactly those
//! bytes through the adapter and asserts the item comes out.
//!
//! # Why the bytes are checked in
//!
//! `HANDOFF.md` records the rule: an expected value must originate outside the
//! code under test. Validating a decoder against bytes our own encoder produced
//! closes perfectly over a shared misunderstanding — hermetic chunk fixtures
//! passed that way and a live gate then produced 49 "unexpected end of input".
//! So the captured payloads are written to `tests/fixtures/` and the hermetic
//! sibling ([`item_entity_metadata`]) replays them with no server. This gate
//! re-captures and asserts the fixtures still match reality, so a server-side
//! wire change fails here rather than rotting silently.
//!
//! Set `LODESTONE_CAPTURE_FIXTURES=1` to rewrite the fixtures from this run.
//!
//! Per §12.52 this fails rather than skips when it cannot run.

#![cfg(feature = "live-item-entity")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, LoginProfile, Reported, ServerAddress, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_testsupport::{RconClient, unique_username};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

const SERVER_ADDR: &str = "127.0.0.1:25565";
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";

/// The command that repairs the missing precondition, printed when the server
/// cannot be reached so the gate fails loudly rather than skipping.
const REPAIR: &str = "recreate the survival oracle with: ./scripts/live-oracles/survival.sh \
    (expected a vanilla 26.2 survival server on 127.0.0.1:25565, RCON :25566)";

/// `PickupDelay` high enough that the joined player can never absorb the drop
/// before we have read its metadata, and an `Age` far enough from the 6000-tick
/// despawn that it survives the read.
const KEEP_ALIVE_TAGS: &str = "PickupDelay:32767s,Age:-32768s";

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Renders a captured payload as the reviewable hex text the fixtures use:
/// a provenance header, then 16 bytes per line.
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

/// Parses the hex-text fixture format back into bytes. Kept in step with the
/// hermetic sibling's reader by construction — both are two lines long.
fn from_hex_fixture(text: &str) -> Vec<u8> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).expect("fixture hex byte"))
        .collect()
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

/// Acknowledges a finished chunk batch so vanilla keeps streaming.
async fn ack_chunk_batch(conn: &mut Connection<TcpStream>) {
    let mut w = Writer::default();
    w.f32(32.0);
    conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &w.into_vec())
        .await
        .expect("ack chunk batch");
}

struct LiveSession {
    conn: Connection<TcpStream>,
    state: ConnectionState,
    world: World,
    adapter: V770Adapter,
    rcon: RconClient,
    username: String,
}

impl LiveSession {
    /// Joins the server and drives the handshake until the world is streaming,
    /// so the player entity exists server-side before anything is summoned.
    async fn join() -> Self {
        let server = ServerAddress {
            host: "127.0.0.1".into(),
            port: 25565,
        };
        let username = unique_username();
        let profile = LoginProfile {
            username: username.clone(),
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

        let mut world = World::new();
        let joined = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let (packet_id, payload) = match conn.read_packet().await {
                    Ok(Some(p)) => p,
                    Ok(None) => return false,
                    Err(err) => panic!("read error during join: {err}"),
                };
                if state == ConnectionState::Play
                    && packet_id == play::clientbound::CHUNK_BATCH_FINISHED
                {
                    ack_chunk_batch(&mut conn).await;
                    return true;
                }
                if let Ok(directives) =
                    adapter.handle_packet(&mut world, state, packet_id, &payload)
                {
                    for directive in directives {
                        apply(&mut conn, &mut state, directive).await;
                    }
                }
            }
        })
        .await
        .expect("timed out joining the world");
        assert!(joined, "connection closed before reaching Play");

        let rcon = RconClient::connect(RCON_ADDR, RCON_PASSWORD)
            .unwrap_or_else(|err| panic!("could not reach RCON {RCON_ADDR}: {err}. {REPAIR}"));

        Self {
            conn,
            state,
            world,
            adapter,
            rcon,
            username,
        }
    }

    /// Summons a drop carrying `item_snbt` at the joined player, then reads real
    /// packets until the matching `set_entity_data` arrives, returning its raw
    /// payload exactly as the server wrote it.
    ///
    /// The correlation is done on the *packet* stream, not on our own decode:
    /// `ADD_ENTITY` tells us which id is a `minecraft:item`, and the payload is
    /// stashed verbatim before the adapter ever sees it. That keeps the captured
    /// bytes independent of the code under test.
    async fn capture_item_metadata(&mut self, item_snbt: &str) -> (i32, Vec<u8>) {
        let summon = format!(
            "execute at {} run summon item ~ ~ ~ {{{KEEP_ALIVE_TAGS},Item:{item_snbt}}}",
            self.username
        );
        let reply = self.rcon.cmd(&summon);
        println!("rcon summon reply: {reply}");
        let lower = reply.to_lowercase();
        assert!(
            !lower.contains("incorrect")
                && !lower.contains("unknown")
                && !lower.contains("expected")
                && !lower.contains("error"),
            "server rejected the summon, so the SNBT is wrong: {reply}"
        );

        let mut item_ids: Vec<i32> = Vec::new();
        let mut payloads: HashMap<i32, Vec<u8>> = HashMap::new();

        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let (packet_id, payload) = match self.conn.read_packet().await {
                    Ok(Some(p)) => p,
                    Ok(None) => panic!("connection closed before the drop's metadata arrived"),
                    Err(err) => panic!("read error after summon: {err}"),
                };
                if self.state == ConnectionState::Play
                    && packet_id == play::clientbound::CHUNK_BATCH_FINISHED
                {
                    ack_chunk_batch(&mut self.conn).await;
                    continue;
                }
                // Stash the untouched payload *before* the adapter runs.
                if self.state == ConnectionState::Play
                    && packet_id == play::clientbound::SET_ENTITY_DATA
                {
                    let mut peek = Reader::new(&payload);
                    if let Ok(id) = peek.var_i32() {
                        payloads.insert(id, payload.clone());
                    }
                }
                let directives = self
                    .adapter
                    .handle_packet(&mut self.world, self.state, packet_id, &payload)
                    .unwrap_or_else(|err| {
                        panic!(
                            "adapter failed to decode a real server packet (id {packet_id}): \
                             {err} — an item entity must never be fatal"
                        )
                    });
                for directive in &directives {
                    if let Directive::Emit(ClientEvent::EntitySpawned {
                        entity_id,
                        entity_type,
                        ..
                    }) = directive
                        && entity_type.to_string() == "minecraft:item"
                    {
                        item_ids.push(*entity_id);
                    }
                }
                for directive in directives {
                    apply(&mut self.conn, &mut self.state, directive).await;
                }
                if let Some(id) = item_ids.iter().rev().find(|id| payloads.contains_key(id)) {
                    return (*id, payloads.remove(id).expect("stashed payload"));
                }
            }
        })
        .await
        .expect("timed out waiting for the drop's set_entity_data")
    }

    /// Reads one more real packet, proving the session survived the decode.
    async fn assert_still_alive(mut self) {
        let alive = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                match self.conn.read_packet().await {
                    Ok(Some((packet_id, payload))) => {
                        if self.state == ConnectionState::Play
                            && packet_id == play::clientbound::CHUNK_BATCH_FINISHED
                        {
                            ack_chunk_batch(&mut self.conn).await;
                        }
                        let _ = self.adapter.handle_packet(
                            &mut self.world,
                            self.state,
                            packet_id,
                            &payload,
                        );
                        return true;
                    }
                    Ok(None) => return false,
                    Err(err) => panic!("read error proving liveness: {err}"),
                }
            }
        })
        .await
        .expect("timed out proving the session survived");
        assert!(alive, "session must survive decoding an item entity");
    }
}

/// Compares the freshly captured payload against the checked-in fixture,
/// rewriting it when `LODESTONE_CAPTURE_FIXTURES=1`.
///
/// Entity ids are session-scoped, so the leading VarInt is expected to differ;
/// only the metadata list itself is compared byte-for-byte.
fn reconcile_fixture(name: &str, header: &str, captured: &[u8]) {
    let path = fixture_path(name);
    if std::env::var("LODESTONE_CAPTURE_FIXTURES").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().expect("fixture dir")).expect("create fixture dir");
        std::fs::write(&path, to_hex_fixture(header, captured)).expect("write fixture");
        println!("wrote fixture {}", path.display());
        return;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "missing fixture {}: {err} — re-capture with LODESTONE_CAPTURE_FIXTURES=1",
            path.display()
        )
    });
    let expected = from_hex_fixture(&text);

    let tail = |bytes: &[u8]| -> Vec<u8> {
        let mut reader = Reader::new(bytes);
        reader.var_i32().expect("entity id");
        bytes[reader.position()..].to_vec()
    };
    assert_eq!(
        tail(&expected),
        tail(captured),
        "the server's metadata list no longer matches the checked-in fixture {name}; \
         the wire changed — re-capture with LODESTONE_CAPTURE_FIXTURES=1 and re-review",
    );
}

/// Runs the captured payload back through the public adapter seam, exactly as
/// the live client does, and returns the metadata event it raised.
fn replay(payload: &[u8]) -> lodestone_model::EntityMetadataUpdate {
    let adapter = V770Adapter::new();
    let mut world = World::new();
    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::SET_ENTITY_DATA,
            payload,
        )
        .expect("replaying a real set_entity_data must not error");
    directives
        .into_iter()
        .find_map(|d| match d {
            Directive::Emit(ClientEvent::EntityMetadataUpdated { metadata, .. }) => Some(metadata),
            _ => None,
        })
        .expect("a drop's metadata packet must raise EntityMetadataUpdated")
}

/// The positive gate: the server's own bytes for a plain diamond drop decode to
/// that item, and the session lives.
#[tokio::test]
#[ignore = "requires the live 26.2 survival oracle (scripts/live-oracles/survival.sh)"]
async fn dropped_item_metadata_carries_its_item() {
    let mut session = LiveSession::join().await;
    let (entity_id, payload) = session
        .capture_item_metadata(r#"{id:"minecraft:diamond",count:1}"#)
        .await;
    println!("captured set_entity_data for item entity {entity_id}: {payload:02x?}");

    reconcile_fixture(
        "item_entity_metadata_diamond.hex",
        "Captured from vanilla 26.2 (protocol 776) survival oracle.\n\
         Packet: clientbound set_entity_data for a `/summon item` drop of\n\
         {id:\"minecraft:diamond\",count:1} with no data components.\n\
         \n\
         ..        VarInt entity id (session-scoped; not compared)\n\
         08        metadata index 8 = ItemEntity.DATA_ITEM\n\
         07        serializer 7 = ITEM_STACK\n\
         01        VarInt stack count = 1\n\
         9e 07     VarInt item registry id 926 = minecraft:diamond\n\
         00        DataComponentPatch: 0 added\n\
         00        DataComponentPatch: 0 removed\n\
         ff        end-of-list sentinel\n\
         \n\
         Note there is no field after the item: a dropped item's whole\n\
         identity is this one index.",
        &payload,
    );

    let metadata = replay(&payload);
    let stack = match metadata.item.clone() {
        Reported::Unreported => panic!("the drop's metadata must carry the item field"),
        Reported::Reported(None) => panic!("a summoned drop is never the empty stack"),
        Reported::Reported(Some(stack)) => stack,
    };
    assert_eq!(stack.item.to_string(), "minecraft:diamond");
    assert_eq!(stack.count, 1);
    assert!(
        !stack.components.has_unmodeled,
        "a plain diamond carries no components, so nothing may be flagged partial"
    );

    session.assert_still_alive().await;
}

/// The tolerance gate: a drop whose stack carries an **unmodeled** data
/// component (`minecraft:repair_cost`) still yields the item's identity, is
/// flagged partial, and never becomes an error. This is the fail-open property
/// the old fail-closed component decode destroyed — an unrecognised component is
/// a degraded item, never an outage.
#[tokio::test]
#[ignore = "requires the live 26.2 survival oracle (scripts/live-oracles/survival.sh)"]
async fn unmodeled_component_still_yields_the_item() {
    let mut session = LiveSession::join().await;
    let (entity_id, payload) = session
        .capture_item_metadata(
            r#"{id:"minecraft:diamond_pickaxe",count:1,components:{"minecraft:repair_cost":7}}"#,
        )
        .await;
    println!("captured partial-stack set_entity_data for entity {entity_id}: {payload:02x?}");

    reconcile_fixture(
        "item_entity_metadata_unmodeled_component.hex",
        "Captured from vanilla 26.2 (protocol 776) survival oracle.\n\
         Packet: clientbound set_entity_data for a `/summon item` drop of\n\
         {id:\"minecraft:diamond_pickaxe\",count:1,\n\
         components:{\"minecraft:repair_cost\":7}}.\n\
         \n\
         ..        VarInt entity id (session-scoped; not compared)\n\
         08        metadata index 8 = ItemEntity.DATA_ITEM\n\
         07        serializer 7 = ITEM_STACK\n\
         01        VarInt stack count = 1\n\
         c6 07     VarInt item registry id 966 = minecraft:diamond_pickaxe\n\
         01        DataComponentPatch: 1 added\n\
         00        DataComponentPatch: 0 removed\n\
         13        component type id 19 = minecraft:repair_cost  <-- UNMODELED\n\
         07        its payload, which we cannot skip and never read\n\
         ff        end-of-list sentinel, likewise never reached\n\
         \n\
         The last two bytes are deliberately NOT consumed: a clientbound\n\
         patch length-prefixes neither itself nor its components, so an\n\
         unmodeled one ends the decode in place. Reading on would take `07`\n\
         as a metadata index and `ff` as a truncated serializer VarInt — a\n\
         plausible-looking misparse, which is why the rest is abandoned.",
        &payload,
    );

    let metadata = replay(&payload);
    let stack = match metadata.item.clone() {
        Reported::Unreported => panic!("an unmodeled component must not cost us the item field"),
        Reported::Reported(None) => panic!("a summoned drop is never the empty stack"),
        Reported::Reported(Some(stack)) => stack,
    };
    assert_eq!(
        stack.item.to_string(),
        "minecraft:diamond_pickaxe",
        "the item key is decoded before any component, so it always survives"
    );
    assert_eq!(stack.count, 1);
    assert!(
        stack.components.has_unmodeled,
        "an unmodeled component must flag the stack as partial"
    );

    session.assert_still_alive().await;
}
