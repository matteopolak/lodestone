# Protocol, networking and multi-version roadmap

## What this is

This roadmap covers wire compatibility for the 26.2 client and server, networking
robustness, and the decision framework for additional protocol families. It does not cover
the gameplay systems transported by the wire, GUI presentation, or command execution.

The client and server are separate compatibility directions. A packet is only complete when
it has a correct codec, reaches a meaningful action or event, and its final consumer changes
session state, server state, pixels, or network output as intended.

## Coverage measurement

Run:

```text
cargo xtask connectedness
```

The command reports independent axes: clientbound decode and emission, serverbound client
encoding, serverbound server decode, and server consumer connectivity. The current 26.2 result
is **141/141 decoded, 139/141 emitted, 68/69 encoded, 66/69 serverbound decoded, and 58/69
connected**. A decoded packet that is intentionally side-effect-only must be documented as such;
a decoded packet that produces no action, event, or state change is an island.

The registry has ten families and every family has at least one hosted revision. Hosted protocols
are 5, 47, 110, 210, 316, 340, 404, 498, 578, 754, 756, 758, 762, 766, 774, and 776. A family can
still join revisions it does not host, so callers must use the registry's explicit server table
rather than infer hosting from the family feature. No family is enabled by default in the registry;
the live shell enables `v26-2`.

`hosted_protocols` exposes that table's hostable rows for diagnostics and the bounded acceptance
matrix in `crates/lodestone-registry/tests/hosted_action_matrix.rs`. With every registry feature
enabled, it runs the rows serially through an in-memory server and real adapter: a block-breaking
Play action must change the received client block, then movement must stream the newly centred
column on the same session. This is an internal compatibility gate, not a substitute for an
external-client join against each release.

## Server compatibility roadmap

The outer connection path is implemented: status and pong replies, phase-appropriate
disconnects, login compression and encryption, online identity validation, configuration
registry/known-pack/tag sends, and the 15-second keep-alive challenge with timeout. The server
sends the configuration burst as an empty known-pack selection, all synchronized registries, and
tags; structured dimension/clock data and opaque payloads share the same ordering contract.

The remaining server work is consumer connectivity and hardening:

1. **Serverbound consumers.** The 58/69 connected result leaves 11 cases without an end-to-end
   consumer: eight decode to `Ignored`, and three still need decode arms. The teleport acknowledgement
   is now connection state: every 26.2 player-position producer receives a unique pending id, and
   movement remains gated until its matching reply arrives. The remaining cases cover tick/
   configuration acknowledgement, chat acknowledgement, custom click controls, and minecart/
   structure/jigsaw/test administration. The control-ping `pong` reply is explicitly decoded and consumed
   without state or output: the hosted server has no matching ping producer, so retaining an
   acknowledgement counter would model behaviour that does not exist. A capability with no server
   meaning should remain explicitly side-effect-only; otherwise it needs a dispatcher and integration
   test.
2. **Chat relay fidelity.** The server validates an inbound signed message but broadcasts it as
   system chat. Implement relay as signed player chat, a receiver-visible signature cache, and
   `chat_ack` handling before treating social/reporting behaviour as complete.
3. **Load and decoder hardening.** Keep queue bounds and game/network ownership explicit, and add
   fuzz/property coverage for every accepted decode path.

Movement, entity interaction, inventory, world/admin, and lifecycle work share the same
completion rule: adapter decode produces a specific `ServerBound` value; the dispatcher applies
it; a server integration test drives the real serve loop. Chunk delivery already uses an
acknowledged-batch gate so transmission pace follows client acknowledgement.

Protocols 756 and 758 accept the signed 16-bit held-item selection packet only for hotbar slots
0 through 8. The selected slot enters the shared `CarriedItemChanged` inventory consumer; malformed,
out-of-range, trailing, and pre-Play inputs are ignored without mutating player state.

## Client compatibility roadmap

- **Secure chat and sessions are wired.** The client obtains and announces a signing session,
  signs outgoing messages, maintains the last-seen window, and verifies remote signed messages.
  Remaining client fidelity is per-sender ordering and expiry enforcement, the `Modified` trust
  decision, trust-badge presentation, and signed commands.
- **Cookies and transfer are wired.** Cookie requests read the session store and emit a response;
  stored cookies survive in `SessionOutcome::Transferred` and can seed the reconnect builder.
  Reconnecting is intentionally owned by the caller, so the remaining task is to make each
  connection owner rebuild a `ClientBuilder` from that outcome.
- **Configuration data is wired.** Registry and tag updates install runtime data, with typed
  dimension and clock values plus ordered names for the other synchronized registries. The
  configuration resource-pack protocol path reaches a response; downloading and applying packs
  belongs to the asset/UI surface.
- **Custom channels are wired.** `ChannelRegistry` dispatches client payloads by channel and the
  server has its corresponding channel registry and bounded broadcast path.
- **Client actions.** Add remaining serverbound encoders only when an action has a defined wire
  representation for that family; distinguish unavailable-by-design actions from unfinished
  encoders.

Packet bundling is an atomic-application concern, not a transport-framing concern: transport
frames each packet independently, while the client driver buffers directives between bundle
boundaries and applies them together. A packet whose only effect is starting a timing window
is not stranded merely because it emits no event.

## Networking invariants

- The framing codec validates length-prefix values and exact pre-allocation ceilings of **2 MiB
  compressed** and **8 MiB decompressed**. Preserve those bounds as codecs evolve.
- Keep packet decode fallible; malformed or unknown input must never reach `.unwrap()` or
  `.expect()` in the wire path.
- A bounded channel only provides backpressure up to its next unbounded relay. Treat queue
  bounds and ownership boundaries as part of protocol correctness, not optional tuning.
- The server emits a keep-alive challenge and disconnects an unresponsive peer; preserve that
  paired liveness behaviour when changing the connection loop.
- Test external interoperability with captured bytes, independent specifications, or a live
  oracle. `decode(encode(x))` only proves two local implementations agree.

## Legacy-family guidance

Additional families are not a mechanical packet-ID exercise. Dispatch and chunk decoding
contain family-specific wire and state-shape decisions; macros can reduce scaffolding but
cannot infer those decisions. Before resuming a dormant family or adding another one:

1. Produce an action table separating unavailable-by-design wire forms from missing work.
2. Identify authoritative external bytes or an oracle for every new packet shape and metadata
   index; do not infer either from a sibling encoder.
3. Keep the family independently removable. Do not borrow a state bridge from another family
   unless it becomes an explicitly owned shared module.
4. Run the registry and adapter review gates, then measure connectedness for that family.

The planning measurement for hand-written family code is 5,139–6,201 lines per family. Treat
that range as a sizing baseline rather than a current census; rerun `cargo xtask protocol-dup`
before using it for an era-grouping decision. Review and integration remain the dominant risk.

## How to change it

Start from the wire reader or writer, follow the value across the adapter and driver or
server dispatcher, and identify the production consumer before adding a variant. Add an
independent-byte test for every wire shape, then an integration test across the relevant
client/server boundary. Re-run `cargo xtask connectedness` after changing dispatch so the
roadmap records only automation-backed coverage.

## Configuration and dependencies

The work depends on `lodestone-net` framing, `lodestone-client` driver state,
`lodestone-server` dispatch and connection serving, the version-family crates, and the
registry feature seam. Live interoperability and captured-byte tests are the external
evidence sources; configuration resource packs, registries, tags, cookies, and account
credentials are session inputs rather than compile-time defaults.
