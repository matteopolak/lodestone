# Friends service

## What it is

`lodestone-auth::friends` is the credential-safe HTTP boundary for the Java 26.2 Friends List. It turns an already-resolved account session into typed friend lists, relationship changes, service preferences, and presence without exposing the bearer token to menu or rendering code.

`lodestone::friends_runtime` is the shell-owned companion boundary. It retains at most one selected account session, schedules all Friends work, and exposes a `FriendsView` with no bearer credential for the eventual title and pause-menu UI.

`app::friends::FriendsApp` is the execution boundary around that coordinator. Native builds run it on a named worker with its own current-thread async runtime; browser builds use a local-task-compatible handle. Both receive only account metadata and activity intent, then return only `FriendsView` snapshots. Selected-account resolution, cache refresh, service calls, and the resolved session stay private to the executor. The frame calls `FriendsApp::sync` with the account switcher's selected profile and the game activity; it sends only changes, filters late views from a prior account, and gives menus a credential-free `view` accessor.

`menu::friends` is the presentation consumer. The title and pause-menu Friends buttons open one two-tab surface: Friends lists established relationships and Pending combines incoming and outgoing requests. It renders loading, failure, empty, and cached-list states from `FriendsView`; its only outbound values are refresh and already-supported `Accept`, `Decline`, `Cancel`, and `Remove` mutations. It deliberately has no text field for sending a request and no preference switches yet: those would need a reviewed user-input and settings flow, not a menu-side call to the service.

## How it works

`FriendsService::production` owns a five-second, redirect-disabled HTTP client and a fixed `https://api.minecraftservices.com/` origin. A caller passes `&Session` to each operation, so the service neither opens token storage nor refreshes credentials. The session's bearer header is added only to the fixed request origin.

The native constructor is the only enabled production path today. On `wasm32`, the constructor returns `Unavailable`: the browser transport exposed here cannot enforce the no-redirect policy or bounded incremental response reads. Keep that fail-closed behavior until a browser-specific service path has been verified.

The implementation uses the documented route family: `GET`/`PUT /friends`, `GET`/`POST /player/attributes`, and `POST /presence`. The 26.2 shipped service-library artifact in the local reference cache was inspected to verify those methods, paths, request names, response fields, cache validators, and presence values. Official 26.2 release notes independently confirm the user-facing Friends behavior, polling cadence, request controls, and presence controls. [Release notes](https://feedback.minecraft.net/hc/en-us/articles/46690753273997-Minecraft-Java-Edition-26-2)

`get_friends` and `publish_presence` preserve `ETag`/`If-None-Match` conditional behavior through `CachedResponse`; `304 Not Modified` never becomes an empty snapshot. A declared length above 1 MiB is rejected early, while chunked responses are accumulated incrementally and rejected immediately once they exceed that cap. Unknown presence values affect only their row. Peer-messaging identity and join metadata are intentionally not represented, persisted, or forwarded.

The non-default `friends-test-service` feature exposes `for_test_base` for downstream hermetic tests. It accepts only `http` or `https` loopback origins. Production has no origin override, including through environment variables or preferences.

The worker always resolves a selected account before the first attributes request, and the coordinator permits only one in-flight operation. A resolution that no longer matches the selected profile is discarded. Service completions are applied while the worker still owns the session, then a view is emitted; no completion channel carries a `Session` or bearer token. The native executor uses the standard production service. The browser executor has the same ordering seam but reports the service unavailable until a redirect-safe browser transport is available.

`FriendsRuntime` drives a pure `FriendsCoordinator` through an injected `FriendsClock`. The coordinator emits a credential-free `FriendsOperation`; only the worker that owns the runtime borrows `FriendsRuntime::session` to run a non-resolution request. The operation result is then fed back into the coordinator. This is the boundary that permits the native single-worker thread and browser local-task runner to share ordering and retry behavior without letting a session into menu state.

The selected account is part of every resolution completion. A completion for an account that was switched away while it ran is ignored, and switching clears snapshots, validators, pending mutations, retry state, and the retained session before the next account can be displayed. Before work, the runtime re-resolves a session inside the existing five-minute expiry margin. One `401` clears it and permits one resolution plus one retry; a mutation that fails after a transport ambiguity is discarded rather than replayed.

Friends and presence keep independent entity tags and due times, but share one in-flight slot. The priority is a queued mutation, due presence, then a list refresh. List polls use a one-minute open-overlay cadence or five-minute background cadence; presence changes debounce for ten seconds and refresh every five minutes. A service delay cannot be bypassed by repeated opens or refresh clicks, and every normal request is separated by a ten-second floor. Rate limits wait for `Retry-After` (or one minute); unavailable requests back off for 15, 30, 60, 120, 240, then 300 seconds. Cached list data remains visible but stale while retrying.

## How to change it

Keep wire request and response structs private in `lodestone_auth::friends`; promote only domain values that a runtime or menu genuinely consumes. Add a loopback test that asserts the exact method, path, bearer header, validators, and JSON body before changing a route or field. Do not add invitation, join, signaling, or peer identity fields here: that transport was removed before 26.2 release, as the official pre-release notes record. [Pre-release boundary](https://feedback.minecraft.net/hc/en-us/articles/46153634280333-Minecraft-Java-Edition-26-2-Pre-release-1)

When changing the shell executor, preserve the `Select → ResolveSession → FetchAttributes` ordering and keep `FriendsView` as the only worker-to-frame type. Native work belongs in `app::friends::run_native_worker`; browser work belongs in the local-task seam, not a blocking executor. The application must call `FriendsApp::shutdown` as it exits so its account-scoped state is cleared and the native worker is asked to stop.

Put polling, cooldown, account replacement, and authentication-retry decisions in `FriendsCoordinator`, not in menu callbacks or the worker. The worker must complete each emitted operation exactly once before polling again. If an app integration adds a new activity source, call `FriendsRuntime::set_desired_presence`; do not send a presence HTTP request directly. `FriendsView` is the only object frame code should retain or clone.

When changing the menu, keep `MenuNav::refresh_friends_view` as the one app-to-menu copy and drain `MenuNav::take_friends_intents` at the app boundary. Do not make a renderer resolve an account or call `FriendsService`, and do not turn repeated refresh clicks into immediate network traffic: the coordinator owns the request floor and backoff. The pause route must be drawn and hit-tested as an overlay over the paused world; the title route is a full menu screen. Both escape and Done must return to the route that opened Friends.

If a new HTTP client is needed, construct it inside this module with redirects disabled. Accepting an arbitrary caller-built client can silently re-enable token forwarding on redirects. Never log response bodies, submitted names, or `Session` values.

## Configuration

- Production origin: fixed `https://api.minecraftservices.com/`.
- Request timeout: five seconds.
- Maximum successful JSON body: 1 MiB; `Content-Length` is an early check, not a requirement.
- Test origin override: `friends-test-service` only, and loopback-only.
- Browser target: disabled fail-closed until its transport can enforce the same redirect and response-size rules.
- Session refresh margin: five minutes before the stored expiry timestamp.
- Friends poll cadence: one minute while the overlay is open; five minutes otherwise.
- Presence cadence/debounce: five minutes and ten seconds respectively.
- Request floor: ten seconds; unavailable backoff: 15, 30, 60, 120, 240, then 300 seconds.

## Dependencies

The service module depends on `reqwest` for HTTPS and headers, `serde`/`serde_json` for private wire shapes, `uuid` for profile identifiers, and the existing `lodestone-auth::Session` resolver boundary. The runtime depends on those public Friends types, `Session`, `uuid`, and the shell's portable clock adapter. Neither layer depends on a protocol family, persistent Friends state, or any multiplayer invitation transport.
