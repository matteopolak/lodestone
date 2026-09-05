//! The shell-owned Friends session and polling coordinator.
//!
//! `lodestone_auth::friends` deliberately stops at an already-resolved
//! [`lodestone_auth::Session`]. This module is the next boundary: it owns that
//! session, decides when it needs resolving again, and exposes only
//! [`FriendsView`] plus credential-free [`FriendsOperation`] values to the
//! window driver. A background worker may borrow [`FriendsRuntime::session`] to
//! execute an operation, but render and menu state never can.
//!
//! The coordinator is synchronous and takes its clock as an argument. That is
//! intentional: one native thread and a browser local task can drive identical
//! ordering and retry rules, while tests use synthetic monotonic times rather
//! than sleeping.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::time::Duration;

use lodestone_auth::friends::{
    CachedResponse, EntityTag, FriendMutation, FriendProfile, FriendsPreferences, FriendsServiceError,
    FriendsSnapshot, PresenceSnapshot, PresenceStatus, RetryHint, UserFriendsAttributes,
};
use lodestone_auth::Session;
use uuid::Uuid;

const SESSION_REFRESH_MARGIN: u64 = 5 * 60;
const REQUEST_FLOOR: Duration = Duration::from_secs(10);
const OPEN_FRIENDS_CADENCE: Duration = Duration::from_secs(60);
const BACKGROUND_FRIENDS_CADENCE: Duration = Duration::from_secs(5 * 60);
const PRESENCE_CADENCE: Duration = Duration::from_secs(5 * 60);
const PRESENCE_DEBOUNCE: Duration = Duration::from_secs(10);
const RATE_LIMIT_FALLBACK: Duration = Duration::from_secs(60);
const BACKOFF_STEPS: [Duration; 6] = [
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(240),
    Duration::from_secs(300),
];

/// The two clocks the runtime needs. `now` is monotonic and schedules service
/// work; `unix_seconds` is used only to decide whether the secret session is
/// inside its existing refresh margin.
pub trait FriendsClock {
    fn now(&self) -> Duration;
    fn unix_seconds(&self) -> u64;
}

/// The production clock used by the application-owned runtime. Its monotonic
/// origin is process-local by design: coordinator deadlines are never persisted
/// or compared across launches.
#[derive(Debug)]
pub struct SystemFriendsClock {
    origin: crate::platform::Instant,
}

impl Default for SystemFriendsClock {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemFriendsClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: crate::platform::Instant::now(),
        }
    }
}

impl FriendsClock for SystemFriendsClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn unix_seconds(&self) -> u64 {
        crate::platform::epoch_duration().as_secs()
    }
}

/// A selected account as it appears in the Friends UI. It has no credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendsAccount {
    pub profile_id: Uuid,
    pub display_name: String,
}

/// Stable, credential-free state that may cross to frame code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FriendsView {
    pub account: Option<FriendsAccount>,
    pub state: FriendsViewState,
    pub preferences: Option<FriendsPreferences>,
    pub snapshot: Option<FriendsSnapshot>,
    pub presence: Option<PresenceSnapshot>,
    pub stale: bool,
    pub error: Option<FriendsError>,
}

/// A relationship change that frame code may present without a session or any
/// service implementation detail.
///
/// The first successful snapshot is only a baseline. Later snapshots emit a
/// request when its profile newly enters `incoming`, or a friendship when a
/// profile moves from `outgoing` to `friends`. Those are the two changes the
/// public view can establish without guessing at an absent notification
/// preference or attributing the player's own mutation to somebody else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FriendsNotification {
    RequestReceived(FriendProfile),
    FriendshipAccepted(FriendProfile),
}

/// Diffs credential-free [`FriendsView`] snapshots for presentation consumers.
///
/// This is deliberately separate from [`FriendsCoordinator`]: the worker
/// retains the session and its cache, while this feed consumes the same safe
/// views an app or HUD already receives. Replacing the selected account or
/// losing its snapshot seeds a new baseline rather than leaking a prior
/// account's changes into the next one.
#[derive(Debug, Default)]
pub struct FriendsNotificationFeed {
    account: Option<Uuid>,
    snapshot: Option<FriendsSnapshot>,
}

impl FriendsNotificationFeed {
    /// Incorporates `view`, returning only changes after a baseline exists for
    /// the selected account.
    pub fn update(&mut self, view: &FriendsView) -> Vec<FriendsNotification> {
        let account = view.account.as_ref().map(|account| account.profile_id);
        if self.account != account {
            self.account = account;
            self.snapshot = None;
        }

        let Some(snapshot) = view.snapshot.as_ref() else {
            self.snapshot = None;
            return Vec::new();
        };
        let Some(previous) = self.snapshot.replace(snapshot.clone()) else {
            return Vec::new();
        };

        let known_incoming = profile_ids(&previous.incoming);
        let pending_outgoing = profile_ids(&previous.outgoing);
        let mut notifications = Vec::new();
        for profile in &snapshot.incoming {
            if !known_incoming.contains(&profile.profile_id) {
                notifications.push(FriendsNotification::RequestReceived(profile.clone()));
            }
        }
        for profile in &snapshot.friends {
            if pending_outgoing.contains(&profile.profile_id) {
                notifications.push(FriendsNotification::FriendshipAccepted(profile.clone()));
            }
        }
        notifications
    }

    /// Discards the baseline on an explicit service-side disable or app
    /// shutdown. A later re-enable starts quietly from its fresh snapshot.
    pub fn clear(&mut self) {
        self.account = None;
        self.snapshot = None;
    }
}

fn profile_ids(profiles: &[FriendProfile]) -> HashSet<Uuid> {
    profiles.iter().map(|profile| profile.profile_id).collect()
}

/// The service lifecycle rendered by menus. It intentionally says nothing
/// about sessions, headers, or network implementation details.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FriendsViewState {
    #[default]
    Disabled,
    Resolving,
    FetchingAttributes,
    Ready,
    FetchingFriends,
    PublishingPresence,
    Mutating,
    SavingPreferences,
    Backoff,
}

/// Safe error classes for user-facing copy and retry decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FriendsError {
    Unavailable,
    Unauthorized,
    PrivacyDenied,
    UnknownProfile,
    RateLimited,
    InvalidInput,
    MalformedResponse,
    Rejected,
    SignedOut,
}

impl FriendsError {
    fn from_service(error: &FriendsServiceError) -> Self {
        match error {
            FriendsServiceError::Transport(_) | FriendsServiceError::Unavailable { .. } => {
                Self::Unavailable
            }
            FriendsServiceError::Unauthorized => Self::Unauthorized,
            FriendsServiceError::PrivacyDenied => Self::PrivacyDenied,
            FriendsServiceError::UnknownProfile => Self::UnknownProfile,
            FriendsServiceError::RateLimited { .. } => Self::RateLimited,
            FriendsServiceError::InvalidInput => Self::InvalidInput,
            FriendsServiceError::MalformedResponse => Self::MalformedResponse,
            FriendsServiceError::Rejected { .. } => Self::Rejected,
            _ => Self::Unavailable,
        }
    }
}

/// Work to execute on the Friends worker. Every variant is free of a bearer
/// token; the worker borrows the selected [`Session`] from [`FriendsRuntime`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FriendsOperation {
    ResolveSession { profile_id: Uuid },
    FetchAttributes,
    FetchFriends { entity_tag: Option<EntityTag> },
    PublishPresence {
        status: PresenceStatus,
        entity_tag: Option<EntityTag>,
    },
    Mutate { mutation: FriendMutation },
    SetPreferences { preferences: FriendsPreferences },
}

impl FriendsOperation {
    fn is_mutation(&self) -> bool {
        matches!(self, Self::Mutate { .. })
    }
}

/// A completed service request. Session resolution is completed through
/// [`FriendsRuntime::complete_resolution`] because that is the one place a
/// resolved secret is permitted to enter this module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FriendsResponse {
    Attributes(UserFriendsAttributes),
    Friends(CachedResponse<FriendsSnapshot>),
    Presence(CachedResponse<PresenceSnapshot>),
    Mutation(FriendsSnapshot),
    Preferences(UserFriendsAttributes),
}

/// Pure scheduling state. It deliberately contains no [`Session`], service,
/// futures, locks, or wall-clock reads.
#[derive(Debug)]
pub struct FriendsCoordinator {
    account: Option<FriendsAccount>,
    session_ready: bool,
    preferences: Option<FriendsPreferences>,
    snapshot: Option<FriendsSnapshot>,
    presence: Option<PresenceSnapshot>,
    friends_tag: Option<EntityTag>,
    presence_tag: Option<EntityTag>,
    friends_due: Option<Duration>,
    presence_due: Option<Duration>,
    friends_not_before: Duration,
    presence_not_before: Duration,
    request_not_before: Duration,
    backoff_until: Option<Duration>,
    failures: usize,
    overlay_open: bool,
    desired_presence: Option<PresenceStatus>,
    queued_mutations: VecDeque<FriendMutation>,
    queued_preferences: Option<FriendsPreferences>,
    retry_after_resolution: Option<FriendsOperation>,
    in_flight: Option<InFlight>,
    error: Option<FriendsError>,
}

#[derive(Clone, Debug)]
struct InFlight {
    operation: FriendsOperation,
    auth_retry: bool,
}

impl Default for FriendsCoordinator {
    fn default() -> Self {
        Self {
            account: None,
            session_ready: false,
            preferences: None,
            snapshot: None,
            presence: None,
            friends_tag: None,
            presence_tag: None,
            friends_due: None,
            presence_due: None,
            friends_not_before: Duration::ZERO,
            presence_not_before: Duration::ZERO,
            request_not_before: Duration::ZERO,
            backoff_until: None,
            failures: 0,
            overlay_open: false,
            desired_presence: None,
            queued_mutations: VecDeque::new(),
            queued_preferences: None,
            retry_after_resolution: None,
            in_flight: None,
            error: None,
        }
    }
}

impl FriendsCoordinator {
    /// Changes the selected profile. All account-scoped caches and queued work
    /// disappear before a request for the new account can be scheduled.
    pub fn select_account(&mut self, account: Option<FriendsAccount>) {
        *self = Self {
            account,
            ..Self::default()
        };
    }

    /// Stops all polling and discards every account-specific value.
    pub fn shutdown(&mut self) {
        *self = Self::default();
    }

    pub fn set_overlay_open(&mut self, now: Duration, open: bool) {
        self.overlay_open = open;
        if open && self.snapshot.is_none() {
            self.friends_due = Some(self.friends_due.map_or(now, |due| due.min(now)));
        }
    }

    /// User intent only. The coordinator honours cooldowns and backoff rather
    /// than converting repeated button presses into repeated HTTP requests.
    pub fn request_refresh(&mut self, now: Duration) {
        self.friends_due = Some(self.friends_due.map_or(now, |due| due.min(now)));
    }

    pub fn queue_mutation(&mut self, mutation: FriendMutation) {
        self.queued_mutations.push_back(mutation);
    }

    pub fn set_preferences(&mut self, preferences: FriendsPreferences) {
        self.queued_preferences = Some(preferences);
    }

    /// Records a semantic activity change. The debounce is applied here, not
    /// in UI code, so menu/world transitions collapse on every target.
    pub fn set_desired_presence(&mut self, now: Duration, status: PresenceStatus) {
        if self.desired_presence != Some(status) {
            self.desired_presence = Some(status);
            self.presence_due = Some(now.saturating_add(PRESENCE_DEBOUNCE));
        }
    }

    /// Marks a resolved session as ready. Attributes are deliberately fetched
    /// before any list work, including when the service later says Friends is
    /// disabled, so the UI can render the per-account opt-in state accurately.
    pub fn session_resolved(&mut self, now: Duration, profile_id: Uuid, online: bool) {
        if self.account.as_ref().map(|account| account.profile_id) != Some(profile_id) {
            return;
        }
        self.in_flight = None;
        self.session_ready = online;
        if online {
            self.error = None;
            self.friends_due = Some(now);
        } else {
            self.error = Some(FriendsError::SignedOut);
        }
    }

    /// Starts at most one operation. `session_needs_refresh` is supplied by
    /// [`FriendsRuntime`] because expiry is a property of its secret session,
    /// not of the pure scheduler.
    pub fn poll(&mut self, now: Duration, session_needs_refresh: bool) -> Option<FriendsOperation> {
        if self.in_flight.is_some() || self.account.is_none() {
            return None;
        }
        if self.backoff_until.is_some_and(|until| now < until) {
            return None;
        }
        if !self.session_ready || session_needs_refresh {
            return self.start(FriendsOperation::ResolveSession {
                profile_id: self.account.as_ref()?.profile_id,
            }, false, now);
        }
        if let Some(operation) = self.retry_after_resolution.take() {
            return self.start(operation, true, now);
        }
        // Attributes must be the first service call of a resolved session.
        if self.preferences.is_none() {
            return self.start(FriendsOperation::FetchAttributes, false, now);
        }
        if now < self.request_not_before {
            return None;
        }
        if let Some(mutation) = self.queued_mutations.pop_front() {
            return self.start(FriendsOperation::Mutate { mutation }, false, now);
        }
        if let Some(preferences) = self.queued_preferences.take() {
            return self.start(FriendsOperation::SetPreferences { preferences }, false, now);
        }
        if !self.preferences.is_some_and(|preferences| preferences.enabled) {
            return None;
        }
        if self
            .presence_due
            .is_some_and(|due| now >= due.max(self.presence_not_before))
        {
            return self.start(
                FriendsOperation::PublishPresence {
                    status: self.desired_presence.unwrap_or(PresenceStatus::Online),
                    entity_tag: self.presence_tag.clone(),
                }, false, now);
        }
        if self
            .friends_due
            .is_some_and(|due| now >= due.max(self.friends_not_before))
        {
            return self.start(
                FriendsOperation::FetchFriends {
                    entity_tag: self.friends_tag.clone(),
                }, false, now);
        }
        None
    }

    fn start(
        &mut self,
        operation: FriendsOperation,
        auth_retry: bool,
        now: Duration,
    ) -> Option<FriendsOperation> {
        self.in_flight = Some(InFlight {
            operation: operation.clone(),
            auth_retry,
        });
        if !matches!(operation, FriendsOperation::ResolveSession { .. }) {
            self.request_not_before = now.saturating_add(REQUEST_FLOOR);
        }
        Some(operation)
    }

    /// Completes the current service request. A mismatched completion is
    /// ignored: a worker result from an old account must never repaint the new
    /// account's view.
    pub fn complete(
        &mut self,
        now: Duration,
        operation: &FriendsOperation,
        result: Result<FriendsResponse, FriendsServiceError>,
    ) {
        let Some(in_flight) = self.in_flight.take() else {
            return;
        };
        if &in_flight.operation != operation {
            self.in_flight = Some(in_flight);
            return;
        }
        match result {
            Ok(response) => self.complete_success(now, operation, response),
            Err(error) => self.complete_error(now, in_flight, error),
        }
    }

    fn complete_success(&mut self, now: Duration, operation: &FriendsOperation, response: FriendsResponse) {
        self.failures = 0;
        self.backoff_until = None;
        self.error = None;
        match (operation, response) {
            (FriendsOperation::FetchAttributes, FriendsResponse::Attributes(attributes)) => {
                self.preferences = Some(attributes.preferences);
                if attributes.preferences.enabled {
                    self.friends_due = Some(now);
                } else {
                    self.clear_live_caches();
                }
            }
            (FriendsOperation::FetchFriends { .. }, FriendsResponse::Friends(response)) => {
                let hint = match response {
                    CachedResponse::Fresh { value, entity_tag, retry_after } => {
                        self.snapshot = Some(value);
                        self.friends_tag = entity_tag;
                        retry_after
                    }
                    CachedResponse::NotModified { entity_tag, retry_after } => {
                        if entity_tag.is_some() {
                            self.friends_tag = entity_tag;
                        }
                        retry_after
                    }
                };
                self.friends_not_before = now.saturating_add(hint_duration(hint));
                self.friends_due = Some(now.saturating_add(self.friends_cadence().max(hint_duration(hint))));
            }
            (FriendsOperation::PublishPresence { .. }, FriendsResponse::Presence(response)) => {
                let hint = match response {
                    CachedResponse::Fresh { value, entity_tag, retry_after } => {
                        self.presence = Some(value);
                        self.presence_tag = entity_tag;
                        retry_after
                    }
                    CachedResponse::NotModified { entity_tag, retry_after } => {
                        if entity_tag.is_some() {
                            self.presence_tag = entity_tag;
                        }
                        retry_after
                    }
                };
                self.presence_not_before = now.saturating_add(hint_duration(hint));
                self.presence_due = Some(now.saturating_add(PRESENCE_CADENCE.max(hint_duration(hint))));
            }
            (FriendsOperation::Mutate { .. }, FriendsResponse::Mutation(snapshot)) => {
                self.snapshot = Some(snapshot);
                self.friends_tag = None;
                self.friends_due = Some(now.saturating_add(self.friends_cadence()));
            }
            (FriendsOperation::SetPreferences { preferences }, FriendsResponse::Preferences(attributes)) => {
                self.preferences = Some(attributes.preferences);
                if attributes.preferences.enabled {
                    self.friends_due = Some(now);
                    self.presence_due = Some(now);
                } else {
                    self.clear_live_caches();
                }
                // The response is authoritative, rather than assuming the request
                // values won a concurrent service-side preference update.
                let _ = preferences;
            }
            // A typed worker must pair an operation with its matching response.
            // Treat a mismatch as a malformed service outcome rather than applying
            // a plausible-looking cache to the wrong slot.
            _ => self.record_nonretryable_error(operation),
        }
    }

    fn complete_error(&mut self, now: Duration, in_flight: InFlight, error: FriendsServiceError) {
        let class = FriendsError::from_service(&error);
        if matches!(error, FriendsServiceError::Unauthorized) && !in_flight.auth_retry {
            self.session_ready = false;
            self.retry_after_resolution = Some(in_flight.operation);
            return;
        }
        self.error = Some(class);
        // A mutation is never put back after an ambiguous failure. The server may
        // already have applied it before the transport timed out.
        if in_flight.operation.is_mutation() {
            return;
        }
        let retry_after = match error {
            FriendsServiceError::RateLimited { retry_after } => {
                Some(retry_after.map_or(RATE_LIMIT_FALLBACK, RetryHint::duration))
            }
            FriendsServiceError::Unavailable { retry_after } => {
                Some(hint_duration(retry_after).max(self.next_backoff()))
            }
            FriendsServiceError::Transport(_) => Some(self.next_backoff()),
            _ => None,
        };
        if let Some(delay) = retry_after {
            self.failures = self.failures.saturating_add(1);
            self.backoff_until = Some(now.saturating_add(delay));
            return;
        }
        self.schedule_normal_retry(now, &in_flight.operation);
    }

    fn record_nonretryable_error(&mut self, operation: &FriendsOperation) {
        self.error = Some(FriendsError::MalformedResponse);
        if operation.is_mutation() {
            return;
        }
    }

    fn schedule_normal_retry(&mut self, now: Duration, operation: &FriendsOperation) {
        match operation {
            FriendsOperation::FetchFriends { .. } => self.friends_due = Some(now.saturating_add(self.friends_cadence())),
            FriendsOperation::PublishPresence { .. } => self.presence_due = Some(now.saturating_add(PRESENCE_CADENCE)),
            FriendsOperation::FetchAttributes | FriendsOperation::SetPreferences { .. } => {
                self.friends_due = Some(now.saturating_add(BACKGROUND_FRIENDS_CADENCE));
            }
            FriendsOperation::ResolveSession { .. } | FriendsOperation::Mutate { .. } => {}
        }
    }

    fn next_backoff(&self) -> Duration {
        BACKOFF_STEPS[self.failures.min(BACKOFF_STEPS.len() - 1)]
    }

    fn friends_cadence(&self) -> Duration {
        if self.overlay_open { OPEN_FRIENDS_CADENCE } else { BACKGROUND_FRIENDS_CADENCE }
    }

    fn clear_live_caches(&mut self) {
        self.snapshot = None;
        self.presence = None;
        self.friends_tag = None;
        self.presence_tag = None;
        self.friends_due = None;
        self.presence_due = None;
    }

    #[must_use]
    pub fn view(&self, now: Duration) -> FriendsView {
        let state = if self.account.is_none() || self.preferences.is_some_and(|p| !p.enabled) {
            FriendsViewState::Disabled
        } else if self.backoff_until.is_some_and(|until| now < until) {
            FriendsViewState::Backoff
        } else if let Some(in_flight) = &self.in_flight {
            match in_flight.operation {
                FriendsOperation::ResolveSession { .. } => FriendsViewState::Resolving,
                FriendsOperation::FetchAttributes => FriendsViewState::FetchingAttributes,
                FriendsOperation::FetchFriends { .. } => FriendsViewState::FetchingFriends,
                FriendsOperation::PublishPresence { .. } => FriendsViewState::PublishingPresence,
                FriendsOperation::Mutate { .. } => FriendsViewState::Mutating,
                FriendsOperation::SetPreferences { .. } => FriendsViewState::SavingPreferences,
            }
        } else {
            FriendsViewState::Ready
        };
        FriendsView {
            account: self.account.clone(),
            state,
            preferences: self.preferences,
            snapshot: self.snapshot.clone(),
            presence: self.presence.clone(),
            stale: self.error.is_some() && self.snapshot.is_some(),
            error: self.error,
        }
    }
}

fn hint_duration(hint: Option<RetryHint>) -> Duration {
    hint.map_or(Duration::ZERO, RetryHint::duration)
}

/// Owns the one live credential for a selected account. It has no `Debug`
/// implementation on purpose: formatting a runtime must not become another
/// path for a bearer token to escape.
pub struct FriendsRuntime<C> {
    clock: C,
    coordinator: FriendsCoordinator,
    session: Option<Session>,
}

impl<C> fmt::Debug for FriendsRuntime<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FriendsRuntime")
            .field("coordinator", &self.coordinator)
            .field("has_session", &self.session.is_some())
            .finish()
    }
}

impl<C: FriendsClock> FriendsRuntime<C> {
    pub fn new(clock: C) -> Self {
        Self {
            clock,
            coordinator: FriendsCoordinator::default(),
            session: None,
        }
    }

    pub fn select_account(&mut self, account: Option<FriendsAccount>) {
        self.session = None;
        self.coordinator.select_account(account);
    }

    pub fn shutdown(&mut self) {
        self.session = None;
        self.coordinator.shutdown();
    }

    pub fn set_overlay_open(&mut self, open: bool) {
        self.coordinator.set_overlay_open(self.clock.now(), open);
    }

    pub fn request_refresh(&mut self) {
        self.coordinator.request_refresh(self.clock.now());
    }

    pub fn queue_mutation(&mut self, mutation: FriendMutation) {
        self.coordinator.queue_mutation(mutation);
    }

    pub fn set_preferences(&mut self, preferences: FriendsPreferences) {
        self.coordinator.set_preferences(preferences);
    }

    pub fn set_desired_presence(&mut self, status: PresenceStatus) {
        self.coordinator.set_desired_presence(self.clock.now(), status);
    }

    #[must_use]
    pub fn view(&self) -> FriendsView {
        self.coordinator.view(self.clock.now())
    }

    /// Returns one operation to submit, if any. The worker must call either
    /// [`Self::complete_resolution`] or [`Self::complete`] exactly once before
    /// asking for more work, preserving the global one-in-flight invariant.
    pub fn poll(&mut self) -> Option<FriendsOperation> {
        let needs_refresh = self.session.as_ref().is_none_or(|session| {
            session.expires_at <= self.clock.unix_seconds().saturating_add(SESSION_REFRESH_MARGIN)
        });
        self.coordinator.poll(self.clock.now(), needs_refresh)
    }

    /// Borrows the secret only for a worker about to execute a non-resolution
    /// operation. Menu/render code should never receive this reference.
    pub fn session(&self, operation: &FriendsOperation) -> Option<&Session> {
        (!matches!(operation, FriendsOperation::ResolveSession { .. }))
            .then_some(self.session.as_ref())
            .flatten()
    }

    /// Accepts a selected-account resolution result. A session for an account
    /// that ceased to be selected while resolution ran is discarded.
    pub fn complete_resolution(&mut self, profile_id: Uuid, session: Option<Session>) {
        let matching = session.filter(|session| session.profile.id == profile_id);
        let online = matching.is_some();
        if self
            .coordinator
            .account
            .as_ref()
            .is_some_and(|account| account.profile_id == profile_id)
        {
            self.session = matching;
        }
        self.coordinator.session_resolved(self.clock.now(), profile_id, online);
    }

    pub fn complete(&mut self, operation: &FriendsOperation, result: Result<FriendsResponse, FriendsServiceError>) {
        let unauthorized = matches!(&result, Err(FriendsServiceError::Unauthorized));
        self.coordinator.complete(self.clock.now(), operation, result);
        if unauthorized {
            self.session = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    struct Clock {
        now: Cell<Duration>,
        unix: Cell<u64>,
    }

    impl Clock {
        fn set(&self, seconds: u64) {
            self.now.set(Duration::from_secs(seconds));
            self.unix.set(seconds);
        }
    }

    impl FriendsClock for Clock {
        fn now(&self) -> Duration { self.now.get() }
        fn unix_seconds(&self) -> u64 { self.unix.get() }
    }

    fn account() -> FriendsAccount {
        FriendsAccount { profile_id: Uuid::from_u128(1), display_name: "Player".to_owned() }
    }

    fn enabled() -> UserFriendsAttributes {
        UserFriendsAttributes { preferences: FriendsPreferences { enabled: true, allow_requests: true } }
    }

    fn resolve_and_enable(coordinator: &mut FriendsCoordinator) {
        let now = Duration::ZERO;
        coordinator.select_account(Some(account()));
        assert!(matches!(coordinator.poll(now, false), Some(FriendsOperation::ResolveSession { .. })));
        coordinator.session_resolved(now, account().profile_id, true);
        let operation = coordinator.poll(now, false).expect("attributes");
        assert_eq!(operation, FriendsOperation::FetchAttributes);
        coordinator.complete(now, &operation, Ok(FriendsResponse::Attributes(enabled())));
    }

    #[test]
    fn account_switch_clears_cache_and_late_resolution_cannot_restore_it() {
        let mut coordinator = FriendsCoordinator::default();
        resolve_and_enable(&mut coordinator);
        let fetch = coordinator.poll(Duration::from_secs(10), false).expect("friends fetch");
        coordinator.complete(Duration::from_secs(10), &fetch, Ok(FriendsResponse::Friends(CachedResponse::Fresh {
            value: FriendsSnapshot { friends: vec![lodestone_auth::friends::FriendProfile { profile_id: Uuid::from_u128(2), name: "Other".to_owned() }], ..FriendsSnapshot::default() },
            entity_tag: None,
            retry_after: None,
        })));
        coordinator.select_account(Some(FriendsAccount { profile_id: Uuid::from_u128(3), display_name: "New".to_owned() }));
        coordinator.session_resolved(Duration::ZERO, account().profile_id, true);
        let view = coordinator.view(Duration::ZERO);
        assert_eq!(view.account.unwrap().profile_id, Uuid::from_u128(3));
        assert!(view.snapshot.is_none());
        assert_eq!(view.state, FriendsViewState::Ready);
    }

    #[test]
    fn only_one_request_is_in_flight_and_presence_beats_list_refresh() {
        let mut coordinator = FriendsCoordinator::default();
        resolve_and_enable(&mut coordinator);
        coordinator.set_desired_presence(Duration::ZERO, PresenceStatus::Online);
        let first = coordinator.poll(Duration::from_secs(10), false).expect("presence");
        assert!(matches!(first, FriendsOperation::PublishPresence { .. }));
        assert!(coordinator.poll(Duration::from_secs(10), false).is_none());
    }

    #[test]
    fn mutation_transport_failure_is_not_replayed() {
        let mut coordinator = FriendsCoordinator::default();
        resolve_and_enable(&mut coordinator);
        coordinator.queue_mutation(FriendMutation::Remove(Uuid::from_u128(2)));
        let operation = coordinator.poll(Duration::from_secs(10), false).expect("mutation");
        coordinator.complete(Duration::from_secs(10), &operation, Err(FriendsServiceError::Unavailable { retry_after: None }));
        assert!(coordinator.poll(Duration::from_secs(300), false).is_some_and(|next| !matches!(next, FriendsOperation::Mutate { .. })));
    }

    #[test]
    fn one_unauthorized_result_resolves_then_retries_once() {
        let mut coordinator = FriendsCoordinator::default();
        resolve_and_enable(&mut coordinator);
        let fetch = coordinator.poll(Duration::from_secs(10), false).expect("fetch");
        coordinator.complete(Duration::from_secs(10), &fetch, Err(FriendsServiceError::Unauthorized));
        assert!(matches!(coordinator.poll(Duration::from_secs(10), false), Some(FriendsOperation::ResolveSession { .. })));
        coordinator.session_resolved(Duration::from_secs(10), account().profile_id, true);
        assert_eq!(coordinator.poll(Duration::from_secs(10), false), Some(fetch));
    }

    #[test]
    fn backoff_grows_and_overlay_cannot_bypass_it() {
        let mut coordinator = FriendsCoordinator::default();
        resolve_and_enable(&mut coordinator);
        let fetch = coordinator.poll(Duration::from_secs(10), false).expect("fetch");
        coordinator.complete(Duration::from_secs(10), &fetch, Err(FriendsServiceError::Unavailable { retry_after: None }));
        coordinator.set_overlay_open(Duration::from_secs(10), true);
        coordinator.request_refresh(Duration::from_secs(10));
        assert!(coordinator.poll(Duration::from_secs(24), false).is_none());
        assert_eq!(coordinator.view(Duration::from_secs(24)).state, FriendsViewState::Backoff);
        assert!(matches!(coordinator.poll(Duration::from_secs(25), false), Some(FriendsOperation::FetchFriends { .. })));
    }

    #[test]
    fn runtime_discards_a_session_returned_for_a_previous_selection() {
        let clock = Clock::default();
        clock.set(1000);
        let mut runtime = FriendsRuntime::new(clock);
        runtime.select_account(Some(account()));
        let _ = runtime.poll();
        runtime.select_account(Some(FriendsAccount { profile_id: Uuid::from_u128(3), display_name: "New".to_owned() }));
        runtime.complete_resolution(account().profile_id, None);
        assert_eq!(runtime.view().account.unwrap().profile_id, Uuid::from_u128(3));
    }

    #[test]
    fn runtime_debug_redacts_the_retained_access_token() {
        let clock = Clock::default();
        let mut runtime = FriendsRuntime::new(clock);
        runtime.select_account(Some(account()));
        runtime.complete_resolution(account().profile_id, Some(Session {
            access_token: "friends-runtime-sentinel".to_owned(),
            profile: lodestone_auth::Profile { name: "Player".to_owned(), id: account().profile_id, skin: None },
            expires_at: u64::MAX,
        }));
        let debug = format!("{runtime:?}");
        assert!(debug.contains("has_session: true"));
        assert!(!debug.contains("friends-runtime-sentinel"));
    }

    fn profile(id: u128, name: &str) -> FriendProfile {
        FriendProfile {
            profile_id: Uuid::from_u128(id),
            name: name.to_owned(),
        }
    }

    fn view(snapshot: FriendsSnapshot) -> FriendsView {
        FriendsView {
            account: Some(account()),
            snapshot: Some(snapshot),
            ..FriendsView::default()
        }
    }

    #[test]
    fn notification_feed_seeds_then_reports_only_observable_relationship_deltas() {
        let mut feed = FriendsNotificationFeed::default();
        let outgoing = profile(2, "Alex");
        let existing_request = profile(3, "Bea");
        assert!(feed
            .update(&view(FriendsSnapshot {
                outgoing: vec![outgoing.clone()],
                incoming: vec![existing_request],
                ..FriendsSnapshot::default()
            }))
            .is_empty());

        let new_request = profile(4, "Chen");
        let notifications = feed.update(&view(FriendsSnapshot {
            friends: vec![outgoing.clone()],
            incoming: vec![new_request.clone()],
            ..FriendsSnapshot::default()
        }));
        assert_eq!(
            notifications,
            vec![
                FriendsNotification::RequestReceived(new_request),
                FriendsNotification::FriendshipAccepted(outgoing),
            ]
        );
        assert!(feed
            .update(&view(FriendsSnapshot {
                friends: vec![profile(2, "Alex")],
                incoming: vec![profile(4, "Chen")],
                ..FriendsSnapshot::default()
            }))
            .is_empty());
    }

    #[test]
    fn notification_feed_does_not_carry_a_baseline_between_accounts() {
        let mut feed = FriendsNotificationFeed::default();
        assert!(feed
            .update(&view(FriendsSnapshot {
                incoming: vec![profile(2, "Alex")],
                ..FriendsSnapshot::default()
            }))
            .is_empty());
        assert!(feed
            .update(&FriendsView {
                account: Some(FriendsAccount {
                    profile_id: Uuid::from_u128(9),
                    display_name: "Other player".to_owned(),
                }),
                snapshot: Some(FriendsSnapshot {
                    incoming: vec![profile(2, "Alex"), profile(3, "Bea")],
                    ..FriendsSnapshot::default()
                }),
                ..FriendsView::default()
            })
            .is_empty());
    }
}
