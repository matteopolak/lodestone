# Offline and Online Authentication Intent Design

## What it is

This change makes the user's selected account mode an explicit input to the login
driver. Offline accounts complete protocol encryption without contacting Mojang;
online accounts use the session service only when the server requests it.

## Problem

The login driver currently treats `should_authenticate = true` plus a missing
online session as proof that login must fail. An explicitly selected offline
identity also has no online session, so cracked or hybrid servers that use an
encryption request while allowing offline authentication incorrectly produce
"this server requires a Minecraft account" instead of completing encryption.

The absence of a session conflates three different states: deliberately offline,
valid online session, and selected-online account whose session could not be
resolved. Those states require different security and error behaviour.

## Goals

- An offline account never calls Microsoft, Xbox, Minecraft, or Mojang session
  services during connection.
- Offline mode may still perform the server's RSA/AES encryption handshake.
- An online account calls Mojang's join service only when requested by the server.
- Online credential and service failures remain explicit; never silently change
  an online connection into an offline identity.
- Preserve the selected profile name/UUID on servers that do not request Mojang
  authentication.

## Non-goals

- Automatically detecting whether an arbitrary server is "cracked" before login.
- Retrying an expired online session as an offline account.
- Hiding Microsoft/Mojang outages or changing account-refresh policy.
- Changing server-side online-mode semantics.

## Design

### Explicit intent

Carry a typed authentication intent from shell account selection through
`ClientBuilder` into the connection driver. The states are conceptually:

```text
Offline
Online(usable session)
OnlineUnavailable(account context and diagnostic)
```

This is an intent, not merely an `Option<Session>`. Shell `RemoteAuth::Offline`
and an explicitly offline selected account create `Offline`. A successfully
resolved Microsoft account creates `Online`. A selected online account whose
credentials cannot be resolved creates `OnlineUnavailable` so its diagnostic is
retained until the protocol proves whether authorization is needed.

### Encryption request decision

Whenever the policy table below permits the handshake to continue, the driver
performs the cryptographic portion of an encryption request: validate the public
key, generate the shared secret, encrypt the response, send it, and enable the
cipher.

The server packet's `should_authenticate` flag controls whether online intent
uses Mojang's join service:

| Intent | `should_authenticate` false | `should_authenticate` true |
| --- | --- | --- |
| Offline | Encrypt; no service call | Encrypt; no service call |
| Online(session) | Encrypt; no service call | Call join service, then encrypt |
| OnlineUnavailable | Encrypt without fallback if authorization is not needed | Return the retained auth error; send no response |

If a server sends no encryption request, login proceeds with the already selected
profile and makes no Mojang join call.

`OnlineUnavailable` joining a server that does not request authentication is not
an offline fallback: the selected online identity is retained and no server proof
was requested. When proof is requested, the connection fails rather than changing
identity or skipping required authorization.

### Error taxonomy

Keep errors distinguishable at the UI boundary:

- no signed-in account / missing session;
- expired or otherwise unusable credentials;
- local account-resolution failure;
- Microsoft/Minecraft authentication service failure;
- Mojang session join rejection or outage;
- encryption/protocol failure.

Session-service errors must occur before the encryption response is sent so the
connection cannot continue in a partially authenticated state.

## Failure handling

Only explicit `Offline` intent may bypass Mojang when
`should_authenticate = true`. Online errors never mutate the intent and never
retry as offline. A server that rejects the resulting offline identity surfaces
its normal disconnect reason.

## Tests

- Offline intent plus `should_authenticate = true` sends an encryption response,
  enables encryption, and performs zero session-service calls.
- Offline intent plus `should_authenticate = false` behaves the same.
- Online intent plus `false` performs zero join calls and continues.
- Online intent plus `true` performs exactly one join call before responding.
- `OnlineUnavailable` plus `true` returns its retained diagnostic and sends no
  encryption response.
- Mojang rejection/outage and expired credentials never retry offline.
- A login path with no encryption request performs no join call.

## How to change it

Account selection and resolution belong in the shell; intent construction belongs
at the client builder boundary; the packet-driven decision belongs in the login
driver. Do not reintroduce inference from `Option<Session>` because it erases the
security-relevant distinction between explicit offline mode and broken online
authentication.

## Configuration and dependencies

There is no new setting. Behaviour depends on the selected account type, the
server encryption request's `should_authenticate` field, the existing RSA/AES
login implementation, and the Mojang session-service client.
