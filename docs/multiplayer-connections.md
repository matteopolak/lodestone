# Multiplayer connections and join diagnostics

## What it is

The multiplayer connection path turns a saved or command-line server address into a concrete TCP endpoint while preserving the address presented during the protocol handshake. It also separates failures to establish TCP from silence after a session has started.

## How it works

### Build boundary

`lodestone-shell`'s `multiplayer` Cargo feature is the explicit authority for
remote joins and saved-server status probes. It is enabled by the default build
and is intentionally independent of `live`: `live` selects the protocol family
used by both remote joins and the in-memory integrated server, whereas
`multiplayer` decides whether a shell build may contact a server it does not
own or publish its integrated world to other machines. Without it, the title screen keeps its Multiplayer button in place but
renders it disabled and explains why on hover; command-line and terminal
remote-join surfaces return a named refusal instead of opening a socket.

The browser package forwards the same feature. Its paired native page server
also has `multiplayer` on by default; disabling it removes the `/relay`
WebSocket-to-TCP route and does not link `lodestone-relay`, so serving a
singleplayer-only page cannot turn that page host into an arbitrary TCP proxy.

Movement reconciliation has a dedicated `net_join` tracing target. It records
each decoded server correction with its raw relative flags and resolved pose,
each correction adopted by the simulation, every encoded outbound movement
packet, and any explosion or direct velocity impulse applied to the local
predicted velocity. Run
`RUST_LOG=warn,net_join=debug just run` for a short reproduction without enabling
unrelated renderer or shader diagnostics. Per-entity velocity packets use the
separate `net_velocity` target because a busy lobby can send hundreds of them
while only local correction timing is under investigation.

Input transitions have two intentionally low-volume targets. `sim_input`
records only a changed local intent after its physics tick, including
shift/jump/sprint and the resulting pose, position, velocity, and ground
contact. `net_input` records the successfully encoded packet for that intent
with its packet metadata. For an airborne Sneak investigation, run
`RUST_LOG=warn,net_join=debug,net_velocity=debug,sim_input=debug,net_input=debug`
followed by `just run --host java.mineplex.com`.
The action queue preserves input before that tick's movement action; a missing
or differently ordered pair therefore localizes the fault to egress, while
matching samples point to simulation or server reconciliation.

Direct entity velocity is decoded and folded into ECS on the net thread, then
mirrored to the simulation channel. The simulation filters that mirror by the
local server entity id and replaces `PhysicsState.velocity` during its early
network drain, before the next physics tick can send a position derived from
the previous velocity. The `net_join` trace records that application with both
the prior and replacement vectors.

Protocol 776 position corrections carry both pose and velocity. The adapter
preserves all nine relative bits, the shell resolves local-player velocity, and
the entity ingest path applies the same component-wise rule to remote entity
teleports. Treating every correction as a stop produces repeated vertical
disagreement on proxy-authored movement and turns server impulses into visible
snaps.

For a position correction, the adapter opens a correction transaction and
surfaces the event before its acknowledgement can reach the wire. The shell
adopts the authoritative pose, resolves relative velocity, and returns the
resolved position and rotation to the driver. Only then does the driver write
the acknowledgement and an unconditional full position-and-look echo with both
ground-contact flags clear. This is a rendezvous, not a spatial heuristic.

Outbound actions carry a monotonic correction generation. Movement submitted
before the shell completes the transaction is discarded; movement submitted
after completion has the next generation and is retained even if it was already
queued. Keep-alives and other non-movement actions remain live across the
boundary. This keeps a delayed pre-correction claim off the wire without
rewriting a valid post-correction position.

An absolute correction snaps both the current and previous camera positions to
its target. Relative axes apply their delta independently to the previous
position. Interpolation belongs to predicted movement; treating absolute server
placements as interpolation samples delays the authoritative camera pose.

A saved server entry retains its port as `Option<u16>`. CLI parsing likewise records whether `--port` appeared instead of treating its display default as entered. An explicit port is dialed unchanged. A bare hostname is passed to `lodestone_net::resolve_server_address`, which checks `_minecraft._tcp.<host>` and uses the selected SRV target when present, otherwise falling back to port `25565`.

The resolved host and port are supplied through `ClientBuilder::connect_target`; the original `ServerAddress` remains untouched for the handshake. This distinction matters for virtual-hosting proxies, which route using the hostname the player entered even when DNS directs the socket elsewhere.

TCP establishment has a 10-second budget and reports `ClientError::ConnectTimeout`. Once connected, the packet reader has a separate 30-second idle budget and reports `ClientError::Timeout`. A read timeout logs the current protocol state, received-packet count, and last packet ID under the `net_join` target.

## How to change it

Change address selection in `lodestone-net::resolve`; keep the saved entry's optional port intact until that function is called. Change socket-versus-handshake routing through `ClientBuilder::connect_target`, not by replacing `ServerAddress`. If the loading screen needs another phase, update `NetUpdate::ConnectPhase` and `menu::loading::ConnectPhase` together.

The packet ID in a timeout diagnostic is state-relative: interpret it together with the logged `ConnectionState` and the selected protocol adapter. Do not treat the same numeric ID as one packet across protocol families or states.

## Configuration

Remote networking is on in normal builds. For a singleplayer-only native shell,
omit its default features and select only the presentation/protocol features
needed by the build, for example:

```text
cargo check -p lodestone-shell --no-default-features --features live,window
```

For the browser deployment, build both halves without their default features:

```text
(cd web && cargo check --no-default-features --target wasm32-unknown-unknown)
(cd web && cargo check -p lodestone-web-server --no-default-features)
```

The second command is important: a singleplayer-only WASM bundle served by a
relay-enabled native server would still leave an arbitrary-server route exposed.

The shell currently fixes TCP connection timeout at 10 seconds and inbound packet idle timeout at 30 seconds in `lodestone_shell::net`. For focused join logs without GPU or shader compiler noise, run:

```text
RUST_LOG=warn,net=info,net_join=info just run
```

Add `sim_input=debug,net_input=debug` to trace changed local input intents and
their encoded packets without enabling per-tick input logs.

An explicitly entered port suppresses SRV lookup. A bare hostname enables it.

## Dependencies

Address resolution uses `lodestone-net` and the system DNS configuration through `hickory-resolver` when `multiplayer` is enabled. Session startup and timeout errors come from `lodestone-client`; `lodestone-shell` owns the server-list entry and loading screen. The focused tracing targets use `tracing` in the client driver and controller.
