//! The account-scoped Friends service.
//!
//! This module owns the service wire boundary and exposes only credential-free
//! domain values. Callers resolve a [`crate::Session`] through the existing
//! account path, borrow it for one request, and retain neither its bearer token
//! nor a clone of it. The production origin is fixed; the test-only override
//! accepts loopback origins only. Both paths use a client with redirects disabled
//! so an authenticated request cannot be forwarded to an arbitrary host.
//!
//! The request shapes were verified against the service library shipped with the
//! 26.2 client artifact in this repository's cache. In particular, Friends uses
//! `GET`/`PUT /friends`, attributes use `GET`/`POST /player/attributes`, and
//! presence uses `POST /presence`. The peer-messaging identity and join metadata
//! returned by presence are intentionally decoded nowhere: they have no consumer
//! in Friends and must not grow into a transport back door.

use std::{fmt, time::Duration};

#[cfg(any(test, feature = "friends-test-service"))]
use std::net::IpAddr;

#[cfg(not(target_arch = "wasm32"))]
use reqwest::redirect::Policy;
use reqwest::{
    StatusCode, Url,
    header::{self, HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Session;

const PRODUCTION_ORIGIN: &str = "https://api.minecraftservices.com/";
#[cfg(not(target_arch = "wasm32"))]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(target_arch = "wasm32"))]
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

/// A validated cache validator returned by the Friends service.
///
/// It is intentionally opaque: callers can pass it back to a later request,
/// but cannot repurpose a service-supplied header value elsewhere.
#[derive(Clone, PartialEq, Eq)]
pub struct EntityTag(HeaderValue);

impl fmt::Debug for EntityTag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EntityTag(..)")
    }
}

impl EntityTag {
    fn from_response(headers: &HeaderMap) -> Result<Option<Self>, FriendsServiceError> {
        let Some(value) = headers.get(header::ETAG) else {
            return Ok(None);
        };
        HeaderValue::from_bytes(value.as_bytes())
            .map(Self)
            .map(Some)
            .map_err(|_| FriendsServiceError::MalformedResponse)
    }
}

/// A server-provided minimum delay before the next poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryHint(Duration);

impl RetryHint {
    /// The positive delay requested by the service.
    #[must_use]
    pub const fn duration(self) -> Duration {
        self.0
    }
}

/// A value returned by a conditional Friends-service operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CachedResponse<T> {
    /// The service supplied a complete replacement value.
    Fresh {
        value: T,
        entity_tag: Option<EntityTag>,
        retry_after: Option<RetryHint>,
    },
    /// The supplied validator still represents the current value.
    ///
    /// The caller retains its previous entity tag if this response carries no
    /// replacement tag.
    NotModified {
        entity_tag: Option<EntityTag>,
        retry_after: Option<RetryHint>,
    },
}

/// A profile in a Friends list or request list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendProfile {
    pub profile_id: Uuid,
    pub name: String,
}

/// All account relationships returned by the Friends service.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FriendsSnapshot {
    pub friends: Vec<FriendProfile>,
    pub incoming: Vec<FriendProfile>,
    pub outgoing: Vec<FriendProfile>,
}

/// The activity state a friend has published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenceStatus {
    Offline,
    Online,
    LocalWorld,
    LanWorld,
    Realm,
    Server,
    /// An unrecognised service value. Only the row carrying this value is
    /// affected; it must not make the whole Friends response unusable.
    Unknown,
}

impl PresenceStatus {
    fn decode(value: &str) -> Self {
        match value {
            "OFFLINE" => Self::Offline,
            "ONLINE" => Self::Online,
            "PLAYING_OFFLINE" => Self::LocalWorld,
            "PLAYING_HOSTED_SERVER" => Self::LanWorld,
            "PLAYING_REALMS" => Self::Realm,
            "PLAYING_SERVER" => Self::Server,
            _ => Self::Unknown,
        }
    }

    fn encode(self) -> Result<&'static str, FriendsServiceError> {
        match self {
            Self::Offline => Ok("OFFLINE"),
            Self::Online => Ok("ONLINE"),
            Self::LocalWorld => Ok("PLAYING_OFFLINE"),
            Self::LanWorld => Ok("PLAYING_HOSTED_SERVER"),
            Self::Realm => Ok("PLAYING_REALMS"),
            Self::Server => Ok("PLAYING_SERVER"),
            Self::Unknown => Err(FriendsServiceError::InvalidInput),
        }
    }
}

/// One friend's latest presence, without peer-messaging fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceEntry {
    pub profile_id: Uuid,
    pub status: PresenceStatus,
    /// The service's RFC 3339 timestamp, preserved as display-independent text.
    pub last_updated: String,
}

/// The presence rows returned after publishing this account's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PresenceSnapshot {
    pub entries: Vec<PresenceEntry>,
}

/// A player-directed change to the Friends graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FriendMutation {
    SendByName(String),
    Accept(Uuid),
    Decline(Uuid),
    Cancel(Uuid),
    Remove(Uuid),
}

/// The Friends-related slice of account attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserFriendsAttributes {
    pub preferences: FriendsPreferences,
}

/// Service-backed Friends preferences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FriendsPreferences {
    pub enabled: bool,
    pub allow_requests: bool,
}

impl Default for FriendsPreferences {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_requests: false,
        }
    }
}

/// A failure that can be rendered without exposing an access token or response
/// body.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FriendsServiceError {
    #[error("Friends service request failed")]
    Transport(#[source] reqwest::Error),
    #[error("Friends service rejected the current session")]
    Unauthorized,
    #[error("Friends service denied this operation for privacy or safety settings")]
    PrivacyDenied,
    #[error("Friends service could not find that profile")]
    UnknownProfile,
    #[error("Friends service rate-limited this operation")]
    RateLimited { retry_after: Option<RetryHint> },
    #[error("Friends service is temporarily unavailable")]
    Unavailable { retry_after: Option<RetryHint> },
    #[error("Friends input is invalid")]
    InvalidInput,
    #[error("Friends service returned a malformed response")]
    MalformedResponse,
    #[error("Friends service rejected the request with HTTP {status}")]
    Rejected { status: u16 },
}

/// Typed, redirect-free access to the fixed Friends-service origin.
pub struct FriendsService {
    client: reqwest::Client,
    base: Url,
}

impl fmt::Debug for FriendsService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FriendsService")
            .field("origin", &self.base.origin().ascii_serialization())
            .finish_non_exhaustive()
    }
}

impl FriendsService {
    /// Constructs the production service at its fixed HTTPS origin.
    ///
    /// A separately configured `reqwest::Client` is deliberately not accepted:
    /// its redirect policy could forward a bearer header to another host.
    pub fn production() -> Result<Self, FriendsServiceError> {
        let base = Url::parse(PRODUCTION_ORIGIN).expect("the fixed Friends origin is a valid URL");
        Self::new(base)
    }

    /// Gets the current Friends preference slice. Missing preference data is a
    /// conservative disabled/deny-requests value, not an error.
    pub async fn get_attributes(
        &self,
        session: &Session,
    ) -> Result<UserFriendsAttributes, FriendsServiceError> {
        let response = self
            .send(
                self.client
                    .get(self.endpoint("player/attributes")?)
                    .bearer_auth(&session.access_token),
            )
            .await?;
        let raw: AttributesResponse = self.success_json(response).await?;
        Ok(UserFriendsAttributes {
            preferences: raw
                .friends_preferences
                .map(FriendsPreferences::from)
                .unwrap_or_default(),
        })
    }

    /// Gets friends and requests, optionally using a previous entity tag.
    pub async fn get_friends(
        &self,
        session: &Session,
        entity_tag: Option<&EntityTag>,
    ) -> Result<CachedResponse<FriendsSnapshot>, FriendsServiceError> {
        let mut request = self
            .client
            .get(self.endpoint("friends")?)
            .bearer_auth(&session.access_token);
        if let Some(entity_tag) = entity_tag {
            request = request.header(header::IF_NONE_MATCH, entity_tag.0.clone());
        }
        self.cached_friends(self.send(request).await?).await
    }

    /// Applies one relationship mutation and returns the service's complete,
    /// authoritative snapshot.
    pub async fn mutate_friend(
        &self,
        session: &Session,
        mutation: FriendMutation,
    ) -> Result<FriendsSnapshot, FriendsServiceError> {
        let request = FriendMutationRequest::try_from(mutation)?;
        let response = self
            .send(
                self.client
                    .put(self.endpoint("friends")?)
                    .bearer_auth(&session.access_token)
                    .json(&request),
            )
            .await?;
        self.parse_friends(self.success_json(response).await?)
    }

    /// Replaces only the Friends preference section, preserving every unrelated
    /// account attribute server-side.
    pub async fn set_preferences(
        &self,
        session: &Session,
        preferences: FriendsPreferences,
    ) -> Result<UserFriendsAttributes, FriendsServiceError> {
        let response = self
            .send(
                self.client
                    .post(self.endpoint("player/attributes")?)
                    .bearer_auth(&session.access_token)
                    .json(&AttributesRequest::from(preferences)),
            )
            .await?;
        let raw: AttributesResponse = self.success_json(response).await?;
        Ok(UserFriendsAttributes {
            preferences: raw
                .friends_preferences
                .map(FriendsPreferences::from)
                .unwrap_or(preferences),
        })
    }

    /// Publishes the selected account's activity and returns friends' current
    /// presence. The request intentionally omits invitation/join metadata.
    pub async fn publish_presence(
        &self,
        session: &Session,
        status: PresenceStatus,
        entity_tag: Option<&EntityTag>,
    ) -> Result<CachedResponse<PresenceSnapshot>, FriendsServiceError> {
        let mut request = self
            .client
            .post(self.endpoint("presence")?)
            .bearer_auth(&session.access_token)
            .json(&PresenceRequest {
                status: status.encode()?,
            });
        if let Some(entity_tag) = entity_tag {
            request = request.header(header::IF_NONE_MATCH, entity_tag.0.clone());
        }
        self.cached_presence(self.send(request).await?).await
    }

    fn new(base: Url) -> Result<Self, FriendsServiceError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::install_crypto_provider();
            let client = reqwest::Client::builder()
                .redirect(Policy::none())
                .timeout(REQUEST_TIMEOUT)
                .build()
                .map_err(FriendsServiceError::Transport)?;
            Ok(Self { client, base })
        }
        #[cfg(target_arch = "wasm32")]
        {
            // Browser fetch does not expose a redirect policy. Refuse the
            // service rather than risk a bearer header following a redirect;
            // a browser implementation needs a separately verified policy.
            let _ = base;
            Err(FriendsServiceError::Unavailable { retry_after: None })
        }
    }

    fn endpoint(&self, path: &str) -> Result<Url, FriendsServiceError> {
        self.base
            .join(path)
            .map_err(|_| FriendsServiceError::MalformedResponse)
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, FriendsServiceError> {
        request.send().await.map_err(FriendsServiceError::Transport)
    }

    async fn cached_friends(
        &self,
        response: reqwest::Response,
    ) -> Result<CachedResponse<FriendsSnapshot>, FriendsServiceError> {
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(CachedResponse::NotModified {
                entity_tag: EntityTag::from_response(response.headers())?,
                retry_after: retry_hint(response.headers()),
            });
        }
        let entity_tag = EntityTag::from_response(response.headers())?;
        let retry_after = retry_hint(response.headers());
        let raw: FriendsResponse = self.success_json(response).await?;
        Ok(CachedResponse::Fresh {
            value: self.parse_friends(raw)?,
            entity_tag,
            retry_after,
        })
    }

    async fn cached_presence(
        &self,
        response: reqwest::Response,
    ) -> Result<CachedResponse<PresenceSnapshot>, FriendsServiceError> {
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(CachedResponse::NotModified {
                entity_tag: EntityTag::from_response(response.headers())?,
                retry_after: retry_hint(response.headers()),
            });
        }
        let entity_tag = EntityTag::from_response(response.headers())?;
        let retry_after = retry_hint(response.headers());
        let raw: PresenceResponse = self.success_json(response).await?;
        Ok(CachedResponse::Fresh {
            value: self.parse_presence(raw)?,
            entity_tag,
            retry_after,
        })
    }

    async fn success_json<T: for<'de> Deserialize<'de>>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, FriendsServiceError> {
        if !response.status().is_success() {
            return Err(classify_status(response).await);
        }
        let body = bounded_body(response).await?;
        serde_json::from_slice(&body).map_err(|_| FriendsServiceError::MalformedResponse)
    }

    fn parse_friends(&self, raw: FriendsResponse) -> Result<FriendsSnapshot, FriendsServiceError> {
        Ok(FriendsSnapshot {
            friends: parse_profiles(raw.friends)?,
            incoming: parse_profiles(raw.incoming_requests)?,
            outgoing: parse_profiles(raw.outgoing_requests)?,
        })
    }

    fn parse_presence(
        &self,
        raw: PresenceResponse,
    ) -> Result<PresenceSnapshot, FriendsServiceError> {
        let entries = raw
            .presence
            .into_iter()
            .map(|entry| {
                let profile_id = parse_uuid(&entry.profile_id)?;
                if entry.last_updated.is_empty() {
                    return Err(FriendsServiceError::MalformedResponse);
                }
                Ok(PresenceEntry {
                    profile_id,
                    status: PresenceStatus::decode(&entry.status),
                    last_updated: entry.last_updated,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(PresenceSnapshot { entries })
    }

    #[cfg(any(test, feature = "friends-test-service"))]
    /// Constructs the production-equivalent request path against a loopback fake.
    ///
    /// This exists only for hermetic tests. A non-loopback or non-HTTP origin is
    /// rejected before a client can carry a bearer credential to it.
    pub fn for_test_base(base: Url) -> Result<Self, FriendsServiceError> {
        if !is_loopback_http_origin(&base) {
            return Err(FriendsServiceError::InvalidInput);
        }
        Self::new(base)
    }
}

fn parse_profiles(
    rows: Vec<FriendProfileResponse>,
) -> Result<Vec<FriendProfile>, FriendsServiceError> {
    rows.into_iter()
        .map(|row| {
            if row.name.is_empty() {
                return Err(FriendsServiceError::MalformedResponse);
            }
            Ok(FriendProfile {
                profile_id: parse_uuid(&row.profile_id)?,
                name: row.name,
            })
        })
        .collect()
}

fn parse_uuid(value: &str) -> Result<Uuid, FriendsServiceError> {
    Uuid::parse_str(value).map_err(|_| FriendsServiceError::MalformedResponse)
}

#[cfg(not(target_arch = "wasm32"))]
async fn bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>, FriendsServiceError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        return Err(FriendsServiceError::MalformedResponse);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(FriendsServiceError::Transport)?
    {
        let Some(remaining) = (MAX_RESPONSE_BYTES as usize).checked_sub(body.len()) else {
            return Err(FriendsServiceError::MalformedResponse);
        };
        if chunk.len() > remaining {
            return Err(FriendsServiceError::MalformedResponse);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(target_arch = "wasm32")]
async fn bounded_body(_response: reqwest::Response) -> Result<Vec<u8>, FriendsServiceError> {
    // `fetch` does not expose a redirect policy or incremental response reader
    // through reqwest here. `FriendsService::production` therefore refuses to
    // construct on this target; keep this defensive arm for type-checking the
    // shared parsing path without a potentially unbounded body read.
    Err(FriendsServiceError::Unavailable { retry_after: None })
}

async fn classify_status(response: reqwest::Response) -> FriendsServiceError {
    let status = response.status();
    let retry_after = retry_hint(response.headers());
    match status {
        StatusCode::UNAUTHORIZED => FriendsServiceError::Unauthorized,
        StatusCode::FORBIDDEN => FriendsServiceError::PrivacyDenied,
        StatusCode::TOO_MANY_REQUESTS => FriendsServiceError::RateLimited { retry_after },
        status if status.is_server_error() => FriendsServiceError::Unavailable { retry_after },
        StatusCode::BAD_REQUEST => match bounded_body(response).await {
            Ok(body) if error_is_unknown_profile(&body) => FriendsServiceError::UnknownProfile,
            _ => FriendsServiceError::Rejected {
                status: status.as_u16(),
            },
        },
        _ => FriendsServiceError::Rejected {
            status: status.as_u16(),
        },
    }
}

fn error_is_unknown_profile(body: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct ErrorResponse {
        details: Option<ErrorDetails>,
    }
    #[derive(Deserialize)]
    struct ErrorDetails {
        status: Option<String>,
    }

    serde_json::from_slice::<ErrorResponse>(body)
        .ok()
        .and_then(|response| response.details)
        .and_then(|details| details.status)
        .is_some_and(|status| status == "UNKNOWN_PROFILE")
}

fn retry_hint(headers: &HeaderMap) -> Option<RetryHint> {
    let seconds = headers
        .get(header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()?;
    (seconds > 0).then_some(RetryHint(Duration::from_secs(seconds)))
}

#[cfg(any(test, feature = "friends-test-service"))]
fn is_loopback_http_origin(url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

fn validate_profile_name(name: &str) -> Result<&str, FriendsServiceError> {
    let name = name.trim();
    let valid = (3..=16).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    valid
        .then_some(name)
        .ok_or(FriendsServiceError::InvalidInput)
}

#[derive(Deserialize)]
struct AttributesResponse {
    #[serde(rename = "friendsPreferences")]
    friends_preferences: Option<FriendsPreferencesResponse>,
}

#[derive(Deserialize)]
struct FriendsPreferencesResponse {
    friends: ToggleValue,
    #[serde(rename = "acceptInvites")]
    accept_invites: ToggleValue,
}

impl From<FriendsPreferencesResponse> for FriendsPreferences {
    fn from(value: FriendsPreferencesResponse) -> Self {
        Self {
            enabled: value.friends == ToggleValue::Enabled,
            allow_requests: value.accept_invites == ToggleValue::Enabled,
        }
    }
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ToggleValue {
    Enabled,
    Disabled,
}

#[derive(Serialize)]
struct AttributesRequest {
    #[serde(rename = "friendsPreferences")]
    friends_preferences: FriendsPreferencesRequest,
}

impl From<FriendsPreferences> for AttributesRequest {
    fn from(value: FriendsPreferences) -> Self {
        let toggle = |enabled| {
            if enabled {
                ToggleValue::Enabled
            } else {
                ToggleValue::Disabled
            }
        };
        Self {
            friends_preferences: FriendsPreferencesRequest {
                friends: toggle(value.enabled),
                accept_invites: toggle(value.allow_requests),
            },
        }
    }
}

#[derive(Serialize)]
struct FriendsPreferencesRequest {
    friends: ToggleValue,
    #[serde(rename = "acceptInvites")]
    accept_invites: ToggleValue,
}

#[derive(Deserialize)]
struct FriendsResponse {
    #[serde(default)]
    friends: Vec<FriendProfileResponse>,
    #[serde(rename = "incomingRequests", default)]
    incoming_requests: Vec<FriendProfileResponse>,
    #[serde(rename = "outgoingRequests", default)]
    outgoing_requests: Vec<FriendProfileResponse>,
}

#[derive(Deserialize)]
struct FriendProfileResponse {
    #[serde(rename = "profileId")]
    profile_id: String,
    name: String,
}

#[derive(Serialize)]
struct FriendMutationRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(rename = "profileId", skip_serializing_if = "Option::is_none")]
    profile_id: Option<String>,
    #[serde(rename = "updateType")]
    update_type: FriendUpdateType,
}

impl TryFrom<FriendMutation> for FriendMutationRequest {
    type Error = FriendsServiceError;

    fn try_from(value: FriendMutation) -> Result<Self, Self::Error> {
        let (name, profile_id, update_type) = match value {
            FriendMutation::SendByName(name) => (
                Some(validate_profile_name(&name)?.to_owned()),
                None,
                FriendUpdateType::Add,
            ),
            FriendMutation::Accept(profile_id) => {
                (None, Some(profile_id.to_string()), FriendUpdateType::Add)
            }
            FriendMutation::Decline(profile_id)
            | FriendMutation::Cancel(profile_id)
            | FriendMutation::Remove(profile_id) => {
                (None, Some(profile_id.to_string()), FriendUpdateType::Remove)
            }
        };
        Ok(Self {
            name,
            profile_id,
            update_type,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum FriendUpdateType {
    Add,
    Remove,
}

#[derive(Serialize)]
struct PresenceRequest {
    status: &'static str,
}

#[derive(Deserialize)]
struct PresenceResponse {
    #[serde(default)]
    presence: Vec<PresenceEntryResponse>,
}

#[derive(Deserialize)]
struct PresenceEntryResponse {
    #[serde(rename = "profileId")]
    profile_id: String,
    status: String,
    #[serde(rename = "lastUpdated")]
    last_updated: String,
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    struct ExpectedResponse {
        status: &'static str,
        headers: &'static str,
        body: &'static str,
    }

    fn session() -> Session {
        Session {
            access_token: "friends-test-access-token".to_owned(),
            profile: crate::Profile {
                id: Uuid::nil(),
                name: "TestPlayer".to_owned(),
                skin: None,
            },
            expires_at: u64::MAX,
        }
    }

    async fn loopback_service(responses: Vec<ExpectedResponse>) -> (Url, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback HTTP fake");
        let address = listener.local_addr().expect("loopback listener address");
        let (requests_tx, requests_rx) = mpsc::channel();
        tokio::spawn(async move {
            for response in responses {
                let (mut stream, _) = listener.accept().await.expect("accept fake request");
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                let header_end = loop {
                    let read = stream.read(&mut buffer).await.expect("read fake request");
                    assert!(read > 0, "request ended before headers");
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers =
                    std::str::from_utf8(&request[..header_end]).expect("request headers UTF-8");
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                while request.len() < header_end + content_length {
                    let read = stream
                        .read(&mut buffer)
                        .await
                        .expect("read fake request body");
                    assert!(read > 0, "request ended before declared body length");
                    request.extend_from_slice(&buffer[..read]);
                }
                requests_tx
                    .send(String::from_utf8(request).expect("request is UTF-8"))
                    .expect("test still reads captured request");
                let chunked = response.headers.contains("Transfer-Encoding: chunked");
                let body = if chunked {
                    format!(
                        "{:X}\r\n{}\r\n0\r\n\r\n",
                        response.body.len(),
                        response.body
                    )
                } else {
                    response.body.to_owned()
                };
                let content_length = if chunked {
                    String::new()
                } else {
                    format!("Content-Length: {}\r\n", response.body.len())
                };
                let wire = format!(
                    "HTTP/1.1 {}\r\nConnection: close\r\n{}{}\r\n{}",
                    response.status, content_length, response.headers, body
                );
                stream
                    .write_all(wire.as_bytes())
                    .await
                    .expect("write fake response");
            }
        });
        (
            Url::parse(&format!("http://{address}/")).expect("loopback fake URL"),
            requests_rx,
        )
    }

    #[tokio::test]
    async fn typed_operations_preserve_the_verified_routes_headers_and_bodies() {
        let (base, requests) = loopback_service(vec![
            ExpectedResponse {
                status: "200 OK",
                headers: "",
                body: r#"{"friendsPreferences":{"friends":"ENABLED","acceptInvites":"DISABLED"}}"#,
            },
            ExpectedResponse {
                status: "200 OK",
                headers: "ETag: \"friends-v1\"\r\nRetry-After: 12\r\n",
                body: r#"{"friends":[{"profileId":"61699b2ed3274a019f1e0ea8c3f06bc6","name":"Dinnerbone"}],"incomingRequests":[{"profileId":"853c80ef3c3749fdaa49938b674adae6","name":"jeb_"}]}"#,
            },
            ExpectedResponse {
                status: "200 OK",
                headers: "",
                body: r#"{"friends":[],"incomingRequests":[],"outgoingRequests":[{"profileId":"069a79f444e94726a5befca90e38aaf5","name":"Notch"}]}"#,
            },
            ExpectedResponse {
                status: "200 OK",
                headers: "",
                body: r#"{"friendsPreferences":{"friends":"DISABLED","acceptInvites":"ENABLED"}}"#,
            },
            ExpectedResponse {
                status: "200 OK",
                headers: "ETag: \"presence-v1\"\r\n",
                body: r#"{"presence":[{"profileId":"61699b2e-d327-4a01-9f1e-0ea8c3f06bc6","status":"PLAYING_SERVER","lastUpdated":"2026-09-05T00:00:00Z"}]}"#,
            },
        ])
        .await;
        let service = FriendsService::for_test_base(base).expect("loopback fake is permitted");
        let session = session();

        let attributes = service
            .get_attributes(&session)
            .await
            .expect("attributes response");
        assert_eq!(
            attributes.preferences,
            FriendsPreferences {
                enabled: true,
                allow_requests: false,
            }
        );

        let friends = service
            .get_friends(&session, None)
            .await
            .expect("friends response");
        let CachedResponse::Fresh {
            value: snapshot,
            entity_tag,
            retry_after,
        } = friends
        else {
            panic!("a 200 Friends response must be fresh");
        };
        assert_eq!(snapshot.friends.len(), 1);
        assert_eq!(snapshot.incoming.len(), 1);
        assert!(
            snapshot.outgoing.is_empty(),
            "missing optional lists are empty"
        );
        assert_eq!(retry_after.unwrap().duration(), Duration::from_secs(12));
        let entity_tag = entity_tag.expect("Friends ETag reaches caller");

        let mutation = service
            .mutate_friend(&session, FriendMutation::SendByName("  Notch  ".to_owned()))
            .await
            .expect("mutation response");
        assert_eq!(mutation.outgoing[0].name, "Notch");

        let preferences = FriendsPreferences {
            enabled: false,
            allow_requests: true,
        };
        assert_eq!(
            service
                .set_preferences(&session, preferences)
                .await
                .expect("preferences response")
                .preferences,
            preferences
        );

        let presence = service
            .publish_presence(&session, PresenceStatus::Online, Some(&entity_tag))
            .await
            .expect("presence response");
        let CachedResponse::Fresh { value, .. } = presence else {
            panic!("a 200 presence response must be fresh");
        };
        assert_eq!(value.entries[0].status, PresenceStatus::Server);

        let captured: Vec<_> = (0..5)
            .map(|_| requests.recv().expect("captured request"))
            .collect();
        for request in &captured {
            assert!(
                request.contains("authorization: Bearer friends-test-access-token\r\n"),
                "every service request uses the session bearer header"
            );
        }
        assert!(captured[0].starts_with("GET /player/attributes HTTP/1.1\r\n"));
        assert!(captured[1].starts_with("GET /friends HTTP/1.1\r\n"));
        assert!(captured[2].starts_with("PUT /friends HTTP/1.1\r\n"));
        assert!(captured[2].ends_with(r#"{"name":"Notch","updateType":"ADD"}"#));
        assert!(captured[3].starts_with("POST /player/attributes HTTP/1.1\r\n"));
        assert!(captured[3].ends_with(
            r#"{"friendsPreferences":{"friends":"DISABLED","acceptInvites":"ENABLED"}}"#
        ));
        assert!(captured[4].starts_with("POST /presence HTTP/1.1\r\n"));
        assert!(captured[4].contains("if-none-match: \"friends-v1\"\r\n"));
        assert!(captured[4].ends_with(r#"{"status":"ONLINE"}"#));
    }

    #[tokio::test]
    async fn not_modified_and_redirects_do_not_become_empty_or_forwarded_requests() {
        let (base, requests) = loopback_service(vec![
            ExpectedResponse {
                status: "304 Not Modified",
                headers: "ETag: \"friends-v2\"\r\nRetry-After: 9\r\n",
                body: "",
            },
            ExpectedResponse {
                status: "302 Found",
                headers: "Location: http://127.0.0.1:1/elsewhere\r\n",
                body: "",
            },
        ])
        .await;
        let service = FriendsService::for_test_base(base).expect("loopback fake is permitted");
        let session = session();
        let tag = EntityTag(HeaderValue::from_static("\"friends-v1\""));

        let not_modified = service
            .get_friends(&session, Some(&tag))
            .await
            .expect("304 is a successful conditional response");
        let CachedResponse::NotModified { retry_after, .. } = not_modified else {
            panic!("304 must not erase the cached snapshot");
        };
        assert_eq!(retry_after.unwrap().duration(), Duration::from_secs(9));

        let error = service
            .get_attributes(&session)
            .await
            .expect_err("a redirect is never followed by the Friends client");
        assert!(matches!(
            error,
            FriendsServiceError::Rejected { status: 302 }
        ));
        let captured: Vec<_> = (0..2)
            .map(|_| requests.recv().expect("captured request"))
            .collect();
        assert!(captured[0].contains("if-none-match: \"friends-v1\"\r\n"));
        assert!(captured[1].starts_with("GET /player/attributes HTTP/1.1\r\n"));
    }

    #[tokio::test]
    async fn chunked_success_response_is_capped_while_remaining_usable() {
        let (base, requests) = loopback_service(vec![ExpectedResponse {
            status: "200 OK",
            headers: "Transfer-Encoding: chunked\r\n",
            body: r#"{"friends":[]}"#,
        }])
        .await;
        let service = FriendsService::for_test_base(base).expect("loopback fake is permitted");
        let fresh = service
            .get_friends(&session(), None)
            .await
            .expect("a chunked response below the cap is valid");
        let CachedResponse::Fresh { value, .. } = fresh else {
            panic!("chunked 200 response must be fresh");
        };
        assert!(value.friends.is_empty());
        assert!(
            requests
                .recv()
                .expect("captured request")
                .starts_with("GET /friends HTTP/1.1\r\n")
        );
    }

    #[test]
    fn test_origins_are_loopback_only_and_friend_names_are_checked_before_networking() {
        assert!(is_loopback_http_origin(
            &Url::parse("http://127.0.0.1:8080/").expect("loopback URL")
        ));
        assert!(is_loopback_http_origin(
            &Url::parse("https://[::1]:8080/").expect("IPv6 loopback URL")
        ));
        assert!(!is_loopback_http_origin(
            &Url::parse("https://api.minecraftservices.com/").expect("production URL")
        ));
        assert!(matches!(
            FriendMutationRequest::try_from(FriendMutation::SendByName("bad name".to_owned())),
            Err(FriendsServiceError::InvalidInput)
        ));
    }
}
