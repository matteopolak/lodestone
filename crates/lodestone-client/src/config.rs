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
