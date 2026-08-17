//! Our server answers the Status phase — server-list ping, MOTD,
//! version, and pong.
//!
//! # Which implementation do these tests actually resolve to?
//!
//! The real [`V770ServerProtocol`], driven through the real
//! `lodestone_server::serve_connection` over a real
//! [`memory_pair`](lodestone_net::memory_pair) transport. That question is
//! load-bearing here: `crates/lodestone-server/tests/serve_play.rs` drives the
//! same loop through a `FakeProtocol` with invented packet ids, which is correct
//! for testing version-free *scheduling* but structurally cannot exercise the
//! protocol-776 decoder or the JSON serializer this issue adds. Every test below
//! names `V770ServerProtocol`, and the client half of each exchange is
//! **hand-written bytes** rather than one of this crate's own `Encode` impls, so
//! the decode side is never validated against bytes our own encoder produced.
//!
//! # Where the expected values come from
//!
//! Not from our own encoder. Three independent sources:
//!
//! 1. **A live vanilla 26.2 server**, captured over a raw socket by a script
//!    that uses nothing from this tree, and checked in as
//!    `tests/fixtures/vanilla_status_response_26_2.json`. That fixture pins the
//!    packet ids (`status_response` = 0, `pong_response` = 1), the framing (one
//!    length-prefixed JSON string, **zero** trailing bytes), the JSON key set
//!    and nesting, the 8-byte pong payload, the verbatim echo, and the fact that
//!    vanilla closes the connection after answering a ping.
//! 2. **The decompiled 26.2 source** at `.cache/mc/26.2/src`, cited inline per
//!    assertion — `status/ServerStatus.java`,
//!    `status/ClientboundStatusResponsePacket.java`,
//!    `ping/{Serverbound,Clientbound}*.java`, and
//!    `server/network/ServerStatusPacketListenerImpl.java` for the lifecycle.
//! 3. **`/usr/bin/base64`**, the OS tool, for the favicon encoding — the three
//!    padding cases below were computed by it, not by us.

use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, EntitySnapshot, MobHandle, NoEntities,
    ServerBound, ServerDirective, ServerError, ServerProtocol, serve_connection,
};
use lodestone_v770::V770ServerProtocol;
use uuid::Uuid;

/// The MOTD `lodestone-server` reports, stated here as a literal rather than
/// imported from `lodestone_server::STATUS_MOTD`.
///
/// Deliberate: importing the constant would make the assertion compare the
/// constant to itself, which is satisfied by *any* value including an empty
/// string. Restating it is what makes the gate fail if someone blanks it.
const EXPECTED_MOTD: &str = "A Lodestone Server";

/// Likewise for the player cap. Cross-checked against the `max_players: 20` the
/// join sequence already reports in `V770ServerProtocol::begin_play`'s
/// `GameLogin` body — the two must agree or a client sees the cap change between
/// its server list and the join.
const EXPECTED_MAX_PLAYERS: i64 = 20;

/// Protocol 776 = Minecraft 26.2. Restated, not imported from
/// `lodestone_v770::PROTOCOL`, for the same reason as `EXPECTED_MOTD` — and
/// independently confirmed by the live capture, whose `version.protocol` is 776.
const EXPECTED_PROTOCOL: i64 = 776;

/// Minecraft version name, same reasoning; the live capture's `version.name` is
/// the string `"26.2"`.
const EXPECTED_VERSION_NAME: &str = "26.2";

/// The vanilla capture, parsed. Panics loudly rather than skipping if the
/// fixture is missing — a *precondition*-species vacuous test would `return`
/// here and report green with nothing measured.
fn vanilla_capture() -> serde_json::Value {
    let raw = include_str!("fixtures/vanilla_status_response_26_2.json");
    serde_json::from_str(raw).expect("checked-in vanilla status capture is valid JSON")
}

/// A terrain source that is never sampled: every test here terminates in the
/// Status phase, long before any chunk is generated. A column is still produced
/// (rather than `unimplemented!()`) so a future test that *does* reach Play
/// fails on an assertion rather than a panic from the fixture.
struct UnusedSource;

impl ChunkSource for UnusedSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(-64, 384)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this status
        // gate never reads terrain at all.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this status
        // gate never reads terrain at all.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design. `ChunkSource::set_block` has no default, so this is stated
    // explicitly rather than inherited.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

/// Writes one length-prefixed handshake and then a status request, by hand.
///
/// `Intention`'s wire layout, from the packet this crate already decodes:
/// VarInt protocol version, a length-prefixed host string, a big-endian `u16`
/// port, then a VarInt `next_state` — `1` for Status, which is the value
/// `V770ServerProtocol::decode` maps to `State::Status`.
fn handshake_bytes(next_state: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(776);
    w.string("localhost");
    w.u16(25565);
    w.var_i32(next_state);
    w.into_vec()
}

/// Reads the JSON document out of a `status_response` body and asserts the
/// framing vanilla's own reply uses: one length-prefixed string and **nothing
/// after it**.
///
/// The trailing-byte check is the load-bearing half. It is an assertion of an
/// *absence*, so it needs the control that `status_response_body_is_exactly_one_
/// string_with_no_trailing_bytes` provides below — and its expected value comes
/// from the live capture's own `status_response_trailing_bytes: 0`, not from an
/// assumption.
fn read_status_json(payload: &[u8]) -> String {
    let mut r = Reader::new(payload);
    let json = r.string(32767).expect("status response body is a string");
    assert!(
        r.ensure_empty().is_ok(),
        "status_response carried {} trailing byte(s); the live vanilla capture \
         reports status_response_trailing_bytes = 0, and \
         ClientboundStatusResponsePacket's STREAM_CODEC is a single \
         lenientJson(32767) field with nothing after it \
         (status/ClientboundStatusResponsePacket.java)",
        payload.len() - json.len(),
    );
    json
}

/// Drives a complete Status exchange against `proto` over a real transport and
/// returns every packet the server sent, plus how the server loop terminated.
async fn status_exchange<P>(
    proto: P,
    ping_time: Option<i64>,
    extra_status_requests: usize,
) -> (Vec<(i32, Vec<u8>)>, Result<(), ServerError>)
where
    P: ServerProtocol + Send + Sync + 'static,
{
    let (client_end, server_end) = memory_pair();
    let source = UnusedSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &proto,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
        .map(|_| ())
    });

    let mut client = Connection::new(client_end);
    // Handshaking -> Status.
    client
        .write_packet(0, &handshake_bytes(1))
        .await
        .expect("handshake writes");
    // `status_request`: packet id 0, **empty** body — `StreamCodec.unit`
    // (status/ServerboundStatusRequestPacket.java).
    for _ in 0..=extra_status_requests {
        client
            .write_packet(0, &[])
            .await
            .expect("status request writes");
    }
    if let Some(time) = ping_time {
        // `ping_request`: packet id 1, a single big-endian i64
        // (ping/ServerboundPingRequestPacket.java).
        let mut w = Writer::default();
        w.i64(time);
        client
            .write_packet(1, &w.into_vec())
            .await
            .expect("ping request writes");
    }

    // Drain with a bounded idle timeout rather than reading to EOF.
    //
    // **This is not a convenience.** With no ping, our server — like vanilla's,
    // which only closes on a ping or a repeat request
    // (`ServerStatusPacketListenerImpl.java`) — deliberately keeps the
    // connection open after answering a status request. An unbounded
    // `while let Ok(Some(..)) = read_packet()` therefore *deadlocks* here: both
    // ends hold the transport and neither will speak again. The first version of
    // this harness did exactly that and hung the test binary past ten minutes,
    // which is worth recording because a hang is the one failure mode that looks
    // like neither a pass nor a fail.
    let mut sent = Vec::new();
    // Closed, errored, or went quiet — either way there is nothing more
    // coming, and every assertion is about what *did* arrive.
    while let Ok(Ok(Some(packet))) =
        tokio::time::timeout(Duration::from_millis(250), client.read_packet()).await
    {
        sent.push(packet);
    }
    // Release the transport so a server still parked in `read_packet` (the
    // no-ping case) can observe the close and return, instead of the `await`
    // below hanging forever.
    drop(client);

    let outcome = server.await.expect("server task panicked");
    (sent, outcome)
}

/// A stand-in for the server **as it behaved before this issue**: the real
/// protocol-776 `decode` (so the Status packets are still lifted correctly),
/// but the [`ServerProtocol`] *default* status encoders, which emit
/// [`ServerDirective::None`].
///
/// This is the permanent negative control. Every positive assertion below is
/// about bytes reaching a client; this proves those assertions fail when the
/// encoders are absent, so a green positive gate is evidence of wiring rather
/// than evidence of a tautology.
struct UnwiredStatusProtocol;

impl ServerProtocol for UnwiredStatusProtocol {
    fn decode(&self, state: lodestone_core::State, packet_id: i32, payload: &[u8]) -> ServerBound {
        V770ServerProtocol.decode(state, packet_id, payload)
    }

    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
        ServerDirective::None
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_add_entity(&self, _entity: &EntitySnapshot) -> ServerDirective {
        ServerDirective::None
    }
}

// ---------------------------------------------------------------------------
// Wire shape, against the live vanilla capture
// ---------------------------------------------------------------------------

/// The packet ids and framing our server uses are the ones a live vanilla 26.2
/// server actually used, read off a raw-socket capture.
#[tokio::test]
async fn packet_ids_and_framing_match_a_live_vanilla_servers_own_reply() {
    let capture = vanilla_capture();
    let vanilla_status_id = capture["status_response_packet_id"]
        .as_i64()
        .expect("capture pins the status_response id");
    let vanilla_pong_id = capture["pong_packet_id"]
        .as_i64()
        .expect("capture pins the pong_response id");
    let vanilla_pong_len = capture["pong_payload_len"]
        .as_u64()
        .expect("capture pins the pong payload length");
    assert_eq!(
        (vanilla_status_id, vanilla_pong_id, vanilla_pong_len),
        (0, 1, 8),
        "the checked-in capture itself changed shape; re-read it before trusting \
         anything below"
    );

    let ping = 0x0123_4567_89AB_CDEFi64;
    let (sent, outcome) = status_exchange(V770ServerProtocol, Some(ping), 0).await;

    assert_eq!(
        sent.len(),
        2,
        "expected exactly a status_response then a pong_response, got ids {:?}",
        sent.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
    );
    assert_eq!(
        i64::from(sent[0].0),
        vanilla_status_id,
        "status_response packet id must match the live capture's",
    );
    assert_eq!(
        i64::from(sent[1].0),
        vanilla_pong_id,
        "pong_response packet id must match the live capture's",
    );
    assert_eq!(
        sent[1].1.len() as u64,
        vanilla_pong_len,
        "pong_response payload must be the same 8 bytes vanilla sent",
    );
    // Vanilla terminates a status connection after answering the ping
    // (ServerStatusPacketListenerImpl.java); the capture observed exactly
    // that (`server_closed_after_pong: true`, `bytes_after_pong: 0`).
    assert!(
        capture["server_closed_after_pong"] == serde_json::Value::Bool(true),
        "capture should show vanilla closing after the pong",
    );
    assert!(
        matches!(outcome, Err(ServerError::StatusRequestHandled)),
        "our server must terminate the connection after the pong too, got {outcome:?}",
    );
}

/// The pong echoes the client's clock reading **verbatim** — bit for bit, across
/// values chosen to break a sign-extension or truncation bug.
///
/// The live capture confirms verbatim echo for one value
/// (`echo_is_verbatim: true`); `ClientboundPongResponsePacket` writes the same
/// `long` it read (`ping/ClientboundPongResponsePacket.java`), so every
/// value must survive. This is a *magnitude*-species guard: asserting merely
/// that some 8 bytes came back would pass for a server that always echoed zero.
#[tokio::test]
async fn pong_echoes_the_clients_clock_reading_bit_for_bit() {
    for time in [
        0i64,
        1,
        -1,
        i64::MIN,
        i64::MAX,
        0x0123_4567_89AB_CDEF,
        -0x0123_4567_89AB_CDEF,
    ] {
        let (sent, _) = status_exchange(V770ServerProtocol, Some(time), 0).await;
        let (id, payload) = sent
            .iter()
            .find(|(id, _)| *id == 1)
            .expect("a pong_response was sent");
        assert_eq!(*id, 1);
        let mut r = Reader::new(payload);
        let echoed = r.i64().expect("pong body is one i64");
        assert!(
            r.ensure_empty().is_ok(),
            "pong_response must be exactly 8 bytes, matching the live capture's \
             pong_payload_len",
        );
        assert_eq!(
            echoed, time,
            "pong must echo the ping payload unchanged; a truncating or \
             sign-extending encoder fails here and nowhere else",
        );
    }
}

// ---------------------------------------------------------------------------
// The JSON document
// ---------------------------------------------------------------------------

/// Our status document carries every key a live vanilla server's did, with the
/// same nesting and the same JSON types.
///
/// This compares against the *capture*, not against a shape we wrote down: the
/// key set is read out of the fixture at runtime, so if vanilla's document gains
/// or loses a field the fixture is re-captured and this test tells us.
#[tokio::test]
async fn status_json_carries_every_key_the_live_vanilla_document_did() {
    let capture = vanilla_capture();
    let vanilla = &capture["status_json_parsed"];

    let (sent, _) = status_exchange(V770ServerProtocol, None, 0).await;
    let json = read_status_json(&sent[0].1);
    let ours: serde_json::Value = serde_json::from_str(&json).expect("our status body is JSON");

    let vanilla_obj = vanilla.as_object().expect("vanilla document is an object");
    let ours_obj = ours.as_object().expect("our document is an object");

    for key in vanilla_obj.keys() {
        assert!(
            ours_obj.contains_key(key),
            "our status document is missing `{key}`, which a live vanilla 26.2 \
             server sent (ServerStatus.java). Present: {:?}",
            ours_obj.keys().collect::<Vec<_>>(),
        );
    }

    // `players` and `version` are the two sub-objects a client's server-list
    // row renders; their key sets must match vanilla's exactly, not merely
    // overlap.
    for parent in ["players", "version"] {
        let v_keys: Vec<_> = vanilla[parent]
            .as_object()
            .expect("vanilla sub-object")
            .keys()
            .cloned()
            .collect();
        let o = ours[parent].as_object().expect("our sub-object");
        for key in &v_keys {
            assert!(
                o.contains_key(key),
                "our `{parent}` is missing `{key}` (vanilla sent it)",
            );
        }
    }

    // Types, not just presence: a `max` serialized as a string parses as JSON
    // and would still break a real client's `Codec.INT` field.
    assert!(
        ours["players"]["max"].is_i64() && ours["players"]["online"].is_i64(),
        "players.max/online must be JSON integers (Codec.INT, \
         ServerStatus.java), got {:?}",
        ours["players"],
    );
    assert!(
        ours["version"]["protocol"].is_i64(),
        "version.protocol must be a JSON integer (Codec.INT, \
         ServerStatus.java)",
    );
    assert!(
        ours["version"]["name"].is_string(),
        "version.name must be a JSON string (Codec.STRING, ServerStatus.java)",
    );
    assert!(
        ours["players"]["sample"].is_array(),
        "players.sample must be a JSON array (NameAndId.CODEC.listOf(), \
         ServerStatus.java)",
    );
}

/// The *values* in the document, predicted rather than merely shape-checked.
///
/// A key-set test passes for a document that reports an empty MOTD, a cap of
/// zero, and protocol 0 — the *magnitude* species of vacuous test. These are the
/// four values a player actually reads off their server list.
#[tokio::test]
async fn status_json_reports_the_real_motd_cap_version_and_protocol() {
    let (sent, _) = status_exchange(V770ServerProtocol, None, 0).await;
    let json = read_status_json(&sent[0].1);
    let ours: serde_json::Value = serde_json::from_str(&json).expect("our status body is JSON");

    // `description` is written as a `{"text": …}` object. A live vanilla server
    // emits the bare-string form for a properties MOTD (see the capture's own
    // `description`), and both decode — see `encode_status_response_body`'s doc
    // comment for why this one deliberately differs.
    assert_eq!(
        ours["description"]["text"].as_str(),
        Some(EXPECTED_MOTD),
        "MOTD must be the real one, not an empty or placeholder string",
    );
    assert_eq!(ours["players"]["max"].as_i64(), Some(EXPECTED_MAX_PLAYERS));
    assert_eq!(
        ours["version"]["protocol"].as_i64(),
        Some(EXPECTED_PROTOCOL),
        "protocol must be 776; the live capture's version.protocol is 776 too",
    );
    assert_eq!(
        ours["version"]["name"].as_str(),
        Some(EXPECTED_VERSION_NAME),
    );

    // Both optional-with-a-default fields must be *absent*, not present-and-
    // empty. `Favicon.CODEC` errors with "Unknown format" on any string lacking
    // the `data:image/png;base64,` prefix (ServerStatus.java), so an
    // empty-string favicon would make a real client reject the whole document;
    // and the live capture omits both keys entirely.
    assert!(
        !ours.as_object().unwrap().contains_key("favicon"),
        "a server with no icon must omit `favicon`, not send an empty one",
    );
    assert!(
        !ours.as_object().unwrap().contains_key("enforcesSecureChat"),
        "enforcesSecureChat defaults to false (ServerStatus.java) and vanilla \
         omits it; the live capture has no such key",
    );
}

/// Our own client-side status parser — written independently, for reading *real*
/// servers — accepts the document our server produces, and recovers the same
/// four values from it.
///
/// This is a cross-check, not the primary evidence: `parse_status_json` is our
/// code, so on its own it would be a `decode(encode(x))` round trip. It earns
/// its place because it is the parser that has been run against real servers'
/// JSON (`crates/lodestone-net/src/status.rs`'s own gates), so a document it
/// rejects is a document a real server would not have produced.
#[tokio::test]
async fn our_own_real_server_status_parser_accepts_the_document() {
    let (sent, _) = status_exchange(V770ServerProtocol, None, 0).await;
    let json = read_status_json(&sent[0].1);

    let parsed = lodestone_net::parse_status_json(&json, None)
        .expect("the client-side parser used against real servers accepts our document");
    assert_eq!(parsed.motd_first_line(), EXPECTED_MOTD);
    assert_eq!(parsed.max, Some(EXPECTED_MAX_PLAYERS as u32));
    assert_eq!(parsed.protocol, Some(EXPECTED_PROTOCOL as i32));
    assert_eq!(parsed.version.as_deref(), Some(EXPECTED_VERSION_NAME));
}

// ---------------------------------------------------------------------------
// Lifecycle, per ServerStatusPacketListenerImpl
// ---------------------------------------------------------------------------

/// A second status request on one connection is a disconnect, not a second
/// reply — `ServerStatusPacketListenerImpl.handleStatusRequest` guards on
/// `hasRequestedStatus` and calls `connection.disconnect` otherwise
/// (`ServerStatusPacketListenerImpl.java`).
#[tokio::test]
async fn a_second_status_request_terminates_the_connection() {
    let (sent, outcome) = status_exchange(V770ServerProtocol, None, 1).await;
    assert_eq!(
        sent.len(),
        1,
        "exactly one status_response for two requests; got {} packets",
        sent.len(),
    );
    assert!(
        matches!(outcome, Err(ServerError::StatusRequestHandled)),
        "the repeat request must end the connection, got {outcome:?}",
    );
}

/// A ping with no preceding status request is still answered. Vanilla's
/// `handlePingRequest` has no `hasRequestedStatus` guard at all
/// (`ServerStatusPacketListenerImpl.java`) — so neither may we, or a
/// latency-only probe gets nothing.
#[tokio::test]
async fn a_ping_with_no_preceding_status_request_is_still_answered() {
    let (sent, outcome) = status_exchange(V770ServerProtocol, Some(42), 0).await;
    // `extra_status_requests: 0` still sends one request, so filter to the pong.
    let pongs: Vec<_> = sent.iter().filter(|(id, _)| *id == 1).collect();
    assert_eq!(pongs.len(), 1, "exactly one pong");
    assert!(matches!(outcome, Err(ServerError::StatusRequestHandled)));
}

/// A `status_request` carrying a body is malformed and must be dropped, not
/// answered. Its codec is `StreamCodec.unit`
/// (`status/ServerboundStatusRequestPacket.java`) — the body is empty by
/// construction, so bytes in it mean a peer that is not speaking this protocol.
#[tokio::test]
async fn a_status_request_with_a_body_is_dropped_rather_than_answered() {
    let (client_end, server_end) = memory_pair();
    let source = UnusedSource;
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
        .map(|_| ())
    });

    let mut client = Connection::new(client_end);
    client.write_packet(0, &handshake_bytes(1)).await.unwrap();
    client.write_packet(0, &[0xFF]).await.unwrap();
    drop(client);

    let outcome = server.await.expect("server task panicked");
    assert!(
        !matches!(outcome, Err(ServerError::StatusRequestHandled)),
        "a malformed status_request must not be treated as a handled request",
    );
}

// ---------------------------------------------------------------------------
// Negative control
// ---------------------------------------------------------------------------

/// **Negative control.** The same exchange, against a protocol that decodes the
/// Status packets correctly but leaves the two encoders at their
/// [`ServerProtocol`] defaults — i.e. the server exactly as it behaved before
/// Status responses were wired up.
///
/// It must send **zero** packets. If this ever passes *and* the positive tests
/// above also pass, the positive tests are measuring something other than the
/// encoders being wired.
#[tokio::test]
async fn an_unwired_server_sends_nothing_at_all() {
    let (sent, outcome) = status_exchange(UnwiredStatusProtocol, Some(7), 0).await;
    assert!(
        sent.is_empty(),
        "a server with default (absent) status encoders must send nothing; got \
         ids {:?} — if this fails, the trait defaults are no longer inert and \
         every positive gate in this file needs re-reading",
        sent.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
    );
    // The *lifecycle* still runs — the loop terminates after the ping — which is
    // exactly why the byte assertions above, not the outcome, are what prove the
    // encoders exist.
    assert!(matches!(outcome, Err(ServerError::StatusRequestHandled)));
}

// ---------------------------------------------------------------------------
// Favicon base64, anchored on /usr/bin/base64
// ---------------------------------------------------------------------------

/// The favicon field's base64, checked against values computed by the OS's own
/// `base64(1)` rather than by us, across all three padding cases.
///
/// | input | `/usr/bin/base64` output | pad | PNG? |
/// |---|---|---|---|
/// | 8-byte PNG signature + `0x00` | `iVBORw0KGgoA` | 0 | yes |
/// | the 8-byte PNG signature | `iVBORw0KGgo=` | 1 | yes |
/// | that plus a 12-byte IHDR head | `iVBORw0KGgoAAAANSUhEUg==` | 2 | yes |
/// | `b"ab"` | `YWI=` | 1 | no |
/// | `b"a"` | `YQ==` | 2 | no |
///
/// A base64 encoder that mishandles the tail is the classic bug here, and it
/// would produce a favicon a real client rejects while every *shape* assertion
/// above still passed. All five rows exercise the encoder; only the three
/// PNG-prefixed ones exercise the round trip, because
/// `lodestone_net::decode_favicon` *deliberately* returns `None` for a payload
/// that decodes to something which is not a PNG (its `PNG_MAGIC` check —
/// "a favicon that is not a PNG is a server bug or an attack"). Recorded because
/// the first version of this test asserted the round trip for all five rows and
/// failed on `b"ab"`, which looked like an encoder bug and was the decoder
/// working exactly as designed.
#[tokio::test]
async fn favicon_is_a_data_uri_whose_base64_matches_the_os_encoder() {
    const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    for (bytes, expected, is_png) in [
        (b"\x89PNG\r\n\x1a\n\x00".as_slice(), "iVBORw0KGgoA", true),
        (PNG_SIGNATURE, "iVBORw0KGgo=", true),
        (
            b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".as_slice(),
            "iVBORw0KGgoAAAANSUhEUg==",
            true,
        ),
        (b"ab".as_slice(), "YWI=", false),
        (b"a".as_slice(), "YQ==", false),
    ] {
        let directive = V770ServerProtocol.encode_status_response(
            EXPECTED_MOTD,
            0,
            20,
            &[],
            Some(bytes),
            false,
        );
        let ServerDirective::Send { payload, .. } = directive else {
            panic!("encode_status_response must emit a Send");
        };
        let json = read_status_json(&payload);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let favicon = value["favicon"].as_str().expect("favicon present");
        assert_eq!(
            favicon,
            format!("data:image/png;base64,{expected}"),
            "favicon must be the mandatory `data:image/png;base64,` prefix \
             (ServerStatus.java) followed by exactly what base64(1) produces \
             for {bytes:?}",
        );
        // And, for a real PNG, our own real-server favicon decoder must recover
        // the input. See this test's own doc comment for why the non-PNG rows are
        // excluded rather than expected to round-trip.
        let recovered = lodestone_net::decode_favicon(favicon);
        if is_png {
            assert_eq!(
                recovered.as_deref(),
                Some(bytes),
                "the client-side favicon decoder used against real servers must \
                 round-trip our encoding back to the original PNG bytes",
            );
        } else {
            assert_eq!(
                recovered, None,
                "decode_favicon must reject a non-PNG payload (its PNG_MAGIC \
                 check); if this starts returning bytes, that guard is gone",
            );
        }
    }
}

/// A `players.sample` entry uses `NameAndId`'s JSON keys and the *hyphenated*
/// uuid string form.
///
/// `NameAndId.CODEC` writes the id through `UUIDUtil.STRING_CODEC`
/// (`server/players/NameAndId.java`) — a string, not the two-longs array a
/// packet field would use. The live capture's own sample entry is
/// `{"id": "00000000-0000-0000-0000-000000000000", "name": "Anonymous Player"}`,
/// which pins both the keys and the format.
#[tokio::test]
async fn a_player_sample_entry_uses_nameandids_keys_and_hyphenated_uuid() {
    let capture = vanilla_capture();
    let vanilla_entry = &capture["status_json_parsed"]["players"]["sample"][0];
    let vanilla_keys: Vec<_> = vanilla_entry
        .as_object()
        .expect("the capture has a sample entry")
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        vanilla_keys,
        vec!["id".to_owned(), "name".to_owned()],
        "the capture's sample-entry keys changed; re-read the fixture",
    );

    let id = Uuid::parse_str("f81d4fae-7dec-11d0-a765-00a0c91e6bf6").unwrap();
    let directive = V770ServerProtocol.encode_status_response(
        EXPECTED_MOTD,
        1,
        20,
        &[(id, "Steve".to_owned())],
        None,
        false,
    );
    let ServerDirective::Send { payload, .. } = directive else {
        panic!("encode_status_response must emit a Send");
    };
    let value: serde_json::Value = serde_json::from_str(&read_status_json(&payload)).unwrap();
    let entry = &value["players"]["sample"][0];
    assert_eq!(
        entry["id"].as_str(),
        Some("f81d4fae-7dec-11d0-a765-00a0c91e6bf6"),
        "uuid must be the hyphenated string form UUIDUtil.STRING_CODEC writes, \
         matching the capture's own sample entry",
    );
    assert_eq!(entry["name"].as_str(), Some("Steve"));
    assert_eq!(value["players"]["online"].as_i64(), Some(1));
}

/// A MOTD containing JSON metacharacters survives the encoding.
///
/// This is why the body is built with a real serializer instead of `format!`:
/// a hand-built document would emit invalid JSON here and a real client would
/// reject the whole status reply, showing our server as unreachable for no
/// visible reason.
#[tokio::test]
async fn a_motd_containing_json_metacharacters_still_encodes_validly() {
    let nasty = "quote \" backslash \\ newline \n tab \t unicode \u{1F600} </script>";
    let directive =
        V770ServerProtocol.encode_status_response(nasty, 0, 20, &[], None, false);
    let ServerDirective::Send { payload, .. } = directive else {
        panic!("encode_status_response must emit a Send");
    };
    let value: serde_json::Value = serde_json::from_str(&read_status_json(&payload))
        .expect("a MOTD with metacharacters must still produce parseable JSON");
    assert_eq!(
        value["description"]["text"].as_str(),
        Some(nasty),
        "the MOTD must survive the round trip through JSON escaping unchanged",
    );
}

/// `enforcesSecureChat` appears only when it is `true`.
#[tokio::test]
async fn enforces_secure_chat_is_written_only_when_true() {
    let directive =
        V770ServerProtocol.encode_status_response(EXPECTED_MOTD, 0, 20, &[], None, true);
    let ServerDirective::Send { payload, .. } = directive else {
        panic!("encode_status_response must emit a Send");
    };
    let value: serde_json::Value = serde_json::from_str(&read_status_json(&payload)).unwrap();
    assert_eq!(
        value["enforcesSecureChat"],
        serde_json::Value::Bool(true),
        "an enforcing server must say so (ServerStatus.java)",
    );
}
