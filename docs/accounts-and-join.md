# Accounts, join, and chat: identity, scheduling, and secure messaging

## What it is

Everything between "the player wants to connect" and "the player is standing in a synced world with working chat": Microsoft account storage and the online-mode handshake, the offline fallback identity, the ownership gate, the single resolver deciding which identity a join presents, the loading-screen readiness rule, the server's join-time chunk scheduler, transfer tracing, the TLS provider choice, secure (signed) chat, and `Text` translation.

## How it works

### Accounts and the online-mode handshake (`lodestone-auth`)

Account data splits by sensitivity. The Microsoft refresh token and the derived Minecraft session (services token plus profile) each sit in the OS keychain under separate services (`dev.lodestone.ms-refresh-token`, `dev.lodestone.mc-session`) so either clears independently. Everything else (username, profile UUID, skin URL, selected account) is a plain JSON file, so the switcher can draw without unlocking the keychain.

- `AccountSecrets::open()` probes the keychain once per process (a `keyring` failure latches) and falls back to an in-memory store recorded as `StorageMode`. A caller wanting a re-check needs a fresh instance, in practice a restart.
- `login::try_cached_session` prefers a still-valid cached session (no network, and the refresh token is not even read, since redemption rotates it), then redeems the refresh token; only a dead one (`invalid_grant`) triggers an interactive device-code sign-in.
- `flow::ProfileSkin` keeps a structurally parsed `ProfileSkinUrl`, retaining original spelling because the fetch allow-list is case sensitive. Metadata and cached-session records remain string persistence boundaries.
- The browser build does not sign in: no switcher, OAuth, token store or backend. Its gate is a local attestation checkbox ("I confirm that I own Minecraft: Java Edition.") plus `Continue`, cleared on module reload. It is not ownership proof or server authorization. Browser singleplayer uses the local permit; remote multiplayer UI is disabled.

The net thread resolves an intent (the only place that can `await`): `RemoteAuth::SelectedAccount` (production multiplayer) goes through keychain and Microsoft; `RemoteAuth::Offline` (singleplayer, Open to LAN, live gates) touches neither; `RemoteAuth::Session` carries a resolved session. A failed resolution does not abort the dial: it becomes `OnlineModeSessionUnavailable { account, detail }`, keeping the account's last-known name and UUID, and the join continues unless the server requests proof. The three failure kinds stay distinguishable: none for explicit offline, that value for a selected-but-unusable account, and `ClientError::Auth(AuthError)` for a session-server rejection. The server's own kick text arrives as `ClientEvent::Disconnect` and is never an auth error.

`Driver::begin_encryption` (`lodestone-client`) runs on `Directive::BeginEncryption`: RSA-wrap the shared secret, write the encryption response in the clear, enable the cipher. When the server requests authentication and a session exists, it computes the server hash and calls the Mojang session-server `join` before sending the encryption response (sending the key packet first races the join POST). Authentication uses Lodestone's own Azure public-client id (`login::DEFAULT_CLIENT_ID`), not Mojang's launcher registration. **No test may reach `sessionserver.mojang.com` or a real OAuth endpoint**; a premium join is verified interactively.

### The ownership gate

In a multiplayer-capable build nothing is reachable until the roster holds an account that owns the game (singleplayer, world creation, the server list and the offline identity included). `Screen::Ownership` offers Add Account (the ordinary switcher, writing the same `profiles.json`) and Quit. A build without the shell's `multiplayer` feature skips the gate and may use the bundled local server with its stable offline identity, since an online proof would add a network prerequisite to authorize nothing.

**Enforcement is a type.** `lodestone_auth::Entitlement` has private fields and one constructor, `Entitlement::from_metadata`, answering `Some` only for a roster with an account. `MenuAction::Connect` carries one; `MenuAction::Singleplayer` carries a `SingleplayerPermit` (the multiplayer variant holds the entitlement; the singleplayer-only variant exists only without the remote capability). A new entry path must choose and construct an authorization.

- The check is presence-based, not a fresh network call: a roster row exists only because a completed sign-in produced a Minecraft profile (`AuthError::NoMinecraftProfile` otherwise). Re-verification happens at the next sign-in or online join; demanding it at launch would break offline play.
- It is not a security boundary. `profiles.json` is a user-owned file and can be forged; only a server-side check stops that.
- `MenuNav::ownership_gate_blocks` is the screen half, consulted by `key`, `click`, `hover` and `render::frame_for`, so drawing and input cannot disagree (and the first frame draws the gate). Exempt: `Screen::Accounts` (the only exit), `Screen::Error`, and screens `Screen::in_session` reports as over a live world.
- The `--headless` and `--connect` diagnostics never build a `MenuAction`, so they take an `Entitlement` parameter resolved in `app::run` and refuse naming the Accounts screen when there is none. A new diagnostic mode must obtain one or it does not compile. `Mode::Window` skips that check, since it can show the gate.

### The offline fallback identity

A display-name choice available once the gate is open, not an entry path. `offline.json` stores only the username. The UUID is derived: MD5 over the UTF-8 bytes of `OfflinePlayer:` plus the username, version nibble 3, RFC 4122 variant. This is a name-based UUID over those bytes alone, **not** `Uuid::new_v3`, which prepends a namespace and hashes different bytes. It matters because the integrated server echoes the client's UUID, so a stable derived one finds the singleplayer save on later launches. A missing, corrupt or invalid stored name falls back to the default; names are validated on load and set, since a rejected name is a disconnect with no obvious cause.

### Which identity a join presents (`join_identity`)

`join_identity::resolve` is the single producer of username and UUID for every production join. It reads `AccountsMetadata::selected`: a selection with a `profiles.json` row uses that identity; a selection without a row uses the offline identity and logs a warning; no selection uses offline with no concern. Every arm logs. Authentication is skipped for singleplayer (no encryption request on a memory connection) while identity still prefers the selected account, so a world entered before selecting an account files saves under the offline UUID and later selection finds none there, as in vanilla. `connect_as` (live gates only) stays offline in both senses with an explicit name, so concurrent gates never share a player file.

### Join readiness: the loading-screen gate

The overlay clears only when both the terrain rule and the asset rule hold, with no time-based escape. Assets are checked first (matching the reference client's precedence).

- **Terrain:** for a new survival world with a declared initial view, the player's own column is admitted and the producer explicitly confirms every column in the view is resident and renderer-settled. The progress counter is telemetry and cannot release the overlay. Remote sessions without a trustworthy denominator keep the own-column fallback. The out-of-range liveness escape needs a known destination vertical extent; unknown is not proof of being outside build height. The dead-player short-circuit is independent.
- **Assets:** no server-pushed resource pack still downloading or waiting for the atlas. Singleplayer satisfies this instantly; the browser never blocks on a pack.
- The world renders under the opaque overlay so chunks keep meshing and skins keep resolving. The atlas reload must run before the overlay check in the same frame or one stale-atlas frame shows; nothing in the type system enforces it. `Sim::world_wait` is the sole aggregation surface: extend `TerrainWait`/`AssetWait` and their predicates, not a parallel flag.

### The join generation scheduler (server)

A joining player's `(2r+1)^2` columns generate through a primed sliding window, avoiding all-at-once (serialises on cache contention that grows with radius) and per-ring barriers (ring `r+1` waits on ring `r`'s slowest column). Window width is `available_parallelism`, floored at 2 and never capped; it grows with cores, not view radius. It is primed: the first column generates alone so the player's own column reaches the client after one generation. Emission is always in wire (Chebyshev-ring) order regardless of finish order, keeping the encoded stream a pure function of radius. The same pipeline streams newly visible columns as a player walks.

The initial centre packet has a stricter contract: before it can release loading, the server admits the centre and its eight neighbours (a 3x3) and settles the centre's border light. The centre goes through full generation even when the view later uses the reduced shaped stage beyond the near band. This nine-column milestone keeps the barrier independent of render distance; a resident centre packet alone is never proof of that admission.

### Transfer tracing

The `transfer` tracing target logs the position handshake around a teleport or transfer (arrival, confirmation, the pose reaching the simulation, each outbound movement's distance from the last accepted teleport) to catch the client claiming a position the server overruled. Two mechanisms look alike: a `minecraft:transfer` reconnect rebuilds all per-connection state, while a proxy backend swap (Velocity, BungeeCord) keeps the socket and carries state across a Configuration round trip and second `LOGIN`; grep `login_ordinal` in the packet log (a proxy swap never sends `minecraft:transfer`). Two defects it exposed are fixed: an outbound `Move` built from a pose that had not adopted the teleport is rewritten to the authorized pose, and a second `LOGIN` on one connection clears the decoded chunk store (a backend swap does not trigger the dimension-change clear).

### TLS crypto provider

All HTTPS goes through `reqwest` to `rustls`, pinned to the `ring` backend to keep `aws-lc-sys`'s roughly 1,500 vendored C units out. This needs `reqwest`'s `rustls-no-provider` feature plus one idempotent `rustls::crypto::ring::default_provider().install_default()` immediately before constructing any `reqwest::Client`, not in `main()` (which would leave test binaries panicking). A missed call site is a runtime panic on the first request, not a compile error. Enabling rustls default features or reqwest's `http3` (pulls `quinn`) anywhere re-adds `aws-lc-rs` through feature unification.

### Secure chat

On an online-mode session the client fetches a Mojang RSA chat-signing key, announces a chat session, and signs each outgoing message over the current last-seen window. Other players' announced keys are retained from the player-info update and used to verify their messages, yielding a `verified` bool a consumer could render as a badge (not built); it does not rewrite message text. Signing defaults on (`LODESTONE_SECURE_CHAT=0` opts out; unset or unparseable means enabled).

The last-seen window must count only signed messages on both peers. A past "Chat message validation failure" was an acknowledgement bug (the client reported an `ack` for unsigned `PLAYER_CHAT` too, drifting the offset until the first signed message exposed it), now guarded where the reference checks it. A reconnect re-fetches and re-announces a session with an empty window; whether every transfer path reconstructs a `ClientBuilder` is unestablished. Not built: the trust badge, the `Modified` trust level, chain-order/expiry enforcement and signed chat commands.

### Message translation

A `Text` component with a `translate` node resolves recursively against `en_us.json` from `client.jar` (8,123 keys) at every display surface (chat, titles, action bar, disconnect reason, scoreboard, container titles). It supports `%s`, `%N$s`, `%%`, recursively resolved arguments that inherit the enclosing style (op-broadcast italics come from the wrapper), a component `fallback`, and the raw key as the last resort. Command feedback from `lodestone-server` is still a plain formatted string, not a component. Only `en_us` is in the jar; other languages (142 files, about 87 MB) are fetchable asset-store objects, unbuilt.

Disconnect reasons also accept proxy-bridge wrappers: a full JSON component inside a string, child or nested serialization layers. Only disconnects rebuild the payload, peeling at most four wrappers; ordinary chat stays literal. Colours and decorations survive through `SessionEnd::reason` and the error-screen span renderer.

## How to change it

- Never let a unit test resolve a real account, refresh a token or reach an auth endpoint. `net.rs` account resolution, `join_identity` and `NetClient::connect` fork on `#[cfg(test)]` (not an early `cfg!(test)` return) so the interception is assertable; keep that shape.
- `Entitlement::from_metadata` must stay the only constructor. A `new`, `From` or test shortcut reachable from production is a bypass. A per-account flag belongs on `AccountProfile`, filtered inside the constructor.
- A test that presses menu keys needs an account in the roster, or the gate intercepts every key and the failure reads `Screen::Ownership`. `menu::nav`'s `nav` and `menu::render`'s `test_nav` seed one; their `unowned_*` twins do not, so the gate has its own tests.
- Keep the offline UUID the unnamespaced Java form; only the published vectors catch a `Uuid::new_v3` substitution.
- Keep the join window a function of `available_parallelism`, never of radius; re-widening needs a fresh sweep, since the cost is a U shape set by cache capacity.
- The signature payload timestamp is epoch seconds; the wire timestamp is epoch milliseconds. Both are `i64`, so name the seconds value accordingly. New signed structures need expected bytes from an outside source, not `decode(encode(x))`.
- Do not commit `en_us.json`; `client.jar` is already required and a copy needs its own drift gate for one language.
- A new keychain secret needs a `SecretStore` trait method on both the real and in-memory backends.

## Configuration

- `LODESTONE_DATA_DIR` relocates the account/offline/profile directory.
- `LODESTONE_MS_CLIENT_ID` overrides the Azure client id; set but blank is a typed error.
- `LODESTONE_SECURE_CHAT=0` disables signing.
- `RUST_LOG=info,transfer=debug` enables the trace; lines are prefixed `xfer:`.
- No variable controls the TLS provider or keychain service names (renaming a service orphans stored secrets).

## Dependencies

- `keyring` (native only); `reqwest`/`rustls`/`ring`; `rsa`/`sha1`/`sha2` (native only) for chat signing and issuer-key verification.
- `lodestone-auth` is a leaf crate used by `lodestone-client` and `lodestone-shell`, duplicating a `data_dir()` helper for that reason. `lodestone-model`/`lodestone-game` for `Text` and `ClientAction`.
- `tokio`'s blocking pool for the scheduler (inline single-column on wasm32). `client.jar` for `en_us.json`; the launcher asset-object store for other languages.
