//! Live chat capture against the `lodestone-creative` server (:25570).
//!
//! J2/G3: chat is where `Text` handling is exercised hardest. This oracle joins
//! the real server and captures the three clientbound chat packets the vanilla
//! server actually emits — `disguised_chat` (a command-sourced message with a
//! translated decoration), `system_chat` (raw and *translated* components), and
//! `player_chat` (a signed/unsigned player message) — then decodes each into the
//! version-free chat model in `lodestone_game::chat` and asserts the decode is
//! byte-exact (zero trailing bytes) and semantically what the server meant.
//!
//! Why live and not hermetic: the whole question chat raises is "does my model
//! of `Text` and the chat packets match what the server serializes?" A fixture
//! I hand-write can round-trip through my own encoder happily while being wrong
//! about the wire. Only the server's own bytes settle it. The zero-trailing-byte
//! assertion after each decode is the alignment detector: a misparse of the
//! variable-length NBT `Text` or the `ChatType.Bound` tail leaves the buffer
//! misaligned and trips instantly.
//!
//! Run:
//! ```text
//! cargo test -p lodestone-game --features live-reconcile --test live_chat \
//!   -- --ignored --nocapture
//! ```
//!
//! The `lodestone-creative` server must be running (see `tests/live_click.rs`
//! for how it is stood up). Hermetic by default: this test is `#[ignore]`d and
//! only compiles under the `live-reconcile` feature.
#![cfg(feature = "live-reconcile")]

use lodestone_testsupport::{AsyncRconClient as Rcon, unique_username};
use std::time::Duration;

use lodestone_core::{Nbt, Reader, Writer, read_network_nbt};
use lodestone_game::chat::{
    ChatDecoration, ChatParameter, DisguisedChatMessage, FilterMask, PlayerChatMessage,
    SystemMessage,
};
use lodestone_model::{Text, TextColor, TextStyle};
use lodestone_net::Connection;
use tokio::net::TcpStream;
use uuid::Uuid;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25570;
const RCON_PORT: u16 = 25571;
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL_776: i32 = 776;

mod pkt {
    pub mod hs_sb {
        pub const INTENTION: i32 = 0;
    }
    pub mod login_cb {
        pub const DISCONNECT: i32 = 0;
        pub const ENCRYPTION_REQUEST: i32 = 1;
        pub const LOGIN_FINISHED: i32 = 2;
        pub const COMPRESSION: i32 = 3;
    }
    pub mod login_sb {
        pub const HELLO: i32 = 0;
        pub const LOGIN_ACKNOWLEDGED: i32 = 3;
    }
    pub mod cfg_cb {
        pub const FINISH_CONFIGURATION: i32 = 3;
        pub const KEEP_ALIVE: i32 = 4;
        pub const SELECT_KNOWN_PACKS: i32 = 14;
    }
    pub mod cfg_sb {
        pub const FINISH_CONFIGURATION: i32 = 3;
        pub const KEEP_ALIVE: i32 = 4;
        pub const SELECT_KNOWN_PACKS: i32 = 7;
    }
    pub mod play_cb {
        pub const DISGUISED_CHAT: i32 = 33;
        pub const PLAYER_CHAT: i32 = 65;
        pub const SYSTEM_CHAT: i32 = 121;
        pub const CHUNK_BATCH_FINISHED: i32 = 11;
        pub const DISCONNECT: i32 = 32;
        pub const KEEP_ALIVE: i32 = 44;
        pub const LOGIN: i32 = 49;
        pub const SET_HEALTH: i32 = 104;
    }
    pub mod play_sb {
        pub const CHAT: i32 = 9;
        pub const CHUNK_BATCH_RECEIVED: i32 = 11;
        pub const CLIENT_COMMAND: i32 = 12;
        pub const KEEP_ALIVE: i32 = 28;
    }
}

// ---- wire helpers ----------------------------------------------------------

fn write_string(w: &mut Writer, s: &str) {
    w.var_i32(s.len() as i32);
    w.bytes(s.as_bytes());
}

fn read_string(r: &mut Reader) -> String {
    let len = r.var_i32().expect("string length") as usize;
    let bytes = r.bytes(len).expect("string bytes");
    String::from_utf8_lossy(bytes).into_owned()
}

/// A `ChatType.Bound` as it appears on the wire: a registry `Holder<ChatType>`
/// (a VarInt reference id, since chat types are registered during config),
/// a decorated sender name, and an optional target name.
#[derive(Debug)]
struct BoundChatType {
    /// Registry reference id + 1 (0 would mean an inline definition, which the
    /// vanilla server never sends for chat types).
    holder_ref: i32,
    sender: Text,
    target: Option<Text>,
}

fn read_bound_chat_type(r: &mut Reader) -> BoundChatType {
    let holder_ref = r.var_i32().expect("chat type holder id");
    assert!(
        holder_ref > 0,
        "chat type must be a registry reference, got inline id {holder_ref}"
    );
    let sender = Text::from_nbt(&read_network_nbt(r).expect("chat type sender name"));
    let target = read_optional_text(r);
    BoundChatType {
        holder_ref,
        sender,
        target,
    }
}

fn read_optional_text(r: &mut Reader) -> Option<Text> {
    if r.u8().expect("optional-text present flag") != 0 {
        Some(Text::from_nbt(&read_network_nbt(r).expect("optional text")))
    } else {
        None
    }
}

// ---- decoded packet shapes -------------------------------------------------

#[derive(Debug)]
struct SystemChat {
    content: Text,
    overlay: bool,
}

/// `ClientboundSystemChatPacket` = trusted NBT component + bool overlay.
fn decode_system_chat(payload: &[u8]) -> SystemChat {
    let mut r = Reader::new(payload);
    let content = Text::from_nbt(&read_network_nbt(&mut r).expect("system chat content"));
    let overlay = r.u8().expect("system chat overlay") != 0;
    assert_eq!(
        r.remaining(),
        0,
        "system_chat left {} trailing bytes",
        r.remaining()
    );
    SystemChat { content, overlay }
}

#[derive(Debug)]
struct DisguisedChat {
    message: Text,
    chat_type: BoundChatType,
}

/// `ClientboundDisguisedChatPacket` = trusted NBT component + `ChatType.Bound`.
fn decode_disguised_chat(payload: &[u8]) -> DisguisedChat {
    let mut r = Reader::new(payload);
    let message = Text::from_nbt(&read_network_nbt(&mut r).expect("disguised chat message"));
    let chat_type = read_bound_chat_type(&mut r);
    assert_eq!(
        r.remaining(),
        0,
        "disguised_chat left {} trailing bytes",
        r.remaining()
    );
    DisguisedChat { message, chat_type }
}

#[derive(Debug)]
struct PlayerChat {
    sender: Uuid,
    index: i32,
    content: String,
    timestamp_ms: i64,
    salt: i64,
    signature: Option<Vec<u8>>,
    unsigned_content: Option<Text>,
    filter_mask: FilterMask,
    chat_type: BoundChatType,
}

/// `ClientboundPlayerChatPacket`: global index, sender UUID, per-sender index,
/// optional signature, the signed body (content + timestamp + salt + last-seen
/// packed), optional unsigned content, the filter mask, and the chat type.
fn decode_player_chat(payload: &[u8]) -> PlayerChat {
    let mut r = Reader::new(payload);
    let _global_index = r.var_i32().expect("global index");
    let sender = r.uuid().expect("sender uuid");
    let index = r.var_i32().expect("sender index");
    let signature = read_optional_signature(&mut r);
    // Signed body (packed).
    let content = r.string(256).expect("body content");
    let timestamp_ms = r.i64().expect("body timestamp");
    let salt = r.i64().expect("body salt");
    let last_seen = r.var_i32().expect("last-seen count");
    for _ in 0..last_seen {
        // Each entry: VarInt id+1 (reference into the cache) or 0 then a full
        // 256-byte signature. A fresh session's last-seen list is empty.
        let id = r.var_i32().expect("last-seen id");
        if id == 0 {
            let _sig = r.bytes(256).expect("last-seen full signature");
        }
    }
    let unsigned_content = read_optional_text(&mut r);
    let filter_mask = read_filter_mask(&mut r);
    let chat_type = read_bound_chat_type(&mut r);
    assert_eq!(
        r.remaining(),
        0,
        "player_chat left {} trailing bytes",
        r.remaining()
    );
    PlayerChat {
        sender,
        index,
        content,
        timestamp_ms,
        salt,
        signature,
        unsigned_content,
        filter_mask,
        chat_type,
    }
}

fn read_optional_signature(r: &mut Reader) -> Option<Vec<u8>> {
    if r.u8().expect("signature present flag") != 0 {
        Some(r.bytes(256).expect("256-byte signature").to_vec())
    } else {
        None
    }
}

/// Decodes the filter mask: `PASS_THROUGH`(0) and `FULLY_FILTERED`(1) carry no
/// payload; `PARTIALLY_FILTERED`(2) is followed by a long-array bit set whose
/// bits mark filtered character positions.
fn read_filter_mask(r: &mut Reader) -> FilterMask {
    match r.var_i32().expect("filter mask type") {
        0 => FilterMask::PassThrough,
        1 => FilterMask::FullyFiltered,
        2 => {
            let words = r.var_i32().expect("filter bitset len");
            let mut bits = Vec::new();
            for _ in 0..words {
                let word = r.i64().expect("filter bitset word");
                for b in 0..64 {
                    bits.push((word >> b) & 1 == 1);
                }
            }
            FilterMask::Partial(bits)
        }
        other => panic!("unknown filter mask type {other}"),
    }
}

// ---- the live session ------------------------------------------------------

#[derive(PartialEq, Debug)]
enum Phase {
    Login,
    Configuration,
    Play,
}

struct Session {
    conn: Connection<TcpStream>,
}

impl Session {
    async fn join(username: &str) -> Self {
        let mut conn = Connection::connect((HOST, PORT)).await.expect("connect to lodestone-creative on 127.0.0.1:25570 (game port); start it with docker run --rm -p 25570:25570 -p 25571:25571 ... lodestone-creative");

        let mut hs = Writer::default();
        hs.var_i32(PROTOCOL_776);
        write_string(&mut hs, HOST);
        hs.u16(PORT);
        hs.var_i32(2);
        conn.write_packet(pkt::hs_sb::INTENTION, &hs.into_vec())
            .await
            .expect("handshake");

        let mut hello = Writer::default();
        write_string(&mut hello, username);
        hello.bytes(Uuid::new_v4().as_bytes());
        conn.write_packet(pkt::login_sb::HELLO, &hello.into_vec())
            .await
            .expect("login hello");

        let mut session = Self { conn };
        session.drive_to_play().await;
        session.settle().await;
        session
    }

    async fn drive_to_play(&mut self) {
        let mut phase = Phase::Login;
        let deadline = Duration::from_secs(45);
        let step = Duration::from_secs(10);
        let ok = tokio::time::timeout(deadline, async {
            loop {
                let (id, payload) = match tokio::time::timeout(step, self.conn.read_packet()).await
                {
                    Ok(Ok(Some(p))) => p,
                    Ok(Ok(None)) => panic!("EOF before Play"),
                    Ok(Err(e)) => panic!("read error before Play: {e}"),
                    Err(_) => panic!("timeout before Play in {phase:?}"),
                };
                match phase {
                    Phase::Login => {
                        if id == pkt::login_cb::COMPRESSION {
                            let mut r = Reader::new(&payload);
                            self.conn.set_compression(r.var_i32().expect("threshold"));
                        } else if id == pkt::login_cb::LOGIN_FINISHED {
                            self.conn
                                .write_packet(pkt::login_sb::LOGIN_ACKNOWLEDGED, &[])
                                .await
                                .expect("login ack");
                            phase = Phase::Configuration;
                        } else if id == pkt::login_cb::ENCRYPTION_REQUEST {
                            panic!("unexpected encryption request (server must be offline-mode)");
                        } else if id == pkt::login_cb::DISCONNECT {
                            let mut r = Reader::new(&payload);
                            panic!("login disconnect: {}", read_string(&mut r));
                        }
                    }
                    Phase::Configuration => {
                        if id == pkt::cfg_cb::KEEP_ALIVE {
                            self.conn
                                .write_packet(pkt::cfg_sb::KEEP_ALIVE, &payload)
                                .await
                                .expect("cfg ka");
                        } else if id == pkt::cfg_cb::SELECT_KNOWN_PACKS {
                            self.conn
                                .write_packet(pkt::cfg_sb::SELECT_KNOWN_PACKS, &payload)
                                .await
                                .expect("packs");
                        } else if id == pkt::cfg_cb::FINISH_CONFIGURATION {
                            self.conn
                                .write_packet(pkt::cfg_sb::FINISH_CONFIGURATION, &[])
                                .await
                                .expect("cfg fin");
                            phase = Phase::Play;
                        }
                    }
                    Phase::Play => {
                        if id == pkt::play_cb::LOGIN {
                            return;
                        } else if id == pkt::play_cb::KEEP_ALIVE {
                            self.conn
                                .write_packet(pkt::play_sb::KEEP_ALIVE, &payload)
                                .await
                                .expect("play ka");
                        } else if id == pkt::play_cb::CHUNK_BATCH_FINISHED {
                            let mut w = Writer::default();
                            w.f32(16.0);
                            self.conn
                                .write_packet(pkt::play_sb::CHUNK_BATCH_RECEIVED, &w.into_vec())
                                .await
                                .expect("chunk ack");
                        }
                    }
                }
            }
        })
        .await;
        assert!(ok.is_ok(), "did not reach Play");
        eprintln!("reached Play");
    }

    /// Drains the join-time packet burst until the socket goes quiet, echoing
    /// keep-alives and respawning a dead player. Leaves the connection ready for
    /// a clean chat capture.
    async fn settle(&mut self) {
        let idle = Duration::from_millis(600);
        loop {
            match tokio::time::timeout(idle, self.conn.read_packet()).await {
                Ok(Ok(Some((id, payload)))) => self.handle_ambient(id, &payload).await,
                Ok(Ok(None)) => panic!("EOF during settle"),
                Ok(Err(e)) => panic!("read error during settle: {e}"),
                Err(_) => return,
            }
        }
    }

    async fn handle_ambient(&mut self, id: i32, payload: &[u8]) {
        if id == pkt::play_cb::KEEP_ALIVE {
            self.conn
                .write_packet(pkt::play_sb::KEEP_ALIVE, payload)
                .await
                .expect("play ka");
        } else if id == pkt::play_cb::CHUNK_BATCH_FINISHED {
            let mut w = Writer::default();
            w.f32(16.0);
            self.conn
                .write_packet(pkt::play_sb::CHUNK_BATCH_RECEIVED, &w.into_vec())
                .await
                .expect("chunk ack");
        } else if id == pkt::play_cb::SET_HEALTH {
            let mut r = Reader::new(payload);
            let health = r.f32().expect("health");
            if health <= 0.0 {
                eprintln!("!! set_health = {health}: inherited a dead player; respawning");
                let mut w = Writer::default();
                w.var_i32(0);
                self.conn
                    .write_packet(pkt::play_sb::CLIENT_COMMAND, &w.into_vec())
                    .await
                    .expect("respawn");
            }
        }
    }

    /// Sends an unsigned serverbound chat message. Offline servers accept
    /// unsigned chat (no secure profile is possible) and rebroadcast it as a
    /// `player_chat` with a "not secure" trust indicator.
    async fn send_chat(&mut self, message: &str) {
        let mut w = Writer::default();
        write_string(&mut w, message);
        let millis = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        w.i64(millis); // timestamp
        w.i64(0); // salt
        w.u8(0); // no signature
        w.var_i32(0); // last-seen: offset 0
        w.bytes(&[0, 0, 0]); // last-seen acknowledged: fixed 20-bit set, empty
        w.u8(0); // last-seen checksum
        self.conn
            .write_packet(pkt::play_sb::CHAT, &w.into_vec())
            .await
            .expect("send chat");
    }

    /// Pumps packets — echoing keep-alives, acking chunk batches, respawning a
    /// dead player — until a packet with `want` id arrives; returns its payload.
    async fn wait_for(&mut self, want: i32, label: &str) -> Vec<u8> {
        let deadline = Duration::from_secs(15);
        let step = Duration::from_secs(10);
        let got = tokio::time::timeout(deadline, async {
            loop {
                let (id, payload) = match tokio::time::timeout(step, self.conn.read_packet()).await
                {
                    Ok(Ok(Some(p))) => p,
                    Ok(Ok(None)) => panic!("EOF awaiting {label}"),
                    Ok(Err(e)) => panic!("read error awaiting {label}: {e}"),
                    Err(_) => panic!("timeout awaiting {label}"),
                };
                if id == want {
                    return payload;
                } else if id == pkt::play_cb::DISCONNECT {
                    let mut r = Reader::new(&payload);
                    let reason = Text::from_nbt(&read_network_nbt(&mut r).unwrap_or(Nbt::End));
                    panic!(
                        "server disconnected while awaiting {label}: {}",
                        reason.to_plain_string()
                    );
                } else {
                    self.handle_ambient(id, &payload).await;
                }
            }
        })
        .await;
        got.unwrap_or_else(|_| panic!("timed out awaiting {label}"))
    }
}

// ---- the oracle ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the creative lodestone-creative server on 127.0.0.1:25570"]
async fn live_chat_agrees_with_server() {
    let user = unique_username();
    eprintln!("=== LIVE CHAT ORACLE (protocol {PROTOCOL_776}, creative :{PORT}) ===");
    eprintln!("username (unique per run): {user}");

    let mut rcon = Rcon::connect((HOST, RCON_PORT), RCON_PASSWORD)
        .await
        .expect("connect/authenticate RCON");
    let mut session = Session::join(&user).await;

    // -- 1. disguised_chat: /say is command-sourced, so the server sends it as a
    //       DISGUISED (unsigned) message decorated by the say_command chat type.
    {
        let marker = "OracleSayXYZ";
        rcon.cmd(&format!("say {marker}")).await;
        let payload = session
            .wait_for(pkt::play_cb::DISGUISED_CHAT, "disguised_chat")
            .await;
        let dc = decode_disguised_chat(&payload);
        eprintln!(
            "[disguised_chat] message={:?} chat_type_ref={} sender={:?}",
            dc.message.to_plain_string(),
            dc.chat_type.holder_ref,
            dc.chat_type.sender.to_plain_string()
        );
        assert_eq!(
            dc.message.to_plain_string(),
            marker,
            "disguised message content"
        );
        // Feed it into the version-free model and reconstruct the rendered line
        // using the say_command decoration (translate "chat.type.text" is the
        // built-in; the server's say uses "chat.type.announcement"). We assert
        // the model composes the decoration around the server's real content.
        let model = DisguisedChatMessage {
            content: dc.message.clone(),
            sender_name: dc.chat_type.sender.clone(),
            target_name: dc.chat_type.target.clone(),
        };
        let decoration = ChatDecoration::new(
            "chat.type.announcement",
            vec![ChatParameter::Sender, ChatParameter::Content],
            TextStyle::default(),
        );
        let rendered = model.display(&decoration);
        eprintln!(
            "[disguised_chat] model rendered = {:?}",
            rendered.to_plain_string()
        );
        assert!(
            rendered.to_plain_string().contains(marker),
            "rendered disguised line must contain the server's content"
        );
        eprintln!("[disguised_chat] OK — decoded byte-exact and modelled");
    }

    // -- 2a. system_chat (raw component): /tellraw sends a trusted component with
    //        no player source, as SYSTEM chat.
    {
        let text = "OracleTellRaw";
        rcon.cmd(&format!(
            "tellraw {user} {{\"text\":\"{text}\",\"color\":\"gold\"}}"
        ))
        .await;
        let payload = session
            .wait_for(pkt::play_cb::SYSTEM_CHAT, "system_chat (raw)")
            .await;
        let sc = decode_system_chat(&payload);
        eprintln!(
            "[system_chat/raw] content={:?} overlay={}",
            sc.content.to_plain_string(),
            sc.overlay
        );
        assert_eq!(
            sc.content.to_plain_string(),
            text,
            "system chat raw content"
        );
        assert_eq!(
            sc.content.style.color,
            Some(TextColor::Gold),
            "server-serialised gold colour must survive the NBT decode"
        );
        assert!(
            !sc.overlay,
            "tellraw is a chat message, not an actionbar overlay"
        );
        let _system = SystemMessage {
            content: sc.content.clone(),
            overlay: sc.overlay,
        };
        eprintln!("[system_chat/raw] OK — trusted NBT component decoded byte-exact");
    }

    // -- 2b. system_chat (TRANSLATED component): the hardest Text path — a
    //        translate component with `with` args, serialised as NBT by the real
    //        server, must resolve through the built-in translation table.
    {
        // chat.type.text => "<%s> %s"; args "Steve","hi" => "<Steve> hi".
        let json = r#"{"translate":"chat.type.text","with":["Steve","hi"]}"#;
        rcon.cmd(&format!("tellraw {user} {json}")).await;
        let payload = session
            .wait_for(pkt::play_cb::SYSTEM_CHAT, "system_chat (translated)")
            .await;
        let sc = decode_system_chat(&payload);
        eprintln!(
            "[system_chat/translate] resolved={:?}",
            sc.content.to_plain_string()
        );
        assert_eq!(
            sc.content.to_plain_string(),
            "<Steve> hi",
            "translate component from the real server must resolve via the built-in table"
        );
        eprintln!("[system_chat/translate] OK — server-serialised translate resolved");
    }

    // -- 3. player_chat: send an unsigned message as the player; the offline
    //       server rebroadcasts it to us as a (not-secure) player_chat.
    {
        let text = "OraclePlayerHi";
        session.send_chat(text).await;
        let payload = session
            .wait_for(pkt::play_cb::PLAYER_CHAT, "player_chat")
            .await;
        let pc = decode_player_chat(&payload);
        eprintln!(
            "[player_chat] sender={} content={:?} signed={} filter={:?} chat_type_ref={} unsigned={:?}",
            pc.sender,
            pc.content,
            pc.signature.is_some(),
            pc.filter_mask,
            pc.chat_type.holder_ref,
            pc.unsigned_content.as_ref().map(Text::to_plain_string),
        );
        assert_eq!(
            pc.content, text,
            "player_chat body content must equal what we sent"
        );
        // Build the version-free model directly from the server's own fields.
        let model = PlayerChatMessage {
            sender: pc.sender,
            index: pc.index,
            signed_content: pc.content.clone(),
            unsigned_content: pc.unsigned_content.clone(),
            timestamp_ms: pc.timestamp_ms,
            salt: pc.salt,
            signature: pc.signature.clone(),
            filter_mask: pc.filter_mask.clone(),
            sender_name: pc.chat_type.sender.clone(),
            target_name: pc.chat_type.target.clone(),
        };
        // An offline unsigned message is "not secure": no signature bytes.
        assert!(
            !model.is_signed(),
            "offline unsigned chat must carry no signature"
        );
        assert!(
            model.filter_mask.is_pass_through(),
            "no chat filter is configured, so nothing is filtered"
        );
        let rendered = model
            .display(&ChatDecoration::vanilla_chat())
            .expect("player chat renders");
        eprintln!(
            "[player_chat] model rendered = {:?}",
            rendered.to_plain_string()
        );
        assert!(
            rendered.to_plain_string().contains(text),
            "rendered player line must contain the message body"
        );
        eprintln!("[player_chat] OK — decoded byte-exact and modelled (not-secure path)");
    }

    eprintln!("=== CHAT ORACLE PASSED: live server agrees with the chat model ===");
}
