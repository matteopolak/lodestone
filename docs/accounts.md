# Microsoft account storage and online-mode join

## What it is

`crates/lodestone-auth` stores **multiple** Microsoft accounts instead of one,
and no longer keeps the long-lived refresh token in a plaintext file — see
"Storage" below — and **the multiplayer join now actually uses the account the
switcher has selected**. A join resolves that selection on the net thread,
presents the profile's real username and UUID, and completes the online-mode
RSA/AES handshake plus an authenticated `POST /session/minecraft/join`; with
nothing selected it joins offline exactly as before, and makes no network call
looking for an account. Offline mode remains the default and singleplayer is
never authenticated. There is also a screen —
`crates/lodestone-shell/src/menu/accounts.rs` draws the account list, every
saved account plus an always-present offline entry, and drives its own
device-code sign-in.

See "Join-flow wiring" below for the whole path and, importantly, for **what is
and is not verified**: no hermetic test can reach a real
`sessionserver.mojang.com` join, so the last link is the owner's own
interactive check.

### The island this doc used to describe

Worth keeping, because it is the most expensive shape in this repo and this was
a textbook instance. Every piece of online-mode support was built and
individually tested — the `Session` type, `login::try_cached_session`,
`ClientBuilder::online_session`, the driver's `begin_encryption`, the RSA/AES
primitives against NIST vectors — and **the first link was missing**:
`NetClient::connect` hardcoded `auth: None`, and `NetClient::connect_online`,
the only constructor that could pass a session, had **zero callers**. So a
player could sign in, see their real premium username in the switcher, join an
online-mode server, and be told *"no Microsoft session was configured for this
connection (see ClientBuilder::online_session)"* — a message about a builder
method, describing a configuration fault that did not exist.

Two things generalise from it:

- **A constructor with no callers may have a signature that makes calling it
  impossible.** `connect_online` demanded an already-resolved `Session`;
  resolving one needs an `await`; every join call site is synchronous UI code on
  the render thread. Nobody could have called it without restructuring, so
  "wire up a caller" was never a small task and never got done. The fix was to
  pass an *intent* (`net.rs`'s `RemoteAuth`) and resolve it where a runtime
  already exists. When something has no producers, ask whether its shape is the
  reason before looking for a lazy author.
- **The doc comment on `connect_online` stated the whole bug, accurately, for as
  long as it existed** — *"every join this shell makes is an offline one … the
  account switcher can hold a signed-in Microsoft account that no join ever
  uses."* Prose that names a defect is not a defect report. Nothing reads it and
  nothing fails because of it.

The crate splits an account's data into three places with very different
sensitivity:

| what | where | why |
|---|---|---|
| the Microsoft **refresh** token | the OS keychain, under `dev.lodestone.ms-refresh-token`, via [`store`] | it is a long-lived bearer credential; it must never sit on disk as plaintext |
| the derived Minecraft **session** (services access token + profile) | the OS keychain too, under a separate service `dev.lodestone.mc-session`, via [`store::CachedSession`] | also a bearer credential, so the same class of storage as the refresh token — kept under a different service so either can be cleared independently |
| username, profile UUID, skin URL, last-used time, which account is selected | a plain JSON file, [`metadata`] | an account switcher needs to draw its whole list on every launch, and must not have to unlock the keychain (and potentially prompt the user) just to render a menu |

The Minecraft *services* access token used to be re-derived from the refresh
token on **every** join (via [`flow::refresh_token`] +
[`flow::session_from_ms_token`]) and never persisted — cheap in theory, but
the refresh token rotates on every redemption, so every join was one more
chance to orphan the account if the downstream chain failed partway. It is
now cached ([`store::CachedSession`]) alongside its real expiry (read from
Mojang's own response, not assumed), and [`login::try_cached_session`]'s fast
path uses it — and skips redeeming the refresh token altogether — whenever
it is still valid outside a 5-minute safety margin. See that function's own
doc for the exact margin/expiry handling.

## How it works

### Secrets: `store::AccountSecrets`

[`store::SecretStore`] is a trait — `save_refresh_token`/`load_refresh_token`/
`delete_refresh_token` for the refresh token, and the equivalent
`save_session`/`load_session`/`delete_session` trio for the cached
[`store::CachedSession`], each keyed by the account's profile `Uuid`. Two
implementations:

- [`store::KeychainStore`] — the real backend, a thin wrapper over the
  [`keyring`](https://crates.io/crates/keyring) crate's `Entry` type. Every
  entry lives under the fixed *service* `"dev.lodestone.ms-refresh-token"`
  with the profile UUID as the *account* — that's what fans a single service
  out into one credential per Microsoft account rather than one shared blob.
- [`store::MemoryStore`] — an in-memory `HashMap` guarded by a `Mutex`. Used
  by every hermetic test in this crate, and as the automatic fallback when
  the real keychain cannot be reached at all.

[`store::AccountSecrets`] is the façade callers actually hold. `open()`
**probes** the real keychain exactly once — it constructs a throwaway
`keyring::Entry` and asks for a password that was never set. Getting back
"no such entry" proves the backend answered and is real; any other error
(no D-Bus Secret Service session, a locked store, `NoDefaultStore`, …) means
the backend could not be reached, and `open()` falls back to
`MemoryStore` instead. Which one it picked is recorded in
[`store::StorageMode`] and exposed via `AccountSecrets::mode()`:

```rust
match secrets.mode() {
    StorageMode::Keychain => { /* fine, say nothing */ }
    StorageMode::SessionOnly { reason } => {
        // Tell the user: tokens will not survive a restart, and why.
    }
}
```

`with_backend(backend, mode)` is the seam for tests (and anything that wants
to inject its own `SecretStore`) — it never touches a real keychain.

**Why the probe is a one-shot decision, not a retry loop:** `keyring`'s own
`v1::Entry::new` latches its backend-initialization failure *process-wide* on
the very first call (an `AtomicBool` flips to `true` whether or not
initialization actually succeeded — see `keyring-4.1.5/src/v1.rs`). Once the
real backend has failed once in this process, retrying it cannot succeed —
so `AccountSecrets` doesn't try. A caller that wants to re-check (e.g. after
telling the user to start a D-Bus session) should construct a fresh
`AccountSecrets::open()`, ideally in a new process.

### Metadata: `metadata::AccountsMetadata`

Plain data, no secrets: [`metadata::AccountProfile`] (`profile_id`,
`username`, `skin_url`, `last_used`) and [`metadata::AccountsMetadata`]
(`selected: Option<Uuid>` plus `Vec<AccountProfile>`). `load()`/`save()` work
exactly like `lodestone-shell`'s `Options`/`ServerList`: a missing or corrupt
file is silently the empty default, never an error and never a panic, and one
malformed *entry* only costs that entry — every other entry, before or after
it in the array, still loads. See [`metadata::AccountsMetadata::from_json`]
for the exact per-field tolerance rules, and
`crates/lodestone-auth/src/metadata.rs`'s `one_malformed_profile_entry_costs_only_itself`
test for the evidence that a broken record doesn't take its neighbours down
with it.

`upsert(profile)` replaces an existing entry by `profile_id` rather than
duplicating it; `remove(profile_id)` also clears `selected` if it pointed at
the profile being removed.

### Where the files live: `paths`

[`paths::data_dir()`] duplicates `lodestone-shell/src/menu/servers.rs`'s
`data_dir()`/`data_dir_from()` **byte-for-byte**: `LODESTONE_DATA_DIR`
overrides everything; otherwise macOS gets
`~/Library/Application Support/lodestone`, Windows gets `%APPDATA%/lodestone`,
and everything else prefers `$XDG_DATA_HOME` and falls back to
`~/.local/share/lodestone`.

This is a deliberate duplication, not an oversight. `lodestone-auth` must
stay a leaf crate the shell depends on, never the reverse, so it cannot call
into `lodestone-shell` to reuse the real function — and `config.rs`, the
natural shared home for a path helper, was held by another agent while this
was written. The metadata file's entire point is to sit *beside*
`servers.json`/`options.json`, so approximating the directory instead of
copying it exactly would silently defeat that. **If the shell's `data_dir()`
ever changes, this copy must change with it** — there is no test that can
catch drift between two independent implementations in two crates.

**Re-diagnosed:** it was proposed hoisting this to `lodestone-core`
because "both crates already depend on it" — checked against the committed
`Cargo.toml`s, false: `lodestone-core` is a narrowly-scoped protocol-codec
crate with no platform-directory business, and neither crate depended on it
anyway. What *is* true is simpler: `lodestone-shell` depends on
`lodestone-auth` (see `crates/lodestone-shell/Cargo.toml`), so
[`paths::data_dir`] is already the correct single-implementation home — no
third crate needed. The two copies were confirmed **byte-for-byte identical**
today (`crates/lodestone-auth/src/paths.rs` vs.
`crates/lodestone-shell/src/menu/servers.rs`'s `data_dir`/`data_dir_from`), so
there is no live drift to fix, only the duplication itself. The remaining
work — deleting `menu/servers.rs`'s copy and its two `config.rs` call sites
(`options_path`/`hidden_players_path`) in favour of calling
[`paths::data_dir`] — lives entirely in `crates/lodestone-shell/**`, outside
this crate's ownership; see the issue for the prepared patch.

[`paths::profiles_path()`] is `data_dir().join("profiles.json")`.
[`paths::legacy_token_cache_path()`] is `data_dir().join("ms_token.json")` —
the exact filename the prior code used, so migration finds it.

### Migrating a legacy install: `cache` + `migrate`

Before this work, `cache::default_cache_path()` returned one fixed path and
`cache::save()` wrote the refresh token there as plain JSON — the two things
this change exists to fix. `cache.rs` now keeps only what a one-time migration
needs:

- `cache::load_legacy_cache(path)` — read-only.
- `cache::delete_legacy_cache(path)` — idempotent; a missing file is not an
  error.

[`migrate::migrate_legacy_cache`] drives the full sequence and is what a
fresh launch should call once, before anything else touches accounts:

1. if no legacy file exists, return `Ok(None)` — no network call at all;
2. otherwise, refresh the cached Microsoft token (it has likely gone stale
   sitting in that file, since the *access* token is short-lived and the
   file could be old);
3. derive the account's Minecraft profile from the refreshed token — the
   legacy file has no profile UUID of its own, so this is the only way to
   learn which keychain entry to use;
4. save the (rotated) refresh token into the keychain under that UUID;
5. upsert a metadata entry and mark it selected;
6. **delete the legacy file**;
7. log that a migration happened.

The file is deleted only *after* step 4 succeeds — a silent leave-behind
(the secret still readable on disk while the rest of the system believes it
is protected) is the one outcome this exists to avoid, so any failure before
that point leaves the legacy file in place for the next launch to retry.
Nothing in the log line names the token or the profile UUID.

`migrate_legacy_cache` does **not** call `AccountsMetadata::save()` itself —
the caller decides when to persist, same as everywhere else `save()` is
opt-in in this codebase.

## How to change it

- **New metadata field:** add it to `AccountProfile`, then update
  `profile_from_json`/`profile_to_json` in `metadata.rs` together — the
  tolerant-parse test `one_malformed_profile_entry_costs_only_itself` is the
  one to extend if the new field can be independently missing/malformed.
- **New keychain-backed secret (not just the refresh token):** add a method
  to the `SecretStore` trait and implement it on both `KeychainStore` and
  `MemoryStore` — the trait is the whole contract, and a real-keychain
  `#[ignore]`d test belongs next to the existing one in `store.rs`.
- **Wiring this into the actual join flow: done — see
  "Join-flow wiring" below.** `login::try_cached_session` +
  `login::finish_interactive` are the composition this bullet used to say
  didn't exist yet.
- **Gotcha — the path duplication:** see the `paths` section above. If a
  future change moves `lodestone-shell`'s `data_dir()`, this copy in
  `lodestone-auth::paths` will silently stop matching and `profiles.json`
  will land in the wrong place relative to `servers.json`. There is no
  automated check for this today.
- **Gotcha — `AccountSecrets::open()` decides the mode once.** It does not
  re-probe mid-session. A save/load call that fails after `open()` returned
  `StorageMode::Keychain` (the user locks their session, revokes access, …)
  surfaces as a plain `AuthError::Keychain`, not an automatic demotion to
  session-only. A caller that wants live re-detection has to construct a
  fresh `AccountSecrets` (in practice, that likely means restarting).
- **Gotcha — the encrypted-at-rest fallback was not built.** The task brief
  offered a choice between a session-only in-memory fallback and an
  explicitly-encrypted-at-rest file when the keychain is unavailable. Only
  the former exists. An honest at-rest-encrypted mode needs a key from
  somewhere; the two options were a user-entered passphrase (real KDF/salt/
  nonce surface, no UI to collect it since the shell is held, and it starts
  to blur the "never accept a password" constraint even though it wouldn't
  be the Microsoft password) or a machine-local key stored next to the
  ciphertext (which is obfuscation, not protection — anyone who can read the
  disk gets both). Session-only was judged the more honest failure mode: it
  fails loudly (re-add the account) instead of pretending to protect a
  secret it structurally cannot.

## Join-flow wiring

Two pieces landed to turn the account storage into an actual
authenticated join: [`login`], the composition layer inside `lodestone-auth`,
and the `Directive::BeginEncryption` handling inside `lodestone-client`'s
driver, which is where the RSA/AES handshake and the session-server call
actually happen.

### `lodestone-auth::login` — cached-refresh-then-interactive composition

`docs/accounts.md`'s own "how to change it" section sketched the sequence a
connect path would need; [`login`] is that sequence,
built so nothing in it blocks:

- [`login::try_cached_session`] first checks for a still-valid cached
  [`store::CachedSession`] (real `expires_at` minus a 5-minute margin,
  `login::SESSION_EXPIRY_MARGIN_SECS`) and returns it directly when found —
  **no network call at all**, and critically, the refresh token is not even
  read, let alone redeemed (redemption rotates it, so skipping it here is a
  durability win, not just latency). Only when that cache is cold, expired,
  or unusable does it fall through to the selected account's cached refresh
  token. That fallback still makes **no network call at all** when there's
  nothing to try (no selected account, or no stored token for it) — the same
  "nothing to do, don't touch the network" fast path
  [`migrate::migrate_legacy_cache`] uses, which is what makes all of this
  hermetically testable. A transport/parse failure while refreshing is
  propagated rather than silently treated as "no cached token" — if Microsoft
  is unreachable, starting an interactive flow won't fare any better either,
  and hiding that behind a prompt the user also can't complete would be worse
  than just saying so. Only [`AuthError::RefreshTokenInvalid`] (OAuth's
  `invalid_grant` — the token itself is dead, not the network) becomes "no
  cached token, go interactive." A successful redemption (or a fresh
  [`login::finish_interactive`] sign-in) writes the derived session back to
  the cache before returning, so the *next* join takes the fast path above.
- If that returns either non-`Ready` outcome — `NoAccountSelected` or
  `SessionExpired` — the caller starts an interactive
  device-code login the existing way (`flow::PendingLogin::begin`, already
  poll-based and non-blocking) and, once it completes, calls
  [`login::finish_interactive`], which derives the session, saves the
  (rotated) refresh token, and upserts + selects the account in
  `AccountsMetadata` — `metadata` is **not** saved to disk by either function,
  same opt-in-save convention [`migrate::migrate_legacy_cache`] already uses.
- [`login::resolve_client_id`] returns [`login::DEFAULT_CLIENT_ID`],
  Lodestone's own registered Azure public-client id, overridable via the
  `LODESTONE_MS_CLIENT_ID` environment variable. It returns a typed
  [`AuthError::MissingClientId`] only when that variable is *set* to a blank
  value — an explicit "use nothing" is a caller mistake, not a request for the
  default. See "The client id" below.

Nothing here drives a poll loop for the caller. A terminal front end can
`.wait()` the `PendingLogin`; a future GUI screen can `.poll_once()` from
a timer and show `pending.prompt()` — `login` doesn't care which.

### Typed XSTS / refresh error taxonomy

`AuthError` grew three additions so a UI can render *why* sign-in failed
instead of a raw HTTP body:

- **[`XstsErrorKind`]** classifies XSTS's `401` body by its numeric `XErr`
  code: no Xbox account, region unavailable, adult/age verification required
  (South Korea), or a child account needing a Family organizer's consent.
  **These five codes are not independently verified against a real account in
  any of these states** — there is no way to put an account into "banned
  region" or "needs adult verification" without actually being in that
  situation. They're carried forward from external, cross-project agreement:
  PrismLauncher, HMCL, and other unrelated Minecraft launchers all hardcode
  the same five values, since Microsoft does not publish an official list.
  `AuthError::Xsts { kind, message }` is the error variant; `kind.describe()`
  gives a short, UI-safe string distinct from Microsoft's own (English,
  developer-oriented) `message`.
- **`AuthError::RefreshTokenInvalid`** is OAuth's `invalid_grant` specifically
  (not every refresh failure) — see `login::try_cached_session` above for why
  the distinction matters: only this one should trigger a silent fallback to
  interactive sign-in.
- **`AuthError::MissingClientId { env }`** — see below.

### The client id

`flow.rs` has always taken `client_id` as a parameter rather than hardcoding
one — [`flow::MOJANG_CLIENT_ID`] exists, but it is Mojang's own launcher's
registered Azure application id, not this project's, and using it would
misrepresent this client to Microsoft (not just violate a style preference).
Mojang gates production access to the Minecraft API per registered Azure
application, so the id matters: it is how Microsoft knows which application is
asking. Lodestone ships its own registration as [`login::DEFAULT_CLIENT_ID`],
which is what makes a default possible at all — this was an unset-and-required
variable for most of its life precisely because no *borrowed* id was
defensible.

It is deliberately **not** [`flow::MOJANG_CLIENT_ID`], the official launcher's
registration. Borrowing that would misrepresent this client to Microsoft rather
than merely break a style rule, and a test pins the inequality so a future
rotation cannot quietly land on it.

A public-client id is an identifier, not a credential — Azure public clients
hold no secret, the flow is device-code with PKCE, and every launcher that
ships one embeds it the same way. Set `LODESTONE_MS_CLIENT_ID` to run against a
different registration (a fork, a private build, a second app for testing).

### The driver: `Directive::BeginEncryption` → an actual join

Before this change, `lodestone-client`'s driver had **no arm at all** for
`Directive::BeginEncryption` — it fell into the generic "ignoring unknown
directive variant" catch-all, silently. The adapter side
(`crates/protocol/v770/src/adapter/connection.rs`) already emitted the directive and
already implemented `build_encryption_response` correctly, and both were
already tested (`crates/protocol/v770/tests/join_flow.rs`); the directive
simply had no consumer, which is the exact "island" shape `CLAUDE.md`'s rule 1
describes. `crates/lodestone-client/src/driver.rs`'s `begin_encryption` is the
new consumer:

1. If the server wants a session-server join (`should_authenticate: true`)
   but the client has no session, fail immediately — before spending a round
   trip on crypto the server was always going to reject anyway (an offline
   profile has nothing to prove ownership with). **Which** error depends on why
   there is no session; see "Three distinguishable failures" below.
2. Generate the shared secret and RSA-wrap it and the verify token against
   the public key *the directive carried* (`lodestone_net::generate_shared_secret`
   / `rsa_encrypt` — already existed, already verified against NIST vectors
   and a real vanilla server; this change is the first thing that actually
   calls them from a live connect path).
3. Ask the adapter to frame the reply (`build_encryption_response`), write it
   in the clear, *then* call `Connection::enable_encryption` — ordering
   matters and mirrors `Connection::enable_encryption`'s own documented
   contract.
4. If `should_authenticate` **and** the intent is a usable online session,
   compute the server-hash (`lodestone_auth::server_hash`, already verified
   against Mojang's three published vectors) and call
   `lodestone_auth::join_server`. Explicit offline intent never contacts
   Mojang, even when a server requests encryption; an unavailable selected
   online account instead stops before the response is sent. Any join failure
   surfaces as `ClientError::Auth(AuthError)`, so a caller matching on it gets
   the fully typed reason, not a string.

`ClientBuilder::online_session` is the new seam: supply a `lodestone_auth::Session`
(from `login::try_cached_session`/`finish_interactive`) to join online-mode
servers. `ClientBuilder::authentication_intent` carries the three explicit
states: `Offline`, `Online(Session)`, and `OnlineUnavailable`. Omitting it is
the backwards-compatible explicit `Offline` default, not an absent session
whose meaning the driver has to guess.

### The shell: `net.rs`'s `RemoteAuth`

`Origin::Remote` carries a `RemoteAuth` — an *intent*, not a resolved session —
and `run_async` resolves it inside the net thread's `block_on`, which is the
only place in this path that can `await`. Three variants:

| variant | who asks for it | what happens |
|---|---|---|
| `SelectedAccount` | `NetClient::connect` — the production multiplayer join | `lodestone_auth::resolve_selected_account`: read `profiles.json`, load the refresh token from the keychain, exchange it with Microsoft |
| `Offline` | `connect_as` (live gates), singleplayer, Open to LAN, every browser join | present the profile the caller resolved verbatim; no keychain, no network |
| `Session(_)` | `connect_online` | use the session the caller already resolved |

**`RemoteAuth` is about authentication, not identity — do not read `Offline`
as "joins as the offline player".** Which username and UUID a join presents is
`join_identity::join_identity`'s answer (`docs/join-identity.md`), and it
prefers the selected account for singleplayer and Open to LAN too. Those paths
are `Offline` here only in the sense that they never authenticate: they read
`profiles.json`, never the keychain and never the network. `connect_as` is the
one constructor that is offline in *both* senses.

`connect_as` staying offline is load-bearing rather than tidy: a gate asks for
an exact username, so resolving the developer's selected account there would
join under a *different* name — on a developer's machine only — and every gate
would then share one premium player file, which is exactly the shared-name
eviction and dead-player-blackout hazard `connect_as` exists to avoid.
Singleplayer matches vanilla: `handleHello` skips the encryption request for
`isMemoryConnection()`, so there is nothing to authenticate.

**A failed resolution does not abort the dial.** It becomes
`OnlineUnavailable`, retaining the selected account's last-known name and UUID.
If the server sends no authentication request (or sends encryption with
`should_authenticate = false`), the connection continues under that selected
identity without a Mojang call. If proof is requested, the retained diagnostic
is returned before an encryption response is sent. It never silently retries as
an offline identity.

**`production_origin` forks on `#[cfg(test)]`.** Resolving an account opens the
OS keychain, POSTs to Microsoft, and — because the refresh token rotates on
every use — writes a new one back. A `cargo test` that did that would reach the
network from a unit test and could invalidate the token the owner's real client
is holding, while the suite reported green. `net.rs` already had a unit test
calling `NetClient::connect` (`two_offline_sessions_publish_the_same_identity`),
so this was not hypothetical. The fork is on `#[cfg(test)]` rather than an
early return on `cfg!(test)` so it is *assertable*:
`unit_tests_never_resolve_a_real_microsoft_account` pins the interception and
`a_production_join_requests_the_selected_microsoft_account` pins the production
decision the fork bypasses, so the two cannot both be satisfied by never
resolving anything.

### Three distinguishable failures

They used to be one sentence that blamed configuration in all three cases,
which is why a working account looked like a broken build. They are now three
distinct typed values, and a fourth path that is not ours at all:

| situation | value | says |
|---|---|---|
| explicit offline identity | no client auth error | encrypt if requested, but never contact Mojang; a server may still reject that identity |
| an account is selected but unusable — expired/revoked token, Microsoft unreachable, locked keychain, no `LODESTONE_MS_CLIENT_ID` | `ClientError::OnlineModeSessionUnavailable { account, detail }` | names the account and the reason |
| the sessionserver `join` failed | `ClientError::Auth(AuthError)` | Mojang's own typed reason |
| the **server** kicked us | `ClientEvent::Disconnect` → `NetUpdate::Disconnected` | the server's own `Text`, untouched |

The last row is a different path on purpose, and it matters for reading bug
reports. A server rejecting an unauthenticated join sends its own kick — the
owner saw something like *"failed premium challenge"* — and that is the
**server's** wording, not ours; the string "premium" appears nowhere in this
repo. It surfaces as `disconnect.lost` rather than `connect.failed`, so it is
already visibly distinct from our own errors and must stay that way.

`lodestone_auth::SelectedAccount` is the source of the middle two rows'
distinction, and it splits what `CachedSessionOutcome` used to merge:
`NoCachedToken` became `NoAccountSelected` and `SessionExpired { profile_id,
username }`. Same remedy, different sentence — "sign in" versus "sign in to
*this* account again" — and a caller cannot produce the second message without
the account name, so the name is in the type.

### What's verified and what isn't

Hermetic (`crates/lodestone-client/tests/online_mode_handshake.rs`):

- the driver actually RSA-wraps the secret/token against the public key the
  directive carried (decrypted independently with a test-generated private
  key the driver never saw);
- the reply is written before the cipher is enabled, and a packet sent
  afterwards decrypts correctly (the cipher really did turn on, with the
  right secret, in both directions);
- explicit offline intent completes RSA/AES for both values of
  `should_authenticate` and skips the session-server call entirely;
- an unavailable online account with `should_authenticate: false` completes
  RSA/AES under its retained selected profile without a Mojang call;
- `should_authenticate: true` with an *unusable* account fails with
  `OnlineModeSessionUnavailable`, and the rendered message names the account and
  the reason and is **not** the nobody-signed-in sentence — the arm that goes red
  if the two variants are ever merged back together for tidiness;
- the three failure messages are pairwise distinct *and* each is about its own
  cause, since three different ways of saying "authentication failed" would pass
  a distinctness check alone.

Also hermetic: every XSTS/refresh-token classification branch in
`lodestone-auth::flow`'s tests, `login::try_cached_session`'s no-network fast
paths, `resolve_selected_account`'s no-selection and cannot-resolve arms
(including that a selection with no `profiles.json` row names its UUID rather
than an empty string), and `net.rs`'s pair of origin gates above.

**Not verified, and not claimable from this repo: the successful premium join.**
Producing it needs a real Azure client id plus a real Microsoft account that
owns Minecraft, and there is no way to build a hermetic fake for Microsoft's
OAuth/Xbox/XSTS/session-server chain — the same limitation `flow.rs` always had,
and the same reason `lodestone-net/tests/online_handshake.rs` is `#[ignore]`d.
Concretely, none of the following is covered by any test here:

- a refresh token actually being exchanged, and the rotated one written back;
- `SelectedAccount::Online` being produced at all;
- `join_server` succeeding against `sessionserver.mojang.com`;
- an online-mode server accepting the resulting username.

That last link is the owner's own interactive check, against a real online-mode
server with the account signed in. No test in this repo may reach
`sessionserver.mojang.com`, and none does.

**Also unverified for the same reason: the join-before-key packet ordering**
in `lodestone-client::driver::Driver::begin_encryption` (`join_server` must
complete before the `EncryptionResponse`/key packet is sent, matching
`ClientHandshakePacketListenerImpl.handleHello` — see that function's own doc
for why sending the key packet first races our `join_server` POST against a
hosting server's own `hasJoined` check and is what `velocity.error.online-mode-only`
actually is). A live gate for it needs a real authenticated session, i.e. the
same `LODESTONE_MS_CLIENT_ID` gap above, plus a running online-mode server —
neither was available in the environment that last touched this doc. The fix
itself is grounded in reading both sides of the vanilla handshake source
directly, not in a live capture.

## The account list screen

`crates/lodestone-shell/src/menu/accounts.rs`'s `AccountsNav` draws the account
list — every account from `AccountsMetadata`, most-recently-used first, plus a
synthetic offline entry always appended last — and drives Add/Select/Remove/
Cancel plus its own device-code sign-in sub-flow. See `docs/main-menu.md` for
where the screen sits in the menu state machine and how it's rendered.

### `finish_interactive`, not a hand-rolled copy

It doesn't need `try_cached_session` at all — that resumes an *existing*
selected account's session for a connect attempt, which is the connect path's
job (net.rs/sim.rs), not this screen's.

For "Add account", both worker threads (`run_device_code_login` and the
loopback flow's `finish_ms_token`) now call
[`login::finish_interactive`] directly, rather than hand-rolling its
`session_from_ms_token` + `secrets.save_refresh_token` composition a second
time. That used to be impossible for one concrete reason, now fixed:
**`finish_interactive` and `migrate_legacy_cache` both take
`secrets: &dyn SecretStore`, but `AccountSecrets` — the façade
[`store::AccountSecrets::open()`] returns, which is what decides
keychain-vs-session-only-fallback — did not implement `SecretStore` and did
not expose its inner boxed backend.** `store.rs` now has
`impl SecretStore for AccountSecrets`, forwarding to the same boxed backend
its own inherent methods already used — see
`store::tests::account_secrets_is_usable_as_a_dyn_secret_store` for the
(mostly compile-time) proof.

Each worker still passes `finish_interactive` a throwaway
`AccountsMetadata::default()`: the upsert it performs is discarded, because
this screen's *real* metadata lives on the render thread and is written back
through [`AccountsNav::pump`] — one funnel, so a background sign-in thread's
write can never race a foreground Remove. Only the returned `Session` and the
keychain write (which `finish_interactive` performs internally now, instead
of the worker calling `secrets.save_refresh_token` itself) matter to the
caller.

**One deliberate seam kept, not merged:** a credential-*save* failure
(`AuthError::Keychain`/`AuthError::Cache`) and a session-*derivation* failure
used to render two different messages ("signed in, but could not save the
credential: …" vs. the general `describe_auth_error`), and `finish_ms_token`
only logged a `tracing::warn!` for the latter. Collapsing both steps into one
`Result` did not have to collapse that distinction: `describe_auth_error`'s
sibling `describe_finish_interactive_failure` (`menu/accounts.rs`) branches on
the error *variant* instead — `save_refresh_token` can only fail with
`Keychain`/`Cache`, and deriving the session can't produce either of those —
so both call sites keep their original two messages and `finish_ms_token`
keeps warning on exactly the same arm it always did.

### The offline entry and `AccountsMetadata::selected`

`AccountsMetadata::selected: Option<Uuid>` has no room for a third "offline"
state without a schema change in this crate, which this screen does not make.
Instead it treats **"no account selected" as offline mode's own selected
state**: the offline row shows the selection marker exactly when
`selected.is_none()`, and choosing it sets `selected = None` and saves. This
is exactly what `login::try_cached_session` does with a
`None` selection (`Ok(CachedSessionOutcome::NoAccountSelected)`, no network
call), and what `resolve_selected_account` turns into
`SelectedAccount::Offline` — so the connect path does not need to know offline
mode exists as a concept; it is already the correct behaviour for "nothing
selected." The one thing this does *not* give a caller is telling
"user explicitly chose offline" apart from "fresh install, never asked" —
both look identical on disk. That distinction would need a real schema change
(a third variant, or a separate `offline: bool` field) if it ever matters.

### The client-id requirement applies here too

Sign-in through this screen calls [`login::resolve_client_id`] exactly like
the connect path does, so it uses the shipped registration unless
`LODESTONE_MS_CLIENT_ID` overrides it — see "The client id" above.
Its failure renders as an ordinary typed-error message on the sign-in screen
([`accounts::describe_auth_error`]), not a panic or a silent no-op.

### Row interaction: click focuses, double-click selects

The same model as the server list and the world list. A single click moves the
cursor — `AccountsNav::click_row` writes both `focus` (what draws highlighted)
and `highlighted` (what Select/Remove/Delete act on) — and a second click
within the double-click threshold runs `select_focused`, which commits the
account switch and persists `profiles.json`. `MenuNav::click_accounts` routes
it, through the same `DoubleClickTracker` the server list uses.

This screen previously had no arm in `MenuNav::click` at all and fell through
to `hover` + `Enter`, and Enter on an account row selects — so one click both
moved the cursor *and* switched account, writing `profiles.json` on a click
that may only have been aiming Remove at a row.

Note `hover` deliberately does **not** write `highlighted` and a click does.
They are not in tension: hover must not re-aim Select/Remove at whatever the
cursor last passed over, while a click *is* how the cursor moves. Both
directions have gates.

The tracker is keyed by `(Screen, usize)` rather than by the row alone,
because two screens share it and a bare row index is not a unique target
across them.

### Row avatars

Each row draws its **own** account's skin: `account_screen::account_head`
reads `AccountProfile::skin_url` and resolves it through `crate::remote_skins`
— the same URL-keyed fetch and decode cache a player's body in the world
fills, so an account's sheet is one more entry rather than a second pipeline.
`favicon::face_mosaic` takes the 8×8 face at `(8, 8)` with the **hat** layer
at `(40, 8)` composited over it, matching `PlayerFaceRenderer`; a skin whose
character is its helmet or hair is unrecognisable from the base layer alone.

Every row drew `render::default_head_icon` — one hand-authored 8×8 square —
until this landed, so the list showed the same face beside every name. The
field had zero production sites assigning it anything else, which is why it
looked deliberate rather than broken, and `skin_url` had been persisted per
account the whole time with no reader.

The placeholder is still the fallback, for an account with no stored URL, a
fetch still in flight, and a sheet too small to hold a face. **Each of those
logs its reason**: a default head looks like a head, so the fallback is
otherwise invisible, and the three are very different bugs behind one
identical pixel.

### What isn't built

No mouse wheel scrolling (keyboard Up/Down only, matching every other
row-stack screen in this menu). No credential form of any kind, by design —
see the module's own doc comment.

The offline row keeps the placeholder head. It is not a Microsoft account and
carries no `skin_url`; drawing the locally cached `skin.png` there would be a
different claim (that sheet belongs to whichever account last signed in, not
to the offline identity).

## Configuration

- `LODESTONE_DATA_DIR` — overrides the whole data directory (same variable
  `lodestone-shell` reads), which moves `profiles.json` too.
- No environment variable controls either keychain *service name* — the
  refresh token's is the constant `"dev.lodestone.ms-refresh-token"` in
  `store.rs`, and the cached session's is `"dev.lodestone.mc-session"`, right
  beside it. Changing either orphans whatever is already stored under the old
  name (refresh tokens and cached sessions independently, since they are
  different services); treat either like a schema version.
- `RUST_LOG`/`tracing-subscriber` filtering controls whether the
  `AccountSecrets::open()` fallback warning and the migration `info!` line
  are visible — nothing here is gated behind a separate flag.
- `LODESTONE_MS_CLIENT_ID` — **optional**; overrides the registered Azure
  public-client id to authenticate as. Unset, [`login::DEFAULT_CLIENT_ID`]
  applies. Setting it *blank* is a typed error rather than a fall-back to the
  default. See "The client id" above.

## Dependencies

- [`keyring`](https://crates.io/crates/keyring) `4.1` — chosen because its
  default `v1` feature auto-selects the correct native backend per target
  (`apple-native-keyring-store` on macOS/iOS, `windows-native-keyring-store`
  on Windows, `zbus-secret-service-keyring-store` — pure-Rust, no libdbus
  system dependency — everywhere else), each already target-gated inside
  `keyring`'s own manifest so nothing platform-specific leaks into the wrong
  build. It's an active fork of the long-standing `hwchen/keyring-rs`
  project, now maintained under the `open-source-cooperative` GitHub org by
  the same maintainer (Daniel Brotsky); at the time of writing its most
  recent release (`4.1.5`) shipped two weeks prior; releases have been
  frequent (six in the three months before that). Native-only — see
  `Cargo.toml`'s `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` —
  there is no wasm32 backend, nor would one make sense: a browser build has
  no OS keychain to reach.
- `serde_json` — the metadata file's format. Object key order in the written
  file is **alphabetical**, not insertion order: `serde_json::Map` is
  `BTreeMap`-backed unless the `preserve_order` feature is enabled, and
  nothing in this workspace's dependency graph enables it. This is a library
  detail, not a deliberate design choice — don't read anything into the key
  order.
- `uuid` — profile identity, and the type both `store` and `metadata` key on.
- `reqwest`/`tokio` — already crate dependencies for the OAuth chain;
  `migrate::migrate_legacy_cache` and `login` reuse them rather than adding
  anything new.
- `lodestone-client` (a new edge) now depends on `lodestone-auth` and
  `reqwest` directly (both native-only, mirroring `lodestone-auth`'s own
  gating) — the first real dependent this crate has ever had; before this
  change the only references outside `lodestone-auth` itself were comments in
  `lodestone-net/src/crypto.rs`. `lodestone-shell` also gained a direct
  `lodestone-auth` (plus `reqwest`) dependency for the same reason
  `menu/accounts.rs` needs the interactive flow and the keychain
  store directly, not just an already-authenticated `Session`.

[`store`]: ../crates/lodestone-auth/src/store.rs
[`store::SecretStore`]: ../crates/lodestone-auth/src/store.rs
[`store::KeychainStore`]: ../crates/lodestone-auth/src/store.rs
[`store::MemoryStore`]: ../crates/lodestone-auth/src/store.rs
[`store::AccountSecrets`]: ../crates/lodestone-auth/src/store.rs
[`store::StorageMode`]: ../crates/lodestone-auth/src/store.rs
[`store::CachedSession`]: ../crates/lodestone-auth/src/store.rs
[`metadata`]: ../crates/lodestone-auth/src/metadata.rs
[`metadata::AccountProfile`]: ../crates/lodestone-auth/src/metadata.rs
[`metadata::AccountsMetadata`]: ../crates/lodestone-auth/src/metadata.rs
[`metadata::AccountsMetadata::from_json`]: ../crates/lodestone-auth/src/metadata.rs
[`paths::data_dir()`]: ../crates/lodestone-auth/src/paths.rs
[`paths::profiles_path()`]: ../crates/lodestone-auth/src/paths.rs
[`paths::legacy_token_cache_path()`]: ../crates/lodestone-auth/src/paths.rs
[`login`]: ../crates/lodestone-auth/src/login.rs
[`login::try_cached_session`]: ../crates/lodestone-auth/src/login.rs
[`login::SESSION_EXPIRY_MARGIN_SECS`]: ../crates/lodestone-auth/src/login.rs
[`login::finish_interactive`]: ../crates/lodestone-auth/src/login.rs
[`login::resolve_client_id`]: ../crates/lodestone-auth/src/login.rs
[`XstsErrorKind`]: ../crates/lodestone-auth/src/error.rs
[`AuthError::RefreshTokenInvalid`]: ../crates/lodestone-auth/src/error.rs
[`AuthError::MissingClientId`]: ../crates/lodestone-auth/src/error.rs
[`flow::MOJANG_CLIENT_ID`]: ../crates/lodestone-auth/src/flow.rs
[`migrate::migrate_legacy_cache`]: ../crates/lodestone-auth/src/migrate.rs
[`flow::refresh_token`]: ../crates/lodestone-auth/src/flow.rs
[`flow::session_from_ms_token`]: ../crates/lodestone-auth/src/flow.rs
[`AccountsNav::pump`]: ../crates/lodestone-shell/src/menu/accounts.rs
[`accounts::describe_auth_error`]: ../crates/lodestone-shell/src/menu/accounts.rs
[`render::default_head_icon`]: ../crates/lodestone-shell/src/menu/render.rs
[`render::head_mosaic`]: ../crates/lodestone-shell/src/menu/render.rs
