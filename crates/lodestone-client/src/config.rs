//! Session configuration types.

/// Policy controlling how the driver reacts to keep-alive challenges.
///
/// Servers disconnect clients that fail to answer keep-alives, so the default
/// is [`KeepAlivePolicy::Automatic`]. Either way the keep-alive is still
/// surfaced to the event stream, so a bot can observe timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum KeepAlivePolicy {
    /// The driver answers keep-alives automatically by encoding a
    /// `KeepAliveResponse` action against the current state and sending it.
    #[default]
    Automatic,

    /// The driver never answers keep-alives; the user is responsible for
    /// submitting a `KeepAliveResponse` action (or letting the connection time
    /// out). Useful for testing server timeout behaviour.
    Manual,
}

impl KeepAlivePolicy {
    /// Returns `true` when the driver should auto-respond to keep-alives.
    #[must_use]
    pub const fn is_automatic(self) -> bool {
        matches!(self, Self::Automatic)
    }
}

/// Policy controlling how the driver reacts to the local player's death.
///
/// On death the vanilla server holds the player on the death screen and stops
/// streaming chunks until it receives a respawn request, so a headless client
/// that never respawns is stuck forever. The default is
/// [`RespawnPolicy::Automatic`], matching every bot library. Either way the
/// [`Death`](lodestone_model::ClientEvent::Death) event is still surfaced so a
/// bot can observe deaths, read the death message, or run its own logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RespawnPolicy {
    /// The driver answers a `Death` event by encoding a `Respawn` action
    /// against the current state and sending it, leaving the death screen.
    #[default]
    Automatic,

    /// The driver never respawns automatically; the user is responsible for
    /// submitting a `Respawn` action. Useful for bots with custom death logic.
    Manual,
}

impl RespawnPolicy {
    /// Returns `true` when the driver should auto-respawn on death.
    #[must_use]
    pub const fn is_automatic(self) -> bool {
        matches!(self, Self::Automatic)
    }
}

/// Policy controlling whether the driver announces client-readiness to the
/// server after joining or respawning.
///
/// Vanilla's server seeds a short (~60-tick, ~3 s) `clientLoadedTimeoutTimer`
/// after join **and** after respawn and silently ignores the player's movement
/// packets until it elapses — unless the client zeroes it early by sending
/// `player_loaded`. A client that never sends it has its movement discarded for
/// the first ~3 s of every join, so the default is
/// [`PlayerLoadedPolicy::Automatic`]: the driver sends `player_loaded` on the
/// first placement teleport of each load-epoch, matching vanilla.
///
/// The only reason to choose [`PlayerLoadedPolicy::Manual`] is to deliberately
/// observe that window — e.g. a test proving movement is ignored until the
/// client is loaded. It is otherwise a footgun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum PlayerLoadedPolicy {
    /// The driver sends `player_loaded` automatically on the first placement
    /// teleport after a join or respawn, zeroing the server's client-load timer.
    #[default]
    Automatic,

    /// The driver never sends `player_loaded`; the server therefore ignores the
    /// player's movement until its client-load timer elapses on its own. Useful
    /// only for testing that window; not recommended for real clients.
    Manual,
}

impl PlayerLoadedPolicy {
    /// Returns `true` when the driver should auto-announce client-readiness.
    #[must_use]
    pub const fn is_automatic(self) -> bool {
        matches!(self, Self::Automatic)
    }
}
