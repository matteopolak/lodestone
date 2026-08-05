//! The GameSpy4 / UT3 server-query protocol (issue #332).
//!
//! ## What it is
//!
//! A small, version-independent UDP protocol used by server-list aggregators and
//! hosting panels to read a Minecraft server's identity without joining it: a
//! challenge-response dance over three packet types.
//!
//! * A **handshake** (type `0x09`) asks for a challenge token.
//! * A **basic stat** request (type `0x00`) proves the token and gets the short
//!   form: MOTD, game type, map, online/max players, host IP/port.
//! * A **full stat** request (type `0x01`) proves the token and gets the long
//!   form: everything in the short form plus the player-name list, plugins,
//!   game id and version.
//!
//! The protocol is deliberately **not** Minecraft-version-specific (the issue
//! calls out that verification needs a reference query client, not a live
//! oracle), so nothing here names a protocol number or a packet id — the same
//! "stat" language GameSpy used for Quake and Unreal servers.
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
//!   protocol change, not a refactor — check a reference query client, not a
//!   live oracle (see the issue's verification note).
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
/// Basic-stat request type.
const TYPE_BASIC_STAT: u8 = 0x00;
/// Full-stat request type.
const TYPE_FULL_STAT: u8 = 0x01;
/// The data-source identifier in a full-stat reply. `"127"` selects the UTF-8
/// key-value encoding this whole reply uses; it is the same magic value every
/// GameSpy4 implementation sends, and reference clients parse past it.
const STAT_SOURCE: &str = "127";
/// The game id reported in a full-stat reply. Vanilla's value, fixed by the
/// protocol's audience, not ours to change.
const GAME_ID: &str = "MINECRAFT";

/// The default human-facing game-version string in a full-stat reply.
///
/// This crate is **version-free**: it must never name a protocol number (see
/// [`crate::protocol::ServerProtocol::encode_status_response`]'s doc comment
/// for the same rule), so the default names the engine rather than a game
/// version, exactly as [`STATUS_MOTD`] does. A host that wants to advertise an
/// exact Minecraft version supplies its own [`QueryConfig::version`].
pub const DEFAULT_QUERY_VERSION: &str = "Lodestone";

/// The default game type, vanilla's `gametype` field (`SMP`).
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
    /// The current challenge token, minted by the latest handshake. `None`
    /// until the first handshake, so a stat request that somehow arrives first
    /// is dropped rather than answered with a stale token.
    challenge: Option<[u8; 4]>,
    /// Mint for challenge tokens. SplitMix64 ([`SpawnRng`]) seeded from the
    /// wall clock, so a third party cannot predict the next token — the whole
    /// point of a challenge-response.
    rng: SpawnRng,
    /// The last minted token's numeric value, so a repeat draw can be nudged
    /// and consecutive handshakes never hand a client the same token twice.
    last_value: u32,
}

impl QuerySession {
    /// A session seeded from the wall clock. The production entry point.
    #[must_use]
    pub fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
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
/// Anything that is not a well-formed request for one of the three packet
/// types — wrong magic, an unknown type, a packet too short to carry its
/// fields, a stat request with a missing or mismatched challenge token — is
/// dropped as `None`. It never panics and never allocates in proportion to the
/// input: the reply is sized by `config` and `player_names` (both trusted), not
/// by the datagram. See the module doc for why that is a hard rule here.
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
            // type + session id + token + null terminator.
            let mut out = Vec::with_capacity(10);
            out.push(TYPE_HANDSHAKE);
            out.extend_from_slice(&session_id);
            out.extend_from_slice(&token);
            out.push(0x00);
            Some(out)
        }
        TYPE_BASIC_STAT | TYPE_FULL_STAT => {
            // A stat request must prove the current challenge token, which
            // follows the session id. The full-stat request carries a trailing
            // padding byte that the protocol ignores; only these 11 bytes are
            // validated.
            if packet.len() < 11
                || session.challenge
                    != Some(packet[7..11].try_into().ok()?)
            {
                return None;
            }
            if packet[2] == TYPE_BASIC_STAT {
                Some(build_basic_stat(&session_id, config, player_names.len()))
            } else {
                Some(build_full_stat(&session_id, config, player_names))
            }
        }
        _ => None,
    }
}

/// Mints a fresh 4-digit ASCII challenge token.
///
/// Vanilla sends a random decimal written as text (`Random.nextInt(1 << 31)` in
/// `QueryResponseHandler`); the protocol fixes "ASCII digits + null", not the
/// width, and 4 digits is the width every reference query client parses. A draw
/// that would repeat the previous token is nudged, so two handshakes in a row
/// always produce visibly different challenges.
fn mint_challenge(session: &mut QuerySession) -> [u8; 4] {
    let mut value = session.rng.next_int(10_000) as u32;
    if value == session.last_value {
        value = (value + 1) % 10_000;
    }
    session.last_value = value;
    [
        b'0' + (value / 1000) as u8,
        b'0' + ((value / 100) % 10) as u8,
        b'0' + ((value / 10) % 10) as u8,
        b'0' + (value % 10) as u8,
    ]
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

/// The short form: `type + session id + stat separator`, then exactly seven
/// key-value pairs, then the section terminator.
fn build_basic_stat(session_id: &[u8; 4], config: &QueryConfig, online: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(TYPE_BASIC_STAT);
    out.extend_from_slice(session_id);
    out.push(0x01); // stat separator
    write_kv(&mut out, "motd", &config.motd);
    write_kv(&mut out, "gametype", &config.game_type);
    write_kv(&mut out, "map", &config.map);
    write_kv(&mut out, "numplayers", &online.to_string());
    write_kv(&mut out, "maxplayers", &config.max_players.to_string());
    write_kv(&mut out, "hostport", &config.host_port.to_string());
    write_kv(&mut out, "hostip", &config.host_ip);
    out.push(0x00); // end of key-value section
    out
}

/// The long form: `type + session id + stat separator`, the data-source
/// identifier (`splitnum`/`127`) and its separator, the identity fields plus
/// game id/version/plugins, then the player-name list.
fn build_full_stat(session_id: &[u8; 4], config: &QueryConfig, players: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(TYPE_FULL_STAT);
    out.extend_from_slice(session_id);
    out.push(0x01); // stat separator
    // Data source identifier, then a second separator.
    write_kv(&mut out, "splitnum", STAT_SOURCE);
    out.push(0x01);
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
    // Player list: a "player_" key with an empty value, then one
    // null-terminated name per player, then the final terminator.
    write_kv(&mut out, "player_", "");
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
        // Requests are tiny (the largest, a full-stat request, is 12 bytes), so
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

    /// Runs a handshake and returns the minted token, asserting the reply's
    /// fixed fields along the way.
    fn handshake(session: &mut QuerySession) -> [u8; 4] {
        let reply = handle_request(
            &[0xFE, 0xFD, TYPE_HANDSHAKE, 1, 2, 3, 4],
            session,
            &test_config(),
            &[],
        )
        .expect("a handshake must always be answered");
        assert_eq!(reply.len(), 10);
        assert_eq!(reply[0], TYPE_HANDSHAKE);
        assert_eq!(&reply[1..5], &[1, 2, 3, 4]);
        assert!(reply[5..9].iter().all(|b| b.is_ascii_digit()));
        assert_eq!(reply[9], 0x00, "the token is null-terminated");
        reply[5..9].try_into().expect("4 bytes")
    }

    fn basic_request(token: &[u8; 4], session_id: [u8; 4]) -> Vec<u8> {
        let mut packet = vec![0xFE, 0xFD, TYPE_BASIC_STAT];
        packet.extend_from_slice(&session_id);
        packet.extend_from_slice(token);
        packet
    }

    fn full_request(token: &[u8; 4], session_id: [u8; 4]) -> Vec<u8> {
        let mut packet = vec![0xFE, 0xFD, TYPE_FULL_STAT];
        packet.extend_from_slice(&session_id);
        packet.extend_from_slice(token);
        packet.push(0x00); // full-stat padding byte
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

    /// The data a full-stat reply must carry, parsed out of the raw bytes.
    struct FullStat {
        source: (String, String),
        kvs: HashMap<String, String>,
        players: Vec<String>,
    }

    fn parse_full_stat(reply: &[u8]) -> FullStat {
        assert_eq!(reply[0], TYPE_FULL_STAT);
        let mut body = &reply[6..];
        let source = (take_cstr(&mut body), take_cstr(&mut body));
        assert_eq!(body[0], 0x01, "a second separator follows the source id");
        body = &body[1..];
        let kvs = parse_kv(body);
        // After the key-value terminator: "player_\0" + ""\0 + names + "\0".
        let mut cursor = body;
        loop {
            let key = take_cstr(&mut cursor);
            if key.is_empty() {
                break;
            }
            let _ = take_cstr(&mut cursor);
        }
        assert_eq!(take_cstr(&mut cursor), "player_");
        assert!(take_cstr(&mut cursor).is_empty(), "player_ key has an empty value");
        let mut players = Vec::new();
        loop {
            let name = take_cstr(&mut cursor);
            if name.is_empty() {
                break;
            }
            players.push(name);
        }
        assert!(cursor.is_empty(), "nothing follows the final terminator");
        FullStat {
            source,
            kvs,
            players,
        }
    }

    #[test]
    fn handshake_echoes_type_and_session_and_mints_a_token() {
        let mut s = session();
        let token = handshake(&mut s);
        assert_eq!(token.len(), 4);
    }

    #[test]
    fn consecutive_handshakes_mint_different_tokens() {
        let mut s = session();
        let first = handshake(&mut s);
        let second = handshake(&mut s);
        assert_ne!(first, second, "a re-handshake must not repeat the token");
    }

    #[test]
    fn basic_stat_reports_identity_and_counts() {
        let mut s = session();
        let token = handshake(&mut s);
        let players = vec!["Alice".to_owned(), "Bob".to_owned()];
        let reply = handle_request(
            &basic_request(&token, [1, 2, 3, 4]),
            &mut s,
            &test_config(),
            &players,
        )
        .expect("a basic stat with the right token must be answered");
        assert_eq!(reply[0], TYPE_BASIC_STAT);
        assert_eq!(&reply[1..5], &[1, 2, 3, 4], "session id is echoed");
        assert_eq!(reply[5], 0x01, "stat separator");
        assert_eq!(reply.last(), Some(&0x00), "section terminator");
        let kvs = parse_kv(&reply[6..]);
        assert_eq!(kvs.get("motd").map(String::as_str), Some("A Lodestone Server"));
        assert_eq!(kvs.get("gametype").map(String::as_str), Some("SMP"));
        assert_eq!(kvs.get("map").map(String::as_str), Some("world"));
        assert_eq!(kvs.get("numplayers").map(String::as_str), Some("2"));
        assert_eq!(kvs.get("maxplayers").map(String::as_str), Some("20"));
        assert_eq!(kvs.get("hostport").map(String::as_str), Some("25565"));
        assert_eq!(kvs.get("hostip").map(String::as_str), Some("127.0.0.1"));
        assert_eq!(kvs.len(), 7, "basic stat has exactly the seven fixed fields");
    }

    #[test]
    fn full_stat_includes_the_player_list_and_extra_fields() {
        let mut s = session();
        let token = handshake(&mut s);
        let players = vec!["Alice".to_owned(), "Bob".to_owned()];
        let reply = handle_request(
            &full_request(&token, [9, 8, 7, 6]),
            &mut s,
            &test_config(),
            &players,
        )
        .expect("a full stat with the right token must be answered");
        assert_eq!(reply[0], TYPE_FULL_STAT);
        assert_eq!(&reply[1..5], &[9, 8, 7, 6], "session id is echoed");
        let parsed = parse_full_stat(&reply);
        assert_eq!(parsed.source, ("splitnum".to_owned(), "127".to_owned()));
        assert_eq!(
            parsed.kvs.get("hostname").map(String::as_str),
            Some("A Lodestone Server")
        );
        assert_eq!(parsed.kvs.get("game_id").map(String::as_str), Some("MINECRAFT"));
        assert_eq!(parsed.kvs.get("version").map(String::as_str), Some("Lodestone"));
        assert_eq!(
            parsed.kvs.get("plugins").map(String::as_str),
            Some("a-plugin: 1.0; b-plugin: 2.0")
        );
        assert_eq!(parsed.kvs.get("numplayers").map(String::as_str), Some("2"));
        assert_eq!(parsed.players, vec!["Alice".to_owned(), "Bob".to_owned()]);
    }

    #[test]
    fn a_stat_request_without_a_valid_token_is_dropped() {
        let mut s = session();
        // No handshake at all: dropped.
        assert_eq!(
            handle_request(&basic_request(&[b'0'; 4], [1, 2, 3, 4]), &mut s, &test_config(), &[]),
            None
        );
        // Handshake, then a stat carrying a wrong token: dropped.
        let token = handshake(&mut s);
        let wrong = [token[0], token[1], token[2], token[3].wrapping_add(1)];
        assert_eq!(
            handle_request(&full_request(&wrong, [1, 2, 3, 4]), &mut s, &test_config(), &[]),
            None
        );
        // The right token still works afterwards: the failed attempt did not
        // consume or invalidate the challenge.
        assert!(
            handle_request(&basic_request(&token, [1, 2, 3, 4]), &mut s, &test_config(), &[]).is_some()
        );
    }

    #[test]
    fn a_handshake_supersedes_the_previous_token() {
        let mut s = session();
        let first = handshake(&mut s);
        handshake(&mut s);
        // The earlier token is now stale: dropped.
        assert_eq!(
            handle_request(&basic_request(&first, [1, 2, 3, 4]), &mut s, &test_config(), &[]),
            None
        );
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
            &[0xFE, 0xFD, TYPE_BASIC_STAT, 1, 2, 3, 4], // stat short of its token
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
        assert_eq!(len, 10);
        let token: [u8; 4] = buf[5..9].try_into().expect("4 token bytes");

        let request = basic_request(&token, [1, 2, 3, 4]);
        client
            .send_to(&request, ("127.0.0.1", port))
            .await
            .expect("basic stat sent");
        let (len, _) = client.recv_from(&mut buf).await.expect("basic stat reply");
        let reply = &buf[..len];
        assert_eq!(reply[0], TYPE_BASIC_STAT);
        assert_eq!(&reply[1..5], &[1, 2, 3, 4]);
        assert_eq!(reply.last(), Some(&0x00));

        task.abort();
    }
}
