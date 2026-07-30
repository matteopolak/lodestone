# Protocol, networking and multi-version — roadmap

1:1 protocol parity for 26.2 (protocol 776) in both directions, on both the client *and*
the server side — a vanilla client must be able to connect to our server, and a vanilla
server to our client — plus the multi-version question. All 23 issues below are filed as
sub-issues of epic [#5](https://github.com/matteopolak/lodestone/issues/5) (Tier 4, 13
issues, server-side) or epic [#4](https://github.com/matteopolak/lodestone/issues/4)
(Tier 3, 10 issues, completeness). A handful of pre-existing issues already covered part
of this domain and were commented on rather than duplicated — see
["What was already filed"](#what-was-already-filed) below.

Not covered here (owned by other tracks): mob AI, redstone, block ticks, world
persistence and the rest of the game-simulation content that a completed protocol layer
would carry ([`server-entities.md`](./server-entities.md),
[`server-simulation.md`](./server-simulation.md) if/when filed); the client-side GUI for
any of this (options screens, chat box UI — [`client-rendering.md`](./client-rendering.md),
[#46](https://github.com/matteopolak/lodestone/issues/46)); command *execution*
server-side ([#48](https://github.com/matteopolak/lodestone/issues/48), pre-existing).

## Measured coverage

```
cargo xtask connectedness
```

```
protocol connectedness (denominators from each family play::{clientbound,serverbound} packet_ids.rs):
v770  clientbound decoded 108/141; emits 107/141; decoded-but-stranded 1 [CHUNK_BATCH_START]; serverbound encoded 53/69; examined 108 arm(s)
```

Re-measured 2026-07-30; unchanged from the last recorded figure. **Use this command, never
a hand count** — a hand-derived figure for this exact domain has been wrong four times in
four different ways.

`cargo xtask check-connected` currently fails on one unrelated finding — `lodestone-nav`
has no workspace dependents outside dev-dependencies — which is a separate,
pre-existing issue in a different domain (autonomous navigation/pathfinding), not
protocol. Not filed here; flagged in the final report instead.

### The measurement everything else in this doc adds to

`53/69` serverbound **encoded** (our client can send it) is the only axis `xtask`
currently measures. **Decoded** (our server can receive it) is a different axis
entirely, and it is not `53/69` — it is measured directly against
`crates/protocol/v770/src/server_protocol.rs::V770ServerProtocol::decode`, which has
match arms only for `Handshaking::INTENTION`, `Login::HELLO`,
`Login::LOGIN_ACKNOWLEDGED`, and `Configuration::FINISH_CONFIGURATION` — every one of
the 69 `play::serverbound` packets falls through the wildcard `_ => ServerBound::Ignored`.
**Serverbound decode is 0/69.** This is the single largest finding of this pass, and it
is *not visible in the client-facing coverage number at all* — being a server is a
genuinely fresh axis, exactly as the domain brief said, and the gap is total rather than
partial.

### Decoded-but-stranded: `CHUNK_BATCH_START`, and why it isn't actually a defect

`xtask`'s one flagged island is `CHUNK_BATCH_START`. On inspection
(`crates/protocol/v770/src/adapter.rs:2030-2034`) the arm calls a real
`self.begin_chunk_batch()` — it starts the rate-timing window that `CHUNK_BATCH_FINISHED`
closes and reports back via `chunk_batch.rs`'s vanilla-matching `ChunkBatchSizeCalculator`.
It emits zero `ClientEvent`s, which is all the heuristic can see, but it is not an
unconsumed island in the harmful sense — nothing needs to be wired to it. Filed as a
correction on [#26](https://github.com/matteopolak/lodestone/issues/26) rather than a new
issue. The **real** chunk-batch gap is one-directional and server-side: the client
computes and sends a real receive-rate reply, but the server (i) never reads it — folded
into [#270](https://github.com/matteopolak/lodestone/issues/270) — and (ii) sends the
entire initial view as one uninterruptible dump rather than pacing it.

The eleven-islands figure the domain brief cited (`ClientboundAnimatePacket`,
`Directive::BeginEncryption`, `TakeItemEntity`/`SET_EQUIPMENT`) is **partially stale**:
see [corrections](#corrections-to-the-briefing-this-roadmap-started-from).

## What was already filed

These pre-existing issues cover ground this domain brief asked about; commented on each
rather than duplicating:

| issue | covers | this pass added |
|---|---|---|
| [#26](https://github.com/matteopolak/lodestone/issues/26) | remaining clientbound packets | family breakdown, `CHUNK_BATCH_START` correction |
| [#34](https://github.com/matteopolak/lodestone/issues/34) | dimension sky light matches on name | pointed at the root cause, [#288](https://github.com/matteopolak/lodestone/issues/288) |
| [#46](https://github.com/matteopolak/lodestone/issues/46) | client command UX (Brigadier tree) | not touched — audited, still accurate |
| [#48](https://github.com/matteopolak/lodestone/issues/48) | server-side command dispatcher | cross-referenced the serverbound-decode issues it depends on |
| [#10](https://github.com/matteopolak/lodestone/issues/10), [#29](https://github.com/matteopolak/lodestone/issues/29) | `ClientboundAnimatePacket`/`TakeItemEntity`/`SET_EQUIPMENT` islands | not touched — audited, still accurate, already correctly labelled `island` |
| [#63](https://github.com/matteopolak/lodestone/issues/63), [#73](https://github.com/matteopolak/lodestone/issues/73) | account switcher UI, auth composition duplication | out of this domain's scope (UI/refactor, not protocol coverage) — not touched |

## Server-side: 13 issues on epic #5

Being a server is a different axis, not a further step along the client axis, and it
starts from much further back than the client does. The **client's** login/configuration
handshake, encryption (verified against a real vanilla server), and compression are all
real and working. The **server's** equivalent is, for encryption and compression,
entirely absent, and for the play-state serverbound decode, completely zero — not
"partial," not "the easy packets are done." Every one of the issues below was found by
direct inspection of `crates/protocol/v770/src/server_protocol.rs` and
`crates/lodestone-server/src/`, not by extrapolation.

| issue | title | note |
|---|---|---|
| [#262](https://github.com/matteopolak/lodestone/issues/262) | Server-side decode: movement and player-state (0/11) | |
| [#264](https://github.com/matteopolak/lodestone/issues/264) | Server-side decode: entity actions, combat, interaction (0/9) | |
| [#266](https://github.com/matteopolak/lodestone/issues/266) | Server-side decode: inventory and container (0/16) | cross-check against `docs/container-clicks.md`'s documented click machine |
| [#268](https://github.com/matteopolak/lodestone/issues/268) | Server-side decode: world/block-admin (0/13) | lowest priority of the five decode issues — creative/admin only |
| [#270](https://github.com/matteopolak/lodestone/issues/270) | Server-side decode: connection-lifecycle and system | includes the real chunk-batch flow-control gap |
| [#271](https://github.com/matteopolak/lodestone/issues/271) | Server-side chat: no decode, no verification, no secure-profile enforcement | pairs with #283 (client-side signing) |
| [#273](https://github.com/matteopolak/lodestone/issues/273) | Server-side login has no encryption or compression | client-side crypto is proven; this is the mirror |
| [#275](https://github.com/matteopolak/lodestone/issues/275) | Server sends no registries/known-packs/tags during configuration | pairs with #288 (client-side ingestion) — one wire format, ideally |
| [#277](https://github.com/matteopolak/lodestone/issues/277) | Server never answers the Status phase | small, but the first thing a real client does |
| [#279](https://github.com/matteopolak/lodestone/issues/279) | Server never sends a Disconnect packet, in any phase | distinct from #68 (client-side stale-key display) |
| [#280](https://github.com/matteopolak/lodestone/issues/280) | Neither side enforces a keep-alive timeout | both directions, one issue |
| [#281](https://github.com/matteopolak/lodestone/issues/281) | No net/game thread split on the server; shell relay channel unbounded | `chore`, not urgent today |
| [#282](https://github.com/matteopolak/lodestone/issues/282) | No fuzz/property-testing harness for any wire decoder | security concern once #262–270 land and we accept real bytes |

### Suggested order

```
Status (#277) ──► Login: encryption+compression (#273) ──► Configuration: registries/tags (#275)
                                                                       │
                                                                       ▼
        Decode: movement (#262) ──► entity actions (#264) ──► inventory (#266) ──► world-admin (#268)
                                                                       │
                                                                       ▼
                                    connection-lifecycle (#270) ──► chat (#271)
                                                                       │
                              ┌────────────────────────────────────────┼───────────────────┐
                              ▼                                        ▼                   ▼
                     keep-alive/timeout (#280)              thread-split/backpressure (#281)   fuzzing (#282)
                                                                                        Disconnect (#279) — needed early for login failures, do in parallel
```

A stranger's vanilla client cannot even find the server without #277, cannot log in as a
real (non-offline-mode) account without #273, and cannot do anything once in the world
without #262–266. #268, #281, #282 and the keep-alive/disconnect issues are hardening
that matters most once the earlier ones are real and strangers' bytes start arriving.

## Client-side completeness: 10 issues on epic #4

The client already does far more than the server does — this list is real gaps, not a
symmetric mirror of the server list.

| issue | title | note |
|---|---|---|
| [#283](https://github.com/matteopolak/lodestone/issues/283) | Secure chat signing entirely absent client-side | would get us silently dropped by `enforce-secure-profile` servers |
| [#286](https://github.com/matteopolak/lodestone/issues/286) | `MessageSignatureCache` built, tested, unconsumed | `island` label — cheap, standalone fix |
| [#288](https://github.com/matteopolak/lodestone/issues/288) | Client never ingests `registry_data` | root cause of #34 and the clock-by-holder-id behaviour |
| [#291](https://github.com/matteopolak/lodestone/issues/291) | Cookies and transfers are dead ends | `transfer` is a player-visible gap (hub/lobby networks) |
| [#294](https://github.com/matteopolak/lodestone/issues/294) | `resource_pack_push`/`pop` only handled in Play, not Configuration | near-direct lift of existing Play-state logic |
| [#296](https://github.com/matteopolak/lodestone/issues/296) | `update_tags` never decoded | invisible against vanilla today because our hardcoded tables happen to match |
| [#299](https://github.com/matteopolak/lodestone/issues/299) | `BUNDLE_DELIMITER` has no decode arm | risk is against a **real vanilla server**, not our own |
| [#301](https://github.com/matteopolak/lodestone/issues/301) | Custom-payload/plugin channels: no registry, no `minecraft:brand` | blocks server-side plugin brand-detection of us |
| [#304](https://github.com/matteopolak/lodestone/issues/304) | 12 serverbound packets we cannot encode at all | mostly creative/admin/debug, low urgency |
| [#306](https://github.com/matteopolak/lodestone/issues/306) | Multi-version: cost of a fifth family, and whether it's worth it | design question, see below |

`registry_data` ingestion (#288) is the highest-leverage single item in this list — it is
the confirmed root cause of two already-observed bugs (#34 and the clock-selection
behaviour) and shares a wire format with the server-side gap (#275), so building both
together is very likely cheaper than reconciling two independent implementations later.

## Chat and secure signing

26.2 requires secure chat. The picture split cleanly into "the bookkeeping is real and
wired" versus "the cryptography does not exist at all, in either role":

- `crates/lodestone-game/src/chat_ack.rs`'s `LastSeenTracker` (the acknowledgement-window
  half) is genuinely wired end to end — `lodestone-client`'s driver maintains it and emits
  `ClientAction::ChatAck`, encoded and tested. Not a gap.
- Everything cryptographic is absent: no session keypair, no per-message signature, no
  `chat_session_update`, no `chat_command_signed`; the one public key the client does
  parse (`player_info_update`'s `INITIALIZE_CHAT`) is explicitly discarded rather than
  stored. Filed as #283 (client) and #271 (server — a strictly harder problem, since a
  server must *verify*, not just produce).
- **Concrete, sourced consequence**: per `.cache/mc/26.2/src`'s
  `ServerGamePacketListenerImpl.handleMessageDecodeFailure`, a real server with
  `enforce-secure-profile=true` silently drops (not kicks) every message we send today.
  Servers with it off show our chat as "not secure" but still deliver it.

## Login, configuration and cookies

The client's login phase is essentially complete and independently verified (the RSA/AES
handshake round-tripped against a real vanilla server, per `docs/accounts.md`).
Everything downstream of that has gaps:

- **Server login** has no encryption and no compression at all (#273) — it only ever
  speaks to our own tolerant client today.
- **Cookies** are a dead end in both phases: the client can receive a `cookie_request` but
  has no `cookie_response` encoder anywhere in the tree (#291).
- **`transfer`** decodes into an event and nothing acts on it — a straightforward
  consumer-wiring gap, folded into #291.
- **Configuration-phase resource-pack push** is unhandled — the Play-state version works
  and is well-tested; it just isn't reachable from the phase real servers most commonly
  use it in (#294).
- **`registry_data`** is not ingested by the client (#288) and not sent by the server
  (#275) — the single biggest data-driven-content gap in the whole domain, and the
  reason two dimension/world-clock bugs exist at all.
- **`update_tags`** is never decoded (#296) — invisible today only because the hardcoded
  fallback tables happen to agree with vanilla defaults.

## Networking robustness

The framing layer itself is in good shape: `crates/lodestone-net/src/codec.rs` bounds the
length-prefix VarInt and caps both compressed (2 MiB) and decompressed (8 MiB) sizes
*before* allocating, and the packet-decode path has zero `.unwrap()`/`.expect()` calls —
an unmatched packet id falls through to a safe no-op rather than erroring or panicking.
That conclusion currently rests entirely on manual code reading, though — there is no
fuzz or property-testing harness anywhere in the tree (#282), so it is a claim someone
made once rather than a gate that runs continuously as new decode paths land.

Backpressure is real but partial: the client library's internal event channel is bounded
and genuinely throttles the socket-read task when a consumer lags, but the shell's own
relay channel downstream of it is an unbounded `std::sync::mpsc`, and the server has no
net/game thread split to have backpressure over at all (#281 — filed as a `chore`, since
neither gap is attacker-facing today).

Keep-alive exists as a mechanism on both sides and is armed on neither: the server never
sends a probe, and the client never sets the read-timeout it already has the plumbing for
(#280). A silent peer is currently held forever by both roles.

`BUNDLE_DELIMITER` (#299) and plugin/custom-payload channels (#301) are the two gaps most
likely to bite specifically when our **client** talks to a **real vanilla server**, since
our own server never generates either case today — a hermetic, self-authored fixture
structurally cannot surface them.

## Multi-version: what it would cost, and the call this roadmap does not make

Filed as [#306](https://github.com/matteopolak/lodestone/issues/306), a design question,
per the domain brief's instruction not to assume the answer. `HANDOFF.md` §1 already
contains the load-bearing analysis — read it before touching this. Reduced to what's
new or worth restating here:

- The reduction from 17 target families to v770-only happened for a structural reason,
  not a scoping preference: **neither adapter dispatch nor wire-shape migration can be
  code-generated.** `xtask new-version` mechanically cloning v340 → v735 produced "a
  1.12.2 client wearing 1.16 packet IDs." Only packet ids and registry tables are the
  cheap, generatable part.
- The measured **irreducible** cost of one family is ~900 hand-written lines (~1 day),
  concentrated in `adapter.rs` (dispatch) and `chunk.rs` (paletted decode/light-split).
- Confirmed today: `v47`/`v340`/`v735` were all last touched at the identical timestamp
  (a mechanical fixture update unrelated to version work), two days before `v770`'s last
  touch — dormant, not actively rotting, receiving zero new work, exactly as
  `CLAUDE.md`'s stated scope implies.
- Confirmed today: there is **no shared packet-definition layer** across families beyond
  the `lodestone-macros` derive codegen and the fixed `VersionAdapter` trait contract.
  Each version hand-writes its own packets and dispatch independently — the *marginal*
  crate has not gotten cheaper in packet-porting terms as more crates were added, only in
  scaffolding/integration terms.
- If resumed: `ClientAction` encode breadth is 16–17/43 on the older families versus
  42/43 on v770 (a 1.8.9 client cannot break a block today), and some of that gap is
  **correct by design** — some actions have no pre-1.9 wire form — so `HANDOFF.md`'s own
  requirement stands: produce the absent-by-design-vs-not-done-yet table *before*
  resuming, because a table where those look identical is exactly how v735 previously
  shipped registered-but-unreviewed.

No recommendation is made here. The mechanical facts say a fifth family is cheap in
isolation (~1 day) and the *integration/review* discipline around it (the
`SHAPE_REVIEW.toml` gate exists specifically because that part goes wrong silently) is
the part this project has already been burned by once.

## Corrections to the briefing this roadmap started from

- **`Directive::BeginEncryption` having "no handler at all" is stale.** It was true when
  written but issue #65 ("Wire lodestone-auth into the join flow so online-mode servers
  work") closed that gap — `crates/lodestone-client/src/driver.rs:316` has a real
  `BeginEncryption` arm today, exercised by `crates/lodestone-client/tests/online_mode_handshake.rs`.
  The client-side crypto path is verified end to end against a real vanilla server per
  `docs/accounts.md`; the actual remaining gap in this area is entirely server-side
  (#273) and chat-signing-specific (#283/#271), not the join-flow handler the brief named.
- **`CHUNK_BATCH_START`'s "decoded-but-stranded" status is a benign mechanical fact, not
  a defect** — see above. It has a real side effect (starting the rate-timing window);
  it just emits no `ClientEvent`, which is all the connectedness heuristic measures.
- **The eleven-confirmed-islands figure is a whole-repo count, not a protocol-domain
  one** — of the three protocol-specific examples the brief named, `ClientboundAnimatePacket`
  and `TakeItemEntity`/`SET_EQUIPMENT` were already filed as their own issues (#10, #29,
  both already correctly labelled `island`) before this pass started; only
  `Directive::BeginEncryption` (above) was actually stale.
- **The server side is not zero-effort greenfield** — `lodestone-server` has a real
  `serve_connection` loop, a real entity-diffing streamer, and a real `TcpListener::bind`
  path proven to serve *our own* client end to end (`crates/lodestone-server/tests/`).
  "A fresh axis" (the brief's framing) is accurate for what a real vanilla client needs
  (encryption, compression, registries, and — starkly — any play-state decode at all);
  it undersells what already exists for serving our own client, which is the right
  foundation to extend rather than a build-from-nothing.
