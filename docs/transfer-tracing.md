# Transfer tracing

## What it is

A single `tracing` target, `transfer`, that records every step of the client's
position handshake with a server — the teleport arriving, the confirmation going
out, the pose reaching the simulation, and each outbound movement packet with the
position it claims. It exists to settle, from a real session on a real server,
whether the client ever tells a server it is somewhere the server has already
overruled: the "rubberbanded after being transferred to another server" report.

Nothing on the wire branches on what the target records — the fields it adds to
the adapter are read by the log and by nothing else. Two defects the trace was
built to expose are fixed, and both are described below.

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
| `xfer: state -- TRANSFER` | `lodestone_v770`'s `handle_play_connection` / `handle_configuration` | `path = "reconnect"` — the server asked us to dial another address |
| `xfer: state -- START_CONFIGURATION` / `FINISH_CONFIGURATION` | the same | `path = "backend-swap"` — the connection crossing Play ↔ Configuration on one socket |
| `xfer: state -- connection state transition` | `lodestone_client`'s `Driver::execute` | the same edge as the driver sees it, which is where `in_play` (and so the shell's movement gate) flips |
| `xfer: LOGIN (join game) received` | `lodestone_v770`'s `handle_play_chunk` | a join packet and its `login_ordinal` on this connection — `1` is a fresh join, anything higher is a backend swap |
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

## Two paths, and they are not the same thing

"Being moved to another server" has two mechanisms with almost nothing in common.
Every line above carries a `path` field naming which one fired.

| | `path = "reconnect"` | `path = "backend-swap"` |
|---|---|---|
| what the server sends | `minecraft:transfer` (a new host and port) | `START_CONFIGURATION`, a configuration round, then a second `LOGIN` |
| the socket | torn down; a new one is dialled | **the same one throughout** |
| per-connection state | rebuilt from nothing — new adapter, new driver, new counters | **carried over**, including everything the adapter holds |
| `login_ordinal` | `1` again, on the new connection | `2`, `3`, … |

Velocity and BungeeCord use the **second**. They never send `minecraft:transfer`
at all, so a log with no `TRANSFER` line and `login_ordinal = 2` is a backend
swap — and that is the case where stale state, rather than absent state, is the
hazard. Grep `login_ordinal` first.

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

## What is fixed, and what a run would still show

Two things this window produced are closed. Both keep emitting on the `transfer`
target, so a log still shows them firing rather than only their absence.

**A `Move` never claims a pose the server has overruled.** `net.rs` counts the
teleports it has put on the sim's channel against the ones `Sim::poll_net` has
adopted, and while it is behind it rewrites any outbound `Move` to the pose the
teleport authorised — position, resolvable rotation, and vanilla's own `false`
on-ground and horizontal-collision flags. That is the claim vanilla's client
makes in the same instant. It rewrites rather than drops for two reasons: the
dirty-tracking that decides whether a `Move` becomes a packet at all lives
*downstream* in `select_move_packet`, whose 20-tick position reminder only
advances when it is invoked, so a producer-side drop silently starves the
periodic resync; and a simulation that somehow never adopted a teleport would
stop being able to move at all, where a rewrite merely holds it at the pose the
server chose. A teleport with a relative positional component authorises nothing
— the net thread cannot resolve a delta against a pose that lives on the frame
thread — which is the harmless direction, a relative correction being a small
one. Look for `xfer: outbound Move rewritten`.

**A second `LOGIN` drops the previous backend's world.** Vanilla's `handleLogin`
assigns a brand-new `ClientLevel` unconditionally, so every chunk and every
entity of what came before is gone before the next packet decodes. This client
cleared its decoded chunk store only on `Respawned`, and only when the dimension
id differed — neither of which a backend swap satisfies, since it emits no
`Respawned` at all and usually lands in `minecraft:overworld` again. The old
backend's terrain therefore sat in the store at the new backend's coordinates,
where the shell's own physics collides against it; a client predicting from
terrain the server does not have is a client the server corrects. The store is
now cleared in `Driver::emit`'s `Login` arm — on the net thread, between two
decodes, which is the only point where the ordering is safe — and the shell
drops the matching meshes and entity tracks when a `LoggedIn` arrives while
already `Connected`. Look for `xfer: cleared the decoded chunk store` and
`xfer: second LOGIN while connected`.

Neither has been observed against a real proxy: they are a divergence from the
decompiled client and a window the architecture demonstrably has, closed with
hermetic gates and their neuters. The `warn` line is what a real run has to be
read for.

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
