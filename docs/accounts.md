# Microsoft account storage and online-mode join

## What it is

`crates/lodestone-auth` stores **multiple** Microsoft accounts instead of one,
and no longer keeps the long-lived refresh token in a plaintext file
(issue #64) — see "Storage" below. **It is now also wired into the actual join
flow (issue #65)**: `lodestone-client`'s driver turns a server's online-mode
`Directive::BeginEncryption` into a real RSA/AES handshake plus an
authenticated `POST /session/minecraft/join`, using a `lodestone_auth::Session`
obtained from a cached refresh token or a completed interactive device-code
sign-in. Offline mode is unaffected and remains the default — see "Join-flow
wiring" below for what changed and exactly what is and isn't verified.
**And it now has a screen** (issue #66): `crates/lodestone-shell/src/menu/accounts.rs`
draws the account list — every saved account plus an always-present offline
entry — and drives its own device-code sign-in flow. See "The account list
screen" below for how it fits alongside `login`'s composition layer above
(it does not call into `login` — see that section for why) and for the
offline-selection convention it establishes that `login::try_cached_session`
already happens to handle correctly.

The crate splits an account's data into two places with very different
sensitivity:

| what | where | why |
|---|---|---|
| the Microsoft **refresh** token | the OS keychain, via [`store`] | it is a long-lived bearer credential; it must never sit on disk as plaintext |
| username, profile UUID, skin URL, last-used time, which account is selected | a plain JSON file, [`metadata`] | an account switcher needs to draw its whole list on every launch, and must not have to unlock the keychain (and potentially prompt the user) just to render a menu |

The Minecraft *services* access token (the JWT actually used to join a
server) is **not** persisted at all, in either place. It is short-lived
(~24 h) and cheap to re-derive from the refresh token via
[`flow::refresh_token`] + [`flow::session_from_ms_token`], so persisting it
would only be one more place a credential could leak for no benefit.

## How it works

### Secrets: `store::AccountSecrets`

[`store::SecretStore`] is a trait — `save_refresh_token`/`load_refresh_token`/
`delete_refresh_token`, each keyed by the account's profile `Uuid`. Two
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
catch drift between two independent implementations in two crates; a shared
`lodestone-paths`-style crate would remove the duplication entirely and
should be the actual fix once `config.rs` is no longer held.

[`paths::profiles_path()`] is `data_dir().join("profiles.json")`.
[`paths::legacy_token_cache_path()`] is `data_dir().join("ms_token.json")` —
the exact filename the pre-#64 code used, so migration finds it.

### Migrating a pre-#64 install: `cache` + `migrate`

Before this work, `cache::default_cache_path()` returned one fixed path and
`cache::save()` wrote the refresh token there as plain JSON — the two things
issue #64 exists to fix. `cache.rs` now keeps only what a one-time migration
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
- **Wiring this into the actual join flow (issue #65): done — see
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

## Join-flow wiring (issue #65)

Two pieces landed to turn the storage from issue #64 into an actual
authenticated join: [`login`], the composition layer inside `lodestone-auth`,
and the `Directive::BeginEncryption` handling inside `lodestone-client`'s
driver, which is where the RSA/AES handshake and the session-server call
actually happen.

### `lodestone-auth::login` — cached-refresh-then-interactive composition

`docs/accounts.md`'s own "how to change it" section (issue #64's write-up)
sketched the sequence a connect path would need; [`login`] is that sequence,
built so nothing in it blocks:

- [`login::try_cached_session`] tries the selected account's cached refresh
  token. It makes **no network call at all** when there's nothing to try (no
  selected account, or no stored token for it) — the same "nothing to do,
  don't touch the network" fast path
  [`migrate::migrate_legacy_cache`] uses, which is what makes both
  hermetically testable. A transport/parse failure while refreshing is
  propagated rather than silently treated as "no cached token" — if Microsoft
  is unreachable, starting an interactive flow won't fare any better either,
  and hiding that behind a prompt the user also can't complete would be worse
  than just saying so. Only [`AuthError::RefreshTokenInvalid`] (OAuth's
  `invalid_grant` — the token itself is dead, not the network) becomes "no
  cached token, go interactive."
- If that returns `NoCachedToken`, the caller starts an interactive
  device-code login the existing way (`flow::PendingLogin::begin`, already
  poll-based and non-blocking) and, once it completes, calls
  [`login::finish_interactive`], which derives the session, saves the
  (rotated) refresh token, and upserts + selects the account in
  `AccountsMetadata` — `metadata` is **not** saved to disk by either function,
  same opt-in-save convention [`migrate::migrate_legacy_cache`] already uses.
- [`login::resolve_client_id`] reads the `LODESTONE_MS_CLIENT_ID` environment
  variable and returns a typed [`AuthError::MissingClientId`] if it's unset or
  blank — see "The client-id gap" below for why there is deliberately no
  fallback.

Nothing here drives a poll loop for the caller. A terminal front end can
`.wait()` the `PendingLogin`; a future GUI (issue #66) can `.poll_once()` from
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

### The client-id gap

`flow.rs` has always taken `client_id` as a parameter rather than hardcoding
one — [`flow::MOJANG_CLIENT_ID`] exists, but it is Mojang's own launcher's
registered Azure application id, not this project's, and using it would
misrepresent this client to Microsoft (not just violate a style preference).
Mojang gates production access to the Minecraft API per registered Azure
application, and there is no id this crate can invent that would actually
work. [`login::resolve_client_id`] reads `LODESTONE_MS_CLIENT_ID` and returns
[`AuthError::MissingClientId`] — a clear, typed error naming exactly what's
missing — rather than a panic or a confusing 401 the first time Microsoft's
API is actually called. **Whoever deploys this client still needs to register
their own Azure AD application and request Minecraft API access for it**;
nothing in this change does that for you.

### The driver: `Directive::BeginEncryption` → an actual join

Before this change, `lodestone-client`'s driver had **no arm at all** for
`Directive::BeginEncryption` — it fell into the generic "ignoring unknown
directive variant" catch-all, silently. The adapter side
(`crates/protocol/v770/src/adapter.rs`) already emitted the directive and
already implemented `build_encryption_response` correctly, and both were
already tested (`crates/protocol/v770/tests/join_flow.rs`); the directive
simply had no consumer, which is the exact "island" shape `CLAUDE.md`'s rule 1
describes. `crates/lodestone-client/src/driver.rs`'s `begin_encryption` is the
new consumer:

1. If the server wants a session-server join (`should_authenticate: true`)
   but the client wasn't built with `ClientBuilder::online_session(session)`,
   fail immediately with `ClientError::OnlineModeSessionRequired` — before
   spending a round trip on crypto the server was always going to reject
   anyway (an offline profile has nothing to prove ownership with).
2. Generate the shared secret and RSA-wrap it and the verify token against
   the public key *the directive carried* (`lodestone_net::generate_shared_secret`
   / `rsa_encrypt` — already existed, already verified against NIST vectors
   and a real vanilla server; this change is the first thing that actually
   calls them from a live connect path).
3. Ask the adapter to frame the reply (`build_encryption_response`), write it
   in the clear, *then* call `Connection::enable_encryption` — ordering
   matters and mirrors `Connection::enable_encryption`'s own documented
   contract.
4. If `should_authenticate`, compute the server-hash (`lodestone_auth::server_hash`,
   already verified against Mojang's three published vectors) and call
   `lodestone_auth::join_server`. Any failure here — including every XSTS
   variant above — surfaces as `ClientError::Auth(AuthError)`, so a caller
   matching on it gets the fully typed reason, not a string.

`ClientBuilder::online_session` is the new seam: supply a `lodestone_auth::Session`
(from `login::try_cached_session`/`finish_interactive`) to join online-mode
servers; omit it and everything behaves exactly as before (the offline-mode
default, unchanged).

`lodestone-shell/src/net.rs`'s `NetClient::connect_online` is the shell-level
equivalent of `NetClient::connect`, for a caller that already has a
`lodestone_auth::Session` — it feeds the profile's *real* username/UUID into
the login-start packet instead of `unique_username()`/a random UUID.
**`NetClient::connect` is completely unchanged** and remains what every
existing caller (`app.rs`, `sim.rs`, every test oracle) uses; offline mode's
`unique_username()`-per-run scheme (and the dead-player-blackout hazard it
exists to dodge — see `CLAUDE.md`) is untouched.

### What's verified and what isn't

Hermetic (`crates/lodestone-client/tests/online_mode_handshake.rs`):

- the driver actually RSA-wraps the secret/token against the public key the
  directive carried (decrypted independently with a test-generated private
  key the driver never saw);
- the reply is written before the cipher is enabled, and a packet sent
  afterwards decrypts correctly (the cipher really did turn on, with the
  right secret, in both directions);
- `should_authenticate: false` skips the session-server call entirely;
- `should_authenticate: true` with no configured session fails fast with the
  typed `OnlineModeSessionRequired`, before any crypto happens.

Also hermetic: every XSTS/refresh-token classification branch in
`lodestone-auth::flow`'s tests, and `login::try_cached_session`'s two
no-network fast paths.

**Not verified, and not claimed:** an actual successful `join_server` call
against the real `sessionserver.mojang.com` with a real Microsoft account.
Same limitation `flow.rs` already had — there is no way to construct a
hermetic fake for Microsoft's OAuth/Xbox/XSTS/session-server chain — and the
same reason `lodestone-net/tests/online_handshake.rs` is `#[ignore]`d rather
than run by default. A caller with a real Azure client id and a real Xbox
account is the only way to close this gap.

## The account list screen (issue #66)

`crates/lodestone-shell/src/menu/accounts.rs`'s `AccountsNav` draws the account
list — every account from `AccountsMetadata`, most-recently-used first, plus a
synthetic offline entry always appended last — and drives Add/Select/Remove/
Cancel plus its own device-code sign-in sub-flow. See `docs/main-menu.md` for
where the screen sits in the menu state machine and how it's rendered.

### Why it does not call `login::try_cached_session`/`finish_interactive`

It doesn't need `try_cached_session` at all — that resumes an *existing*
selected account's session for a connect attempt, which is issue #65's job
(net.rs/sim.rs), not this screen's. For "Add account" it hand-rolls the
equivalent of `finish_interactive` (`session_from_ms_token` +
`secrets.save_refresh_token` + upserting `AccountProfile`) instead of calling
it, for one concrete reason: **`finish_interactive` and `migrate_legacy_cache`
both take `secrets: &dyn SecretStore`, but `AccountSecrets` — the façade
[`store::AccountSecrets::open()`] returns, which is what decides
keychain-vs-session-only-fallback — does not implement `SecretStore` and does
not expose its inner boxed backend.** There is currently no way to hold an
`AccountSecrets` and also hand a caller the `&dyn SecretStore` these
composition functions want. `menu/accounts.rs` works around this by calling
`AccountSecrets`'s own three methods directly rather than going through
`login`, which costs a small amount of duplicated glue (persisting the
refresh token, deriving the session) but not much: the real value of
`finish_interactive` is the metadata upsert, and this screen was going to do
that itself anyway, funnelled through one call ([`AccountsNav::pump`]) so a
background sign-in thread's write can never race a foreground Remove.
**Fixing the seam** — adding `impl SecretStore for AccountSecrets` that
delegates to the inner backend, or changing `login`'s functions to accept
`&AccountSecrets` — would let a future caller (this screen, or #65's connect
path) use `login`'s composition directly instead of each hand-rolling it.

### The offline entry and `AccountsMetadata::selected`

`AccountsMetadata::selected: Option<Uuid>` has no room for a third "offline"
state without a schema change in this crate, which this screen does not make.
Instead it treats **"no account selected" as offline mode's own selected
state**: the offline row shows the selection marker exactly when
`selected.is_none()`, and choosing it sets `selected = None` and saves. This
happens to be exactly what `login::try_cached_session` already does with a
`None` selection (`Ok(CachedSessionOutcome::NoCachedToken)`, no network call)
— so a future connect path built on `login` does not need to know offline
mode exists as a concept; it is already the correct behaviour for "nothing
selected." The one thing this does *not* give a future caller is telling
"user explicitly chose offline" apart from "fresh install, never asked" —
both look identical on disk. That distinction would need a real schema change
(a third variant, or a separate `offline: bool` field) if it ever matters.

### The client-id requirement applies here too

Sign-in through this screen calls [`login::resolve_client_id`] exactly like
the connect path does, so `LODESTONE_MS_CLIENT_ID` must be set for "Add
account" to get past the very first step — see "The client-id gap" above.
Its failure renders as an ordinary typed-error message on the sign-in screen
([`accounts::describe_auth_error`]), not a panic or a silent no-op.

### What isn't built

No skin fetch (issue #62) — every row's head icon is
[`render::default_head_icon`], a hand-authored placeholder pixel grid, not a
downloaded texture. It is deliberately written so the swap is a data change:
[`render::head_mosaic`] takes raw RGBA bytes and dimensions, exactly the shape
a decoded skin PNG would be in, so nothing about the row, the draw call, or
the geometry builder needs to change once #62 lands a real fetch. No mouse
wheel scrolling (keyboard Up/Down only, matching every other row-stack screen
in this menu). No credential form of any kind, by design — see the module's
own doc comment.

## Configuration

- `LODESTONE_DATA_DIR` — overrides the whole data directory (same variable
  `lodestone-shell` reads), which moves `profiles.json` too.
- No environment variable controls the keychain *service name* — it is the
  constant `"dev.lodestone.ms-refresh-token"` in `store.rs`. Changing it
  orphans any already-stored tokens (they'd sit under the old service name);
  treat it like a schema version.
- `RUST_LOG`/`tracing-subscriber` filtering controls whether the
  `AccountSecrets::open()` fallback warning and the migration `info!` line
  are visible — nothing here is gated behind a separate flag.
- `LODESTONE_MS_CLIENT_ID` (issue #65) — the registered Azure public-client id
  to authenticate as. Required for any interactive sign-in or refresh;
  [`login::resolve_client_id`] returns a typed error rather than falling back
  to Mojang's own launcher id. See "The client-id gap" above.

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
- `lodestone-client` (issue #65, new edge) now depends on `lodestone-auth` and
  `reqwest` directly (both native-only, mirroring `lodestone-auth`'s own
  gating) — the first real dependent this crate has ever had; before this
  change the only references outside `lodestone-auth` itself were comments in
  `lodestone-net/src/crypto.rs`. `lodestone-shell` also gained a direct
  `lodestone-auth` (plus `reqwest`) dependency for the same reason
  `menu/accounts.rs` (issue #66) needs the interactive flow and the keychain
  store directly, not just an already-authenticated `Session`.

[`store`]: ../crates/lodestone-auth/src/store.rs
[`store::SecretStore`]: ../crates/lodestone-auth/src/store.rs
[`store::KeychainStore`]: ../crates/lodestone-auth/src/store.rs
[`store::MemoryStore`]: ../crates/lodestone-auth/src/store.rs
[`store::AccountSecrets`]: ../crates/lodestone-auth/src/store.rs
[`store::StorageMode`]: ../crates/lodestone-auth/src/store.rs
[`metadata`]: ../crates/lodestone-auth/src/metadata.rs
[`metadata::AccountProfile`]: ../crates/lodestone-auth/src/metadata.rs
[`metadata::AccountsMetadata`]: ../crates/lodestone-auth/src/metadata.rs
[`metadata::AccountsMetadata::from_json`]: ../crates/lodestone-auth/src/metadata.rs
[`paths::data_dir()`]: ../crates/lodestone-auth/src/paths.rs
[`paths::profiles_path()`]: ../crates/lodestone-auth/src/paths.rs
[`paths::legacy_token_cache_path()`]: ../crates/lodestone-auth/src/paths.rs
[`login`]: ../crates/lodestone-auth/src/login.rs
[`login::try_cached_session`]: ../crates/lodestone-auth/src/login.rs
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
