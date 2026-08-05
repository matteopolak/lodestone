# Server-list status (the Status phase)

## What it is

How Lodestone's integrated server answers a client's **server-list ping** — the
MOTD, player count, version, protocol number, and optional favicon a player sees
in their multiplayer list before they ever try to join, plus the latency `pong`
that renders the connection-strength bars. Issue #277.

Before this landed, a handshake asking for the Status phase *reached*
`State::Status` in `V770ServerProtocol::decode` and then fell through to the
wildcard `Ignored` arm, so our server was invisible in a real client's list: the
client sent `status_request`, waited, and timed out.

## How it works

Three layers, in the order a packet travels:

1. **Decode** — `crates/protocol/v770/src/server_protocol.rs`, two arms gated on
   `State::Status`:
   - `status::serverbound::STATUS_REQUEST` → `ServerBound::StatusRequest`. The
     body is **empty by construction** (`StreamCodec.unit(INSTANCE)`,
     `status/ServerboundStatusRequestPacket.java:10`), so a payload carrying any
     bytes is malformed and drops.
   - `status::serverbound::PING_REQUEST` → `ServerBound::PingRequest { time }`, a
     single big-endian `i64` (`ping/ServerboundPingRequestPacket.java:19`). Reuses
     `packets::common::PingRequest`, which vanilla also shares between the Status
     and Play states.

2. **Lifecycle** — `crates/lodestone-server/src/server.rs`,
   `serve_connection_inner`. Version-free, and a faithful mirror of vanilla's
   `ServerStatusPacketListenerImpl` (`:34-47`):

   | inbound | our response | vanilla source |
   |---|---|---|
   | first `status_request` | send `status_response`, stay open | `:35-40` |
   | second `status_request` | **terminate** (no second reply) | `:36` |
   | `ping_request` | send `pong_response`, then terminate | `:45-46` |

   A ping needs no preceding status request — vanilla has no guard there, so
   neither do we, or a latency-only probe gets nothing.

   Termination surfaces as `ServerError::StatusRequestHandled`. That is an `Err`
   for a *successful* exchange, which deserves the explanation in its own doc
   comment: `ServeSummary` is shaped around a session that logged in, and a status
   connection has no username, chunks, or inventory to put in one. Vanilla calls
   this a disconnect too — reason `multiplayer.status.request_handled`.

3. **Encode** — `ServerProtocol::encode_status_response` /
   `encode_pong_response`, implemented in `server_protocol.rs`. Both default to
   `ServerDirective::None`, so a protocol family with no status support behaves
   exactly as before. Only `v770` implements `ServerProtocol` at all, so only 26.2
   answers a status ping today.

### The JSON document

The whole `status_response` body is one length-prefixed JSON string
(`ByteBufCodecs.lenientJson(32767)`,
`status/ClientboundStatusResponsePacket.java:16`) with **nothing after it**.
`encode_status_response_body` builds it field-by-field against
`ServerStatus.CODEC` (`status/ServerStatus.java:24-33`); that function's doc
comment carries the per-field table and citations.

Two behaviours worth knowing before you change it:

- **`favicon` and `enforcesSecureChat` are omitted, not emptied.**
  `Favicon.CODEC` *errors* on any string lacking the `data:image/png;base64,`
  prefix (`:38-40`), so an empty-string favicon would make a real client reject
  the entire document — our server would look unreachable with no visible cause.
  `enforcesSecureChat` is omitted when `false` because that is its declared
  default (`:30`).
- **`description` is written as `{"text": …}`, which is deliberately *not* what
  vanilla emits.** A live 26.2 server sends the bare-string form for a MOTD set in
  `server.properties` — captured, not assumed; see the fixture below. Both decode.
  If a future gate ever wants byte-identity with vanilla's own output, this is the
  field that will differ.

### Configuration

`crates/lodestone-server/src/server.rs`:

| constant | value | why |
|---|---|---|
| `STATUS_MOTD` | `"A Lodestone Server"` | vanilla's own default is `server.properties`' `motd=A Minecraft Server`; naming Lodestone avoids impersonating vanilla |
| `STATUS_MAX_PLAYERS` | `20` | matches the `max_players` the join sequence already reports in `GameLogin`, so a client does not see the cap change between its list and the join |

`version.name` and `version.protocol` are **not** configurable here and must not
be: they come from the version crate's own `MINECRAFT_VERSION` (`"26.2"`) and
`PROTOCOL` (`776`), exactly as vanilla's `ServerStatus.Version.current()` reads
them from `SharedConstants` (`:71-74`). `lodestone-server` is version-free and may
never name a protocol number, which is why `encode_status_response` takes scalars
for everything else and fills these two itself.

## Known gap: `players.online` is always `0`

Real, deliberate, and the one thing on this screen that is not truthful.

A status request arrives on its **own connection**, before and independent of any
join, so the per-connection `serve_connection_inner` loop cannot see the sessions
other connections are serving. A truthful count needs a cross-connection player
registry that this crate does not have. Everything else in the row a client
renders — MOTD, cap, version, protocol — is real.

For reference, the live vanilla capture below was taken while one player was
connected and reports `"online": 1`, so vanilla does report a real count.

To close it: add a shared counter (an `Arc<AtomicUsize>`, or a registry keyed by
username) owned by `IntegratedServer` and incremented around the Play phase, then
thread it into `serve_connection_inner`. Note the hazard: a plain connection
counter that only ever increments is the *duration* species of vacuous test
subject — it accumulates past any gate's lifetime, so a test must assert a delta
or use a fresh local counter, and must prefer a **count** over a timing.

## How to change it, and the gotchas

- **`serde_json` is a production dependency of `lodestone-v770` because of this
  feature.** Do not replace the serializer with `format!`. A MOTD containing a
  quote, backslash, or newline would emit invalid JSON, and the failure is
  invisible from our side — the client just silently rejects the document.
  `a_motd_containing_json_metacharacters_still_encodes_validly` guards this.
- **`base64_encode` is hand-rolled** (mirroring `lodestone-net`'s hand-rolled
  `decode_base64`). Its tail handling is the classic bug; the gate anchors all
  three padding cases on `/usr/bin/base64`'s own output rather than on ours.
- **`lodestone_net::decode_favicon` rejects any payload that is not a PNG** (its
  `PNG_MAGIC` check). A round-trip test over non-PNG bytes therefore *correctly*
  fails, which looks like an encoder bug and is not — this cost one debugging pass.
- Adding a `ServerBound` variant changes directive **sequences**, not just types.
  `serve_connection_inner`'s and `dispatch_play_packet`'s match arms are
  compiler-enforced, but a choreography test asserting an exact
  `vec![Directive…]` is a silent caller. Grep the **packet id**, not the variant
  name, and run affected crates with `--no-fail-fast`.

## Evidence

`crates/protocol/v770/tests/server_status.rs`, 13 tests, all driving the **real**
`V770ServerProtocol` through the real `serve_connection` over a real
`memory_pair` transport — not `lodestone-server`'s `FakeProtocol`, which has
invented packet ids and structurally cannot exercise the protocol-776 decoder.
The client half of each exchange is hand-written bytes, so the decoder is never
validated against bytes our own encoder produced.

Expected values come from three sources outside our code:

1. **A live vanilla 26.2 server**, captured over a raw socket by a script using
   nothing from this tree, checked in as
   `crates/protocol/v770/tests/fixtures/vanilla_status_response_26_2.json`. It
   pins the packet ids (`status_response` = 0, `pong_response` = 1), the framing
   (one length-prefixed string, zero trailing bytes), the JSON key set and
   nesting, `NameAndId`'s `id`/`name` keys with a hyphenated uuid, the 8-byte pong
   payload, verbatim echo, and that vanilla closes after answering a ping.
2. **The decompiled 26.2 source** at `.cache/mc/26.2/src`, cited per assertion.
3. **`/usr/bin/base64`** for the favicon encoding.

Four controls were **run and observed to fail**, each isolated to the assertions
that should catch it:

| control | neuter | observed |
|---|---|---|
| A | `encode_status_response` returns `None` (the pre-#277 server) | 9 of 13 fail; the negative control and the pong test still pass |
| B | pong echoes a constant `0` instead of `time` | only `pong_echoes_…` fails, at `time = 1` |
| C | repeat-`status_request` guard removed | only `a_second_status_request_…` fails: 2 packets, expected 1 |
| D | off-by-one in `base64_encode`'s tail group | only the favicon test fails: `iVB=Rw0=Ggo=` vs `iVBORw0KGgoA` |

A permanent negative control ships alongside them: `UnwiredStatusProtocol` keeps
the real decode but leaves both encoders at their trait defaults, and must send
**zero** packets.

### What remains unproven

**No real vanilla client has rendered our server in its list.** That is the
strongest available evidence for this feature and it requires launching the game
client, which is the owner's call, not an agent's. What is proven is that our
bytes match — id for id, key for key, byte for byte on the pong — what a live
vanilla 26.2 *server* sent for the same exchange, and that our own
independently-written real-server parser accepts the document. What is not proven
is that a real client's list row *renders* it, and the `players.online: 0` gap
above will be visible when someone checks.

## Dependencies

- `lodestone-core` — `Reader`/`Writer` for the length-prefixed string framing.
- `serde_json` — builds the status document (production dep of `lodestone-v770`).
- `lodestone-net` — cross-check only, in tests: `parse_status_json` and
  `decode_favicon`, the client-side halves written for reading *real* servers.
- `.cache/mc/26.2/src` — the decompiled reference for every wire shape above.

## Related

- `docs/main-menu.md` — the client side of the same wire format: our multiplayer
  list already *queries* other servers' status.
- Issue #421 — our client did not decode `players.sample`, so the list could not
  show who was online. The server half emits `sample` correctly
  (`a_player_sample_entry_uses_nameandids_keys_and_hyphenated_uuid` pins the keys
  and the hyphenated-uuid form against vanilla's own capture), and the client now
  decodes it: `lodestone_net::status::parse_status_json` fills
  `ServerStatus.sample` (a `Vec<PlayerSample>` of `name` + raw `id`, malformed
  entries skipped), and `lodestone-shell`'s `menu::status::net_probe` threads the
  names into its display `ServerStatus.sample`. What remains is rendering: the
  "who's online" row tooltip (`ServerSelectionList.java:410,430`) does not exist
  in the shell yet.
- Issue #280 — the keep-alive timeout, which is Play's version of the same
  "never hold a connection open forever" concern this phase handles with its
  disconnect-after-answer.
