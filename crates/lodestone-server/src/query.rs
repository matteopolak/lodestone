//! The GameSpy4 / UT3 server-query protocol (issue #332).
//!
//! ## What it is
//!
//! A small, version-independent UDP protocol used by server-list aggregators and
//! hosting panels to read a Minecraft server's identity without joining it: a
//! challenge-response dance over two wire packet types (a **handshake** and a
//! **stat** request — see "How it works" for why that is two, not three).
//!
//! * A **handshake** (type `0x09`) asks for a challenge token.
//! * A **stat** request (type `0x00`) proves the token and gets either the short
//!   form (MOTD, game type, map, online/max players, host IP/port) or the long
//!   form (everything in the short form plus the player-name list, plugins,
//!   game id and version) — selected by the request's own **length**, not by a
//!   second type byte (see below).
//!
//! The protocol is deliberately **not** Minecraft-version-specific, so nothing
//! here names a protocol number or a packet id — the same "stat" language
//! GameSpy used for Quake and Unreal servers. Every wire detail below is
//! hand-expanded from vanilla's own `QueryThreadGs4`/`NetworkDataOutputStream`/
//! `PktUtils` (`.cache/mc/26.2/src/net/minecraft/server/rcon/`) rather than
//! inferred from a description, because an earlier version of this module got
//! several of these details wrong in a way `decode(encode(x)) == x` could not
//! catch — see "Corrected against the real protocol" below.
//!
//! ### There is no separate "full stat" packet type
//!
//! `QueryThreadGs4.processPacket`'s `switch (buf[2])` has exactly two live arms:
//! `0` (stat) and `9` (handshake); anything else falls through to
//! `default: return true;` with **no reply sent**. Basic vs. full stat is
//! decided *inside* the `case 0` arm by the request's total length: `15 == len`
//! selects the full form (`buildRuleResponse`, called "Rules" in vanilla's own
//! debug log), anything else falls to the short form ("Status"). A real client
//! signals "full" by appending 4 padding bytes after the challenge token, not
//! by changing the type byte — a client that sent type `0x01` for a full
//! request (which is what this module used to require) would get **no answer
//! at all** from a real vanilla server, and would get the *wrong* (short) form
//! from an earlier version of this one.
//!
//! ### The response's own type byte is always `0x00`
//!
//! Both `buildRuleResponse` and the inline short-form writer in
//! `processPacket` start with `write(0)` — the reply's leading byte is the
//! stat type, not an echo of which form was requested. [`handle_request`]
//! matches this: [`build_basic_stat`] and [`build_full_stat`] both lead with
//! [`TYPE_STAT`].
//!
//! ### The challenge token is text in the response, a 4-byte big-endian int in the request
//!
//! `RequestChallenge`'s constructor builds the handshake reply as
//! `String.format("\t%s%d\u0000", ident, challenge)` — a **tab byte** (`0x09`,
//! doubling as the type byte), the client's own 4 session-id bytes reinterpreted
//! as text, the challenge as **decimal digits with no fixed width** (`nextInt`
//! over `[0, 16_777_216)`, so 1–8 digits, never zero-padded), then a null
//! terminator. The client is expected to parse those digits back into an
//! integer and send *that* integer as a plain 4-byte **big-endian** field
//! (`PktUtils.intFromNetworkByteArray`) in its stat request — [`mint_challenge`]
//! and [`QuerySession::challenge`] follow that split: minted and stored as a
//! `u32`, formatted as ASCII decimal (no padding) into the handshake reply, and
//! compared as a parsed big-endian integer against the stat request's token
//! field. The previous version of this module minted and compared a **fixed
//! 4-byte ASCII digit string**, which cannot represent a real GameSpy4 client's
//! challenge value at all once it exceeds 4 digits (holds for roughly 90% of
//! draws from a 24-bit range).
//!
//! ### The basic form has no keys, and its port is a raw `u16`, not a string
//!
//! `processPacket`'s short-form branch writes seven **positional** fields —
//! `motd`, the literal `"SMP"`, `map`, `numplayers` (decimal string),
//! `maxplayers` (decimal string), `hostport`, `hostip` — with **no key names at
//! all**; the key/value encoding is a full-stat-only concept. `hostport` is
//! written with `writeShort`, which XORs the usual byte order twice
//! (`Short.reverseBytes` into a big-endian `DataOutputStream.writeShort`,
//! netting **little-endian**) — the only field in the whole protocol that is
//! not a null-terminated string. [`build_basic_stat`] reproduces exactly this
//! shape; the previous version encoded the basic form as seven key/value pairs
//! (the *full*-stat shape) with `hostport` decimal-stringified, which no real
//! basic-stat parser expects.
//!
//! ## How it works
//!
//! One [`QueryServer`] owns one UDP socket plus a [`QueryConfig`] of static
//! identity fields; the live player list is read from the shared
//! [`PlayerRegistry`] at request time, so the online count and names are real —
//! unlike a status reply, which must report `0` because a status connection is
//! its own, registry-less session (see `crate::server`'s comment on that). A
//! [`QuerySession`] inside the run loop holds the current challenge token:
//! minted per handshake, replaced by each later one, and required byte-for-byte
//! on every stat request, so a third party cannot spoof a stat reply to a
//! handshake it never made.
//!
//! [`handle_request`] is the whole protocol in one pure function — bytes in,
//! reply bytes (or nothing, for malformed input) out. [`QueryServer::run`] is a
//! thin `recv_from`/`send_to` wrapper around it, so the wire logic is
//! unit-testable without a socket.
//!
//! ## How to change it
//!
//! * The reply *content* is [`QueryConfig`]. Change a default there, or have the
//!   caller supply its own config.
//! * The wire layout lives in [`handle_request`] and the two private response
//!   builders. The layouts are pinned by the protocol, so a change there is a
//!   protocol change, not a refactor — re-derive from `QueryThreadGs4` /
//!   `NetworkDataOutputStream` in `.cache/mc/26.2/src`, the same source this
//!   module's own layout was hand-expanded from, rather than from a
//!   description; see the module doc's "Corrected against the real protocol"
//!   section for what reading a summary instead of the writer cost here.
//! * Malformed input must stay a silent drop (`None`): never a panic, and never
//!   an allocation scaled by attacker input. The reply is sized by `config` and
//!   `player_names` (both trusted), not by the datagram — the server-plumbing
//!   epic's standing rule for bytes from a stranger.
//!
//! ## Configuration
//!
//! [`QueryConfig`] fields. The port is whatever [`QueryServer::bind`] is given;
//! [`IntegratedServer::bind`](crate::IntegratedServer::bind) hands it the same
//! address it binds its TCP game socket to, so a host on port 25565 also answers
//! query on UDP 25565 without conflict (TCP and UDP port spaces are
//! independent). A query-port failure is deliberately non-fatal to the game: it
//! is logged and the query side simply does not come up.
//!
//! ## Dependencies
//!
//! [`crate::players::PlayerRegistry`] (the live player list),
//! [`crate::mob_spawn::SpawnRng`] (challenge-token minting), and tokio's
//! `UdpSocket` — native-only, which is why the whole socket half of this module
//! is `#[cfg(not(target_arch = "wasm32"))]`; the protocol logic in
//! [`handle_request`] has no socket and compiles everywhere.

use crate::mob_spawn::SpawnRng;
use crate::players::PlayerRegistry;
use crate::server::{STATUS_MAX_PLAYERS, STATUS_MOTD};

/// The two-byte magic that starts every query request (`0xFE 0xFD`).
const MAGIC: [u8; 2] = [0xFE, 0xFD];
/// Handshake request type: asks for a challenge token.
const TYPE_HANDSHAKE: u8 = 0x09;
/// The **only** stat request type on the wire. Vanilla's `processPacket`
/// switches on `buf[2]` with exactly two live arms (`0` and `9`); basic vs.
/// full stat is a length distinction inside this one arm, not a second type —
/// see the module doc.
const TYPE_STAT: u8 = 0x00;
/// The exact request length (magic + type + session id + challenge token +
/// 4 padding bytes) that selects the full form. Anything else with
/// [`TYPE_STAT`] is the basic form, mirroring `15 == len` in
/// `QueryThreadGs4.processPacket`.
const FULL_STAT_REQUEST_LEN: usize = 15;
/// The literal two bytes vanilla writes after `"splitnum\0"` in a full-stat
/// reply (`write(128); write(0);`) — not a null-terminated string "value" the
/// way every other key in the section is, which is why it is not built with
/// [`write_kv`].
const SPLITNUM_VALUE: [u8; 2] = [0x80, 0x00];
/// The game id reported in a full-stat reply. Vanilla's value, fixed by the
/// protocol's audience, not ours to change.
const GAME_ID: &str = "MINECRAFT";
/// Upper bound (exclusive) for a minted challenge, `nextInt(1 << 24)` in
/// `RequestChallenge`'s constructor. Every value fits in 3 bytes, so the
/// 4-byte big-endian field a stat request carries it back in always has a
/// leading zero byte — this module never relies on that, but it is why a
/// hand-built fixture can safely use a full `u32` field for it.
const CHALLENGE_BOUND: i32 = 1 << 24;

/// The default human-facing game-version string in a full-stat reply.
///
/// This crate is **version-free**: it must never name a protocol number (see
/// [`crate::protocol::ServerProtocol::encode_status_response`]'s doc comment
/// for the same rule), so the default names the engine rather than a game
/// version, exactly as [`STATUS_MOTD`] does. A host that wants to advertise an
/// exact Minecraft version supplies its own [`QueryConfig::version`].
pub const DEFAULT_QUERY_VERSION: &str = "Lodestone";

/// The default game type, vanilla's `gametype` field. Vanilla itself never
/// varies this (`processPacket` and `buildRuleResponse` both write the
/// literal `"SMP"` rather than reading a config value), but nothing about the
/// wire format requires that, so [`QueryConfig::game_type`] stays a real field
/// rather than a hardcoded literal here.
pub const DEFAULT_GAME_TYPE: &str = "SMP";

/// The default level name (`map` in both stat forms). Vanilla's default world
/// folder is `world` (`DedicatedServerProperties.level-name`), and this crate
/// has no world-name concept of its own yet.
pub const DEFAULT_MAP: &str = "world";

/// Every piece of static identity a query reply carries. The dynamic state —
/// online count and player names — is read from the live [`PlayerRegistry`] at
/// request time, not cached here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryConfig {
    /// The MOTD (`motd` in the basic form, `hostname` in the full form).
    pub motd: String,
    /// The game type (`gametype`, vanilla's `SMP`).
    pub game_type: String,
    /// The level name (`map` in both forms).
    pub map: String,
    /// The human-facing game-version string (full form only).
    pub version: String,
    /// The host IP string (`hostip` in both forms).
    pub host_ip: String,
    /// The host's *game* port (`hostport` in both forms) — the port a joining
    /// client dials, which is not necessarily the query listener's own port.
    /// Written as a raw little-endian `u16` in the basic form and as a decimal
    /// string in the full form; see the module doc.
    pub host_port: u16,
    /// The player cap (`maxplayers` in both forms).
    pub max_players: usize,
    /// The plugin list (`plugins`, full form only), joined as
    /// `"name: version; …"` in the reply. Empty when nothing is installed.
    pub plugins: Vec<String>,
}

impl Default for QueryConfig {
    /// Host-facing defaults, reusing the crate's canonical MOTD and player cap
    /// (the same numbers the join sequence and status reply already report, so
    /// a host does not advertise a cap its game does not honour). `host_ip` /
    /// `host_port` are placeholders — [`QueryServer::bind`] callers override
    /// them with the address actually bound.
    fn default() -> Self {
        Self {
            motd: STATUS_MOTD.to_owned(),
            game_type: DEFAULT_GAME_TYPE.to_owned(),
            map: DEFAULT_MAP.to_owned(),
            version: DEFAULT_QUERY_VERSION.to_owned(),
            host_ip: "0.0.0.0".to_owned(),
            host_port: 0,
            max_players: STATUS_MAX_PLAYERS as usize,
            plugins: Vec::new(),
        }
    }
}

/// Per-listener state that survives across datagrams.
#[derive(Debug)]
pub struct QuerySession {
    /// The current challenge, minted by the latest handshake. `None` until
    /// the first handshake, so a stat request that somehow arrives first is
    /// dropped rather than answered with a stale token. Stored as the numeric
    /// value — the handshake reply formats it as decimal text and a stat
    /// request carries it as a 4-byte big-endian integer; see the module doc
    /// for why those are two different encodings of the same number.
    challenge: Option<u32>,
    /// Mint for challenge values. SplitMix64 ([`SpawnRng`]) seeded from the
    /// wall clock, so a third party cannot predict the next token — the whole
    /// point of a challenge-response.
    rng: SpawnRng,
    /// The last minted value, so a repeat draw can be nudged and consecutive
    /// handshakes never hand a client the same challenge twice. Vanilla does
    /// not bother with this (`RandomSource.nextInt` unmodified); it costs
    /// nothing to keep here and makes
    /// [`consecutive_handshakes_mint_different_tokens`](tests::consecutive_handshakes_mint_different_tokens)
    /// a real assertion instead of a flaky one.
    last_value: u32,
}

impl QuerySession {
    /// A session seeded from the wall clock. The production entry point.
    #[must_use]
    pub fn new() -> Self {
        let seed = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self::with_seed(seed)
    }

    /// A session with a caller-chosen RNG seed. [`new`](Self::new) is the
    /// production entry point; a fixed seed makes the token sequence
    /// deterministic for tests.
    #[must_use]
    pub fn with_seed(seed: u64) -> Self {
        Self {
            challenge: None,
            rng: SpawnRng::new(seed),
            last_value: u32::MAX,
        }
    }
}

impl Default for QuerySession {
    fn default() -> Self {
        Self::new()
    }
}

/// Handles one inbound query datagram and returns the bytes to send back, or
/// `None` for a datagram to drop silently.
///
/// # Malformed input
///
/// Anything that is not a well-formed request for [`TYPE_HANDSHAKE`] or
/// [`TYPE_STAT`] — wrong magic, an unknown type, a packet too short to carry
/// its fields, a stat request with a missing or mismatched challenge — is
/// dropped as `None`, matching vanilla's `default: return true;` arm (no
/// reply). It never panics and never allocates in proportion to the input:
/// the reply is sized by `config` and `player_names` (both trusted), not by
/// the datagram. See the module doc for why that is a hard rule here.
///
/// `player_names` is the current roster, in any order; the basic form uses only
/// its length, the full form writes each name.
pub fn handle_request(
    packet: &[u8],
    session: &mut QuerySession,
    config: &QueryConfig,
    player_names: &[String],
) -> Option<Vec<u8>> {
    // Every request is at least a 2-byte magic + 1-byte type + 4-byte session
    // id; anything shorter cannot be one of ours.
    if packet.len() < 7 || packet[0] != MAGIC[0] || packet[1] != MAGIC[1] {
        return None;
    }
    // The session id is the client's own 4 bytes, echoed verbatim so the
    // client can correlate the reply.
    let session_id: [u8; 4] = packet[3..7].try_into().ok()?;
    match packet[2] {
        TYPE_HANDSHAKE => {
            let token = mint_challenge(session);
            session.challenge = Some(token);
            Some(build_handshake_response(&session_id, token))
        }
        TYPE_STAT => {
            // A stat request must prove the current challenge, carried as a
            // 4-byte big-endian integer right after the session id
            // (`PktUtils.intFromNetworkByteArray`).
            if packet.len() < 11 {
                return None;
            }
            let claimed = u32::from_be_bytes(packet[7..11].try_into().ok()?);
            if session.challenge != Some(claimed) {
                return None;
            }
            if packet.len() == FULL_STAT_REQUEST_LEN {
                Some(build_full_stat(&session_id, config, player_names))
            } else {
                Some(build_basic_stat(&session_id, config, player_names.len()))
            }
        }
        _ => None,
    }
}

/// Mints a fresh challenge value in `[0, 2^24)`, matching
/// `RandomSource.nextInt(16_777_216)` in vanilla's `RequestChallenge`
/// constructor. A draw that would repeat the previous value is nudged, so two
/// handshakes in a row always produce visibly different challenges (vanilla
/// does not guarantee this; see [`QuerySession::last_value`]'s doc for why it
/// costs nothing to guarantee here anyway).
fn mint_challenge(session: &mut QuerySession) -> u32 {
    let mut value = session.rng.next_int(CHALLENGE_BOUND) as u32;
    if value == session.last_value {
        value = (value + 1) % CHALLENGE_BOUND as u32;
    }
    session.last_value = value;
    value
}

/// Appends `s` and a null terminator — the protocol's only string encoding.
fn write_cstring(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// Appends one null-terminated key and one null-terminated value.
fn write_kv(out: &mut Vec<u8>, key: &str, value: &str) {
    write_cstring(out, key);
    write_cstring(out, value);
}

/// The handshake reply: `0x09` (the tab byte inside vanilla's
/// `"\t%s%d\u0000"` format string doubles as the type here), the echoed
/// session id, the challenge as **unpadded decimal digits**, and a null
/// terminator. Not zero-padded to any fixed width — `RequestChallenge` builds
/// this with `String.format`'s plain `%d`, so a small challenge value is a
/// short reply.
fn build_handshake_response(session_id: &[u8; 4], token: u32) -> Vec<u8> {
    let digits = token.to_string();
    let mut out = Vec::with_capacity(1 + 4 + digits.len() + 1);
    out.push(TYPE_HANDSHAKE);
    out.extend_from_slice(session_id);
    out.extend_from_slice(digits.as_bytes());
    out.push(0x00);
    out
}

/// The short form: [`TYPE_STAT`] + session id, then seven **positional**
/// fields with no key names — `motd`, the literal game type, `map`,
/// `numplayers`, `maxplayers`, `hostip` and `hostport` — matching
/// `processPacket`'s inline short-form writer exactly, field for field.
/// `hostport` is the one non-string field in the whole protocol: a raw
/// little-endian `u16` (`writeShort` double-reverses vanilla's usual
/// big-endian `DataOutputStream` order — see the module doc).
fn build_basic_stat(session_id: &[u8; 4], config: &QueryConfig, online: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(TYPE_STAT);
    out.extend_from_slice(session_id);
    write_cstring(&mut out, &config.motd);
    write_cstring(&mut out, &config.game_type);
    write_cstring(&mut out, &config.map);
    write_cstring(&mut out, &online.to_string());
    write_cstring(&mut out, &config.max_players.to_string());
    out.extend_from_slice(&config.host_port.to_le_bytes());
    write_cstring(&mut out, &config.host_ip);
    out
}

/// The long form: [`TYPE_STAT`] + session id, the `"splitnum"` data-source
/// marker (a null-terminated key followed by the two literal bytes `0x80
/// 0x00` — not a string "value", see [`SPLITNUM_VALUE`]'s doc), the identity
/// fields as key/value pairs, a section terminator, the `0x01` + `"player_"`
/// preamble, one null-terminated name per player, then a final terminator —
/// matching `buildRuleResponse` byte for byte.
fn build_full_stat(session_id: &[u8; 4], config: &QueryConfig, players: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(TYPE_STAT);
    out.extend_from_slice(session_id);
    write_cstring(&mut out, "splitnum");
    out.extend_from_slice(&SPLITNUM_VALUE);
    write_kv(&mut out, "hostname", &config.motd);
    write_kv(&mut out, "gametype", &config.game_type);
    write_kv(&mut out, "game_id", GAME_ID);
    write_kv(&mut out, "version", &config.version);
    write_kv(&mut out, "plugins", &config.plugins.join("; "));
    write_kv(&mut out, "map", &config.map);
    write_kv(&mut out, "numplayers", &players.len().to_string());
    write_kv(&mut out, "maxplayers", &config.max_players.to_string());
    write_kv(&mut out, "hostport", &config.host_port.to_string());
    write_kv(&mut out, "hostip", &config.host_ip);
    out.push(0x00); // end of key-value section
    // Player list preamble: `write(1)` then a "player_" key with an empty
    // value (a lone extra `\0`, not `write_kv`'s two terminators — vanilla
    // writes the key string and one more raw byte, not a second string).
    out.push(0x01);
    write_cstring(&mut out, "player_");
    out.push(0x00);
    for name in players {
        write_cstring(&mut out, name);
    }
    out.push(0x00);
    out
}

/// A live query listener: one UDP socket answering handshake / basic-stat /
/// full-stat requests for a [`QueryConfig`] and a shared [`PlayerRegistry`].
///
/// Native targets only — the socket half of this module does not exist on wasm32
/// (tokio has no `net` there); the protocol logic in [`handle_request`] does, so
/// it is what the wasm build still compiles. See the module doc.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct QueryServer {
    socket: tokio::net::UdpSocket,
    config: QueryConfig,
    players: PlayerRegistry,
}

#[cfg(not(target_arch = "wasm32"))]
impl QueryServer {
    /// Binds the listener to `addr`. The caller typically passes the same
    /// address it bound its TCP game socket to: UDP and TCP port spaces are
    /// independent, so the query listener shares the game port without
    /// conflict — vanilla's own default (query port = server port).
    ///
    /// # Errors
    ///
    /// Returns the [`std::io::Error`] from binding the UDP socket.
    pub async fn bind<A>(
        addr: A,
        config: QueryConfig,
        players: PlayerRegistry,
    ) -> std::io::Result<Self>
    where
        A: tokio::net::ToSocketAddrs,
    {
        let socket = tokio::net::UdpSocket::bind(addr).await?;
        Ok(Self {
            socket,
            config,
            players,
        })
    }

    /// The bound local address — for tests binding port `0`, and for a caller
    /// that wants to report the query port it ended up on.
    ///
    /// # Errors
    ///
    /// Returns the [`std::io::Error`] from reading the socket's address.
    #[must_use]
    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.socket.local_addr()
    }

    /// Serves requests until the socket errors, then returns. Aborted by
    /// whoever spawned it (an `IntegratedServer` races it against its shutdown
    /// `Notify`), so a bound socket never leaks.
    pub async fn run(self) {
        let mut session = QuerySession::new();
        // Requests are tiny (the largest, a full-stat request, is 15 bytes), so
        // one modest buffer is all the receive side needs; anything larger is
        // truncated and fails the parse, which is a drop, not a fault.
        let mut buf = [0u8; 2048];
        loop {
            let Ok((len, peer)) = self.socket.recv_from(&mut buf).await else {
                return;
            };
            let players = self
                .players
                .view(None)
                .roster
                .iter()
                .map(|p| p.username.clone())
                .collect::<Vec<_>>();
            if let Some(reply) = handle_request(&buf[..len], &mut session, &self.config, &players) {
                if self.socket.send_to(&reply, peer).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn test_config() -> QueryConfig {
        QueryConfig {
            motd: "A Lodestone Server".to_owned(),
            game_type: "SMP".to_owned(),
            map: "world".to_owned(),
            version: "Lodestone".to_owned(),
            host_ip: "127.0.0.1".to_owned(),
            host_port: 25565,
            max_players: 20,
            plugins: vec!["a-plugin: 1.0".to_owned(), "b-plugin: 2.0".to_owned()],
        }
    }

    fn session() -> QuerySession {
        // A fixed seed so a test that depends on token values is deterministic.
        QuerySession::with_seed(0xC0FFEE)
    }

    /// Runs a handshake and returns the minted challenge, asserting the
    /// reply's shape along the way: type byte, echoed session id, unpadded
    /// ASCII decimal digits, null terminator — no fixed width assumed, since
    /// vanilla's own reply is not fixed-width either.
    fn handshake(session: &mut QuerySession) -> u32 {
        let reply = handle_request(
            &[0xFE, 0xFD, TYPE_HANDSHAKE, 1, 2, 3, 4],
            session,
            &test_config(),
            &[],
        )
        .expect("a handshake must always be answered");
        assert_eq!(reply[0], TYPE_HANDSHAKE);
        assert_eq!(&reply[1..5], &[1, 2, 3, 4]);
        assert_eq!(*reply.last().unwrap(), 0x00, "the challenge is null-terminated");
        let digits = &reply[5..reply.len() - 1];
        assert!(!digits.is_empty(), "the challenge must not be an empty string");
        assert!(
            digits.iter().all(u8::is_ascii_digit),
            "the challenge must be plain ASCII decimal digits, got {digits:?}"
        );
        assert!(
            digits.len() == 1 || digits[0] != b'0',
            "a multi-digit challenge must not be zero-padded, got {digits:?}"
        );
        std::str::from_utf8(digits)
            .expect("ascii digits are valid utf8")
            .parse()
            .expect("digits parse as u32")
    }

    /// A stat request carrying `token` as the 4-byte big-endian field a real
    /// client sends — distinct from the handshake reply's own ASCII-text
    /// encoding of the same number, which is the whole point of this pair of
    /// helpers existing separately from a single "token bytes" concept.
    fn basic_request(token: u32, session_id: [u8; 4]) -> Vec<u8> {
        let mut packet = vec![0xFE, 0xFD, TYPE_STAT];
        packet.extend_from_slice(&session_id);
        packet.extend_from_slice(&token.to_be_bytes());
        packet
    }

    fn full_request(token: u32, session_id: [u8; 4]) -> Vec<u8> {
        let mut packet = basic_request(token, session_id);
        packet.extend_from_slice(&[0, 0, 0, 0]); // full-stat padding, len 15
        assert_eq!(packet.len(), FULL_STAT_REQUEST_LEN);
        packet
    }

    /// Reads one null-terminated string, advancing the cursor past it.
    fn take_cstr<'a>(body: &mut &'a [u8]) -> String {
        let end = body
            .iter()
            .position(|&b| b == 0)
            .expect("every string in a stat reply is null-terminated");
        let s = std::str::from_utf8(&body[..end])
            .expect("stat reply text is ASCII")
            .to_owned();
        *body = &body[end + 1..];
        s
    }

    /// Walks a `key\0value\0key\0value\0…` stream, stopping at the first empty
    /// key (the `\0` terminator that ends every stat section).
    fn parse_kv(body: &[u8]) -> HashMap<String, String> {
        let mut pairs = HashMap::new();
        let mut cursor = body;
        loop {
            let key = take_cstr(&mut cursor);
            if key.is_empty() {
                return pairs;
            }
            let value = take_cstr(&mut cursor);
            pairs.insert(key, value);
        }
    }

    #[test]
    fn handshake_echoes_type_and_session_and_mints_a_challenge() {
        let mut s = session();
        let token = handshake(&mut s);
        assert!(token < CHALLENGE_BOUND as u32);
    }

    #[test]
    fn consecutive_handshakes_mint_different_tokens() {
        let mut s = session();
        let first = handshake(&mut s);
        let second = handshake(&mut s);
        assert_ne!(first, second, "a re-handshake must not repeat the challenge");
    }

    #[test]
    fn basic_stat_has_no_keys_and_a_little_endian_port() {
        let mut s = session();
        let token = handshake(&mut s);
        let players = vec!["Alice".to_owned(), "Bob".to_owned()];
        let reply = handle_request(
            &basic_request(token, [1, 2, 3, 4]),
            &mut s,
            &test_config(),
            &players,
        )
        .expect("a basic stat with the right challenge must be answered");
        assert_eq!(reply[0], TYPE_STAT, "the reply's own type byte, not an echo of the request form");
        assert_eq!(&reply[1..5], &[1, 2, 3, 4], "session id is echoed");

        let mut cursor = &reply[5..];
        assert_eq!(take_cstr(&mut cursor), "A Lodestone Server", "motd, unlabelled");
        assert_eq!(take_cstr(&mut cursor), "SMP", "gametype, unlabelled");
        assert_eq!(take_cstr(&mut cursor), "world", "map, unlabelled");
        assert_eq!(take_cstr(&mut cursor), "2", "numplayers, unlabelled");
        assert_eq!(take_cstr(&mut cursor), "20", "maxplayers, unlabelled");
        // hostport: raw little-endian u16, not a string — the one
        // non-string, unlabelled field left after the four cstrings above.
        assert_eq!(cursor[0..2], 25565u16.to_le_bytes(), "hostport, little-endian, not text");
        cursor = &cursor[2..];
        assert_eq!(take_cstr(&mut cursor), "127.0.0.1", "hostip, unlabelled");
        assert!(cursor.is_empty(), "exactly seven positional fields, nothing after hostip");
    }

    #[test]
    fn full_stat_includes_the_player_list_and_extra_fields() {
        let mut s = session();
        let token = handshake(&mut s);
        let players = vec!["Alice".to_owned(), "Bob".to_owned()];
        let reply = handle_request(
            &full_request(token, [9, 8, 7, 6]),
            &mut s,
            &test_config(),
            &players,
        )
        .expect("a full stat with the right challenge must be answered");
        assert_eq!(reply[0], TYPE_STAT, "the reply's own type byte, not a distinct 'full' type");
        assert_eq!(&reply[1..5], &[9, 8, 7, 6], "session id is echoed");

        let mut cursor = &reply[5..];
        assert_eq!(take_cstr(&mut cursor), "splitnum");
        assert_eq!(cursor[0..2], SPLITNUM_VALUE, "the two literal bytes, not a \"127\" string");
        cursor = &cursor[2..];

        // Find the key/value section's own terminator (single `\0`) by
        // re-parsing with `parse_kv`, which stops there.
        let kvs = parse_kv(cursor);
        assert_eq!(kvs.get("hostname").map(String::as_str), Some("A Lodestone Server"));
        assert_eq!(kvs.get("gametype").map(String::as_str), Some("SMP"));
        assert_eq!(kvs.get("game_id").map(String::as_str), Some("MINECRAFT"));
        assert_eq!(kvs.get("version").map(String::as_str), Some("Lodestone"));
        assert_eq!(
            kvs.get("plugins").map(String::as_str),
            Some("a-plugin: 1.0; b-plugin: 2.0")
        );
        assert_eq!(kvs.get("map").map(String::as_str), Some("world"));
        assert_eq!(kvs.get("numplayers").map(String::as_str), Some("2"));
        assert_eq!(kvs.get("maxplayers").map(String::as_str), Some("20"));
        assert_eq!(kvs.get("hostport").map(String::as_str), Some("25565"), "hostport is text here, unlike the basic form");
        assert_eq!(kvs.get("hostip").map(String::as_str), Some("127.0.0.1"));
        assert_eq!(kvs.len(), 10, "ten key/value pairs, not counting splitnum");

        // Skip back over the section to find the player-list preamble: walk
        // the same pairs again to find the byte offset just past the
        // section terminator.
        let mut walk = cursor;
        loop {
            let key = take_cstr(&mut walk);
            if key.is_empty() {
                break;
            }
            let _ = take_cstr(&mut walk);
        }
        // `walk` now starts at the `0x01` player-list preamble byte.
        assert_eq!(walk[0], 0x01, "player-list preamble's leading byte");
        walk = &walk[1..];
        assert_eq!(take_cstr(&mut walk), "player_");
        assert_eq!(walk[0], 0x00, "the lone extra byte after \"player_\", not a second cstring");
        walk = &walk[1..];
        let mut names = Vec::new();
        loop {
            let name = take_cstr(&mut walk);
            if name.is_empty() {
                break;
            }
            names.push(name);
        }
        assert_eq!(names, vec!["Alice".to_owned(), "Bob".to_owned()]);
        assert!(walk.is_empty(), "nothing follows the final terminator");
    }

    #[test]
    fn a_stat_request_without_a_valid_challenge_is_dropped() {
        let mut s = session();
        // No handshake at all: dropped.
        assert_eq!(
            handle_request(&basic_request(0, [1, 2, 3, 4]), &mut s, &test_config(), &[]),
            None
        );
        // Handshake, then a stat carrying a wrong challenge: dropped.
        let token = handshake(&mut s);
        let wrong = token.wrapping_add(1);
        assert_eq!(
            handle_request(&full_request(wrong, [1, 2, 3, 4]), &mut s, &test_config(), &[]),
            None
        );
        // The right challenge still works afterwards: the failed attempt did
        // not consume or invalidate it.
        assert!(
            handle_request(&basic_request(token, [1, 2, 3, 4]), &mut s, &test_config(), &[]).is_some()
        );
    }

    #[test]
    fn a_handshake_supersedes_the_previous_challenge() {
        let mut s = session();
        let first = handshake(&mut s);
        handshake(&mut s);
        // The earlier challenge is now stale: dropped.
        assert_eq!(
            handle_request(&basic_request(first, [1, 2, 3, 4]), &mut s, &test_config(), &[]),
            None
        );
    }

    /// A request with a real (but unimplemented) alternate stat type is
    /// dropped exactly like any other unknown byte — matching vanilla's
    /// `default: return true;` fallthrough, which sends nothing. This is
    /// also the regression case for the bug this module used to have: an
    /// earlier version *answered* `0x01` (with the wrong-shaped reply, on
    /// top of it), which a real GameSpy4 client would never even send.
    #[test]
    fn a_non_zero_non_handshake_type_is_dropped() {
        let mut s = session();
        let token = handshake(&mut s);
        let mut packet = vec![0xFE, 0xFD, 0x01, 1, 2, 3, 4];
        packet.extend_from_slice(&token.to_be_bytes());
        assert_eq!(handle_request(&packet, &mut s, &test_config(), &[]), None);
    }

    #[test]
    fn malformed_and_unknown_packets_are_dropped() {
        let mut s = session();
        let cases: &[&[u8]] = &[
            &[],                                    // empty
            &[0xFE, 0xFD],                          // magic only
            &[0xFE, 0xFD, TYPE_HANDSHAKE, 1, 2, 3], // handshake short of its session id
            &[0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06], // wrong magic
            &[0xFE, 0xFD, 0x42, 1, 2, 3, 4],        // unknown type
            &[0xFE, 0xFD, TYPE_STAT, 1, 2, 3, 4],   // stat short of its challenge
        ];
        for case in cases {
            assert_eq!(
                handle_request(case, &mut s, &test_config(), &[]),
                None,
                "must drop {case:02x?}"
            );
        }
    }

    /// End-to-end over a real UDP socket: bind a [`QueryServer`] on an
    /// OS-assigned port and drive the full handshake → basic-stat sequence from
    /// a client socket. Native only — the socket half is wasm-gated.
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn serves_handshake_and_basic_stat_over_udp() {
        let server = QueryServer::bind(
            "127.0.0.1:0",
            test_config(),
            PlayerRegistry::new(),
        )
        .await
        .expect("query listener binds");
        let port = server.local_addr().expect("bound address").port();
        let task = tokio::spawn(server.run());

        let client = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("client socket");
        client
            .send_to(&[0xFE, 0xFD, TYPE_HANDSHAKE, 1, 2, 3, 4], ("127.0.0.1", port))
            .await
            .expect("handshake sent");
        let mut buf = [0u8; 2048];
        let (len, _) = client.recv_from(&mut buf).await.expect("handshake reply");
        assert_eq!(buf[0], TYPE_HANDSHAKE);
        assert_eq!(&buf[1..5], &[1, 2, 3, 4]);
        assert_eq!(buf[len - 1], 0x00, "null-terminated challenge");
        let token: u32 = std::str::from_utf8(&buf[5..len - 1])
            .expect("ascii digits")
            .parse()
            .expect("challenge parses as u32");

        let request = basic_request(token, [1, 2, 3, 4]);
        client
            .send_to(&request, ("127.0.0.1", port))
            .await
            .expect("basic stat sent");
        let (len, _) = client.recv_from(&mut buf).await.expect("basic stat reply");
        let reply = &buf[..len];
        assert_eq!(reply[0], TYPE_STAT);
        assert_eq!(&reply[1..5], &[1, 2, 3, 4]);

        task.abort();
    }
}
