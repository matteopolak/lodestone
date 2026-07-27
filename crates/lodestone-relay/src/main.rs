//! Command-line entry point for the [`lodestone_relay`] WebSocket→TCP relay.
//!
//! ```text
//! lodestone-relay --listen 127.0.0.1:25580 --target 127.0.0.1:25565
//! ```
//!
//! A browser connects to `ws://127.0.0.1:25580` and is bridged to the Minecraft
//! server at `127.0.0.1:25565`. The relay is protocol-blind (see the crate docs).

use std::net::SocketAddr;

use anyhow::{Context, Result};
use tokio::net::TcpListener;

/// Parsed command-line configuration.
#[derive(Debug)]
struct Config {
    /// Address the relay listens on for WebSocket clients.
    listen: SocketAddr,
    /// Address of the real Minecraft (TCP) server to bridge to.
    target: String,
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
                    "lodestone-relay --listen <addr:port> --target <host:port>\n\
                     \n\
                     A protocol-blind WebSocket->TCP relay. Browser clients connect\n\
                     over ws:// to --listen and are bridged to the TCP server at\n\
                     --target."
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
    let target = target.unwrap_or_else(|| "127.0.0.1:25565".to_string());
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
    tracing::info!(listen = %config.listen, target = %config.target, "relay listening");

    lodestone_relay::serve(listener, config.target).await
}
