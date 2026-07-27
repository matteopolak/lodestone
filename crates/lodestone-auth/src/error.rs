//! Error type for the authentication crate.

/// Errors produced while authenticating with Microsoft/Mojang services or
/// joining a session.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// A network-level or HTTP transport error from the underlying client.
    #[cfg(not(target_arch = "wasm32"))]
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// A response could not be parsed as the expected JSON shape.
    #[error("failed to parse response json: {0}")]
    Json(#[from] serde_json::Error),

    /// The device-code authorization is still pending; the caller should keep
    /// polling. Carries the server-mandated polling interval in seconds.
    #[error("authorization pending")]
    AuthorizationPending {
        /// Seconds to wait before the next poll.
        interval: u64,
    },

    /// The user declined the sign-in request.
    #[error("authorization declined by user")]
    AuthorizationDeclined,

    /// The device code expired before the user completed sign-in.
    #[error("device code expired before sign-in completed")]
    DeviceCodeExpired,

    /// A step in the chain returned a well-formed error payload.
    #[error("{step} failed: {message}")]
    Service {
        /// Which stage of the chain failed (e.g. `"xsts"`).
        step: &'static str,
        /// A human-readable message extracted from the response.
        message: String,
    },

    /// The account does not own Minecraft (no profile is provisioned).
    #[error("account does not own a minecraft profile")]
    NoMinecraftProfile,

    /// Reading or writing the on-disk token cache failed.
    #[cfg(not(target_arch = "wasm32"))]
    #[error("token cache io error: {0}")]
    Cache(#[from] std::io::Error),
}

/// Convenience result alias for this crate.
pub type Result<T> = core::result::Result<T, AuthError>;
