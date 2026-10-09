# Multiplayer connections and join diagnostics

## What it is

The multiplayer connection path turns a saved or command-line server address into a concrete TCP endpoint while preserving the address presented in the handshake. It also separates failure to establish TCP from silence after a session has started, and carries the movement-reconciliation and outbound-relay rules that keep a remote session responsive.

## How it works

### Build boundary

`lodestone-shell`'s `multiplayer` feature (on by default, independent of `live`) is the authority for remote joins, saved-server status probes and publishing the integrated world to other machines; `live` only selects the protocol family. Without it the title screen keeps a disabled Multiplayer button with an explanatory hover, and command-line and terminal remote joins return a named refusal instead of opening a socket. The browser package forwards the feature; disabling it removes the native page server's `/relay` WebSocket-to-TCP route and does not link `lodestone-relay`, so a singleplayer-only page host cannot become an arbitrary TCP proxy. It also removes the online account gate: a confined browser build starts its in-memory server with the stable offline identity (no Microsoft sign-in).

### Address resolution and timeouts

A saved server entry keeps its port as `Option<u16>`, and CLI parsing records whether `--port` appeared. An explicit port is dialed unchanged and suppresses SRV lookup; a bare hostname goes to `lodestone_net::resolve_server_address`, which checks `_minecraft._tcp.<host>` and falls back to port `25565`. The resolved host and port go in through `ClientBuilder::connect_target`; the original `ServerAddress` is untouched for the handshake, since virtual-hosting proxies route on the hostname the player entered.

TCP establishment has a 10-second budget (`ClientError::ConnectTimeout`); remote packet readers have a separate 30-second idle budget (`ClientError::Timeout`). Integrated singleplayer has no read deadline (initial generation stays authoritative; the loading UI and join profiler expose delay). A remote read timeout logs the protocol state, received-packet count and last packet id under `net_join`; the id is state-relative, so interpret it with the logged `ConnectionState` and adapter.

### Movement reconciliation

- A position correction opens a correction transaction: the adapter surfaces the event before its acknowledgement reaches the wire, the shell adopts the authoritative pose, resolves relative velocity and returns the resolved position and rotation to the driver, and only then does the driver write the acknowledgement plus an unconditional full position-and-look echo with both ground flags clear. This is a rendezvous, not a spatial heuristic.
- Outbound actions carry a monotonic correction generation: movement submitted before the shell completes the transaction is discarded; movement after it (even if already queued) is retained; keep-alives and other non-movement actions stay live.
- Protocol 776 corrections carry pose and velocity; the adapter preserves all nine relative bits, the shell resolves local-player velocity, and entity teleports apply the same component-wise rule (treating every correction as a stop causes repeated vertical disagreement on proxy-authored movement and snaps server impulses).
- An absolute correction snaps both current and previous camera positions to the target; relative axes apply their delta to the previous position independently. Interpolation belongs to predicted movement only.
- Direct entity velocity is decoded and folded into ECS on the net thread, mirrored to the simulation channel, filtered by the local server entity id and replaces `PhysicsState.velocity` during the early network drain, before the next physics tick can send a position derived from the old velocity.

### Outbound relay

The client driver polls inbound packets and outbound actions fairly. The shell's outbound relay never blocks the render thread: movement and the empty `EndClientTick` marker use newest-value replacement lanes (the server uses the marker only to close the latest movement sample), while chat, commands, drops, uses and other controls use a bounded FIFO. Sequence tags merge the lanes without moving a retained action across a control queued before or after it. A held attack can keep the action side ready, so the driver must not bias select priority toward outbound work: the reader must get chances to receive and answer keep-alives.

Every accepted or coalesced action also signals a shared `Notify`. The net loop enables its notification before draining the relay, then waits on an inbound event or another outbound action, so a readiness acknowledgement reaches the driver even when the integrated server is silent behind its initial tick gate. Browser waits need no timer; native waits keep a 15 ms poll for local control channels; dropping `NetClient` signals the waiter after setting the stop flag.

### Tracing targets

- `net_join`: each decoded correction (raw relative flags, resolved pose), each adopted correction, every encoded outbound movement packet, and any explosion or direct velocity impulse applied to local predicted velocity. `net_velocity` covers per-entity velocity packets (hundreds in a busy lobby).
- `sim_input` records a changed local intent after its physics tick (shift/jump/sprint, resulting pose, position, velocity, ground contact); `net_input` the successfully encoded packet for it. The action queue puts input before that tick's movement, so a missing or reordered pair localises the fault to egress, while matching samples point to simulation or server reconciliation.
- `lodestone_keepalive` records the challenge id at client receive, before/after its automatic write, and at the server send/acknowledgement sites.

## How to change it

- Address selection lives in `lodestone-net::resolve`; keep the saved entry's optional port intact until it is called. Socket-versus-handshake routing goes through `ClientBuilder::connect_target`, never by replacing `ServerAddress`.
- A new loading phase: update `NetUpdate::ConnectPhase` and `menu::loading::ConnectPhase` together.
- Change outbound admission in `ActionRelaySender::send` and net-loop waiting in `run_async` together: arm the notification before the drain, signal replacement lanes as well as the FIFO, preserve native control polling. `outbound_relay_wakes_without_inbound_traffic_and_preserves_later_wakes` covers an idle waiter, a retained notification followed by a later action, and both coalescing lanes.

## Configuration

Remote networking is on in normal builds. Singleplayer-only native: `cargo check -p lodestone-shell --no-default-features --features live,window`. Browser deployment, build both halves without default features:

```text
(cd web && cargo check --no-default-features --target wasm32-unknown-unknown)
(cd web && cargo check -p lodestone-web-server --no-default-features)
```

The second matters: a singleplayer-only WASM bundle served by a relay-enabled native server still exposes an arbitrary-server route. Timeouts (10 s connect, 30 s remote idle) are fixed in `lodestone_shell::net`. Join logs without renderer noise: `RUST_LOG=warn,net=info,net_join=info just run`; add `sim_input=debug,net_input=debug` for input intents, `lodestone_keepalive=debug` (paired with the server target of the same name) for one keep-alive's hops. An airborne-Sneak investigation: `RUST_LOG=warn,net_join=debug,net_velocity=debug,sim_input=debug,net_input=debug just run --host <server>`.

## Dependencies

`lodestone-net` and system DNS through `hickory-resolver` (with `multiplayer`); `lodestone-client` for session startup and timeout errors; `lodestone-shell` for the server-list entry and loading screen; `tracing` in the client driver and controller.
