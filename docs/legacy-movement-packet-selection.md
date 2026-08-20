# Legacy movement packet selection

## What it is

The v47 (Minecraft 1.8.9), v340 (1.12.2), and v735 (1.16.5) protocol adapters
select a serverbound movement packet from the pose actually last transmitted on
that connection. This keeps the controller's per-tick `ClientAction::Move`
producer independent from each protocol's wire cadence.

## How it works

Each adapter owns an `Arc<Mutex<MovementSendState>>`; the state is connection
state, not a controller resource or a global throttle. Position is dirty when
the squared distance from the last sent position is strictly greater than
`9e-4`, and rotation is dirty when either angle changes exactly. A position and
rotation update selects `position_look`; either one alone selects `position` or
`look`.

v340 and v735 increment their `position_reminder` before that comparison and
force a `position` update when it reaches 20. When neither pose is dirty, they
send the one-byte `flying` packet only if `on_ground` changed; otherwise they
send nothing. They deliberately ignore `horizontal_collision`, which their
wire packets cannot carry.

v47 has the same `9e-4` position threshold but an older idle rule: it always
sends `flying` when neither position nor rotation is dirty. It checks the
reminder before incrementing it, so after a position update it sends 20 idle
`flying` packets and forces `position` on the 21st idle movement action.

All stored fields begin at Java's zero defaults: zero position and rotation,
false on-ground, and zero reminder. Thus the first action only sends a pose
packet when it differs from that baseline (or, for v340/v735, when it changes
the initial false on-ground value).

The source evidence is Mojang client bytecode: 1.8.9 `bew.p()` and 1.12.2
`bud.N()` from the checked-in cache, plus 1.16.5's official client artifact
resolved through Mojang's `piston-meta` version manifest and `piston-data`
download endpoint. That client JAR's SHA-1 is
`37fd3c903861eeff3bc24b71eed48f828b5269c8`; Mojang's official client mappings
resolve `LocalPlayer -> dzm` and `sendPosition -> O`. The tests in each
protocol crate pin decoded packet bodies and wire bytes, rather than only
individual packet ids.

## How to change it

Keep state next to the owning adapter and call selection for every
`ClientAction::Move`; moving the gate upstream breaks each version's reminder
counter. Update the matching `tests/movement_selection.rs` whenever selection
logic changes. Do not merge these into v770's tracker without first preserving
the version differences: v770 uses a `(2e-4)^2` threshold and additionally
tracks horizontal collision, while v47 must keep its every-tick idle packet.

## Configuration

There is no runtime configuration. Protocol choice determines the rule:
`PROTOCOL = 47`, `340`, or `754` (the v735 directory represents 1.16.5's
protocol 754).

## Dependencies

The feature depends on `lodestone-model` for `ClientAction`, `Vec3`,
`Rotation`, and `VersionAdapter`; `lodestone-core` supplies packet encoding.
It is consumed by `lodestone-client`'s driver, which owns one adapter instance
for a connection, and by `lodestone-controller`, which deliberately emits the
movement action on every simulation tick.
