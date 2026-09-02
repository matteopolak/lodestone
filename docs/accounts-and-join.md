# Accounts, join, and chat: identity, scheduling, and secure messaging

## What it is

Everything between "the player wants to connect" and "the player is standing in a synced
world with working chat": Microsoft account storage and the online-mode handshake, the
offline fallback identity, the single resolver that decides which identity a join presents,
the loading-screen readiness rule, the server's join-time chunk-generation scheduler, the
`transfer` tracing target used to debug rubberbanding across a server transfer, the TLS
crypto-provider choice underneath every HTTPS call, secure (signed) chat, and how a server's
`Text` component becomes displayed words.

## How it works

### Accounts and the online-mode handshake (`lodestone-auth`)

An account's data splits across three stores by sensitivity: the Microsoft **refresh token**
and the derived Minecraft **session** (services access token + profile) each live in the OS
keychain, under two different keychain *services* (`dev.lodestone.ms-refresh-token` and
`dev.lodestone.mc-session`) so either can be cleared independently; everything else (username,
profile UUID, skin URL, which account is selected) is a plain JSON file, since the account
switcher must be able to draw its list without unlocking the keychain. `AccountSecrets::open()`
probes the real keychain exactly once per process (a `keyring` backend failure latches
process-wide) and falls back to an in-memory store, recorded as `StorageMode`, if the real
backend cannot be reached — a caller wanting to re-check has to open a fresh instance, in
practice a fresh process. `login::try_cached_session` prefers a still-valid cached session (no
network call, and critically the refresh token is not even read, since redemption rotates it)
before falling back to redeeming the stored refresh token, and only a dead refresh token
(`invalid_grant`) triggers an interactive device-code sign-in.

The join path itself resolves an **intent**, not a resolved session, on the net thread (the
only place that can `await`): `RemoteAuth::SelectedAccount` (the production multiplayer path)
resolves the switcher's selection through the keychain and Microsoft; `RemoteAuth::Offline`
(singleplayer, Open to LAN, and every live gate) never touches either; `RemoteAuth::Session`
carries an already-resolved session in. A failed resolution does not abort the dial — it
becomes `OnlineModeSessionUnavailable`, retaining the selected account's last-known name/UUID,
and the join continues under that identity unless the server actually requests proof. Three
distinguishable failure values exist so a UI (and a bug report) can tell them apart: no client
auth error for explicit offline; `OnlineModeSessionUnavailable { account, detail }` for a
selected-but-unusable account; `ClientError::Auth(AuthError)` for a session-server rejection —
separate again from the **server's own kick text**, which travels as an ordinary
`ClientEvent::Disconnect` and must never be confused with a client-side auth error.

`Driver::begin_encryption` (in `lodestone-client`) is what actually performs the RSA/AES
handshake once `Directive::BeginEncryption` arrives: it RSA-wraps the shared secret against the
server's public key, writes the encryption response in the clear, enables the cipher, and —
only when the server requests authentication and a usable session exists — computes the
server-hash and calls the Mojang session-server `join` **before** the encryption response is
sent (matching vanilla's own handshake ordering; sending the key packet first would race the
`join` POST against the server's own `hasJoined` check). Lodestone authenticates under its own
registered Azure public-client id (`login::DEFAULT_CLIENT_ID`, overridable with
`LODESTONE_MS_CLIENT_ID`), never Mojang's own launcher registration, which would misrepresent
the client to Microsoft. **None of this repo's tests may reach `sessionserver.mojang.com` or a
real OAuth endpoint** — the successful premium join is verified interactively by the owner,
not by any test here.

### The ownership gate

Nothing in the client is reachable until the local account roster holds at least one account
that owns the game — singleplayer, world creation, the multiplayer list and the offline
identity included. `Screen::Ownership` is what the player sees instead: a title, one paragraph
saying what is being asked, and two buttons (Add Account, Quit). Add Account opens the ordinary
account switcher, so an account added through the gate lands in the same `profiles.json` the
switcher reads — there is no second store and no second sign-in path.

**The enforcement is a type, not a check, and that distinction is the whole design.**
`lodestone_auth::Entitlement` has private fields and exactly one constructor,
`Entitlement::from_metadata`, which answers `Some` only for a roster holding an account. Both
play verbs — `nav::MenuAction::Singleplayer` and `nav::MenuAction::Connect` — carry one, so a
future entry path that forgets the gate does not compile. A `bool` consulted at one call site
would have been forgotten by exactly that path, silently.

Two consequences worth knowing:

- **The check is presence-based, not a fresh network call.** A row can only exist in
  `profiles.json` because a completed sign-in produced a Minecraft profile for it, and an
  account with no profile fails the chain with `AuthError::NoMinecraftProfile` before there is
  anything to store — the roster is keyed on the Minecraft profile UUID such an account does
  not have. So "a row exists" and "that account owned the game when it was added" are the same
  statement. Re-verification happens the next time the player signs in or joins an online-mode
  server; requiring it at launch would break offline play, which is not what the gate is for.
- **It is not a security boundary.** `profiles.json` is a plain file in the user's own data
  directory and a determined user can forge one. Nothing stored on a machine its owner controls
  can prevent that; only a server-side check can, which is what online-mode join already does.
  What the gate enforces is that the client will not knowingly let anyone play without having
  added an account that owns the game.

`MenuNav::ownership_gate_blocks` is the screen-level half — one expression consulted by
`MenuNav::key`, `MenuNav::click`, `MenuNav::hover` and `render::frame_for`, so what is drawn and
what a keystroke does cannot disagree. `frame_for` is what draws the gate on the very *first*
frame, before any input has arrived to move `UiState`. Two screens are exempt: `Screen::Accounts`
(the only exit — blocking it would make the gate unopenable) and `Screen::Error` (a real
diagnosis the gate must not paint over), plus every screen `Screen::in_session` reports as
sitting over a live world.

The two native CLI diagnostic modes — `--headless` (render one frame of a world to a PPM) and
`--connect` (dial a server and stream events) — never build a `MenuAction`, so the menu's gate
structurally cannot cover them. They take an `Entitlement` as a parameter instead, resolved in
`app::run` from the real roster, and refuse with a message naming the Accounts screen when there
is none. A third diagnostic mode added later has to obtain one too, or it does not compile.
`Mode::Window` deliberately does *not* go through that check: it has a UI, so showing the gate
and letting the player add an account is a better answer than exiting.

**The browser build cannot currently pass the gate**, because it has no Microsoft sign-in: the
flow needs an OS keychain for the refresh token and a blocking HTTP client, neither of which
exists on `wasm32`. The Add Account button there reports that in a sentence rather than doing
nothing. Implementing a browser sign-in is what unblocks the web build.

### The offline fallback identity

The offline identity is a **display-name choice available once the ownership gate is open**, not
an entry path of its own. Selecting the account switcher's offline row still means "join
without a Microsoft session" — that is what `AccountsMetadata::selected == None` means — but it
no longer lets anyone into the game who has not added an owning account first, because the
switcher itself is behind the gate.


`offline.json` (beside `profiles.json`/`servers.json`) stores exactly one field, the username;
the UUID is never stored, only derived: an MD5 over the UTF-8 bytes of the literal
`OfflinePlayer:` followed by the username, with the version nibble forced to 3 and the variant
bits to RFC 4122. That is a **name-based UUID over those bytes alone**, and specifically **not**
a namespaced `Uuid::new_v3`, which prepends a namespace and so hashes different
bytes and would still look like a stable, plausible UUID. This matters because our own
integrated server, unlike vanilla, echoes back whatever UUID the client presents rather than
deriving one from the username — so a stable derived UUID is what lets a singleplayer save be
found again on a later launch, independent of whatever fixed vanilla's own offline-mode UUID
scheme covers. A missing, corrupt, or invalid stored name silently falls back to the default
name, the same tolerance rule every other preferences file in this codebase follows; a name is
validated both on load and on set, since a name that reaches the wire and gets rejected by a
server is a disconnect with no obvious cause otherwise.

### Which identity a join presents (`join_identity`)

One function, `join_identity::resolve`, is the single producer of the username/UUID a login
packet carries, for every production join. It reads `AccountsMetadata::selected`: if it names
an account with a row in `profiles.json`, that row's identity is used; if it names an account
with no row, the offline identity is used and a **warning** is logged (the player asked for an
account and did not get it); if nothing is selected, the offline identity is used with no log
concern at all. Every arm logs, deliberately, because a session that honoured the selection and
one that silently fell back used to be indistinguishable from the outside. Identity and
*authentication* are different axes that happen to key off the same selected-account UUID:
authentication is skipped entirely for singleplayer (vanilla skips the encryption request for a
memory connection, so there is nothing to prove), while identity resolution still prefers the
selected account there — meaning a world entered before an account was selected has its saves
filed under the offline UUID, and selecting an account afterward finds no save there, matching
vanilla's own behavior when a world is opened by a different account. The one constructor that
stays offline in both senses is `connect_as`, used only by live gates, which take an explicit
name so that concurrent gates never share one player file (offline mode's per-name save is a
known live-server hazard — see the root project rules).

### Join readiness: the loading-screen gate

The loading screen clears only when **two** independent conditions are both satisfied: the
terrain rule (the player's own chunk column has arrived — vanilla's own wait-for-player-chunk
rule) and the asset rule (no server-pushed resource pack is still downloading or waiting to be
applied to the block atlas). Both are measured from one shared clock, the moment the client
enters `ConnectPhase::LoadingTerrain`, with vanilla's own 30-second `CLIENT_WAIT_TIMEOUT` — a
single deadline for the whole client load rather than one per sub-wait, matching vanilla's
`LevelLoadTracker`. Assets are checked first, matching vanilla's precedence between a resource
reload overlay and a loading screen. The world keeps rendering underneath the opaque loading
overlay throughout, so chunks keep meshing and remote-player skins keep resolving while the
screen is up. The ordering inside the frame loop is load-bearing and nothing in the type system
enforces it: the atlas reload must run **before** the loading-overlay check on the same frame,
or the player sees one frame of the stale atlas before the cover goes up. Singleplayer pays
nothing for this — no pack push exists on the integrated server, so the asset half is satisfied
instantly — and a browser build never blocks on a pack download at all, since it has no HTTP
client in its dependency graph for that path.

### The join generation scheduler (server-side)

When a player joins, the server generates the `(2r+1)²` chunk columns of their view through a
**primed sliding window** rather than the naive alternative of generating everything at once
(which serializes on cache contention scaling with view radius) or a per-ring barrier (which
serializes ring `r+1` behind ring `r`'s slowest column). The window width is
`available_parallelism`, floored at 2 and **never given a ceiling** — growth with core count
rather than with view radius is what makes this different from both discarded designs. The
window is "primed": the very first column is generated alone before the window opens to full
width, so the player's own column reaches the client after generating just one column rather
than the whole view (a deliberate one-column serialization, not an oversight). Emission is
always in wire (Chebyshev-ring) coordinate order regardless of which column the pool finishes
first, which is what keeps the encoded byte sequence a pure function of view radius. This
scheduler is no longer only for joins: the same pipeline is fed newly-visible columns as a
player walks, so a move is streamed through it rather than generated in a separate fan-out.

### Transfer tracing

A single `tracing` target, `transfer`, logs every step of the position handshake around a
teleport or a server-initiated transfer — arrival, confirmation, the pose reaching the
simulation, and every outbound movement packet's distance from the last accepted teleport —
specifically to catch the client claiming a position the server has already overruled (a
rubberband). Two mechanisms exist under one user-visible symptom and must not be conflated: a
`minecraft:transfer` **reconnect** tears the socket down and rebuilds all per-connection state
from nothing, while a proxy-driven **backend swap** (Velocity, BungeeCord) keeps the same
socket and carries per-connection state across a `Configuration` round-trip and a second
`LOGIN` — grep the packet log's `login_ordinal` to tell them apart, since a proxy backend swap
never sends `minecraft:transfer` at all. Two real defects this tracing surfaced are fixed: an
outbound `Move` built from a pose the simulation had not yet adopted a teleport correction into
is now rewritten to the teleport-authorized pose before being queued, and a second `LOGIN` on
one connection now clears the previously-decoded chunk store (vanilla assigns a brand-new
client-side level unconditionally on every `LOGIN`; this client used to only clear on a
dimension-changing respawn, which a backend swap never triggers).

### TLS crypto provider

All HTTPS traffic goes through `reqwest` → `rustls`, and this workspace pins the **`ring`**
crypto backend rather than `rustls`'s default `aws-lc-rs`, to keep `aws-lc-sys`'s roughly 1,500
vendored C translation units out of the dependency graph. This requires `reqwest`'s
`rustls-no-provider` feature (there is no `rustls-ring` feature to select directly) plus one
process-wide, idempotent `rustls::crypto::ring::default_provider().install_default()` call,
made immediately before constructing any `reqwest::Client` — not in `main()`, since a
`main()`-only install would leave every test binary panicking. **The failure mode if a call
site is missed is a runtime panic on the first HTTPS request, not a compile error** — every
`cargo check` variant stays green regardless. Enabling rustls's default features anywhere in
the graph (or reqwest's `http3` feature, which pulls in `quinn`) drags `aws-lc-rs` back in
through feature unification.

### Secure chat: session keys and per-message signatures

The client fetches a Mojang-issued RSA chat-signing key pair on login (when signed in with an
online-mode session), announces a chat session to the server, and signs every outgoing chat
message over the real, current last-seen-message window — mirroring vanilla's
`SignedMessageChain`. On the receiving side, another player's announced session (their public
key) is retained from `PLAYER_INFO_UPDATE` and used to verify their signed messages, surfacing
a `verified` bool that a consumer can render as a trust badge (not yet built) — it does **not**
rewrite the message text, since vanilla's "unverified" indicator is a separate sprite beside
the line, not part of the message body. Signing defaults to **on** (`LODESTONE_SECURE_CHAT=0`
opts out, and an unset or unparseable value means enabled — the safe-by-default direction for a
feature whose entire purpose is to be exercised). It was briefly disabled by default after a
real server disconnected the owner with "Chat message validation failure"; the actual cause was
an acknowledgement-bookkeeping bug, not a cryptographic one — both peers must count only
*signed* messages in the last-seen window, and this client's decode reported an `ack` for every
`PLAYER_CHAT` including unsigned ones, so an unsigned message silently advanced the
acknowledgement offset past what the server had counted, and the very first *signed* message
(which finally transmitted the real offset) exposed the drift. The mitigation has been lifted;
the missing signed/unsigned guard is fixed at the point vanilla itself checks it. A server
transfer/reconnect is already transfer-safe by construction — a fresh connection always
re-fetches and re-announces a fresh chat session and starts an empty last-seen window, mirroring
vanilla's own wholesale per-connection rebuild — except that whether any caller in this repo
actually reconstructs a `ClientBuilder` after a transfer is itself unestablished. **Not built**:
rendering the trust badge itself, the `Modified` trust level (comparing signed content against
displayed content), per-sender chain-ordering/expiry enforcement, and signed chat commands.

### Message translation

A server-authored `Text` component with a `translate` node is fully resolved today, recursively,
against the real `en_us.json` table extracted from `client.jar` (8,123 keys) — this is
**already wired**, not a gap, at every display surface (chat, titles, action bar, disconnect
reason, scoreboard, container titles). The resolver implements vanilla's format language in
full: `%s`, `%N$s` (1-based indexed), `%%`, recursively-resolved arguments (an argument is
itself often a `translate` node — command feedback nests one inside another, and inherits its
enclosing style, which is why op-broadcast italics come from the *broadcast* wrapper and not
from the failure message itself), a component's own `fallback`, and the raw key as the final
fallback — a missing key intentionally shows the key rather than going blank, matching vanilla.
Command feedback sent by `lodestone-server` today is a plain formatted string, not yet a real
component matching vanilla's wording and argument types (an item argument should be its display
name, not its id) — that gap is tracked separately from the resolver itself, which needs no
further work. Only `en_us` exists in the jar; every other language (142 files, ~87 MB total) is
a separate downloadable asset-store object, fetchable the same way sound and texture objects
already are — unbuilt, but not blocked on anything new.

## How to change it, and the gotchas

- **Never let a unit test resolve a real Microsoft account, refresh a real token, or reach a
  real session-server / OAuth endpoint.** Several call paths (`net.rs`'s account resolution,
  `join_identity`, `NetClient::connect`) fork explicitly on `#[cfg(test)]` rather than an
  early `cfg!(test)` return, specifically so the interception is itself assertable; preserve
  that shape rather than adding an early return.
- **`Entitlement::from_metadata` must stay the only constructor.** Adding a second way to
  produce one — a `new`, a `From`, a test-only shortcut reachable from production — is adding a
  bypass to the ownership gate, and it will not look like one. If a per-account ownership flag
  is ever wanted, put it on `AccountProfile` and filter inside that constructor.
- **A test that presses menu keys needs an account in its roster.** With an empty one, the gate
  intercepts every keystroke, and the symptom is a screen assertion failing on
  `Screen::Ownership` rather than anything that names ownership. Both nav test helpers
  (`menu::nav`'s `nav`, `menu::render`'s `test_nav`) seed one; their `unowned_*` twins do not.
  That seeding means the whole existing corpus is blind to the gate by construction, which is
  why the gate has its own tests instead of relying on theirs.
- **`AccountSecrets::open()` decides keychain-vs-memory once per process and never re-probes.**
  A caller wanting live re-detection needs a fresh instance (in practice, a restart).
- **The offline UUID derivation must stay the unnamespaced Java form** — do not simplify it to
  `Uuid::new_v3`, which hashes different bytes and is indistinguishable by a stability check
  alone; only the exact published vectors catch the substitution.
- **The join-scheduler window must stay a function of `available_parallelism`, never of the
  view radius** — that regression is exactly the defect this design replaced, and re-widening
  the window to chase throughput needs a fresh sweep, since the cost curve is a U shaped by
  cache capacity, not a monotonic tradeoff.
- **The signature payload's timestamp is epoch *seconds*; the wire packet's timestamp field is
  epoch *milliseconds*.** Both are `i64`, so this is exactly the adjacent-same-typed-field
  mistake that survives a round-trip through your own code — keep the seconds value named
  accordingly wherever it is computed.
- **A dynamic-registry-shaped assumption never applies here, but the transposition-hazard
  discipline does**: any new packet or signed structure needs its expected bytes derived from
  an outside source (decompiled vanilla, a captured fixture, or an independent oracle), never
  from `decode(encode(x)) == x`.
- **Do not bundle `en_us.json` as a committed dump.** `client.jar` is already a hard dependency
  for textures and fonts; a committed copy would need its own drift gate and still only ever
  cover one language.
- **Adding a new keychain-backed secret** needs a method on the `SecretStore` trait implemented
  on both the real and in-memory backends — the trait is the whole contract.

## Configuration

- `LODESTONE_DATA_DIR` relocates the whole account/offline/profile data directory.
- `LODESTONE_MS_CLIENT_ID` overrides the Azure public-client id; set but blank is a typed error,
  not a silent fallback to the default.
- `LODESTONE_SECURE_CHAT=0` disables outgoing chat signing; unset or unparseable means enabled.
- `RUST_LOG=info,transfer=debug` (or similar) enables the transfer trace target; every emitted
  line is prefixed `xfer:` since the shell's subscriber does not print target names.
- No environment variable controls the TLS crypto provider or either keychain service name —
  changing a keychain service name orphans whatever is already stored under the old one.

## Dependencies

- `keyring` (native-only) for OS keychain access; `reqwest`/`rustls`/`ring` for HTTPS and TLS;
  `rsa`/`sha1`/`sha2` (native-only) for chat-session signing and Mojang issuer-key verification.
- `lodestone-auth` is a leaf crate depended on by `lodestone-client` and `lodestone-shell`,
  never the reverse; it duplicates (rather than shares) a `data_dir()` helper with the shell for
  the same leaf-crate reason.
- `lodestone-model`/`lodestone-game` for the version-free `Text`/`ClientAction` types both chat
  signing and message translation build on.
- `tokio`'s blocking pool for the join scheduler's `spawn_blocking` calls (forced to an inline,
  single-column path on `wasm32`).
- `client.jar` for `en_us.json`; the launcher asset-object store for every other language file.
