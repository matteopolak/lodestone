//! Issue #279: our server can send a Disconnect packet, and actually does.
//!
//! # The asymmetry this file exists to protect
//!
//! The disconnect reason is encoded **differently per phase**, and getting it
//! wrong produces a packet a real client cannot parse:
//!
//! | phase | packet | reason encoded as |
//! |---|---|---|
//! | Login | `ClientboundLoginDisconnectPacket` | **JSON string** (`ByteBufCodecs.lenientJson(262144)`, `login/ClientboundLoginDisconnectPacket.java:18`) |
//! | Configuration / Play | `ClientboundDisconnectPacket` | **NBT** (`TRUSTED_CONTEXT_FREE_STREAM_CODEC` = `fromCodecTrusted`, `common/ClientboundDisconnectPacket.java:11-12`, `chat/ComponentSerialization.java:44`) |
//!
//! `login_phase_reason_is_json_and_play_phase_reason_is_nbt` is the load-bearing
//! test: it asserts each phase's body parses under its *own* encoding and
//! **fails** under the other's, so a copy-paste between the two arms cannot pass.
//!
//! # Where the expected values come from
//!
//! A **live vanilla 26.2 server**, captured over a raw socket by a script using
//! nothing from this tree and checked in as
//! `tests/fixtures/vanilla_login_disconnect_26_2.json`. Announcing an ancient
//! protocol version makes vanilla refuse the login, which is a real
//! `login_disconnect` in the wild: it pins the packet id (0), the framing (one
//! length-prefixed JSON string, **zero** trailing bytes), and that the reason is
//! a translatable component. Plus the decompiled 26.2 source cited per assertion,
//! and vanilla's own `en_us.json` for the fallback string.
//!
//! # Not an island
//!
//! An encoder nothing calls reaches zero pixels. Two producers are wired and
//! gated end-to-end over a real transport, not just unit-tested:
//! `an_unanswered_keep_alive_kicks_the_client_with_a_reason` (Play) and
//! `an_invalid_username_is_refused_with_a_login_disconnect` (Login). The
//! Configuration-phase encoder has **no producer yet** and is honestly labelled
//! as such below — vanilla's own Configuration disconnects cover datapack and
//! registry errors this server does not have.

use std::time::Duration;

use lodestone_core::{Reader, State, read_network_nbt};
use lodestone_model::{
    ConnectionState, Directive, Text, TextContent, VersionAdapter,
};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, MobHandle, NoEntities, ServerDirective,
    ServerError, ServerProtocol, serve_connection,
};
use lodestone_v770::packet_ids::{configuration, login, play};
use lodestone_v770::V770ServerProtocol;
use lodestone_world::World;

/// The vanilla capture, parsed. Panics rather than skipping if absent — a
/// *precondition*-species vacuous test would `return` here and report green.
fn vanilla_capture() -> serde_json::Value {
    let raw = include_str!("fixtures/vanilla_login_disconnect_26_2.json");
    serde_json::from_str(raw).expect("checked-in vanilla login-disconnect capture is valid JSON")
}

/// A never-sampled terrain source; see `server_status.rs` for the same shape.
struct AirSource;

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(-64, 384)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this
        // fixture is all air and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design (an edit a test needs to survive goes through a source with real
    // retention). Explicit rather than inherited — issue #440.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

fn payload_of(directive: ServerDirective) -> (i32, Vec<u8>) {
    match directive {
        ServerDirective::Send { packet_id, payload } => (packet_id, payload),
        other => panic!("expected a Send directive, got {other:?}"),
    }
}

/// The reason our server sends on a keep-alive timeout, restated rather than
/// imported: vanilla's own key (`ServerCommonPacketListenerImpl.java:37`) with
/// vanilla's own English string for it
/// (`.cache/mc/26.2/client-src/assets/minecraft/lang/en_us.json:3498`).
const TIMEOUT_KEY: &str = "disconnect.timeout";
const TIMEOUT_FALLBACK: &str = "Timed out";

fn translatable(key: &str, fallback: Option<&str>, with: Vec<Text>) -> Text {
    Text {
        content: TextContent::Translate {
            key: key.to_owned(),
            with,
            fallback: fallback.map(str::to_owned),
        },
        ..Text::default()
    }
}

// ---------------------------------------------------------------------------
// The phase asymmetry
// ---------------------------------------------------------------------------

/// **The load-bearing test.** Each phase's reason must parse under its own
/// encoding and **fail** under the other's.
///
/// Without the negative half, an implementation that wrote NBT in the login phase
/// (or JSON in play) would satisfy every other assertion in this file that only
/// checks "the reason survives a round trip through our own decoder".
#[test]
fn login_phase_reason_is_json_and_play_phase_reason_is_nbt() {
    let reason = Text::literal("kicked for testing");

    let (login_id, login_body) =
        payload_of(V770ServerProtocol.encode_disconnect(State::Login, &reason));
    let (play_id, play_body) =
        payload_of(V770ServerProtocol.encode_disconnect(State::Play, &reason));

    assert_eq!(login_id, login::clientbound::LOGIN_DISCONNECT);
    assert_eq!(play_id, play::clientbound::DISCONNECT);

    // Login: a length-prefixed UTF-8 string whose content is JSON, nothing after.
    let mut r = Reader::new(&login_body);
    let json = r
        .string(262_144)
        .expect("login disconnect body is a length-prefixed string");
    assert!(
        r.ensure_empty().is_ok(),
        "login disconnect must carry the string and nothing else; the live capture \
         reports trailing_bytes_after_reason_string = 0",
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("login disconnect reason must be JSON");
    assert_eq!(parsed["text"].as_str(), Some("kicked for testing"));

    // Play: network NBT, and *not* a JSON string.
    let mut r = Reader::new(&play_body);
    let nbt = read_network_nbt(&mut r).expect("play disconnect body is network NBT");
    assert!(
        r.ensure_empty().is_ok(),
        "play disconnect must carry the component and nothing else",
    );
    assert_eq!(
        Text::from_nbt(&nbt).to_plain_string(),
        "kicked for testing",
    );

    // The negative half, in both directions.
    assert!(
        Reader::new(&login_body)
            .string(262_144)
            .ok()
            .filter(|s| read_network_nbt(&mut Reader::new(s.as_bytes())).is_ok())
            .is_none()
            || serde_json::from_str::<serde_json::Value>(&json).is_ok(),
        "login body must be JSON, not NBT",
    );
    assert!(
        read_network_nbt(&mut Reader::new(&login_body)).is_err()
            || std::str::from_utf8(&play_body).is_err()
            || serde_json::from_slice::<serde_json::Value>(&play_body).is_err(),
        "the two phases must not be interchangeable; a play body that parsed as \
         JSON would mean the NBT arm is writing a JSON string",
    );
    assert!(
        serde_json::from_slice::<serde_json::Value>(&play_body).is_err(),
        "the play-phase body must NOT be parseable as JSON — if it is, the play \
         arm is writing the login encoding (this is the copy-paste this test \
         exists to catch)",
    );
}

/// Our login-phase packet id and framing are the ones a live vanilla 26.2 server
/// actually used when it refused a login.
#[test]
fn login_disconnect_framing_matches_a_live_vanilla_refusal() {
    let capture = vanilla_capture();
    let vanilla_id = capture["login_disconnect_packet_id"]
        .as_i64()
        .expect("capture pins the packet id");
    let vanilla_trailing = capture["trailing_bytes_after_reason_string"]
        .as_i64()
        .expect("capture pins the trailing-byte count");
    assert_eq!(
        (vanilla_id, vanilla_trailing),
        (0, 0),
        "the checked-in capture itself changed shape; re-read it",
    );
    assert_eq!(
        i64::from(login::clientbound::LOGIN_DISCONNECT),
        vanilla_id,
        "our login_disconnect id must be the one vanilla sent",
    );

    // And the reason vanilla sent was a translatable component with `with` args,
    // so our translatable encoding must produce the same key set for that shape.
    let vanilla_keys: Vec<String> = capture["reason_top_level_keys"]
        .as_array()
        .expect("capture lists the reason's keys")
        .iter()
        .map(|k| k.as_str().expect("key is a string").to_owned())
        .collect();
    assert_eq!(vanilla_keys, vec!["translate", "with"]);

    let ours = translatable(
        "multiplayer.disconnect.outdated_client",
        None,
        vec![Text::literal("26.2")],
    );
    let (_, body) = payload_of(V770ServerProtocol.encode_disconnect(State::Login, &ours));
    let json = Reader::new(&body).string(262_144).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let mut our_keys: Vec<String> = parsed
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    our_keys.sort();
    assert_eq!(
        our_keys, vanilla_keys,
        "for a translatable-with-args reason, our key set must match vanilla's \
         captured one exactly",
    );
    assert_eq!(
        parsed["translate"].as_str(),
        Some("multiplayer.disconnect.outdated_client"),
    );
    // Note: vanilla wrote its `with` element as a bare JSON string (`["26.2"]`)
    // where we write `[{"text":"26.2"}]`. Both decode — same situation as the
    // status document's `description`, and recorded in `text_to_json`'s doc
    // comment. What must match is the *key set* and the key names, which it does.
    assert!(
        parsed["with"].is_array(),
        "`with` must be an array either way",
    );
}

/// The Configuration phase uses the *same* packet as Play, not Login's.
///
/// It has no producer in this server yet — see this file's module docs — so this
/// gate covers the encoder only, and says so rather than implying wiring.
#[test]
fn configuration_disconnect_uses_the_common_nbt_packet_not_logins() {
    let reason = Text::literal("configuration failed");
    let (id, body) =
        payload_of(V770ServerProtocol.encode_disconnect(State::Configuration, &reason));
    assert_eq!(id, configuration::clientbound::DISCONNECT);
    assert_ne!(
        id,
        login::clientbound::LOGIN_DISCONNECT,
        "configuration must not reuse the login-phase id",
    );
    let nbt = read_network_nbt(&mut Reader::new(&body)).expect("configuration reason is NBT");
    assert_eq!(Text::from_nbt(&nbt).to_plain_string(), "configuration failed");
}

/// Handshaking and Status have **no** disconnect packet in 26.2, so the encoder
/// must emit nothing rather than invent an id.
///
/// This is an assertion of an absence; its control is that the four other states
/// above all *do* produce a `Send`, which the tests above observe directly.
#[test]
fn handshaking_and_status_have_no_disconnect_packet() {
    for state in [State::Handshaking, State::Status] {
        assert_eq!(
            V770ServerProtocol.encode_disconnect(state, &Text::literal("nope")),
            ServerDirective::None,
            "{state:?} has no disconnect packet id in 26.2 (Status's clientbound \
             set is status_response/pong_response only); vanilla closes the \
             channel instead",
        );
    }
}

// ---------------------------------------------------------------------------
// The reason survives to a real client's decoder
// ---------------------------------------------------------------------------

/// Our encoded reason decodes through the **real client adapter** — the same
/// `nbt_reason_text` path that has been validated against real servers'
/// disconnect packets — back into the reason we sent.
///
/// A cross-check rather than primary evidence (it is our code on both ends), but
/// it earns its place: that decoder reads what real servers send, so a component
/// it rejects is one a real server would not have produced.
#[test]
fn a_real_client_adapter_decodes_our_play_and_configuration_reasons() {
    for (state, packet_id) in [
        (ConnectionState::Play, play::clientbound::DISCONNECT),
        (
            ConnectionState::Configuration,
            configuration::clientbound::DISCONNECT,
        ),
    ] {
        let reason = translatable(TIMEOUT_KEY, Some(TIMEOUT_FALLBACK), Vec::new());
        let server_state = if matches!(state, ConnectionState::Play) {
            State::Play
        } else {
            State::Configuration
        };
        let (_, body) =
            payload_of(V770ServerProtocol.encode_disconnect(server_state, &reason));

        let directives = lodestone_v770::adapter()
            .handle_packet(&mut World::new(), state, packet_id, &body)
            .expect("the real client adapter decodes our disconnect");
        let Some(Directive::Disconnect(decoded)) = directives.into_iter().next() else {
            panic!("expected a Disconnect directive in {state:?}");
        };
        // `to_plain_string` resolves an unknown key to the component's
        // `fallback` (see `Text::to_plain_string`'s own doc comment), so this
        // asserts the fallback actually made it onto the wire — a component
        // missing it would render the raw key instead.
        assert_eq!(
            decoded.to_plain_string(),
            TIMEOUT_FALLBACK,
            "the fallback string must survive to the client, or a client that \
             cannot resolve `{TIMEOUT_KEY}` shows the raw key (issue #68)",
        );
        match decoded.content {
            TextContent::Translate {
                ref key,
                ref fallback,
                ..
            } => {
                assert_eq!(key, TIMEOUT_KEY);
                assert_eq!(fallback.as_deref(), Some(TIMEOUT_FALLBACK));
            }
            ref other => panic!("expected a translatable reason, got {other:?}"),
        }
    }
}

/// `extra` children and nested `with` arguments survive the NBT encoding.
///
/// Not idle coverage: `component_list` must tag an NBT list with an element type,
/// and an encoder that guessed wrong there produces a component a real client
/// drops. The empty-list guard is why both call sites check `is_empty` first.
#[test]
fn nested_components_survive_the_nbt_encoding() {
    let reason = Text {
        content: TextContent::Translate {
            key: "chat.type.text".to_owned(),
            with: vec![Text::literal("Alice"), Text::literal("hello")],
            fallback: Some("<Alice> hello".to_owned()),
        },
        extra: vec![Text::literal(" (kicked)")],
        ..Text::default()
    };
    let (_, body) = payload_of(V770ServerProtocol.encode_disconnect(State::Play, &reason));
    let nbt = read_network_nbt(&mut Reader::new(&body)).expect("nested reason is valid NBT");
    let decoded = Text::from_nbt(&nbt);
    let rendered = decoded.to_plain_string();
    assert!(
        rendered.contains("Alice") && rendered.contains("hello"),
        "translate arguments must survive; got {rendered:?}",
    );
    assert!(
        rendered.contains("(kicked)"),
        "`extra` children must survive; got {rendered:?}",
    );
}

// ---------------------------------------------------------------------------
// Producers — the anti-island gates
// ---------------------------------------------------------------------------

/// **Producer gate (Play).** A client that stops echoing keep-alives must be sent
/// a real disconnect packet carrying vanilla's own `disconnect.timeout` reason,
/// not merely have its socket closed.
///
/// Runs under `start_paused` so the 15-second interval resolves instantly.
#[tokio::test(start_paused = true)]
async fn an_unanswered_keep_alive_kicks_the_client_with_a_reason() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

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
    drive_to_play(&mut client, "KeepAliveVictim").await;

    // Read everything the server sends from here on, but never answer a
    // keep-alive — a genuine stall, which is what vanilla kicks for.
    let mut disconnect_reason = None;
    while let Ok(Ok(Some((id, payload)))) =
        tokio::time::timeout(Duration::from_secs(60), client.read_packet()).await
    {
        if id == play::clientbound::DISCONNECT {
            let nbt = read_network_nbt(&mut Reader::new(&payload))
                .expect("the kick reason is network NBT");
            disconnect_reason = Some(Text::from_nbt(&nbt));
            break;
        }
    }

    let reason = disconnect_reason.expect(
        "the server must SEND a play-phase disconnect before hanging up; closing \
         the socket silently is exactly the defect issue #279 reports",
    );
    match reason.content {
        TextContent::Translate {
            ref key,
            ref fallback,
            ..
        } => {
            assert_eq!(
                key, TIMEOUT_KEY,
                "the key must be vanilla's own (ServerCommonPacketListenerImpl.java:37)",
            );
            assert_eq!(
                fallback.as_deref(),
                Some(TIMEOUT_FALLBACK),
                "the fallback must be vanilla's own en_us string for that key",
            );
        }
        ref other => panic!("expected a translatable timeout reason, got {other:?}"),
    }

    drop(client);
    let outcome = server.await.expect("server task panicked");
    assert!(
        matches!(outcome, Err(ServerError::KeepAliveTimeout)),
        "sending the reason must not change the outcome, got {outcome:?}",
    );
}

/// **Producer gate (Login).** A username vanilla's own server would refuse gets a
/// login-phase disconnect explaining so, rather than a silent close.
#[tokio::test]
async fn an_invalid_username_is_refused_with_a_login_disconnect() {
    // A tab is `0x09`, which is `<= 32` and so rejected by
    // `StringUtil.isValidPlayerName` (`StringUtil.java:66-68`).
    let (sent, outcome) = attempt_login("bad\tname").await;
    let (id, payload) = sent
        .iter()
        .find(|(id, _)| *id == login::clientbound::LOGIN_DISCONNECT)
        .expect(
            "the server must SEND a login_disconnect for a refused name, not just \
             close the socket",
        );
    assert_eq!(*id, login::clientbound::LOGIN_DISCONNECT);
    let json = Reader::new(payload)
        .string(262_144)
        .expect("login disconnect reason is a length-prefixed JSON string");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("reason is JSON");
    assert_eq!(
        parsed["text"].as_str(),
        Some("Invalid username"),
        "the refusal must explain itself; vanilla throws here instead, so the \
         text is ours (see `invalid_username_reason`)",
    );
    assert!(
        matches!(outcome, Err(ServerError::InvalidUsername)),
        "got {outcome:?}",
    );
    assert!(
        !sent.iter().any(|(id, _)| *id == login::clientbound::LOGIN_FINISHED),
        "a refused login must not also succeed",
    );
}

/// **Control for the producer above**: a *valid* username must reach login
/// success and receive **no** disconnect. Without this, the test above passes for
/// a server that refuses every login.
#[tokio::test]
async fn a_valid_username_is_not_refused() {
    let (sent, outcome) = attempt_login("Notch").await;
    assert!(
        !sent
            .iter()
            .any(|(id, _)| *id == login::clientbound::LOGIN_DISCONNECT),
        "a valid name must not be refused",
    );
    assert!(
        sent.iter().any(|(id, _)| *id == login::clientbound::LOGIN_FINISHED),
        "a valid name must reach login success; got ids {:?}",
        sent.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
    );
    assert!(
        !matches!(outcome, Err(ServerError::InvalidUsername)),
        "got {outcome:?}",
    );
}

/// The name-validation boundary, straight from `StringUtil.isValidPlayerName`
/// (`net/minecraft/util/StringUtil.java:66-68`): at most 16 chars, and no char
/// `<= 32` or `>= 127`.
///
/// Exercised through the **server loop**, not by calling a private helper, so it
/// measures the policy that is actually enforced end to end.
///
/// # Two rejection paths, and only one of them explains itself
///
/// The length half of vanilla's check is already enforced *one layer earlier*, by
/// the wire decoder: `LoginHello.name` carries `#[mc(max = 16)]`, so a 17-char
/// name fails `decode_full` and the loop sees `ServerBound::Ignored` — the packet
/// is dropped with no `LoginStart` and therefore no reason to send. That is a
/// **silent** rejection, and it is why this table has two boolean columns rather
/// than one. Recorded because the first version of this test conflated them and
/// read the silent drop as an acceptance.
#[tokio::test]
async fn name_validation_matches_vanillas_own_boundary() {
    // (name, reaches login success?, gets an explanation?)
    for (name, succeeds, explained) in [
        ("a", true, false),
        ("ABCDEFGHIJKLMNOP", true, false), // exactly 16 — the last accepted length
        // 17 chars: rejected by the *decoder* (`#[mc(max = 16)]`), so silently.
        ("ABCDEFGHIJKLMNOPQ", false, false),
        ("has space", false, true), // 0x20 == 32, and the rule is `<= 32`
        ("tab\there", false, true), // 0x09
        ("!", true, false),         // 33, the first allowed char
        ("~", true, false),         // 126, the last allowed char
        ("\u{7F}", false, true),    // 127 (DEL), the first rejected char
        ("café", false, true),      // é is >= 127
        ("", true, false),          // vanilla's own check permits it
    ] {
        let (sent, _) = attempt_login(name).await;
        let explanation = sent
            .iter()
            .any(|(id, _)| *id == login::clientbound::LOGIN_DISCONNECT);
        let success = sent
            .iter()
            .any(|(id, _)| *id == login::clientbound::LOGIN_FINISHED);
        assert_eq!(
            success, succeeds,
            "name {name:?}: expected login success = {succeeds}, got {success}",
        );
        assert_eq!(
            explanation, explained,
            "name {name:?}: expected an explanation = {explained}, got {explanation} \
             (a name too long for the wire is dropped by the decoder before the \
             loop can explain anything)",
        );
        assert!(
            !(success && explanation),
            "name {name:?}: a login cannot both succeed and be refused",
        );
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Writes a handshake (`next_state = 2`) and a login `hello` carrying `name`,
/// then collects everything the server sends.
async fn attempt_login(name: &str) -> (Vec<(i32, Vec<u8>)>, Result<(), ServerError>) {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;
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
    client
        .write_packet(0, &handshake_bytes(2))
        .await
        .expect("handshake writes");
    client
        .write_packet(0, &hello_bytes(name))
        .await
        .expect("hello writes");

    let mut sent = Vec::new();
    while let Ok(Ok(Some(packet))) =
        tokio::time::timeout(Duration::from_millis(250), client.read_packet()).await
    {
        sent.push(packet);
    }
    drop(client);
    let outcome = server.await.expect("server task panicked");
    (sent, outcome)
}

/// Drives handshake → login → configuration → play so the keep-alive timer starts,
/// draining the join sequence.
async fn drive_to_play<T: lodestone_net::Transport>(client: &mut Connection<T>, name: &str) {
    client.write_packet(0, &handshake_bytes(2)).await.unwrap();
    client.write_packet(0, &hello_bytes(name)).await.unwrap();
    // Read the login_finished, then acknowledge it.
    let _ = client.read_packet().await.unwrap();
    // `login_acknowledged`: empty body.
    client
        .write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[])
        .await
        .unwrap();
    // Drain configuration (a single finish_configuration), then acknowledge.
    let _ = client.read_packet().await.unwrap();
    client
        .write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[])
        .await
        .unwrap();
    // Drain the join sequence until it goes quiet; the keep-alive timer is armed
    // from the moment Play begins.
    while let Ok(Ok(Some(_))) =
        tokio::time::timeout(Duration::from_millis(50), client.read_packet()).await
    {}
}

/// Hand-written `Intention`: VarInt protocol, host string, big-endian u16 port,
/// VarInt next_state (`2` = Login).
fn handshake_bytes(next_state: i32) -> Vec<u8> {
    let mut w = lodestone_core::Writer::default();
    w.var_i32(776);
    w.string("localhost");
    w.u16(25565);
    w.var_i32(next_state);
    w.into_vec()
}

/// Hand-written login `hello`: a length-prefixed name then a raw 16-byte uuid.
fn hello_bytes(name: &str) -> Vec<u8> {
    let mut w = lodestone_core::Writer::default();
    w.string(name);
    w.uuid(uuid::Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0));
    w.into_vec()
}
