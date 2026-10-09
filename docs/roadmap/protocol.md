# Protocol, networking and multi-version roadmap

## What it is

The open work for wire compatibility of the 26.2 client and server, networking robustness, and the decision framework for more protocol families. It excludes the gameplay systems the wire carries, GUI presentation and command execution. A packet is complete only when it has a correct codec, reaches a meaningful action or event, and its final consumer changes session state, server state, pixels or network output.

## How it works

### Coverage measurement

`cargo xtask connectedness` reports independent axes: clientbound decode and emission, serverbound client encoding, serverbound server decode and server consumer connectivity. Current 26.2 result: **141/141 decoded, 139/141 emitted, 68/69 encoded, 66/69 serverbound decoded, 58/69 connected**. A decoded packet that is intentionally side-effect-only must be documented as such; one that produces no action, event or state change is an island.

The registry has ten families, each hosting at least one revision; hosted protocols are 5, 47, 110, 210, 316, 340, 404, 498, 578, 754, 756, 758, 762, 766, 774 and 776. A family can join revisions it does not host, so callers must use the explicit server table. No family is on by default; the live shell enables `v26-2`. `hosted_protocols` feeds diagnostics and `crates/lodestone-registry/tests/hosted_action_matrix.rs`, which (with every feature on) runs rows serially through an in-memory server and real adapter: a block-breaking Play action must change the received client block, then movement must stream the newly centred column. It is an internal gate, not a substitute for an external-client join ([external-client acceptance](../external-client-acceptance.md)).

### Server compatibility

Implemented: status and pong, phase-appropriate disconnects, login compression and encryption, online identity validation, the configuration burst (empty known-pack selection, all synchronized registries, tags) and the 15-second keep-alive with timeout. Remaining work:

1. **Serverbound consumers.** 11 of 69 cases lack an end-to-end consumer (eight decode to `Ignored`, three need decode arms): tick/configuration acknowledgement, chat acknowledgement, custom click controls, minecart/structure/jigsaw/test administration. The teleport acknowledgement is connection state (every 26.2 position producer gets a unique pending id and movement is gated until its reply). The control-ping `pong` is decoded and consumed without state because the host has no matching ping producer. A capability with no server meaning stays explicitly side-effect-only; otherwise it needs a dispatcher and integration test.
2. **Chat relay fidelity.** The server validates an inbound signed message but broadcasts system chat; relay as signed player chat, a receiver-visible signature cache and `chat_ack` handling are needed.
3. **Load and decoder hardening.** Explicit queue bounds and ownership, and fuzz/property coverage for every accepted decode path.

Each class of serverbound work completes the same way: adapter decode yields a specific `ServerBound`, the dispatcher applies it, a server integration test drives the real serve loop. Chunk delivery uses an acknowledged-batch gate. Protocols 756 and 758 accept the signed 16-bit held-item packet only for hotbar slots 0-8 into the shared `CarriedItemChanged` consumer (malformed, out-of-range, trailing and pre-Play input is ignored); 498/578/754 enforce it through their own id tables, 404 at the shared consumer, 47 completes it. 498/578/754 and 756/758 use their own packet ids for the signed 64-bit keep-alive; challenges and exact replies reach common liveness state and malformed, trailing or pre-Play replies cannot acknowledge.

### Client compatibility

- **Secure chat** is wired (signing session, outgoing signing, last-seen window, remote verification). Remaining: per-sender ordering and expiry, the `Modified` trust decision, trust-badge presentation, signed commands.
- **Cookies and transfer** are wired (cookie requests read the session store; stored cookies survive in `SessionOutcome::Transferred` and can seed the reconnect builder). Reconnecting is owned by the caller, so each connection owner must rebuild a `ClientBuilder` from the outcome.
- **Configuration data** installs registry and tag updates (typed dimension and clock values, ordered names otherwise); the resource-pack protocol path reaches a response, while download and apply belong to the asset/UI surface.
- **Custom channels** are wired (`ChannelRegistry`, server-side registry with bounded broadcast).
- **Client actions:** add serverbound encoders only for actions with a defined wire form per family; distinguish unavailable-by-design from unfinished.
- Packet bundling is atomic application, not framing: transport frames each packet independently and the client driver buffers directives between bundle boundaries.

### Networking invariants

- Framing validates length prefixes against pre-allocation ceilings of **2 MiB compressed** and **8 MiB decompressed**; preserve them.
- Packet decode is fallible; malformed or unknown input never reaches `.unwrap()`/`.expect()` on the wire path.
- A bounded channel gives backpressure only up to its next unbounded relay; queue bounds and ownership are part of protocol correctness.
- Preserve the paired keep-alive challenge and timeout disconnect. Test interoperability with captured bytes, independent specifications or a live oracle, since `decode(encode(x))` proves only local agreement.

### Legacy-family guidance

Additional families are not mechanical packet-id work: dispatch and chunk decoding carry family-specific wire and state-shape decisions macros cannot infer. Before resuming a dormant family or adding one: (1) produce an action table separating unavailable-by-design from missing; (2) identify authoritative bytes or an oracle for every packet shape and metadata index (never from a sibling encoder); (3) keep the family independently removable and borrow no state bridge unless it becomes an owned shared module; (4) run the registry and adapter review gates and measure connectedness. Planning baseline: 5,139-6,201 hand-written lines per family (rerun `cargo xtask protocol-dup` before using it for era grouping); review and integration dominate risk. See [multi-version protocol sharing](../plans/multi-version-protocol-dedup.md).

## How to change it

Start from the wire reader or writer, follow the value across adapter and driver or server dispatcher, and identify the production consumer before adding a variant. Add an independent-byte test per wire shape and an integration test across the client/server boundary, then re-run `cargo xtask connectedness` so this roadmap records only automation-backed coverage.

## Dependencies

`lodestone-net` framing, `lodestone-client` driver state, `lodestone-server` dispatch and serving, the version-family crates and the registry feature seam. Captured bytes and live interoperability are the external evidence; resource packs, registries, tags, cookies and credentials are session inputs.
