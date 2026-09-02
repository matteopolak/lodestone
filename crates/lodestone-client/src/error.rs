//! Error and session-outcome types for the client.

use std::collections::HashMap;

use lodestone_model::{AdapterError, ResourceKey, Text};
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

    /// Compatibility error for a login path that asked for online-mode
    /// encryption but supplied no authentication policy.
    ///
    /// [`crate::ClientBuilder`] now always supplies an explicit policy, so its
    /// default offline identity completes RSA/AES without Mojang and an
    /// unavailable online account produces
    /// [`ClientError::OnlineModeSessionUnavailable`] instead. The variant is
    /// retained for public API compatibility with callers that match it.
    ///
    /// # Why the text names a player action rather than a builder method
    ///
    /// It used to end *"no Microsoft session was configured for this connection
    /// (see ClientBuilder::online_session)"*, which is accurate about the
    /// library and useless on a disconnect screen — it reads as a build fault,
    /// and it was what a player saw while an account was signed in and working
    /// in the switcher (nothing produced the session; see
    /// `lodestone_shell::net`'s `RemoteAuth`). It is now the sentence for
    /// exactly one situation — nobody is signed in — and
    /// [`ClientError::OnlineModeSessionUnavailable`] covers the case where
    /// somebody is.
    #[cfg(not(target_arch = "wasm32"))]
    #[error(
        "this server requires a Minecraft account, and no Microsoft account is \
         signed in — add one in Options ▸ Accounts and select it, then rejoin"
    )]
    OnlineModeSessionRequired,

    /// The server's login sequence asked for online-mode encryption, an account
    /// **is** selected, and it could not be turned into a usable session — an
    /// expired/revoked refresh token, an unreachable Microsoft, a locked
    /// keychain, or no configured Azure client id.
    ///
    /// Distinct from [`ClientError::OnlineModeSessionRequired`] because the
    /// remedy is distinct: the player already signed in once, so the honest
    /// instruction is "sign in to *this account* again", not "add an account".
    /// Distinct from [`ClientError::Auth`] because nothing was sent to Mojang —
    /// the failure happened locally, before the handshake.
    ///
    /// `detail` is produced by `lodestone_auth::SelectedAccount::Unavailable`
    /// and is already user-facing; it never contains a token.
    #[cfg(not(target_arch = "wasm32"))]
    #[error("this server requires a Minecraft account, but signing in as {account} failed: {detail}")]
    OnlineModeSessionUnavailable {
        /// The account's username, or its UUID when no metadata row exists.
        account: String,
        /// One sentence about why that account could not be used.
        detail: String,
    },

    /// The session-server `join` call (or a step of the auth chain reached
    /// while getting there) failed. Carries the crate's own typed error, so a
    /// UI can match e.g. `lodestone_auth::AuthError::Xsts` for a specific
    /// message instead of parsing this variant's `Display` text.
    #[cfg(not(target_arch = "wasm32"))]
    #[error("online-mode authentication failed: {0}")]
    Auth(#[from] lodestone_auth::AuthError),
}

impl ClientError {
    /// This error and its `source()` chain, joined with `": "` — the text
    /// `ClientEvent::SessionFailed` carries to the screen.
    ///
    /// # Why the chain is deduplicated rather than simply concatenated
    ///
    /// `thiserror`'s `#[from]` sets `source()` *and* the `{0}` in the
    /// `#[error(…)]` attribute already interpolates that source's own `Display`.
    /// So for `Transport(NetError::Codec(e))` the top-level `to_string()` is
    /// already `"transport error: protocol codec error: <e>"`, and appending
    /// each link of the chain on top of that would print every layer twice. A
    /// link is therefore only appended when its text is not already present —
    /// which keeps the *whole* chain for a variant that forgets to interpolate
    /// its source, and prints each layer exactly once for every variant here
    /// today. The naive concatenation is not a hypothetical: it is what
    /// `anyhow`-style chain printing does, and `error_cause_chain_does_not_repeat_a_layer`
    /// is the gate that separates the two.
    #[must_use]
    pub fn cause_chain(&self) -> String {
        let mut out = self.to_string();
        let mut source = std::error::Error::source(self);
        while let Some(error) = source {
            let text = error.to_string();
            if !out.contains(&text) {
                out.push_str(": ");
                out.push_str(&text);
            }
            source = error.source();
        }
        out
    }
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

    /// The server asked the client to reconnect elsewhere
    /// ([`lodestone_model::ClientEvent::TransferRequested`]).
    ///
    /// Vanilla's own client tears down
    /// the connection and immediately opens a new one to `host:port`,
    /// carrying its in-memory cookie store across the boundary
    /// so a `cookie_request` on the far side can still be
    /// answered. The driver cannot open that new connection itself — a
    /// native TCP socket and a `wasm32` `ws-web`/in-memory transport are
    /// different [`crate::builder::ClientBuilder::connect`] /
    /// [`crate::builder::ClientBuilder::connect_with`] entry points, and
    /// nothing generic bridges them — so this ends the session with
    /// everything a caller needs to do the reconnect itself: the target
    /// address, and the cookies this session had collected so the new one
    /// can seed `cookie_response`s from them (see
    /// `crate::driver::Driver::cookies`).
    Transferred {
        /// Target server host.
        host: String,
        /// Target server port.
        port: i32,
        /// Cookies this session had stored, to carry into the reconnect.
        cookies: HashMap<ResourceKey, Vec<u8>>,
    },

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

#[cfg(test)]
mod cause_chain_tests {
    use super::ClientError;
    use lodestone_net::NetError;

    /// The whole point of [`ClientError::cause_chain`]: the *innermost* detail
    /// reaches the string, because that is the half a screen saying "stream
    /// closed" was throwing away.
    ///
    /// Both hypotheses are written out and the measurement must land on one.
    /// The wrong one here is not "no chain" — `Display` alone would pass that —
    /// it is the naive concatenation every chain-printing helper does, which
    /// repeats a layer `thiserror` has already interpolated.
    #[test]
    fn error_cause_chain_does_not_repeat_a_layer() {
        let error = ClientError::Transport(NetError::UnexpectedClose(7));
        let chain = error.cause_chain();

        // Derived from the two `#[error(…)]` attributes, not guessed: the
        // `ClientError::Transport` arm is `"transport error: {0}"` and
        // `NetError::UnexpectedClose`'s is
        // `"connection closed mid-frame ({0} bytes buffered)"`.
        let deduplicated = "transport error: connection closed mid-frame (7 bytes buffered)";
        let repeated = format!(
            "{deduplicated}: {}",
            "connection closed mid-frame (7 bytes buffered)"
        );
        assert_ne!(
            deduplicated, repeated,
            "the two hypotheses must differ, or this gate measures nothing"
        );
        assert_eq!(chain, deduplicated, "expected the deduplicated chain");

        // The half that matters to a reader of the screen: the inner error's own
        // detail survives. `7` is a value only the innermost layer knows.
        assert!(
            chain.contains("7 bytes buffered"),
            "the innermost cause must reach the text: {chain}"
        );
    }

    /// The control for the dedupe rule: an error with *no* source is its own
    /// `Display` and nothing is appended. Without this, an implementation that
    /// returned `to_string()` and ignored the chain entirely would be
    /// indistinguishable from one that walks it, since every `ClientError`
    /// variant today interpolates its source.
    #[test]
    fn a_sourceless_error_is_exactly_its_own_display() {
        assert_eq!(
            ClientError::Timeout.cause_chain(),
            "timed out waiting for server data"
        );
        assert!(
            std::error::Error::source(&ClientError::Timeout).is_none(),
            "detector control: this variant must genuinely have no source, or the \
             assertion above proves nothing about the walk"
        );
    }
}
