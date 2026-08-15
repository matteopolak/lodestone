//! Command-line entry point for the [`lodestone_relay`] WebSocket→TCP relay.
//!
//! ```text
//! lodestone-relay --listen 127.0.0.1:25580
//! ```
//!
//! Each browser connection names its own destination as it dials
//! (`ws://127.0.0.1:25580/relay?host=<host>&port=<port>`) — see
//! `lodestone_relay`'s crate docs for why, and for what `--target` means now
//! that the destination is per-connection rather than fixed at startup.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use tokio::net::TcpListener;

/// Parsed command-line configuration.
#[derive(Debug)]
struct Config {
    /// Address the relay listens on for WebSocket clients.
    listen: SocketAddr,
    /// Fallback destination for a connection whose upgrade request carries no
    /// `host`/`port` query pair. `None` means every connection must name its
    /// own destination — see [`lodestone_relay::bridge`]'s doc.
    target: Option<String>,
}

fn parse_config() -> Result<Config> {
    let mut listen: Option<String> = None;
    let mut target: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = args.next(),
            "--target" => target = args.next(),
            "-h" | "--help" => {
                eprintln!(
                    "lodestone-relay --listen <addr:port> [--target <host:port>]\n\
                     \n\
                     A protocol-blind WebSocket->TCP relay. Browser clients connect\n\
                     over ws:// to --listen and each names its own destination on the\n\
                     upgrade request: ws://<listen>/relay?host=<host>&port=<port>.\n\
                     --target is only a fallback for a connection that names none —\n\
                     omit it to require every connection to say where it wants to go."
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("unexpected argument: {other}"),
        }
    }
    let listen = listen
        .unwrap_or_else(|| "127.0.0.1:25580".to_string())
        .parse()
        .context("--listen must be an address like 127.0.0.1:25580")?;
    Ok(Config { listen, target })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lodestone_relay=info".into()),
        )
        .init();

    let config = parse_config()?;
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("failed to bind {}", config.listen))?;
    tracing::info!(
        listen = %config.listen,
        target = config.target.as_deref().unwrap_or("(none — every connection must name its own)"),
        "relay listening"
    );

    lodestone_relay::serve(listener, config.target).await
}
