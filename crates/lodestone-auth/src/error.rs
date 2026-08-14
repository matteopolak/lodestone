//! Error type for the authentication crate.

/// A distinguishable XSTS authorization failure, identified by Xbox Live's
/// numeric `XErr` code in the 401 response body.
///
/// Microsoft does not publish an official list of these codes, but they are
/// stable and widely documented: every major third-party Minecraft launcher
/// (PrismLauncher, HMCL, and others, independently of each other) hardcodes
/// the same five values. **Not independently verified in this crate** — there
/// is no way to put a real account into each of these failure states without
/// owning one, so this mapping is carried forward from that external,
/// cross-project agreement rather than measured here. If a real account ever
/// hits [`Self::Other`], that is worth capturing as a new named variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum XstsErrorKind {
    /// `XErr` 2148916233: the Microsoft account has no Xbox Live account at
    /// all. The user must create one (Microsoft's flow does this at
    /// `signup.live.com`/`xbox.com`) before Minecraft sign-in can succeed.
    NoXboxAccount,
    /// `XErr` 2148916235: Xbox Live is not available from the account's
    /// country/region.
    RegionUnavailable,
    /// `XErr` 2148916236: adult verification (South Korea) is required on
    /// the Xbox account.
    AdultVerificationRequired,
    /// `XErr` 2148916237: age verification (South Korea) is required on the
    /// Xbox account.
    AgeVerificationRequired,
    /// `XErr` 2148916238: the account is a child account and must be added
    /// to a Microsoft Family by an adult organizer before it can sign in.
    ChildAccountNeedsFamily,
    /// A `401` with a recognisable XSTS error shape, but a code not in the
    /// above list (or no `XErr` field at all — carried as `0`).
    Other(i64),
}

impl XstsErrorKind {
    /// Classifies a raw `XErr` code from the XSTS response body.
    #[must_use]
    pub fn from_code(code: i64) -> Self {
        match code {
            2_148_916_233 => Self::NoXboxAccount,
            2_148_916_235 => Self::RegionUnavailable,
            2_148_916_236 => Self::AdultVerificationRequired,
            2_148_916_237 => Self::AgeVerificationRequired,
            2_148_916_238 => Self::ChildAccountNeedsFamily,
            other => Self::Other(other),
        }
    }

    /// A short, user-facing description a UI can show directly, distinct from
    /// the raw Microsoft response text in [`AuthError::Xsts`]'s `message`.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::NoXboxAccount => {
                "This Microsoft account has no Xbox account. Create one at xbox.com, then sign in again."
            }
            Self::RegionUnavailable => "Xbox Live is not available in this account's region.",
            Self::AdultVerificationRequired => {
                "This account needs adult verification on xbox.com before it can sign in."
            }
            Self::AgeVerificationRequired => {
                "This account needs age verification on xbox.com before it can sign in."
            }
            Self::ChildAccountNeedsFamily => {
                "This is a child account. An adult must add it to a Microsoft Family before it can sign in."
            }
            Self::Other(_) => "Xbox Live rejected this account for an unrecognised reason.",
        }
    }
}

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

    /// XSTS authorization returned a distinguishable, well-known failure
    /// (see [`XstsErrorKind`]) rather than a token. A UI should show
    /// [`XstsErrorKind::describe`] rather than the raw `message`, which is
    /// Microsoft's own (English, developer-oriented) response body.
    #[error("xsts authorization failed ({kind:?}): {message}")]
    Xsts {
        /// The classified failure reason.
        kind: XstsErrorKind,
        /// The raw response body, kept for diagnostics/logging.
        message: String,
    },

    /// A refresh attempt was rejected with OAuth's `invalid_grant`: the
    /// cached refresh token is dead (revoked, expired past its own renewal
    /// window, or the user changed their password). Distinct from
    /// [`AuthError::Service`] so a caller can treat this one case as "the
    /// cache is stale, fall back to interactive sign-in" without having to
    /// string-match an error message.
    #[error("cached refresh token was rejected (invalid_grant); interactive sign-in is required")]
    RefreshTokenInvalid,

    /// No Azure public-client id was configured (see
    /// [`crate::login::resolve_client_id`]). Mojang gates production access
    /// to the Minecraft API per registered Azure application, so there is no
    /// id this crate can safely default to — surfacing this as a typed error
    /// is deliberately not a panic and not a confusing 401 from Microsoft.
    #[error(
        "no Microsoft Azure client id configured (set {env}); \
         register an Azure AD application and request Minecraft API access for it"
    )]
    MissingClientId {
        /// The environment variable a caller was expected to set.
        env: &'static str,
    },

    /// A texture URL was refused by
    /// [`crate::texture::is_allowed_texture_domain`] — authlib's
    /// `TextureUrlChecker`. Typed rather than folded into
    /// [`AuthError::Service`] because it is the **security-relevant** outcome:
    /// the URL arrived over the network, and no request was made. A caller that
    /// wants to log-and-continue needs to tell this apart from "the host was
    /// allowed but the fetch failed".
    #[cfg(not(target_arch = "wasm32"))]
    #[error("refusing to fetch a texture from a host outside vanilla's allow list: {url}")]
    TextureDomainNotAllowed {
        /// The refused URL, verbatim, for the log line.
        url: String,
    },

    /// Reading or writing the on-disk token cache failed.
    #[cfg(not(target_arch = "wasm32"))]
    #[error("token cache io error: {0}")]
    Cache(#[from] std::io::Error),

    /// The OS keychain returned an error: unavailable, locked, denied, or a
    /// platform-specific failure. This is never turned into a silent
    /// fallback to a plaintext file — see
    /// [`crate::store::AccountSecrets::open`], which decides once whether to
    /// use the real keychain or an explicit, visible in-memory fallback
    /// before any of these can surface from a save/load/delete call.
    #[cfg(not(target_arch = "wasm32"))]
    #[error("keychain error: {0}")]
    Keychain(#[from] keyring::Error),

    /// A `/player/certificates` 2xx body did not decode into a usable RSA
    /// chat-signing key pair: malformed PEM framing, private-key bytes that
    /// are not PKCS#8 DER, a public key that is not X.509 SPKI DER, a
    /// `publicKeySignatureV2` that is not valid base64 or is empty, or an
    /// `expiresAt`/`refreshedAfter` that does not parse as an instant. See
    /// [`crate::chat_session`].
    #[cfg(not(target_arch = "wasm32"))]
    #[error("chat session key pair response was malformed: {0}")]
    ChatSessionKeyMalformed(String),
}

/// Convenience result alias for this crate.
pub type Result<T> = core::result::Result<T, AuthError>;
