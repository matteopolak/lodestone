# Friends service

## What it is

The Java 26.2 Friends List, in four layers. `lodestone-auth::friends` is the credential-safe HTTP boundary turning a resolved account session into typed friend lists, relationship changes, preferences and presence without exposing the bearer token. `lodestone::friends_runtime` retains one selected session, schedules all Friends work and exposes a bearer-free `FriendsView`. `app::friends::FriendsApp` executes it (a named worker with its own current-thread runtime natively, a local-task handle in the browser). `menu::friends` is the presentation.

## How it works

### Service

`FriendsService::production` owns a five-second, redirect-disabled HTTP client and the fixed origin `https://api.minecraftservices.com/`. Callers pass `&Session` per operation; the service opens no token storage and refreshes nothing, and the bearer header goes only to that origin. Routes: `GET`/`PUT /friends`, `GET`/`POST /player/attributes`, `POST /presence`, verified against the shipped 26.2 service library and the official release notes. On `wasm32` the constructor returns `Unavailable`: the browser transport cannot enforce no-redirect or bounded incremental reads, so keep it fail-closed until a verified browser path exists.

`get_friends` and `publish_presence` keep `ETag`/`If-None-Match` through `CachedResponse` (a `304` is never an empty snapshot). A declared length over 1 MiB is rejected early and chunked bodies are rejected as soon as they pass the cap. An unknown presence value affects only its row. Peer-messaging identity and join metadata are deliberately not represented, persisted or forwarded (that transport was removed before 26.2 release). The `friends-test-service` feature exposes `for_test_base`, loopback `http`/`https` only; production has no origin override.

### Runtime and executor

A pure `FriendsCoordinator` runs under `FriendsRuntime` with an injected `FriendsClock` and emits credential-free `FriendsOperation`s. Only the owning worker borrows `FriendsRuntime::session` to run one, then feeds the result back; no completion channel carries a `Session`. The worker resolves the selected account before the first attributes request (`Select, ResolveSession, FetchAttributes`), permits one in-flight operation, and discards resolutions that no longer match the selection. Switching accounts clears snapshots, validators, pending mutations, retry state and the session. The runtime re-resolves inside the five-minute expiry margin; one `401` clears the session and permits one resolution plus one retry; a mutation failing after a transport ambiguity is discarded, not replayed.

Friends and presence keep independent entity tags and due times but share one in-flight slot, with priority queued mutation, due presence, list refresh. List polls run each minute with the overlay open and five minutes in the background; presence debounces ten seconds and refreshes every five minutes. Every request is separated by a ten-second floor (repeated opens or refresh clicks cannot bypass it). Rate limits wait for `Retry-After` (or a minute); unavailable backs off 15, 30, 60, 120, 240, then 300 seconds, with cached data kept visible but stale.

The frame calls `FriendsApp::sync` with the selected profile and activity; it sends only changes, filters late views from a prior account and exposes a bearer-free `view`. Call `FriendsApp::shutdown` on exit to clear state and stop the worker.

### Menu

The title and pause Friends buttons open one three-tab surface: Friends (established relationships), Pending (incoming plus outgoing) and Settings (service-backed availability and request permission). The Online settings page's `Friends List...` and `Allow Requests...` enter the same Settings tab; its `In-Game Notification` and `Visibility` rows are local persisted controls. `PresenceSharing::All` publishes the world type, `Limited` only online, `None` offline.

Friend rows show the latest presence as `MenuRow::trailing` text, joined by profile UUID from `FriendsView::presence.entries`; a missing row stays blank (never inferred as offline) and request rows never consume presence. The Pending tab shows a bounded incoming badge (`1` to `5+`) only after a usable snapshot, never a fabricated zero. The surface renders loading, failure, empty, cached, disabled and saving states. Outbound values are refresh, add-by-profile-name, `Accept`, `Decline`, `Cancel`, `Remove` and a full `FriendsPreferences` replacement. A successful mutation shows a specific inline confirmation; a failure keeps the confirmed relationship state with classified copy (unknown profile, unavailable, rate limit, privacy, rejected, invalid response, expired session, signed out). The Add Friend footer opens a profile-name prompt emitting `FriendMutation::SendByName`. Service settings are disabled until attributes arrive and while saving. For an account that resolves to the disabled default with no local choice, an `Enable Friends` / `Not Now` first-use prompt queues the normal preference update or records a decline (held in the selected runtime; durable per-account consent is a separate persistence concern).

`key.friends` (default `O`) opens the same surface from gameplay via the paused-world route, as an overlay; Escape and Done return to the route that opened it, and menu, chat and container focus suppress the hotkey. The pause route is drawn and hit-tested as an overlay, the title route is a full screen.

### Notifications

`FriendsNotificationFeed` consumes successive `FriendsView`s. It adopts the first list for an account silently; after that, a profile newly in `incoming` produces a received-request toast and one moving from `outgoing` to `friends` an accepted-request toast. These are the only changes recoverable from the safe view that cannot be confused with the player's own action. Account changes, an absent snapshot or a service-side disable reset the baseline and clear the queue. The app coalesces duplicate profile/action pairs, keeps at most five waiting, and starts the five-second display only once the shared top-right toast slot is free (behind recipe and advancement toasts). The in-world notification option is applied only at presentation, so a disabled option cannot consume the slot; title and menu toasts stay eligible.

## How to change it

- Keep wire structs private in `lodestone_auth::friends`; promote only values the runtime or menu consumes. Before changing a route or field, add a loopback test asserting exact method, path, bearer header, validators and JSON body. No invitation, join, signaling or peer identity fields.
- A new HTTP client is constructed inside the module with redirects disabled (a caller-built client can re-enable token forwarding). Never log bodies, names or `Session` values.
- Polling, cooldown, account replacement and auth-retry decisions belong in `FriendsCoordinator`. Each emitted operation completes exactly once before polling again. A new activity source calls `FriendsRuntime::set_desired_presence`, never an HTTP request. `FriendsView` is the only worker-to-frame type. Native work goes in `app::friends::run_native_worker`, browser work in the local-task seam.
- Relationship-delta interpretation stays in `FriendsNotificationFeed`; keep the `outgoing`-to-`friends` acceptance rule, seed new accounts before notifying, and keep local options in `config::Options`, not the service payload.
- Menu: `MenuNav::refresh_friends_view` is the single app-to-menu copy; drain `MenuNav::take_friends_intents` at the app boundary; `FriendsIntent::SetPreferences` routes to `FriendsApp::set_preferences`, never `FriendsService`. Renderers never resolve accounts or call the service.
- A new presence state needs player text in `menu::friends::presence_label` and a loopback test of the raw status.

## Configuration

- Fixed origin, five-second timeout, 1 MiB JSON cap (`Content-Length` is an early check only), five-minute session refresh margin.
- Cadences: list one minute (overlay) or five minutes; presence debounce ten seconds, refresh five minutes; ten-second request floor; backoff 15 to 300 seconds.
- Toast: five seconds, backlog five.
- `options.json`: `in_game_notification` (default `false`), `share_presence` (default `all`; `limited`, `none`).
- Browser target is disabled fail-closed; the test origin override is feature-gated and loopback-only.

## Dependencies

`reqwest`, `serde`/`serde_json`, `uuid`, `lodestone-auth::Session`, and the shell's portable clock adapter. No protocol family, persistent Friends state or invitation transport.
