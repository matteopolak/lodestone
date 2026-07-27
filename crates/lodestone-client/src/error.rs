//! Error and session-outcome types for the client.

use lodestone_model::{AdapterError, Text};
use lodestone_net::NetError;

/// A fatal error that ended, or prevented, a client session.
///
/// These are the reasons a session stops abnormally. A clean server close or a
/// local shutdown are *not* errors and are reported through [`SessionOutcome`]
/// instead.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// The underlying transport failed, including a mid-frame connection close.
    #[error("transport error: {0}")]
    Transport(#[from] NetError),

    /// The version adapter failed to lift an inbound packet or the initial
    /// login sequence.
    #[error("protocol adapter error: {0}")]
    Adapter(#[from] AdapterError),

    /// No data arrived from the server within the configured read timeout.
    #[error("timed out waiting for server data")]
    Timeout,

    /// The background driver task panicked. This indicates a bug.
    #[error("client driver task panicked")]
    DriverPanicked,
}

/// Why a client session ended.
///
/// A session always ends with exactly one of these. Only [`SessionOutcome::Failed`]
/// carries a [`ClientError`]; the other variants are orderly terminations.
#[derive(Debug)]
#[non_exhaustive]
pub enum SessionOutcome {
    /// The server sent an explicit disconnect with a reason.
    ServerDisconnected {
        /// The disconnect reason supplied by the server.
        reason: Text,
    },

    /// The server closed the connection cleanly at a frame boundary.
    ServerClosed,

    /// The local user requested shutdown via [`crate::ClientHandle::shutdown`].
    LocalClose,

    /// The session ended because of an error.
    Failed(ClientError),
}

impl SessionOutcome {
    /// Returns the error if this outcome is [`SessionOutcome::Failed`].
    #[must_use]
    pub fn error(&self) -> Option<&ClientError> {
        match self {
            Self::Failed(error) => Some(error),
            _ => None,
        }
    }

    /// Returns `true` if the session ended without an error.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        !matches!(self, Self::Failed(_))
    }
}

/// Returned by [`crate::ClientHandle::send_action`] when the driver is gone.
///
/// The session has already ended (cleanly or otherwise), so submitted actions
/// can no longer be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("client driver is no longer running")]
pub struct ClientClosed;

/// Why a high-level bot action could not be performed.
///
/// These make illegal states explicit rather than silently no-op: asking the
/// bot to move relative to a position the server has not sent yet, or acting
/// after the session has ended, is a typed error a caller can react to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BotError {
    /// The action needs the player's position, but the server has not placed
    /// the player yet (no teleport received). Wait for
    /// [`crate::ClientHandle::wait_for_spawn`] first.
    #[error("player position is not known yet")]
    PositionUnknown,

    /// The session has already ended, so the action cannot be delivered.
    #[error("client driver is no longer running")]
    Closed,
}

impl From<ClientClosed> for BotError {
    fn from(_: ClientClosed) -> Self {
        Self::Closed
    }
}

/// Why a [`crate::ClientHandle::wait_for`] call stopped before its condition
/// was met.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WaitError {
    /// The condition did not become true within the supplied timeout.
    #[error("timed out waiting for condition")]
    Timeout,

    /// The session ended before the condition became true.
    #[error("client driver ended before condition was met")]
    Closed,
}
