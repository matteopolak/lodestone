# Friends Service Integration Design

## What it is

This design adds the Friends List that ships with Java Edition 26.2: friend and
request lists, presence, service-backed preferences, title- and pause-menu entry
points, badges, and notifications. It uses the Minecraft-services session that
`lodestone-auth` already resolves for the selected account and keeps every bearer
credential outside render and menu state.

Internet world invitations are not part of this design. That transport appeared in
an earlier 26.2 snapshot and was removed before the release. No production control,
endpoint, presence payload, or dependency for that withdrawn feature is introduced
here.

## Goals and boundaries

The feature is complete when a signed-in player can:

- opt into Friends and later turn it off;
- see friends, incoming requests, outgoing requests, and current presence;
- send a request by profile name, accept or decline an incoming request, cancel an
  outgoing request, and remove a friend;
- control whether requests are accepted, whether notifications appear in a world,
  and how much activity presence reveals;
- receive a bounded pending-request badge and change notifications; and
- distinguish authentication, privacy, rate-limit, service, input, and malformed-
  response failures without exposing a credential.

The initial implementation does not add blocked-player management, cross-server
parties, shared worlds, Realms integration, chat filtering, or a general-purpose
social graph. The existing Social Interactions screen remains a separate list of
players in the current session. Friends is an account-scoped service that exists
before a world has been joined.

The [26.2 release notes](https://feedback.minecraft.net/hc/en-us/articles/46690753273997-Minecraft-Java-Edition-26-2)
are the supported behavioral target. The
[26.2 pre-release notes](https://feedback.minecraft.net/hc/en-us/articles/46153634280333-Minecraft-Java-Edition-26-2-Pre-release-1)
are the boundary evidence for excluding internet world invitations.

## How it works

### Chosen architecture

The feature has three layers:

1. `lodestone-auth::friends` owns typed HTTP requests and responses. It accepts an
   already-resolved `Session` and never opens account storage or refreshes a token.
2. A shell-owned `FriendsRuntime` owns the live session, cache validators, timers,
   retries, and service calls for the lifetime of the application. It is the only
   layer that resolves or refreshes the selected account.
3. `menu::friends` owns pure navigation, layout, and view models. It receives a
   credential-free `FriendsView` and emits typed `FriendsCommand` values.

This split follows the existing account boundary: `lodestone-auth` understands
Minecraft services and secret storage, while the shell understands screens and
whether a world is active. It also gives tests a service-level HTTP seam and a
separate pure state-machine seam.

Two alternatives are deliberately rejected. Putting the HTTP client directly in
the menu would let credentials, retry state, and network futures leak into frame
code. Making a combined Friends-and-P2P subsystem would couple a released HTTP
surface to a withdrawn signaling transport and make the supported part impossible
to finish independently.

## Typed `lodestone-auth` service API

### Service construction

`FriendsService::production(reqwest::Client)` uses the fixed
`https://api.minecraftservices.com` origin. There is no environment variable,
command-line flag, or preferences-file field that can replace this origin: a
runtime override could silently send the selected account's bearer token elsewhere.

Hermetic integration tests use
`FriendsService::for_test_base(reqwest::Client, url::Url)`. The constructor is
compiled only for crate tests or a non-default `friends-test-service` feature, and
it accepts only loopback HTTP origins. `lodestone-shell` enables that feature only
for its dev-dependency. Redirects are disabled for this client so a loopback fake
cannot redirect an authenticated request to another host.

Production and test constructors build the same endpoint paths and execute the same
serialization, header handling, status classification, and response parsing. Tests
therefore replace only the origin, not the behavior under test.

### Public domain types

The module exposes credential-free types:

- `FriendProfile { profile_id, name }`;
- `FriendsSnapshot { friends, incoming, outgoing }`;
- `PresenceSnapshot { entries }` and `PresenceEntry { profile_id, status,
  last_updated }`;
- `PresenceStatus::{Offline, Online, LocalWorld, LanWorld, Realm, Server,
  Unknown}`;
- `FriendMutation::{SendByName, Accept, Decline, Cancel, Remove}`;
- `FriendsPreferences { enabled, allow_requests }`;
- opaque `EntityTag` and `RetryHint` wrappers; and
- `CachedResponse<T>::Fresh { value, entity_tag, retry_after }` or
  `NotModified { entity_tag, retry_after }`.

The wire's peer-messaging identity is decoded but discarded. It has no consumer in
the shipped Friends feature, is not persisted, and must not become a back door into
the deferred world-invitation work. Unknown presence values become `Unknown` on the
affected row rather than rejecting the whole response.

Names are treated as display data on responses. A name submitted by the player is
trimmed and checked against the existing profile-name length and character rules
before any request is sent. Profile UUIDs, not names, identify every later action.

### Operations

The typed API is:

```text
get_attributes(session) -> UserFriendsAttributes
get_friends(session, entity_tag) -> CachedResponse<FriendsSnapshot>
mutate_friend(session, FriendMutation) -> FriendsSnapshot
set_preferences(session, FriendsPreferences) -> UserFriendsAttributes
publish_presence(session, desired_status, entity_tag)
    -> CachedResponse<PresenceSnapshot>
```

`get_attributes` reads `/player/attributes` and extracts only the Friends fields
needed here. Missing optional sections use conservative defaults: Friends disabled
and requests disabled. It does not copy unrelated account attributes into the shell.

`get_friends` sends `GET /friends`, bearer authentication, and `If-None-Match` when
an entity tag is available. A `304` is a successful `NotModified`, never an empty
list. A fresh response decodes `friends`, `incomingRequests`, and
`outgoingRequests`; a missing list is treated as empty, while a malformed row is a
typed response error rather than silently changing the graph.

`mutate_friend` sends `PUT /friends`. Adding by name and accepting by profile UUID
use the service's `ADD` wire value; declining, canceling, and removing by UUID use
`REMOVE`. The public enum preserves the player's intent even though several intents
share a wire value. A successful mutation returns the service's complete snapshot,
which replaces the cache atomically. It also invalidates the old friends entity tag.

`set_preferences` sends only the Friends preference section to
`POST /player/attributes`, using `ENABLED` or `DISABLED` for Friends and request
acceptance. It does not overwrite unrelated attributes. The runtime reads the
returned attributes when present; an empty successful response commits the exact
requested values.

`publish_presence` sends `POST /presence` with the desired status and an independent
presence entity tag. It decodes the returned presence of friends and handles `304`
the same way as the Friends list. The module never invents an invitation or join
field.

Every request has a five-second total timeout and a bounded response body. JSON
request and response structures remain private so callers cannot depend on wire
accidents instead of the domain types above.

## Token ownership and refresh boundary

`FriendsService` does not refresh credentials. Each method borrows `&Session` only
for the request and does not retain or clone its `access_token`. Its `Debug` output
contains the service origin but never request headers, bodies containing account
data, or the session.

`FriendsRuntime` owns at most one resolved `Session`. At startup and whenever the
account screen changes the selected profile, it calls the existing selected-account
resolver. The returned profile UUID must equal the selection that triggered the
resolution; a concurrent account change discards the result. Changing accounts also
clears snapshots, validators, queued notifications, and pending mutations before any
new account is displayed.

Before a due request, the runtime re-resolves a session when the cached session is
inside the existing refresh margin. A `401` discards the session and permits one
fresh resolution and one retry. No operation retries more than once after
authentication, and a mutation with an ambiguous transport failure is never replayed
automatically. This avoids turning a timeout after a successful write into a second
user action.

No bearer token crosses the runtime-to-UI channel. The UI receives only a profile
UUID, display name, service state, and user-facing error classification. A selected
offline identity cannot use Friends; the button instead routes to the existing
account screen with an explanation that a signed-in account is required.

## Polling and mutation state machine

One `FriendsRuntime` exists for the application, independent of `NetClient` and a
world session. This matters because the Friends button and background badge are live
on the title screen. Native builds run it on one named background thread with a
current-thread async runtime; browser builds use the existing local-task mechanism.
Both drive the same pure `FriendsCoordinator` state machine against an injected
monotonic clock.

The coordinator states are:

- `Disabled`: no selected online account, Friends disabled, or application shutdown;
- `Resolving`: one selected-session resolution is in flight;
- `Ready`: a cache exists or an initial fetch is due;
- `FetchingFriends` or `PublishingPresence`: exactly one service request is in flight;
- `Mutating`: one user mutation is in flight and later mutations remain queued in UI
  order; and
- `Backoff`: stale data may remain visible, but no request may start before the
  recorded deadline.

The coordinator owns separate validators and due times for Friends and presence.
It never starts a foreground fetch beside a background fetch. Opening the overlay
raises the desired cadence and marks an absent or stale snapshot due, but an existing
request remains the single producer and a service-supplied minimum delay is still
honored.

The default Friends cadence is one minute while the overlay is open and five minutes
otherwise. A valid positive `Retry-After` delta takes precedence by setting the
earliest next request. Invalid or absent headers fall back to the local cadence. A
ten-second floor prevents UI actions or repeated opens from bypassing the service's
cooldown. Every positive, representable service delay is honored; deadline arithmetic
saturates instead of wrapping when a value is too large. A malformed value is recorded
at debug level without logging the response body.

Presence is sent immediately after a semantic status change, with a ten-second
debounce so rapid screen/world transitions collapse into one update. Its local desired
refresh cadence is five minutes while Friends is enabled; a service-provided interval
may move that deadline later and is the earliest legal send time. Friends and presence
requests share a single in-flight slot, with a user mutation first, an overdue presence
update second, and a list refresh third.

Rate limiting uses `Retry-After`, or one minute when it is absent. Transport and 5xx
failures back off for 15, 30, 60, 120, 240, then 300 seconds, remaining at five minutes
after that. A success resets this sequence. Opening the overlay may expose the last
error and a stale marker but cannot override backoff. The runtime stays quiet for
repeated background failures; it emits a new notification only when the error class
changes or user action fails.

A successful mutation atomically replaces all three lists, clears the friends
validator, resets backoff, and schedules the next normal poll. There is no optimistic
graph edit. Buttons for the affected row remain busy until the response arrives, so
a failed operation cannot briefly display a relationship the service never accepted.

## Preferences and presence semantics

Friends-enabled and allow-requests are service-backed attributes. The shell keeps a
per-account mirror only to render immediately; startup always reconciles it with
`get_attributes`, and only a successful update changes the durable mirror. Turning
Friends off also clears live caches and suppresses polling and notifications.

A profile with no local row starts as `Undecided`, with in-world notifications off
and visibility `Full`. Service attributes replace the unknown Friends/request values
after the first successful fetch. `Undecided` is what triggers the one-time opt-in
prompt; dismissing or declining it records `Disabled` without changing the other
local defaults.

In-world notifications and visibility are local preferences stored per account:

- `Full` publishes the actual activity;
- `Limited` publishes only `Online` while the application is active; and
- `Hidden` publishes `Offline`.

The actual activity is `Online` at menus, `LocalWorld` in an unpublished integrated
world, `LanWorld` after that world is published, and `Server` in ordinary remote
multiplayer. `Realm` is decoded for friends but is never emitted until Lodestone has
a real Realm session. Closing cleanly attempts one bounded `Offline` update, but
shutdown never waits indefinitely for it and the UI does not claim the update
succeeded without a response.

The local settings file is `friends.json`, versioned and keyed by profile UUID. It
stores only opt-in state, the last successfully confirmed service preferences,
in-world notification choice, and visibility. It never stores friend rows, request
rows, presence, peer-messaging identities, entity tags, access tokens, or raw service
errors.

## Shell state and overlay wiring

`WindowApp` owns a `FriendsHandle`, which has a bounded command sender and a
credential-free latest-state cell. Frame code reads a cloned `FriendsView`; it never
waits for HTTP or locks while drawing. Commands include opening/closing the overlay,
refresh intent, every mutation, preference changes, selected-account changes, and
activity changes.

`menu::friends` adds one Friends screen with two tabs, Friends and Pending. The
Pending tab presents incoming rows before outgoing rows and keeps the two sections
visibly distinct. The add-by-name field uses the existing `EditBox`; lists use the
shared scroll/focus machinery and preserve selection by profile UUID across refreshes.
An empty, loading, stale, disabled, signed-out, and failed state each has explicit
copy rather than sharing an empty list.

The screen records whether it was opened from `MainMenu` or `Paused`. From the title
it owns a normal menu frame over the panorama. From a world it uses the established
overlay path: the world continues rendering and ticking, gameplay input is captured,
and closing returns to the pause menu. Draw and hit-testing consume the same frame
builder in both cases.

The existing title and pause Friends buttons become live when an owning account is
selected. If that account has not opted in, activation opens the confirmation flow;
otherwise it opens the list. Declining opt-in leaves the service disabled but does
not create a dead button: a later click offers the choice again. The `O` binding
uses the same action and return-source rules as clicking the button.

For a new per-account `Undecided` row, the first eligible title-screen display after
attributes resolve schedules that confirmation once through `UiState`; render code
does not mutate navigation while drawing. If the service already reports Friends
enabled, reconciliation skips the prompt and opens the list normally. A declined
prompt is not shown automatically again, although the button remains the explicit
route for reconsidering it.

The badge is derived from `incoming.len()`, rendered as `1` through `5` and `5+`
above that. It is hidden for signed-out, disabled, unresolved, and never-loaded
states rather than showing a misleading zero. The title and pause buttons read the
same value.

Change notifications are derived by diffing two successful snapshots for the same
profile UUID. The first snapshot seeds silently. Later snapshots may notify for a new
incoming request or an outgoing request becoming a friend. A user-initiated action
gets an inline confirmation and is marked in the diff generation so it does not also
produce a duplicate background notification. In-world notifications obey the local
toggle; title-screen notifications remain visible. The queue is bounded, coalesces
duplicate profile/action pairs, and reuses the existing top-right toast timing and
layout vocabulary.

## Errors, privacy, and logging

`FriendsServiceError` preserves actionable classes:

- `Unauthorized` for `401`, surfaced to the UI only if the runtime's single refresh
  and retry also receives it;
- `PrivacyDenied` for `403`;
- `UnknownProfile` for a rejected name lookup;
- `RateLimited { retry_after }` for `429`;
- `Unavailable { retry_after }` for transport failures and 5xx responses;
- `InvalidInput` for local profile-name validation;
- `MalformedResponse` for invalid successful bodies; and
- `Rejected { status }` for other non-success responses.

The service layer retains machine-readable status and retry data. The shell maps it
to stable user-facing copy and never displays an arbitrary response body. A privacy
denial says that account or safety settings may prevent the action; it does not claim
which player's setting caused it. A stale cache remains visible after a recoverable
failure and is labeled stale with the last successful update time.

The `friends` tracing target may record operation kind, response class, elapsed time,
cache hit/not-modified, retry delay, selected profile UUID, and list counts. It must
not record bearer headers, submitted profile names, returned names, response bodies,
peer-messaging identities, or serialized `Session` values. Public credential-holding
types have redacted `Debug` implementations. `Session` and `CachedSession` therefore
replace their current derived `Debug`; `CachedSession` retains the serialization it
needs for secret-store persistence. New service request types remain private and no
new credential-bearing type implements `Serialize`.

## Hermetic verification

No repository test contacts a live account or Minecraft service.

`lodestone-auth` integration tests run a loopback fake server and verify:

- exact methods, paths, bearer and content headers, JSON payloads, and UUID/name
  selection for every operation;
- `If-None-Match`, fresh entity tags, `304`, and independent Friends/presence caches;
- positive, missing, malformed, zero, and oversized `Retry-After` values;
- complete snapshots, missing optional lists, malformed rows, unknown presence, and
  bounded bodies;
- classification of `400`, `401`, `403`, `429`, 5xx, timeout, and connection loss;
- redirects never receive the bearer token;
- the production constructor cannot be redirected by environment or config; and
- debug formatting of `Session`, `CachedSession`, the service, and every error omits
  a sentinel access token.

Pure coordinator tests use a fake clock, fake session resolver, and scripted service.
They prove foreground/background cadence, the ten-second request floor, no duplicate
in-flight fetch, priority ordering, one authentication refresh, mutation non-replay
after ambiguous failure, exponential service backoff, account-switch cache clearing,
first-snapshot notification seeding, and clean disabling.

Shell tests drive real navigation and frame construction. They cover both entry
points and return paths, keyboard and pointer activation, focus/scroll retention,
busy controls, every empty/error state, the shared badge value, notification
suppression in a world, and pixel coverage in the badge and toast rectangles. A
negative-control test must show that removing the `FriendsView` producer makes the
badge/toast witness fail.

One separately invoked live check uses two owned accounts to verify friend-request
round trips, presence propagation, service preference persistence, account switching,
and rate behavior. Its result is evidence for the current service, not a test-suite
dependency. Browser live verification is separate because the service's CORS policy
can differ from native HTTPS; a failed browser check disables the feature visibly on
that target rather than introducing an unaudited credential-bearing relay.

## Staged delivery

The work is split so every stage has a consumer:

1. Add the typed service API, loopback injection seam, domain types, and HTTP tests.
2. Add the runtime, selected-session refresh boundary, state machine, and pure tests;
   expose its credential-free state through `FriendsHandle`.
3. Wire account selection, opt-in, the title entry point, Friends/Pending tabs, and
   mutations. At this point the service changes visible pixels and is not an island.
4. Wire service-backed settings, local visibility/notification preferences, presence,
   the pause entry point, and the `O` binding.
5. Add badges, snapshot-diff notifications, pixel gates, target-specific live checks,
   and update the durable Friends documentation and generated docs index.

The service layer and runtime may land separately only when the next stage is linked
as the immediate consumer. A completed implementation must run the repository health
checks and the wasm confinement checks because authentication, HTTP, timers, and menu
classification all cross target boundaries.

## Explicit P2P feature gate and deferred boundary

There is no P2P Cargo feature in this work because there is no implementation to
compile. The reserved boundary is a future, non-default `friends-p2p` feature that
must own its signaling client, ICE/TURN dependency, transport adapter, invitation
state, and integrated-server admission path. Enabling ordinary Friends must never
enable that feature transitively.

The future feature remains unavailable until its own design identifies a supported
backend and validates its authentication and compatibility contract. It may consume
the stable friend profile UUIDs exposed here, but not hidden wire fields retained by
accident. Production builds hide all invite, request-to-join, and online-world scope
controls unless both the feature and a supported backend capability are present.

No code in this design calls the withdrawn signaling service, publishes join metadata
through presence, adds a WebRTC dependency, changes the Minecraft packet protocol,
or makes an integrated world reachable outside the existing LAN path.

## How to change it

Wire changes stay private inside `lodestone-auth::friends`; add or change a public
domain type only when a shell consumer needs the information. Polling and retry rules
belong in `FriendsCoordinator`, not in menu callbacks. New presentation belongs in
`menu::friends` and must preserve the single frame-builder rule for draw and hit-test.
Account-selection or token changes must continue through the one runtime refresh
boundary rather than giving another subsystem direct access to the secret store.

If internet invitations return, write a separate design against the then-current
official release and launch artifacts. Do not extend this module from snapshot-only
behavior or infer transport availability from the presence enum.

## Configuration

- Production service origin: fixed to `https://api.minecraftservices.com`.
- Friends poll cadence: one minute with the overlay open, five minutes otherwise,
  subject to `Retry-After` and the ten-second floor.
- Presence debounce: ten seconds; forced refresh at five minutes unless the service
  asks for a later time.
- Service backoff: 15, 30, 60, 120, 240, then 300 seconds maximum.
- Request timeout: five seconds with bounded response bodies and redirects disabled.
- Local preferences: versioned `friends.json`, keyed by profile UUID.
- Test endpoint override: loopback only and compiled behind tests or the non-default
  `friends-test-service` feature.
- Future transport gate: non-default `friends-p2p`, absent from this delivery.

## Dependencies

The service layer depends on the existing `reqwest`, `serde`, `serde_json`, `uuid`,
and time utilities already used by `lodestone-auth`; `url` is required for validated
endpoint construction if it is not already available transitively. Tests use the
workspace's loopback HTTP fixture style rather than a live-service SDK.

The runtime depends on `lodestone-auth::Session`, the selected-account resolver, the
existing native/browser task split, a bounded channel, and an injected monotonic
clock. The shell layer depends on the shared menu widgets, `EditBox`, list/focus
machinery, toast geometry, account-selection state, and session-kind/publish state.
It does not depend on `lodestone-server`, a protocol-family crate, WebRTC, a signaling
SDK, or a proxy service.
