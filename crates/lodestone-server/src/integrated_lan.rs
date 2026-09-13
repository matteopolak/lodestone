//! Configuration types for native integrated-server LAN hosting.
//!
//! The listener and connection lifecycle remain owned by [`super::IntegratedServer`].
//! This module owns the public configuration vocabulary passed into that lifecycle,
//! keeping LAN-facing options together without changing the server's API or behavior.

use crate::command::CommandDispatch;
use crate::server::{OnlineModeConfig, ResourcePackPushFeed};
use crate::spawn::Task;

use super::{ShutdownSignal, spawn_tick_task};

/// Everything an open-to-LAN host can configure.
#[derive(Debug, Default)]
pub struct LanConfig {
    /// The server's own view-distance cap. Every connection's requested
    /// distance is clamped to it.
    pub view_radius: i32,
    /// Start an RCON listener. `None` leaves the port closed.
    pub rcon: Option<crate::rcon::RconConfig>,
    /// Serve the GameSpy4/UT3 query protocol on the same port's UDP space.
    pub query: bool,
    /// Announce this world on the LAN discovery multicast group.
    pub discovery: Option<LanDiscovery>,
    /// The command dispatcher every accepted connection's slash-commands reach.
    pub commands: CommandDispatch,
    /// Server-initiated resource-pack pushes.
    pub resource_packs: ResourcePackPushFeed,
    /// The wire-level plugin-channel registry.
    pub plugin_channels: crate::plugin_channels::PluginChannelRegistry,
    /// Ops, whitelist and the two ban lists this host enforces at join.
    pub access: crate::access::AccessHandle,
    /// Online-mode encryption and session-server ownership verification.
    pub online_mode: Option<OnlineModeConfig>,
}

/// How to announce a LAN world on the standard discovery multicast group.
#[derive(Debug, Clone)]
pub struct LanDiscovery {
    /// The world name shown in the multiplayer list's LAN section.
    pub motd: String,
}

impl LanDiscovery {
    /// The discovery multicast group and port.
    pub const GROUP: std::net::Ipv4Addr = std::net::Ipv4Addr::new(224, 0, 2, 60);
    /// See [`GROUP`](Self::GROUP).
    pub const PORT: u16 = 4445;
    /// The discovery broadcast interval.
    pub const INTERVAL: std::time::Duration = std::time::Duration::from_millis(1500);

    /// The exact datagram body the client parses.
    #[must_use]
    pub fn payload(&self, port: u16) -> String {
        format!("[MOTD]{}[/MOTD][AD]{port}[/AD]", self.motd)
    }
}

/// Per-connection configuration for [`super::IntegratedServer::publish_with_config`].
#[derive(Debug, Default)]
pub struct PublishConfig {
    /// Ops, whitelist and the two ban lists this listener enforces at join.
    pub access: crate::access::AccessHandle,
    /// The command dispatcher every accepted connection's slash-commands reach.
    pub commands: CommandDispatch,
    /// Online-mode encryption and session-server ownership verification.
    pub online_mode: Option<OnlineModeConfig>,
}

/// Spawn LAN discovery pings until the integrated server shuts down.
pub(super) fn spawn_lan_discovery(
    shutdown: &std::sync::Arc<ShutdownSignal>,
    discovery: &LanDiscovery,
    port: u16,
) -> Option<Task> {
    let socket = match std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)) {
        Ok(socket) => socket,
        Err(err) => {
            tracing::warn!("LAN discovery disabled (UDP bind failed): {err}");
            return None;
        }
    };
    if let Err(err) = socket.set_nonblocking(true) {
        tracing::warn!("LAN discovery disabled (non-blocking mode failed): {err}");
        return None;
    }
    let socket = match tokio::net::UdpSocket::from_std(socket) {
        Ok(socket) => socket,
        Err(err) => {
            tracing::warn!("LAN discovery disabled (socket registration failed): {err}");
            return None;
        }
    };
    let target = std::net::SocketAddrV4::new(LanDiscovery::GROUP, LanDiscovery::PORT);
    let payload = discovery.payload(port);
    Some(spawn_tick_task(shutdown, async move {
        let mut ticker = tokio::time::interval(LanDiscovery::INTERVAL);
        loop {
            ticker.tick().await;
            if let Err(err) = socket.send_to(payload.as_bytes(), target).await {
                tracing::debug!("LAN discovery ping failed: {err}");
            }
        }
    }))
}
