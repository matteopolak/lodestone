# Transfer tracing

## What it is

A single `tracing` target, `transfer`, that records every step of the client's
position handshake with a server — the teleport arriving, the confirmation going
out, the pose reaching the simulation, and each outbound movement packet with the
position it claims. It exists to settle, from a real session on a real server,
whether the client ever tells a server it is somewhere the server has already
overruled: the "rubberbanded after being transferred to another server" report.

It is an instrument, not a fix. Nothing branches on what it records.

## How to collect it

```bash
RUST_LOG=info,transfer=debug cargo run --release -p lodestone-shell --bin lodestone
```

Every line's message begins `xfer:`, so a log captured with any filter can be
narrowed with `grep xfer:`. The prefix is load-bearing: the shell's default
subscriber is built with `.with_target(false)`, so the target name never reaches
the output and there is nothing else to key on.

The one line that states a defect rather than a fact is emitted at `warn`, so it
survives even `RUST_LOG=warn`:

```
xfer: move packet -- FIRST move after a teleport claims a position far from the
teleport target; the server will read this as movement it did not authorise
```

## What the lines are

Reading down is reading the chain, in the order a teleport travels it.

| line | emitted by | says |
|---|---|---|
| `xfer: state -- TRANSFER` | `lodestone_v770`'s `handle_play_connection` / `handle_configuration` | the server asked us to move to another host, in Play or in Configuration |
| `xfer: state -- START_CONFIGURATION` / `FINISH_CONFIGURATION` | the same | the connection crossing Play ↔ Configuration — what a proxy backend switch looks like from here |
| `xfer: state -- connection state transition` | `lodestone_client`'s `Driver::execute` | the same edge as the driver sees it, which is where `in_play` (and so the shell's movement gate) flips |
| `xfer: LOGIN (join game) received` | `lodestone_v770`'s `handle_play_chunk` | a *second* join packet on a live connection: the other shape of a backend switch |
| `xfer: PLAYER_POSITION received; ACCEPT_TELEPORTATION echoed with the same id` | `lodestone_v770`'s `handle_player_position` | a teleport arrived, with id, target, and `relatives` mask; the confirmation goes out on this same call |
| `xfer: teleport forwarded to the sim channel` | `lodestone_shell`'s `net.rs` `forward` | the teleport left the driver for the simulation's channel |
| `xfer: teleport applied to the simulation` | `lodestone_shell`'s `sim/net_apply.rs` | the pose is finally ours — `from_*` is where we were, `moved` is how far the correction was |
| `xfer: Move queued for the socket` | `lodestone_shell`'s `Sim::drain_action_queue` | this tick's outbound claim, and the `teleport_count` the simulation held when it built it |
| `xfer: move packet` | `lodestone_v770`'s `select_move_packet` | the movement packet on the wire, and its distance from the last teleport target we accepted |

Every wire-side line carries `seq`, from a single process-wide counter, so lines
are strictly ordered even when the driver task and the shell's frame thread
interleave — and a gap in `seq` is a line the subscriber dropped rather than an
event that did not happen. Wall-clock timestamps cannot do that job: the window
this instrument exists to resolve is a fraction of one frame.

## The window it measures

Vanilla's client has no such window. `ClientPacketListener.handleMovePlayer`
applies the pose, sends `ServerboundAcceptTeleportationPacket`, and sends a
`ServerboundMovePlayerPacket.PosRot` at the **new** pose — three statements, one
call, one thread.

This client is split across two. The accept is written by the driver the instant
the packet decodes; the pose reaches `PhysicsState` a channel hop and up to a
frame later; and the simulation queues a `Move` from whatever pose it currently
holds on every 20 Hz tick. So a `Move` built before the teleport was applied, but
handed to the driver after the accept was written, claims a pre-teleport position
at a moment the server has already cleared `awaitingPositionFromClient` and reset
`lastGood*` to the teleport target.

`ServerGamePacketListenerImpl.handleMovePlayer` answers exactly that input with
*"moved wrongly!"* and a corrective teleport back — which is what a rubberband is.
The distances involved are what makes this show up after a transfer and not in
ordinary play: an anti-cheat correction moves you centimetres, a backend switch
moves you a world.

`select_move_packet` therefore reports `moves_since_teleport` and
`dist_from_teleport` on every outbound movement packet, and escalates to `warn`
when `moves_since_teleport == 0` and the distance exceeds one block. One tick of
ordinary movement is well under half a block, so a first post-teleport move
beyond a block did not come from a simulation that had adopted the teleport.

## How to change it

The distance verdict needs an absolute target. A teleport whose `relatives` mask
marks any of X/Y/Z relative is logged with its mask but does **not** become the
yardstick: the adapter holds no player position of its own and cannot resolve a
delta. If you widen the yardstick to relative teleports, the resolved pose has to
come from the shell, not from the adapter.

`moves_since_teleport` and `last_teleport` live on `MovementSendState` beside the
fields that mirror vanilla's `LocalPlayer`. They are diagnostic: no encode or
decode path reads them, and deleting them changes no packet. Keep it that way —
the moment a wire decision depends on one, this stops being an instrument.

The shell-side lines carry `teleport_count` (`Sim`'s existing counter, reset by
`Sim::end_session`) rather than `seq`, because the shell cannot depend on a
protocol family without breaking the version seam — `cargo check -p lodestone-shell
--no-default-features` is the check that enforces it. Their ordering against the
wire-side lines comes from their position in the log.

`Sim::drain_action_queue`'s loop is guarded by `tracing::enabled!`, so a build
without `transfer=debug` does not walk the drained batch at 20 Hz.

## Configuration

* `RUST_LOG` — `transfer=debug` for the whole chain; the `warn` line needs nothing.
* No feature flag, no environment variable of its own, no build-time gate.

## Dependencies

* `tracing`, already a dependency of all three crates involved.
* `lodestone_v770`'s `adapter::xfer` module carries the counter and the yardstick
  type; `lodestone-shell` and `lodestone-client` emit into the same target
  without linking it.
