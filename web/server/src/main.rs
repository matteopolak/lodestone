//! `lodestone-web-server` — serves the built browser page and the WebSocket→TCP
//! relay from **one listener**, so a deployed build (and `just run-wasm`) never
//! needs a second port, a proxy, or a second long-lived process.
//!
//! ```text
//! lodestone-web-server --listen 127.0.0.1:8080 --dist dist --target 127.0.0.1:25565
//! ```
//!
//! `GET /relay` is a WebSocket upgrade, bridged byte-for-byte to a destination
//! resolved exactly the way the standalone `lodestone-relay` crate's own
//! `bridge`/`serve` resolve it: a `host`/`port` query parameter on the
//! upgrade request first, falling back to this binary's `--target` when
//! absent — see "Per-connection destination" below. Every other request is
//! served as a static file out of `--dist` (the directory `trunk
//! build`/`trunk watch` writes to), falling back to `index.html` for
//! anything not found.
//!
//! # This replaces `web/Trunk.toml`'s `[[proxies]]` entry, not `trunk`
//!
//! `trunk serve` bundled a dev HTTP server, a filesystem watcher and a
//! same-origin `/relay` proxy into one process, at the cost of that proxy
//! being a `trunk serve`-only feature — a built `dist/` served any other way
//! had no `/relay` at all. This binary is the deployable half of that split:
//! pair it with `trunk watch` (which only rebuilds `dist/`, never serves) to
//! reproduce the same "one command, rebuilds on change, one port" workflow —
//! see `scripts/run-wasm.sh` — and it is *also* what a real deployment runs,
//! which `trunk serve` categorically could not be.
//!
//! # Why the relay bridge is re-expressed here instead of calling
//! [`lodestone_relay::bridge`]
//!
//! `bridge` takes ownership of a raw, un-upgraded `TcpStream` and performs its
//! own `tokio_tungstenite::accept_async` HTTP handshake. That is the right
//! shape for a listener whose only job is relaying, but wrong here: axum has
//! to see and route the request (`GET /relay` vs. everything else) before any
//! WebSocket handshake happens, so by the time this file has a socket to
//! bridge, axum has already completed the upgrade itself and hands back its
//! own `axum::extract::ws::WebSocket`/`Message` types — wire-compatible with
//! `tokio-tungstenite`'s, but a different Rust type, so `bridge` cannot be
//! called on it. [`bridge`] below is the ~25 lines that have to be
//! re-expressed per WebSocket library; `lodestone-relay` stays a real
//! dependency of this binary (the target address, the byte-pipe design its
//! own doc comment explains) rather than a spawned child.
//!
//! # Per-connection destination
//!
//! `lodestone-relay`'s `bridge`/`serve` no longer bridge every connection to
//! one process-wide target: each WebSocket upgrade can carry its own
//! `?host=<host>&port=<port>` query pair (see that crate's module doc,
//! "The destination is per-connection, not per-relay-process"), with
//! `--target`/`default_target` used only when a connection's request carries
//! neither. [`destination_from_query`] and [`percent_decode`] below
//! re-implement that crate's identically-named **private** helpers (not
//! `pub`, so they cannot be imported) for exactly this reason — this binary's
//! `/relay` route has to honour the same contract the client
//! (`crate::platform::relay::relay_ws_url_for` in `lodestone-shell`) encodes
//! into the URL, or every row in the multiplayer list would silently resolve
//! to this binary's fixed `--target` again, regardless of which server a row
//! actually named — the exact bug that per-connection routing exists to fix.
//! Keep this in lockstep with `lodestone-relay`'s copy by inspection; nothing
//! here enforces it at compile time since the helper is not shared code.
//!
//! # Port selection
//!
//! `--listen` defaults to a fixed, documented `127.0.0.1:8080` so the URL is
//! predictable. Passing port `0` asks the OS for a free port instead (the
//! conflict case owner asked for); pass `--port-file <path>` to have the
//! *actually bound* port written there as a bare decimal — the file is the
//! machine-readable answer, since a shell pipeline is not a reliable way to
//! recover a value like this (see this repo's own hazard notes on `| head`
//! and `| grep | tail`).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use axum::Router;
#[cfg(feature = "multiplayer")]
use axum::extract::{RawQuery, State};
#[cfg(feature = "multiplayer")]
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderName, HeaderValue};
#[cfg(feature = "multiplayer")]
use axum::response::IntoResponse;
#[cfg(feature = "multiplayer")]
use axum::routing::get;
#[cfg(feature = "multiplayer")]
use futures_util::{SinkExt, StreamExt};
#[cfg(feature = "multiplayer")]
use lodestone_relay::destination_from_query;
#[cfg(feature = "multiplayer")]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
#[cfg(feature = "multiplayer")]
use tokio::net::TcpStream;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;

/// Size of the scratch buffer used when forwarding TCP -> WebSocket. Matches
/// `lodestone_relay::FORWARD_CHUNK`, which is not itself `pub`.
#[cfg(feature = "multiplayer")]
const FORWARD_CHUNK: usize = 16 * 1024;

struct Config {
    listen: String,
    dist: PathBuf,
    #[cfg(feature = "multiplayer")]
    target: String,
    port_file: Option<PathBuf>,
}

fn parse_config() -> Result<Config> {
    let mut listen = "127.0.0.1:8080".to_string();
    let mut dist = PathBuf::from("dist");
    #[cfg(feature = "multiplayer")]
    let mut target = "127.0.0.1:25565".to_string();
    let mut port_file = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = args.next().context("--listen needs a value")?,
            "--dist" => dist = PathBuf::from(args.next().context("--dist needs a value")?),
            #[cfg(feature = "multiplayer")]
            "--target" => target = args.next().context("--target needs a value")?,
            #[cfg(not(feature = "multiplayer"))]
            "--target" => anyhow::bail!(
                "--target requires the `multiplayer` Cargo feature; this server is static-only"
            ),
            "--port-file" => {
                port_file = Some(PathBuf::from(
                    args.next().context("--port-file needs a value")?,
                ));
            }
            "-h" | "--help" => {
                #[cfg(feature = "multiplayer")]
                eprintln!(
                    "lodestone-web-server --listen <addr:port> --dist <dir> --target <host:port> [--port-file <path>]\n\
                     \n\
                     Serves the built browser page (--dist, default ./dist) and the\n\
                     WebSocket->TCP relay under /relay (bridged to --target, a real\n\
                     Minecraft server) from one listener.\n\
                     \n\
                     --listen 127.0.0.1:0 asks the OS for a free port. Pass --port-file\n\
                     to have the actually-bound port written there as a bare decimal, for\n\
                     a script to read without a pipeline."
                );
                #[cfg(not(feature = "multiplayer"))]
                eprintln!(
                    "lodestone-web-server --listen <addr:port> --dist <dir> [--port-file <path>]\n\
                     \n\
                     Serves the built browser page (--dist, default ./dist). This\n\
                     build is static-only because the multiplayer Cargo feature is\n\
                     disabled.\n\
                     \n\
                     --listen 127.0.0.1:0 asks the OS for a free port. Pass --port-file\n\
                     to have the actually-bound port written there as a bare decimal, for\n\
                     a script to read without a pipeline."
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("unexpected argument: {other}"),
        }
    }
    Ok(Config {
        listen,
        dist,
        #[cfg(feature = "multiplayer")]
        target,
        port_file,
    })
}

#[cfg(feature = "multiplayer")]
#[derive(Clone)]
struct RelayTarget(String);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lodestone_web_server=info".into()),
        )
        .init();

    let config = parse_config()?;
    let addr: SocketAddr = config
        .listen
        .parse()
        .with_context(|| format!("--listen must be an address like 127.0.0.1:8080 (or :0 for an OS-assigned port), got {:?}", config.listen))?;

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    let bound = listener.local_addr().context("bound listener has no local address")?;

    if let Some(path) = &config.port_file {
        std::fs::write(path, bound.port().to_string())
            .with_context(|| format!("failed to write bound port to {}", path.display()))?;
    }

    #[cfg(feature = "multiplayer")]
    tracing::info!(listen = %bound, dist = %config.dist.display(), target = %config.target, "lodestone-web-server listening");
    #[cfg(not(feature = "multiplayer"))]
    tracing::info!(listen = %bound, dist = %config.dist.display(), "lodestone-web-server listening (static-only)");
    #[cfg(feature = "multiplayer")]
    println!("lodestone-web-server: http://{bound}/ (relay -> {})", config.target);
    #[cfg(not(feature = "multiplayer"))]
    println!("lodestone-web-server: http://{bound}/ (static-only; multiplayer disabled)");

    let index = config.dist.join("index.html");
    let serve_dir = ServeDir::new(&config.dist).fallback(ServeFile::new(index));

    // Same two headers web/Trunk.toml's `[serve]` sets under `trunk serve` —
    // see that file and web/README.md's "COOP/COEP" section for why they are
    // required the moment threading lands and free to set today.
    let app = {
        #[cfg(feature = "multiplayer")]
        {
            Router::new()
                .route("/relay", get(relay_handler))
                .with_state(RelayTarget(config.target))
        }
        #[cfg(not(feature = "multiplayer"))]
        Router::new()
    }
        .fallback_service(serve_dir)
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("cross-origin-embedder-policy"),
            HeaderValue::from_static("require-corp"),
        ));

    axum::serve(listener, app)
        .await
        .context("lodestone-web-server: server error")
}

#[cfg(feature = "multiplayer")]
async fn relay_handler(
    ws: WebSocketUpgrade,
    RawQuery(query): RawQuery,
    State(default_target): State<RelayTarget>,
) -> impl IntoResponse {
    let target = destination_from_query(query.as_deref())
        .map(|(host, port)| format!("{host}:{port}"))
        .unwrap_or(default_target.0);
    ws.on_upgrade(move |socket| bridge(socket, target))
}

// The destination query is parsed by `lodestone_relay::destination_from_query`,
// the same function the standalone relay binary uses. This file used to carry a
// private copy of it — written when those helpers were not yet `pub` — and the
// copy was the hazard, not the duplication of effort: the *transport* types are
// what forced `bridge` to be re-expressed here, and a query string is not a
// transport. Two parsers of one wire contract compile independently, pass
// independently, and diverge the first time anyone adds a field to it, with the
// divergence surfacing in this crate, the deployed one.
/// Bridges one accepted browser WebSocket to a fresh TCP connection to
/// `target`. See this file's module doc for why this does not call
/// `lodestone_relay::bridge` directly — the forwarding logic below is
/// otherwise identical to it.
#[cfg(feature = "multiplayer")]
async fn bridge(socket: WebSocket, target: String) {
    let tcp = match TcpStream::connect(&target).await {
        Ok(tcp) => tcp,
        Err(error) => {
            tracing::warn!(%target, %error, "relay: failed to reach target");
            return;
        }
    };
    tcp.set_nodelay(true).ok();
    tracing::info!(%target, "relay: bridge open");

    let (mut ws_tx, mut ws_rx) = socket.split();
    let (mut tcp_rd, mut tcp_wr) = tcp.into_split();

    // WebSocket -> TCP: unwrap each binary frame straight onto the socket.
    let ws_to_tcp = async move {
        while let Some(message) = ws_rx.next().await {
            match message {
                Ok(Message::Binary(data)) => {
                    if tcp_wr.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                // Control/text frames are not part of the byte pipe; ignore them.
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Text(_)) => {}
            }
        }
        tcp_wr.shutdown().await.ok();
    };

    // TCP -> WebSocket: forward raw bytes as binary frames.
    let tcp_to_ws = async move {
        let mut buf = vec![0u8; FORWARD_CHUNK];
        loop {
            match tcp_rd.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if ws_tx.send(Message::Binary(buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            }
        }
        ws_tx.close().await.ok();
    };

    // When either direction finishes (server closed, client left, error), the
    // whole bridge is done; the other future is dropped.
    tokio::select! {
        () = ws_to_tcp => {}
        () = tcp_to_ws => {}
    }

    tracing::info!(%target, "relay: bridge closed");
}
