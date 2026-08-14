# Server-side login compression

## What it is

`lodestone-server` (via `V770ServerProtocol`) now enables zlib packet
compression during login, the mechanical half of issue #273 ("server-side
login has no encryption or compression"). The much larger half — encryption
and the online-mode session-server ownership check — is now implemented too;
see [`docs/server-online-mode.md`](./server-online-mode.md).

## How it works

Real vanilla's `ServerLoginPacketListenerImpl` sends `ClientboundLoginCompressionPacket`
then immediately switches its own connection to compressed framing
(`.cache/mc/26.2/src/net/minecraft/server/network/ServerLoginPacketListenerImpl.java`),
before sending `ClientboundGameProfilePacket` (this repo's `LOGIN_FINISHED`).
`V770ServerProtocol::login_success` reproduces exactly that:

```rust
vec![
    send(login::clientbound::LOGIN_COMPRESSION, &LoginCompression { threshold: COMPRESSION_THRESHOLD }),
    ServerDirective::SetCompression(COMPRESSION_THRESHOLD),
    send(login::clientbound::LOGIN_FINISHED, &finished),
]
```

`crate::server::apply` (in `lodestone-server`) executes directives strictly in
order, and `Connection::write_packet` reads the codec's compression state at
the moment it writes. So this ordering is load-bearing, the same hazard
`Connection::enable_encryption`'s own doc comment names for the encryption
case: `LOGIN_COMPRESSION` itself must go out **before** compression is active
(the client cannot decompress a packet announcing that compression is
starting), and everything after — starting with `LOGIN_FINISHED` itself —
must go out **compressed**, or the two sides disagree about which layer came
first.

`Connection` holds one `Codec` for both directions, so a single
`set_compression` call flips both encode and decode. That is symmetric with
the client: `V770Adapter` already emits `Directive::SetCompression` the
instant it decodes `LOGIN_COMPRESSION`, before reading the next packet — this
is the client-side codepath issue #273 calls "already proven against real
vanilla servers", exercised by every `live-registry`/`live-*` gate that joins
a real oracle (all of which run with vanilla's own default
`network-compression-threshold=256`, so they have been round-tripping
compressed frames the whole time).

**No changes were needed in `lodestone-net`.** `Codec`'s
`set_compression`/`zlib_compress`/`decompress_frame` and
`Connection::set_compression` already existed and are shared by both roles;
`crate::server::apply`'s `ServerDirective::SetCompression(threshold) =>
conn.set_compression(threshold)` arm already existed too, unused until this
change gave it a real caller. The work was entirely "make the server emit the
directives", not new codec plumbing.

## How to change it

- The threshold is `crate::server_protocol::COMPRESSION_THRESHOLD` (`256`),
  matching vanilla's own default — measured identical across every
  `server.properties` under `.cache/mc/`. There is no config knob yet; wiring
  one through `lodestone-server`'s own settings would replace this constant
  with a value read at connection time.
- To disable compression, a negative threshold means "off"
  (`Connection::set_compression`'s own doc comment) — do **not** simply drop
  the `LOGIN_COMPRESSION` send, since a real vanilla client that never
  receives it just never activates compression on its own side either; the
  two directives (`Send`, then `SetCompression`) must always travel together
  in that order.
- `client_integration.rs` and similar `lodestone-server` tests that hand-roll
  their **own** `ServerProtocol` test double (not `V770ServerProtocol`) do not
  send compression at all — that is fine, since those doubles talk to a real
  `V770Adapter` client over an in-memory transport where the extra framing
  adds nothing to the test's own assertions. Only `V770ServerProtocol` needs
  to match real vanilla wire behaviour.

## Configuration

None (see "the threshold" bullet above) — compression is unconditionally
enabled for every `V770ServerProtocol` login.

## Dependencies

- `lodestone-net`'s `Codec`/`Connection` — the shared compression
  implementation, unmodified by this change.
- `crates/protocol/v770/src/packets/login.rs`'s `LoginCompression` — already
  existed for the client-side decode; reused as-is for the server-side encode
  (same struct, same `Encode`/`Decode` derive, so the two directions cannot
  drift).
- Behavioural reference only, never transliterated:
  `net.minecraft.server.network.ServerLoginPacketListenerImpl`,
  `net.minecraft.network.protocol.login.ClientboundLoginCompressionPacket`.

## Encryption and online-mode

Issue #273's larger half — the RSA/AES-128-CFB8 handshake and the
session-server ownership check — is implemented; see
[`docs/server-online-mode.md`](./server-online-mode.md) for the sequence,
what's tested, and the one thing still missing (a real dedicated/LAN host
does not yet call the online-mode entry point — that wiring lives in
`crate::integrated`, outside this change's ownership split).
