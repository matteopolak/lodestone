//! `lodestone-relay` — a deliberately protocol-blind WebSocket→TCP relay.
//!
//! Browsers cannot open raw TCP sockets, but a real Minecraft server speaks only
//! raw TCP. This crate bridges the gap: it accepts a WebSocket connection from a
//! browser (or a native [`lodestone_net::WsTransport`]) and opens a plain TCP
//! connection to the real server, then shuttles bytes between the two.
//!
//! # Why it must stay dumb
//!
//! The relay never parses a Minecraft packet. Each inbound binary WebSocket frame
//! is written to TCP verbatim, and TCP bytes are forwarded back as binary frames.
//! Because Minecraft's framing is length-prefixed and carried entirely inside the
//! byte stream, the codec on the client reassembles frames regardless of how the
//! relay chunks them — so the relay needs no knowledge of packet ids, states, or
//! versions. That is the whole point: **one relay serves every protocol version
//! and every server.** The moment it inspected a packet it would become a
//! per-version component, and we would need one per protocol.
//!
//! # The destination is per-connection, not per-relay-process
//!
//! This used to be a single fixed `target` chosen at process startup (`--target`)
//! and shared by every client — a point-to-point bridge, not a router. That was
//! wrong the moment a browser session needed to reach more than one backend (the
//! multiplayer server list can hold several rows), and it failed silently rather
//! than loudly: every row pinged the *same* server underneath, so a made-up
//! address like `hypixel.net` showed a real, live MOTD that happened to belong to
//! whatever `--target` pointed at instead. Every existing live-oracle gate also
//! pointed the relay's `--target` and the fixture server at the same host, so
//! nothing in the corpus could see the bug — the shared-coincidence blindness
//! this repo's `CLAUDE.md` names elsewhere, one level lower.
//!
//! The destination now travels with each connection as a `host`/`port` query
//! parameter on the WebSocket upgrade request, e.g.
//! `ws://127.0.0.1:25580/relay?host=hypixel.net&port=25565`. [`destination_from_query`]
//! is the only place this crate looks at anything beyond the WebSocket handshake
//! itself, and it is pure HTTP-layer addressing — a query string, read the same
//! way an ordinary reverse proxy reads a `Host` header or a path prefix — not a
//! byte of Minecraft protocol. The "why it must stay dumb" property above is
//! unaffected: the relay still never looks past the WebSocket handshake into the
//! byte stream it forwards.
//!
//! A query parameter was chosen over two other shapes that were considered:
//! a path segment (`/relay/<host>/<port>`) would need every proxy in front of the
//! relay — trunk's dev proxy, and any future reverse proxy in a real deployment —
//! to forward a *wildcard* path rather than the one fixed `/relay` this repo
//! already commits to keeping stable; a query string rides on that same fixed
//! path unchanged. A first-frame preamble (read the destination as the first
//! bytes of the WebSocket byte stream, before starting the raw pipe) would work
//! too, but it turns "malformed addressing" and "the Minecraft server behaved
//! oddly" into the same failure shape from the client's point of view — both
//! would just be bytes that arrived at the wrong time — where a query parameter
//! fails at the HTTP handshake, before any bytes belonging to the Minecraft
//! session exist at all. See [`bridge`] for what an unresolvable destination
//! does: the handshake itself is refused with `400 Bad Request`, visibly and
//! immediately, never a hang and never a silent fallback to *some* server.
//!
//! `--target` did not disappear; see [`serve`]'s doc for what it means now.
//!
//! # This is an open, unauthenticated forwarder — by design, scoped by bind address
//!
//! Accepting an arbitrary destination per connection makes the relay exactly as
//! free as the Minecraft client it stands in for: a real client can point itself
//! at any `host:port` the player types in, and restricting this relay to a fixed
//! allowlist would break the feature it exists to provide (joining whatever
//! server the browser's multiplayer menu names). So there is deliberately **no
//! allowlist and no default-deny** on the destination — anyone who can complete a
//! WebSocket handshake against `--listen` can make this process open a TCP
//! connection to anywhere that process can reach.
//!
//! The trust boundary is therefore `--listen`'s bind address, unchanged by this
//! work: the shipped defaults (this crate's own `127.0.0.1:25580`, and every
//! `just run-wasm`/`just run-relay` invocation in this repo) bind loopback-only,
//! so only processes on the same machine can reach it at all. Binding `--listen`
//! to a non-loopback address turns this into a general-purpose open TCP forwarder
//! reachable from wherever that address is routable — a real capability change,
//! and deliberately left as the operator's decision rather than guarded here,
//! the same way a plain SSH `-D` SOCKS proxy or `socat` pipe is not
//! destination-restricted by default either.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;

/// Size of the scratch buffer used when forwarding TCP → WebSocket.
const FORWARD_CHUNK: usize = 16 * 1024;

/// Reads `host`/`port` off a WebSocket upgrade request's query string.
///
/// Returns `None` if either parameter is missing or `port` does not parse as a
/// `u16` — both are treated identically by [`bridge`] (fall through to
/// `default_target`, or refuse the connection), so this does not need to
/// distinguish "absent" from "malformed".
///
/// Values are percent-decoded (see [`percent_decode`]) so a destination with a
/// character `application/x-www-form-urlencoded` would escape — unusual for a
/// hostname, but IPv6 literals and some internationalized domain names need it,
/// and the decode is cheap enough to always apply rather than special-case.
#[must_use]
fn destination_from_query(query: Option<&str>) -> Option<(String, u16)> {
    let query = query?;
    let mut host: Option<String> = None;
    let mut port: Option<u16> = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "host" => host = Some(percent_decode(value)),
            "port" => port = percent_decode(value).parse().ok(),
            _ => {}
        }
    }
    let host = host?;
    let port = port?;
    if host.is_empty() {
        return None;
    }
    Some((host, port))
}

/// Minimal percent-decoding for query-parameter values (`%XX` → byte, `+` →
/// space). Not a full URL-parsing library — this crate takes no such dependency
/// (see the module doc's "why it must stay dumb") — just enough to round-trip
/// what [`destination_from_query`]'s counterpart on the client
/// (`crate::platform::relay::relay_ws_url_for` in `lodestone-shell`) encodes.
/// An incomplete or invalid `%XX` escape is passed through literally rather than
/// dropped, so a malformed destination surfaces as "no such host" from the
/// eventual TCP dial rather than as silently truncated input.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Accepts WebSocket clients on `listener` forever, bridging each to a fresh TCP
/// connection to its own resolved destination.
///
/// Every accepted connection is handled on its own task, so one slow or stuck
/// client never blocks another.
///
/// `default_target` is a fallback used only when a connection's own upgrade
/// request carries no (or an unparseable) `host`/`port` query pair — see
/// [`destination_from_query`]. This is what `--target` now means on the CLI
/// (`main.rs`): a convenience default for callers that do not yet send a
/// destination (existing native tests in this crate, and any ad hoc `ws://`
/// client), not the only server this process can ever reach. Pass `None` to
/// require every connection to name its own destination — refused with `400 Bad
/// Request` otherwise, per [`bridge`].
///
/// # Errors
///
/// Returns an error only if accepting a connection fails fatally; per-client
/// bridge errors are logged and do not stop the loop.
pub async fn serve(listener: TcpListener, default_target: Option<String>) -> Result<()> {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!(%error, "accept failed");
                continue;
            }
        };
        let default_target = default_target.clone();
        tokio::spawn(async move {
            match bridge(stream, peer, default_target.as_deref()).await {
                Ok(()) => tracing::info!(%peer, "bridge closed"),
                Err(error) => tracing::warn!(%peer, %error, "bridge ended with error"),
            }
        });
    }
}

/// Bridges one accepted WebSocket client to a fresh TCP connection to its own
/// resolved destination.
///
/// The destination is read from the WebSocket upgrade request's `host`/`port`
/// query parameters (see [`destination_from_query`]), falling back to
/// `default_target` when the request carries neither. **When neither source
/// resolves a destination, the handshake is refused outright** — `400 Bad
/// Request`, sent back over the still-open TCP connection before this function
/// ever tries to reach a Minecraft server — rather than silently doing nothing
/// or hanging: a client with no destination has no server to be surprised by.
///
/// # Errors
///
/// Returns an error if the WebSocket handshake fails (including "no
/// destination"), the target is unreachable, or forwarding fails in a way other
/// than a clean close.
pub async fn bridge(stream: TcpStream, peer: SocketAddr, default_target: Option<&str>) -> Result<()> {
    stream.set_nodelay(true).ok();

    // The callback runs synchronously, inside `accept_hdr_async`, before this
    // function sees its result — a `Mutex` rather than a `RefCell` because the
    // enclosing future is spawned with `tokio::spawn` and so must stay `Send`
    // across the `.await` below.
    let resolved: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let resolved_in_callback = Arc::clone(&resolved);
    let default_target = default_target.map(str::to_owned);

    let ws = tokio_tungstenite::accept_hdr_async(
        stream,
        move |request: &Request, response: Response| {
            let target = destination_from_query(request.uri().query())
                .map(|(host, port)| format!("{host}:{port}"))
                .or(default_target);
            match target {
                Some(target) => {
                    *resolved_in_callback.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(target);
                    Ok(response)
                }
                None => {
                    let body = "no destination: connect with ?host=<host>&port=<port>, or \
                                 start the relay with --target for a default"
                        .to_string();
                    let rejection: ErrorResponse = tokio_tungstenite::tungstenite::http::Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Some(body))
                        .expect("a fixed status and a plain string body always build");
                    Err(rejection)
                }
            }
        },
    )
    .await
    .context("websocket handshake failed (including: no destination given)")?;

    // The callback above only returns `Ok` after storing a target, so this is
    // always `Some` once `accept_hdr_async` itself returned `Ok`.
    let target = resolved
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .expect("accept_hdr_async succeeded, so the callback resolved and stored a target");

    let tcp = TcpStream::connect(&target)
        .await
        .with_context(|| format!("failed to reach target {target}"))?;
    tcp.set_nodelay(true).ok();
    tracing::info!(%peer, %target, "bridge open");

    let (mut ws_tx, mut ws_rx) = ws.split();
    let (mut tcp_rd, mut tcp_wr) = tcp.into_split();

    // WebSocket -> TCP: unwrap each binary frame straight onto the socket.
    let ws_to_tcp = async move {
        while let Some(message) = ws_rx.next().await {
            match message? {
                Message::Binary(data) => tcp_wr.write_all(&data).await?,
                Message::Close(_) => break,
                // Control/text frames are not part of the byte pipe; ignore them.
                Message::Ping(_) | Message::Pong(_) | Message::Text(_) | Message::Frame(_) => {}
            }
        }
        tcp_wr.shutdown().await.ok();
        Ok::<(), anyhow::Error>(())
    };

    // TCP -> WebSocket: forward raw bytes as binary frames.
    let tcp_to_ws = async move {
        let mut buf = vec![0u8; FORWARD_CHUNK];
        loop {
            let n = tcp_rd.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            ws_tx.send(Message::Binary(buf[..n].to_vec())).await?;
        }
        ws_tx.close().await.ok();
        Ok::<(), anyhow::Error>(())
    };

    // When either direction finishes (server closed, client left, error), the
    // whole bridge is done; the other future is dropped.
    tokio::select! {
        result = ws_to_tcp => result,
        result = tcp_to_ws => result,
    }
}
