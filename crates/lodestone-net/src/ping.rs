//! Server List Ping: query a server's status (MOTD, version, player counts)
//! without logging in.
//!
//! Two protocols exist. The **modern** one (1.7+) is a normal handshake with
//! `next_state = 1`, then a status request/response exchange of JSON, then an
//! optional ping/pong for latency. It reuses the ordinary [`Connection`] and
//! runs before any compression or encryption, so it composes with everything
//! else for free and is testable over an in-memory transport.
//!
//! The **legacy** one (`0xFE 0x01`) predates the JSON protocol; some old servers
//! and proxies still answer it. Its response is a `0xFF` kick packet carrying a
//! `§`-delimited, UTF-16BE payload. The parser here is pure and unit-tested
//! against synthetic bytes; the socket I/O is a thin native wrapper.

use lodestone_core::{Reader, Writer};

use crate::connection::Connection;
use crate::error::{NetError, Result};
use crate::transport::Transport;

/// Packet id of the handshake (state 0) and status request/response (state 1).
const ID_HANDSHAKE: i32 = 0x00;
/// Packet id of the ping/pong exchange.
const ID_PING: i32 = 0x01;
/// `next_state` value selecting the status protocol.
const STATE_STATUS: i32 = 1;
/// Maximum characters accepted in a status-response JSON string.
const MAX_STATUS_CHARS: usize = 262_144;

/// A parsed modern status response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusResponse {
    /// The raw status JSON exactly as the server sent it.
    ///
    /// It is left unparsed on purpose: the JSON schema (description, players,
    /// version, favicon, mod lists) varies across versions and forks, so the
    /// caller decodes only the fields it needs.
    pub json: String,
    /// Round-trip latency of the ping/pong exchange, in milliseconds, when a
    /// ping was performed and timing is available (never on `wasm32`).
    pub latency_ms: Option<u64>,
}

/// A modern Server List Ping client.
#[derive(Debug, Clone, Copy)]
pub struct ServerListPing {
    protocol_version: i32,
}

impl Default for ServerListPing {
    fn default() -> Self {
        // -1 is the conventional "version-agnostic" value for a status ping;
        // vanilla ignores it in the status state.
        Self {
            protocol_version: -1,
        }
    }
}

impl ServerListPing {
    /// Creates a pinger advertising `protocol_version` in the handshake.
    #[must_use]
    pub fn new(protocol_version: i32) -> Self {
        Self { protocol_version }
    }

    /// Performs a status exchange over an already-connected transport.
    ///
    /// `host`/`port` are echoed into the handshake exactly as the client dialed
    /// them, which matters for servers that virtual-host on the address field.
    /// This drives handshake → status request → status response → ping/pong.
    ///
    /// # Errors
    ///
    /// Returns a [`NetError`] on I/O failure, an unexpected packet id, an early
    /// EOF, or a malformed status string.
    pub async fn status_over<T: Transport>(
        &self,
        conn: &mut Connection<T>,
        host: &str,
        port: u16,
    ) -> Result<StatusResponse> {
        let mut hs = Writer::default();
        hs.var_i32(self.protocol_version);
        hs.string(host);
        hs.u16(port);
        hs.var_i32(STATE_STATUS);
        conn.write_packet(ID_HANDSHAKE, hs.as_slice()).await?;

        // Status request: empty body.
        conn.write_packet(ID_HANDSHAKE, &[]).await?;

        let (id, fields) = conn
            .read_packet()
            .await?
            .ok_or(NetError::UnexpectedClose(0))?;
        if id != ID_HANDSHAKE {
            return Err(NetError::MalformedFrame(
                "status response had unexpected packet id",
            ));
        }
        let mut reader = Reader::new(&fields);
        let json = reader.string(MAX_STATUS_CHARS)?;

        let latency_ms = self.ping_pong(conn).await?;

        Ok(StatusResponse { json, latency_ms })
    }

    /// Sends a ping with a nonce and awaits the matching pong, returning the
    /// measured latency in milliseconds when timing is available.
    async fn ping_pong<T: Transport>(&self, conn: &mut Connection<T>) -> Result<Option<u64>> {
        let nonce: i64 = 0x0000_5AFE_D00D_1234;
        let mut body = Writer::default();
        body.i64(nonce);

        #[cfg(not(target_arch = "wasm32"))]
        let start = std::time::Instant::now();

        conn.write_packet(ID_PING, body.as_slice()).await?;

        let (id, fields) = conn
            .read_packet()
            .await?
            .ok_or(NetError::UnexpectedClose(0))?;
        if id != ID_PING {
            return Err(NetError::MalformedFrame("expected pong packet"));
        }
        let echoed = Reader::new(&fields).i64()?;
        if echoed != nonce {
            return Err(NetError::MalformedFrame("pong nonce did not match ping"));
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
            Ok(Some(ms))
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(None)
        }
    }

    /// Resolves and connects to `host` (with an optional explicit `port`), then
    /// performs a status ping.
    ///
    /// SRV records are honored per the vanilla rules (see [`crate::resolve`]).
    ///
    /// # Errors
    ///
    /// Returns a [`NetError`] on DNS, connect, I/O, or protocol failure.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn status(&self, host: &str, port: Option<u16>) -> Result<StatusResponse> {
        let addr = crate::resolve::resolve_server_address(host, port).await?;
        let mut conn = Connection::connect(addr.socket_addr()).await?;
        // Report the address the user dialed, not the SRV target.
        self.status_over(
            &mut conn,
            host,
            port.unwrap_or(crate::resolve::DEFAULT_PORT),
        )
        .await
    }
}

/// A parsed legacy (`0xFE 0x01`) status response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyStatus {
    /// Server protocol version number, when present in the modern legacy format.
    pub protocol_version: Option<i32>,
    /// Human-readable server version string, when present.
    pub server_version: Option<String>,
    /// The message of the day.
    pub motd: String,
    /// Current online player count.
    pub online_players: i32,
    /// Maximum player slots.
    pub max_players: i32,
}

/// The two bytes that request a legacy status ping.
#[must_use]
pub fn encode_legacy_ping_request() -> [u8; 2] {
    [0xFE, 0x01]
}

/// Parses a legacy status response packet (starting with the `0xFF` kick id).
///
/// Supports the 1.4+ format `§1\0<proto>\0<version>\0<motd>\0<online>\0<max>`
/// and the older plain `<motd>§<online>§<max>` format.
///
/// # Errors
///
/// Returns [`NetError::MalformedFrame`] if the packet id, length prefix, field
/// layout, or numeric fields are invalid.
pub fn parse_legacy_status(packet: &[u8]) -> Result<LegacyStatus> {
    // [0xFF][u16 length in UTF-16 code units][UTF-16BE payload]
    if packet.len() < 3 || packet[0] != 0xFF {
        return Err(NetError::MalformedFrame(
            "legacy status: bad packet id or length",
        ));
    }
    let units = u16::from_be_bytes([packet[1], packet[2]]) as usize;
    let payload = &packet[3..];
    if payload.len() != units * 2 {
        return Err(NetError::MalformedFrame(
            "legacy status: payload length mismatch",
        ));
    }
    let mut u16s = Vec::with_capacity(units);
    for chunk in payload.chunks_exact(2) {
        u16s.push(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    let text = String::from_utf16(&u16s)
        .map_err(|_| NetError::MalformedFrame("legacy status: invalid UTF-16"))?;

    let parse_i32 = |s: &str| -> Result<i32> {
        s.trim()
            .parse::<i32>()
            .map_err(|_| NetError::MalformedFrame("legacy status: non-integer field"))
    };

    if let Some(rest) = text.strip_prefix("\u{00a7}1\u{0000}") {
        // Modern legacy format, NUL-separated.
        let parts: Vec<&str> = rest.split('\u{0000}').collect();
        if parts.len() != 5 {
            return Err(NetError::MalformedFrame(
                "legacy status: expected 5 NUL-separated fields",
            ));
        }
        Ok(LegacyStatus {
            protocol_version: parts[0].trim().parse::<i32>().ok(),
            server_version: Some(parts[1].to_owned()),
            motd: parts[2].to_owned(),
            online_players: parse_i32(parts[3])?,
            max_players: parse_i32(parts[4])?,
        })
    } else {
        // Pre-1.4 format: MOTD§online§max.
        let parts: Vec<&str> = text.split('\u{00a7}').collect();
        if parts.len() != 3 {
            return Err(NetError::MalformedFrame("legacy status: expected 3 fields"));
        }
        Ok(LegacyStatus {
            protocol_version: None,
            server_version: None,
            motd: parts[0].to_owned(),
            online_players: parse_i32(parts[1])?,
            max_players: parse_i32(parts[2])?,
        })
    }
}

/// Performs a legacy (`0xFE 0x01`) status ping over an already-connected
/// transport, returning the parsed response.
///
/// This is the pre-JSON protocol; prefer [`ServerListPing`] for modern servers
/// and fall back to this only when the modern handshake is refused.
///
/// # Errors
///
/// Returns a [`NetError`] on I/O failure, EOF, or a malformed response packet.
pub async fn legacy_status_over<T: Transport>(transport: &mut T) -> Result<LegacyStatus> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    transport.write_all(&encode_legacy_ping_request()).await?;
    transport.flush().await?;

    let mut buf = Vec::new();
    transport.read_to_end(&mut buf).await?;
    parse_legacy_status(&buf)
}

/// Connects to `host`/`port` and performs a legacy status ping.
///
/// # Errors
///
/// Returns a [`NetError`] on connect, I/O, or parse failure.
#[cfg(not(target_arch = "wasm32"))]
pub async fn legacy_status(host: &str, port: u16) -> Result<LegacyStatus> {
    let mut stream = tokio::net::TcpStream::connect((host, port)).await?;
    legacy_status_over(&mut stream).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::memory_pair;

    /// Encodes a legacy `0xFF` response packet from a payload string.
    fn legacy_packet(payload: &str) -> Vec<u8> {
        let units: Vec<u16> = payload.encode_utf16().collect();
        let mut p = vec![0xFF];
        p.extend_from_slice(&(units.len() as u16).to_be_bytes());
        for u in units {
            p.extend_from_slice(&u.to_be_bytes());
        }
        p
    }

    #[test]
    fn legacy_request_is_fe_01() {
        assert_eq!(encode_legacy_ping_request(), [0xFE, 0x01]);
    }

    #[test]
    fn parses_modern_legacy_format() {
        let packet =
            legacy_packet("\u{00a7}1\u{0000}127\u{0000}1.20.1\u{0000}A Server\u{0000}3\u{0000}20");
        let s = parse_legacy_status(&packet).unwrap();
        assert_eq!(s.protocol_version, Some(127));
        assert_eq!(s.server_version.as_deref(), Some("1.20.1"));
        assert_eq!(s.motd, "A Server");
        assert_eq!(s.online_players, 3);
        assert_eq!(s.max_players, 20);
    }

    #[test]
    fn parses_pre_1_4_legacy_format() {
        let packet = legacy_packet("Old MOTD\u{00a7}5\u{00a7}10");
        let s = parse_legacy_status(&packet).unwrap();
        assert_eq!(s.protocol_version, None);
        assert_eq!(s.motd, "Old MOTD");
        assert_eq!(s.online_players, 5);
        assert_eq!(s.max_players, 10);
    }

    #[test]
    fn rejects_wrong_packet_id() {
        let mut packet = legacy_packet("A\u{00a7}1\u{00a7}2");
        packet[0] = 0x00;
        assert!(matches!(
            parse_legacy_status(&packet),
            Err(NetError::MalformedFrame(_))
        ));
    }

    #[test]
    fn rejects_truncated_payload() {
        let mut packet = legacy_packet("A\u{00a7}1\u{00a7}2");
        packet.pop();
        assert!(matches!(
            parse_legacy_status(&packet),
            Err(NetError::MalformedFrame(_))
        ));
    }

    #[tokio::test]
    async fn modern_status_round_trip_over_memory() {
        let (client_io, server_io) = memory_pair();

        // Fake status server: read handshake + request, reply, echo ping.
        let server = tokio::spawn(async move {
            let mut server = Connection::new(server_io);
            let (hs_id, hs_fields) = server.read_packet().await.unwrap().unwrap();
            assert_eq!(hs_id, ID_HANDSHAKE);
            // Verify the handshake decodes and selects status.
            let mut r = Reader::new(&hs_fields);
            let _proto = r.var_i32().unwrap();
            let addr = r.string(255).unwrap();
            let port = r.u16().unwrap();
            let next = r.var_i32().unwrap();
            assert_eq!(addr, "mc.example.com");
            assert_eq!(port, 25565);
            assert_eq!(next, STATE_STATUS);

            let (req_id, req_fields) = server.read_packet().await.unwrap().unwrap();
            assert_eq!(req_id, ID_HANDSHAKE);
            assert!(req_fields.is_empty());

            let mut resp = Writer::default();
            resp.string("{\"description\":\"hi\"}");
            server
                .write_packet(ID_HANDSHAKE, resp.as_slice())
                .await
                .unwrap();

            let (ping_id, ping_fields) = server.read_packet().await.unwrap().unwrap();
            assert_eq!(ping_id, ID_PING);
            server.write_packet(ID_PING, &ping_fields).await.unwrap();
        });

        let mut client = Connection::new(client_io);
        let status = ServerListPing::new(770)
            .status_over(&mut client, "mc.example.com", 25565)
            .await
            .unwrap();
        assert_eq!(status.json, "{\"description\":\"hi\"}");
        assert!(status.latency_ms.is_some());

        server.await.unwrap();
    }

    #[tokio::test]
    async fn modern_status_rejects_bad_ping_echo() {
        let (client_io, server_io) = memory_pair();

        let server = tokio::spawn(async move {
            let mut server = Connection::new(server_io);
            let _ = server.read_packet().await.unwrap().unwrap();
            let _ = server.read_packet().await.unwrap().unwrap();
            let mut resp = Writer::default();
            resp.string("{}");
            server
                .write_packet(ID_HANDSHAKE, resp.as_slice())
                .await
                .unwrap();
            let _ = server.read_packet().await.unwrap().unwrap();
            // Echo a wrong nonce.
            let mut bad = Writer::default();
            bad.i64(0);
            server.write_packet(ID_PING, bad.as_slice()).await.unwrap();
        });

        let mut client = Connection::new(client_io);
        let err = ServerListPing::default()
            .status_over(&mut client, "h", 25565)
            .await
            .unwrap_err();
        assert!(matches!(err, NetError::MalformedFrame(_)));
        server.await.unwrap();
    }
}
