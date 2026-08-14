# Server-side encryption and online-mode

## What it is

The other half of issue #273: `lodestone-server` can now run the online-mode
RSA/AES-128-CFB8 handshake and verify a connecting client's identity against
Mojang's session server, mirroring the client-side join path
`docs/accounts.md` documents. `docs/server-login-compression.md` covers the
compression half that landed first; this doc covers what it called "the much
larger half."

Before this change `V770ServerProtocol::login_success` sent `LOGIN_FINISHED`
straight off a `LoginHello`, so every join was offline: the server trusted
whatever username/uuid the client claimed, no encryption was ever offered,
and a real vanilla client set to online mode had no way to see either. That
is still every pre-existing entry point's behaviour after this change —
`serve_connection`, `serve_connection_with_commands`, and the other wrappers
in `crates/lodestone-server/src/server.rs` all continue to pass no online-mode
configuration and behave byte-for-byte as before.

## How it works

### The pieces, and which crate owns which

| piece | crate | symbol |
|---|---|---|
| server RSA keypair, decrypt | `lodestone-net` | `crypto::ServerKeyPair` |
| verify-token generation | `lodestone-net` | `crypto::generate_verify_token` |
| AES-128-CFB8 stream cipher | `lodestone-net` | `Connection::enable_encryption` (already existed, shared by both roles) |
| server-id hash | `lodestone-auth` | `server_hash` (already existed, shared by both roles) |
| session-server `hasJoined` check | `lodestone-auth` | `flow::has_joined` |
| wire packets (`hello`/`key`) | `protocol/v770` | `packets::login::{EncryptionRequest, EncryptionResponse}` (already existed for client-side decode; reused as-is) |
| `hello` encode, `key` decode | `protocol/v770` | `V770ServerProtocol::{encode_encryption_request, decode}` |
| the login sequence itself | `lodestone-server` | `server::serve_connection_inner`'s `LoginStart`/`EncryptionResponse` arms |

Nothing in `lodestone-net`'s cipher or RSA plumbing is new *machinery* — the
client side already proved `Cfb8Cipher` and `rsa_encrypt` against NIST
vectors and a real vanilla server (`docs/accounts.md`). What's new is the
server's *half* of the same primitives: [`ServerKeyPair`] generates a
1024-bit RSA keypair (vanilla's own size) and decrypts what the client
encrypted with `rsa_encrypt`, the exact inverse operation.

**`ServerKeyPair` is generated fresh per connection, not once per server.**
Vanilla caches one keypair for the process lifetime
(`MinecraftServer.keyPair`); this crate's `ServerProtocol` implementors are
deliberately stateless (`V770ServerProtocol` is a unit struct — see its own
doc comment on why `ChunkEncoder` is implemented directly on it rather than
through a field), so `serve_connection_inner`'s `LoginStart` arm calls
`ServerKeyPair::generate()` itself and holds it in a local
(`pending_encryption`) until the matching `EncryptionResponse` arrives.
Nothing in the wire protocol requires a keypair shared across connections:
within one connection, all that matters is that the public key sent in
`EncryptionRequest` and the private key used to decrypt that connection's
`EncryptionResponse` are the same pair.

### The sequence, and why the order is load-bearing

Ported from `net.minecraft.server.network.ServerLoginPacketListenerImpl`
(`.cache/mc/26.2/src/`), specifically `handleHello`/`handleKey`/
`verifyLoginAndFinishConnectionSetup`:

1. `ServerBound::LoginStart` arrives. If `online_mode` is configured, the
   loop generates a keypair and a 4-byte verify token
   (`generate_verify_token`, matching vanilla's
   `Ints.toByteArray(RandomSource.create().nextInt())`), sends
   `EncryptionRequest` (server-id `""`, the DER public key, the token,
   `should_authenticate: true` — vanilla never constructs this packet with
   `false`), and stops **without** calling `login_success` yet.
2. `ServerBound::EncryptionResponse` arrives (the `key` packet, decoded with
   no crypto of its own — see `ServerBound::EncryptionResponse`'s own doc
   comment). The loop:
   - RSA-decrypts the verify-token echo and compares it to what it sent —
     any mismatch is `ServerError::VerifyTokenMismatch`, vanilla's
     `isChallengeValid` check.
   - RSA-decrypts the shared secret and applies
     `ServerDirective::EnableEncryption`, which calls
     `Connection::enable_encryption` — **this must happen before anything is
     sent in reply**, the same ordering hazard
     `docs/server-login-compression.md` already documents for
     `SetCompression`: everything up to and including `EncryptionResponse`
     travels in the clear, and everything after (starting with whatever
     `login_success` sends) must already be enciphered on the server's side
     too.
   - Computes the server-id hash (`lodestone_auth::server_hash("", secret,
     public_key_der)`) and calls the session-server check.
3. The session-server check's outcome decides everything that follows:
   - `Some(profile)`: `login_success` is called with **the session server's**
     `id`/`name`, not the client's self-reported ones — this substitution is
     the entire point of online mode, and the test suite (see below) asserts
     it directly by making the two deliberately differ.
   - `None`: disconnected with vanilla's own
     `multiplayer.disconnect.unverified_username` text.
   - An error (network/parse failure): disconnected with vanilla's own
     `multiplayer.disconnect.authservers_down` text — a distinct
     `ServerError::AuthServiceUnavailable`, not folded into "unverified",
     because an outage and a genuine identity mismatch are different
     problems for an operator to see in a log.

`login_success` itself is unchanged: it still sends
`[LOGIN_COMPRESSION, SetCompression, LOGIN_FINISHED]` in that order. Calling
it *after* `EnableEncryption` reproduces vanilla's actual ordering exactly —
`verifyLoginAndFinishConnectionSetup` (which sends the compression packet)
runs from `tick()` once `state == VERIFYING`, which is only reached from
inside `handleKey`'s async session-server callback, i.e. strictly after
`connection.setEncryptionKey` has already run. Encryption first, then
compression, then `login_finished`, matching this port's directive order.

### `ServerProtocol`'s new trait method has a safe default

`ServerProtocol::encode_encryption_request` defaults to returning
`ServerDirective::None`. `serve_connection_inner` reads that as "this
protocol has no online-mode wire support" and falls back to an offline
login rather than sending a request nothing would answer — so every
`ServerProtocol` implementor other than `V770ServerProtocol` (test doubles
across this crate, legacy protocol families with no `ServerProtocol` at all)
needed no changes.

### The seam that keeps a test off the real session server

`OnlineModeConfig` (native-only, `lodestone-server::server`) holds an HTTP
client plus a boxed `verify` closure. `OnlineModeConfig::new(http)` wires
`verify` to the real `lodestone_auth::has_joined`; `OnlineModeConfig::for_test`
substitutes a fixture. This exists because of the exact hazard `CLAUDE.md`
records for the client-side half of this same handshake — a pre-existing
test that would otherwise reach a real external service the moment
online-mode auth is wired into a code path tests already call. `for_test` is
**not** `#[cfg(test)]`-gated, unlike the pattern `CLAUDE.md` otherwise asks
for; see that constructor's own doc comment for the specific reason (a
`lodestone-v770` dev-dependency needed for a unit test that drives the real
`V770ServerProtocol` would create two instantiations of the `ServerProtocol`
trait in this crate's own lib-test compilation — measured, not theorised).
The test lives in `crates/lodestone-server/tests/online_mode.rs` instead, an
external integration test with no such self-reference, and `for_test` had to
become `pub` for that file to reach it.

`crates/protocol/v770/tests/online_mode.rs` covers the crypto and wire shape
without any server-loop machinery: `encode_encryption_request`'s exact wire
bytes, `decode`'s lift of a `key` packet with zero crypto, and a full
cross-role round trip (a real `ServerKeyPair`'s public key travels through a
real directive, a simulated client RSA-encrypts against exactly those bytes
with `lodestone_net::rsa_encrypt`, and the server recovers the exact original
secret and token with `ServerKeyPair::decrypt`).

## How to change it

- **Wiring this into a real dedicated/LAN host is not done by this change,
  deliberately.** The one production caller of the mob-events-and-commands
  wrapper (`serve_connection_with_mob_events_and_commands_shared`) is
  `IntegratedServer` in `crate::integrated`, which this change's ownership
  split does not cover (see this file's own comment on why that function's
  signature could not simply grow a new parameter — the established pattern
  every capability in `crate::server` already follows,
  `serve_connection_with_resource_pack`/`serve_connection_with_plugin_channels`
  among them). `serve_connection_with_online_mode` is the
  `_and_commands_shared`-shaped sibling that passes `Some`; turning a real
  host on needs `integrated.rs` (or whatever code path a future dedicated
  server binary uses) to call it instead, with a real
  `OnlineModeConfig::new(reqwest_client)` and — critically — a config flag an
  operator can actually set (there is no `online-mode` knob read from
  anywhere yet, matching `server-login-compression.md`'s own compression
  threshold having no config knob either).
- **New field on the verified identity (e.g. skin properties):**
  `lodestone_auth::HasJoinedProfile::properties` already carries them from
  the session server, but `ServerProtocol::login_success(&self, username,
  uuid)` has no parameter for them — `V770ServerProtocol::login_success`
  hardcodes `properties: Vec::new()` in `LoginFinished` regardless of mode.
  Changing `login_success`'s signature to accept properties would touch
  every `ServerProtocol` implementor in the workspace (several test doubles
  in `crate::server`'s own test module, plus any future protocol family), so
  this was deliberately left as a known gap rather than force that blast
  radius through this change.
- **Verify-token or secret length assumptions:** both live in
  `lodestone-net::crypto` as named constants
  (`VERIFY_TOKEN_LEN`/`SHARED_SECRET_LEN`), not magic numbers, so a version
  crate that needs a different shape has one place to check against.
- **Gotcha — `should_authenticate` is never `false` on the wire this crate
  sends.** Vanilla's own `ClientboundHelloPacket` constructor is always
  called with `true`; there is no real "encrypt without also verifying with
  the session server" server state, so `encode_encryption_request` does not
  expose it as a parameter.

## Configuration

None yet — see "How to change it" above. `online_mode: Option<&OnlineModeConfig>`
is a parameter on `serve_connection_inner` and `serve_connection_with_online_mode`;
there is no `server.properties`-style `online-mode=true` flag read from disk,
and no host in this repository turns it on today.

## Dependencies

- `lodestone-net`'s `Codec`/`Connection` (`enable_encryption`, already
  existed) and `crypto` (`ServerKeyPair`, `generate_verify_token`, both new;
  `rsa_encrypt`/`generate_shared_secret`, already existed).
- `lodestone-auth`'s `server_hash` (already existed) and `flow::has_joined`
  (new) — a native-only dependency of `lodestone-server`, target-gated the
  same way `lodestone-anvil` already is in that crate's `Cargo.toml`, for the
  same reason: nothing for a `wasm32` browser build to gain by linking an
  HTTPS session-server client no browser singleplayer world ever calls.
- `reqwest`, via `lodestone-auth`, for the `hasJoined` GET.
- Behavioural reference only, never transliterated:
  `net.minecraft.server.network.ServerLoginPacketListenerImpl`,
  `net.minecraft.network.protocol.login.{ClientboundHelloPacket,
  ServerboundKeyPacket}`.
