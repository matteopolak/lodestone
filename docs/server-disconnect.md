# Server-side disconnect packets

## What it is

How Lodestone's integrated server tells a client **why** it is being disconnected,
instead of silently closing the socket. Issue #279.

Before this landed, `grep DISCONNECT crates/protocol/v770/src/server_protocol.rs`
returned nothing: the `Disconnect`/`LoginDisconnect` structs existed but only in
the *decode* direction, so we could display a real server's kick reason and never
produce one. A real client saw a generic "connection lost" for every reason we
had — a timeout, a refused login, a shutdown.

## How it works

One seam method, `ServerProtocol::encode_disconnect(state, reason)`, plus two
producers in the version-free loop.

### The phase asymmetry — the one thing to get right

The reason field is encoded **differently per phase**:

| phase | vanilla packet | reason encoded as | source |
|---|---|---|---|
| Login | `ClientboundLoginDisconnectPacket` | **JSON string** | `ByteBufCodecs.lenientJson(262144)`, `login/ClientboundLoginDisconnectPacket.java:18` |
| Configuration | `ClientboundDisconnectPacket` | **NBT** | `TRUSTED_CONTEXT_FREE_STREAM_CODEC`, `common/ClientboundDisconnectPacket.java:11-12` |
| Play | `ClientboundDisconnectPacket` | **NBT** | same codec |
| Handshaking / Status | *none exists* | — | Status's clientbound set is `status_response`/`pong_response` only; vanilla closes the channel |

Login is the odd one out because the login phase predates NBT components on the
wire. **Writing NBT there produces a packet a real client cannot parse**, and this
is confirmed empirically, not just from the source: a live vanilla 26.2 server's
own login refusal was captured and its body is a length-prefixed JSON string with
zero trailing bytes (see Evidence).

Hence two serializers in `server_protocol.rs`, `text_to_json` and `text_to_nbt`,
rather than one — and a test whose whole job is to assert each phase's body parses
under its own encoding and **fails** under the other's.

### Reasons are `Text`, and they carry a fallback

`encode_disconnect` takes a `&lodestone_model::Text`, the same type the
client-side decode produces. Both current reasons live in
`crates/lodestone-server/src/server.rs`:

| producer | reason | provenance |
|---|---|---|
| keep-alive timeout | `translate("disconnect.timeout")` + fallback `"Timed out"` | key from `ServerCommonPacketListenerImpl.java:37`; fallback is vanilla's own `en_us.json:3498` |
| refused username | `literal("Invalid username")` | text is **ours** — vanilla rejects by throwing, with no translatable reason at all |

Carrying a `fallback` beside a `translate` key is a deliberate improvement over
vanilla's bare `translatable`, and it is a real vanilla feature rather than an
extension: `TranslatableContents` resolves
`currentLanguage.getOrDefault(key, fallback)`
(`network/chat/contents/TranslatableContents.java:41,90`). So a real client shows
its own localized "Timed out", and any client that cannot resolve the key —
including **our** client today, which renders raw translation keys (issue #68) —
shows readable English instead of the literal string `disconnect.timeout`.

### Producers — why this is not an island

An encoder nothing calls reaches zero pixels, which is this repo's dominant defect
class. Two producers are wired *and* gated end-to-end over a real transport:

1. **Play — keep-alive timeout.** `serve_play`'s keep-alive branch previously did
   `return Err(ServerError::KeepAliveTimeout)` with no packet. It now sends the
   disconnect first. The write is deliberately best-effort (`let _ = apply(...)`):
   a peer that stopped answering keep-alives may already be gone, and a failed
   write must still surface `KeepAliveTimeout` rather than masking it as a
   transport error. That is the one place in that loop where dropping an error is
   correct.
2. **Login — refused username.** `is_valid_player_name` mirrors
   `StringUtil.isValidPlayerName` (`net/minecraft/util/StringUtil.java:66-68`): at
   most 16 characters, and no character `<= 32` or `>= 127`. Vanilla checks the
   same thing on the same packet (`ServerLoginPacketListenerImpl.java:120`) but by
   *throwing*, which closes the socket with no explanation. We reject the same
   names and say why. Not merely cosmetic: an offline-mode server derives the
   account uuid from the username and persists player data under it, so a name
   carrying control characters is a name that reaches storage.

**The Configuration-phase encoder has no producer yet**, stated plainly rather
than implied — vanilla's own Configuration disconnects cover datapack and
registry errors this server does not have. It is encoder-tested only.

### Two rejection paths, and only one explains itself

Worth knowing before you extend the name policy: the **length** half of vanilla's
check is already enforced one layer earlier, by the wire decoder.
`LoginHello.name` carries `#[mc(max = 16)]`, so a 17-character name fails
`decode_full` and the loop sees `ServerBound::Ignored` — no `LoginStart`, and
therefore no reason to send. That rejection is **silent**. The character-class
half runs in the loop and does explain itself.

This cost a debugging pass: the first version of the boundary test had one boolean
column and read the silent drop as an *acceptance*. The test now has two columns
(`reaches login success?`, `gets an explanation?`) and asserts both.

## How to change it, and the gotchas

- **`text_to_nbt` / `text_to_json` are NOT general `Text` serializers.** They
  write `text`, `translate` (with `fallback` and `with`), and `extra`, and
  **deliberately drop style, click, hover and insertion**, because a disconnect
  reason renders on a screen with no interactivity. Both are private and named for
  their one caller for exactly this reason. A general serializer belongs in
  `lodestone-model` next to `Text::from_nbt`, as its inverse — do not promote
  these.
- **`component_list` must tag an NBT list with an element type**, and an empty
  list has no element to derive one from. Both call sites guard on `is_empty`
  first; keep that if you add a third.
- **Vanilla writes a bare JSON string for a `with` argument** (`"with": ["26.2"]`)
  where we write `[{"text": "26.2"}]`. Both decode. Same situation as the status
  document's `description` — see `docs/server-status.md`.
- Sending a disconnect does **not** close the connection; the caller does, after
  the write, exactly as vanilla's `Connection::disconnect` flushes before closing.
- `encode_disconnect` defaults to `ServerDirective::None`, so a family without
  disconnect support closes silently — the pre-#279 behaviour, unchanged.

## Configuration

None. The two reasons are constants (`timeout_reason`, `invalid_username_reason`
in `crates/lodestone-server/src/server.rs`) rather than settings, because both are
policy this server does not yet expose.

## Evidence

`crates/protocol/v770/tests/server_disconnect.rs`, 10 tests. Expected values from
outside our code:

- **A live vanilla 26.2 server's own login refusal**, captured over a raw socket
  by a script using nothing from this tree and checked in as
  `crates/protocol/v770/tests/fixtures/vanilla_login_disconnect_26_2.json`.
  Announcing an ancient protocol version makes vanilla refuse the login, which is
  a real `login_disconnect` in the wild. It pins the packet id (`0`), the framing
  (one length-prefixed JSON string, **zero** trailing bytes — the JSON-not-NBT
  asymmetry, measured), and that the reason is a translatable component with
  `with` arguments: `{"translate": "multiplayer.disconnect.outdated_client",
  "with": ["26.2"]}`.
- **The decompiled 26.2 source**, cited per assertion.
- **Vanilla's own `en_us.json`** for the `disconnect.timeout` fallback string.

Three controls were **run and observed to fail**:

| control | neuter | observed |
|---|---|---|
| E | Play arm writes the *login* (JSON) encoding | 4 fail, led by the asymmetry test: "the play-phase body must NOT be parseable as JSON" |
| F | keep-alive producer's send removed (the island case) | only the Play producer gate fails: "the server must SEND a play-phase disconnect before hanging up" |
| G | login name validation disabled | the Login producer gate and the boundary table fail: "the server must SEND a login_disconnect for a refused name" |

Control G was observed with F still applied, so its run also shows F's failure;
the two failures that isolate G — `an_invalid_username_is_refused_with_a_login_disconnect`
and `name_validation_matches_vanillas_own_boundary` — are on a code path F does
not touch.

A permanent control ships alongside: `a_valid_username_is_not_refused` must reach
login success with no disconnect, so the refusal gate cannot pass for a server
that refuses everything.

### What remains unproven

- **No real vanilla client has displayed one of our disconnect reasons.** That
  needs launching the game client, which is the owner's call. What is proven is
  that our login-phase bytes match, id and framing, what a live vanilla *server*
  sent for its own refusal, and that our real client adapter's `nbt_reason_text`
  — validated against real servers' disconnect packets — decodes our Play and
  Configuration reasons back to the reason we sent.
- **No live capture of a Play- or Configuration-phase disconnect.** Triggering one
  externally means completing a full login/configuration handshake against the
  oracle first, which the probe script does not do. The NBT encoding for those two
  rests on the jar citation plus our real-server-validated decoder, not on a
  capture. That is the weakest link in this landing.

## Dependencies

- `lodestone-model` — `Text` / `TextContent`, the reason type.
- `lodestone-core` — `Nbt`/`NbtTag`/`write_network_nbt` for the NBT encoding.
- `serde_json` — the login-phase JSON encoding.
- `.cache/mc/26.2/src` and its `en_us.json` — the decompiled reference.

## Related

- `docs/server-status.md` — the Status phase, which handles the same "never hold a
  connection open forever" concern by disconnecting after it answers.
- Issue #68 — our *client* renders raw translation keys for disconnect reasons.
  The `fallback` we now send makes our own reasons readable despite it, but does
  not fix the general case.
- Issue #280 — the keep-alive timeout itself, whose server half predates this and
  is now the main producer here.
